//! The real-hardware backend: one DRM device (tegra-drm), one output (the internal panel), GBM + EGL +
//! `GlesRenderer` on grate, `DrmCompositor` with damage tracking and direct scanout, libinput, libseat.
//!
//! Surface RT specifics (DESKTOP-SHELL.md Log, notes/12-desktop-shell.md):
//! - Swapchain formats ARGB8888 then XRGB8888 only: grate cannot render to ABGR8888.
//! - No cursor plane: plane 87 shows nothing via Smithay, so the cursor is a GL element (render/cursor.rs).
//! - Redraws happen only when something asked for one (`queue_redraw`); no timers while idle.
//!
//! Debug switches (environment): `RTSHELL_DRM_DEVICE=/dev/dri/cardN`, `RTSHELL_NO_SCANOUT=1` (always
//! composite), `RTSHELL_NO_OVERLAY=1` (primary plane only), `RTSHELL_DEBUG_FPS=1` (log frames/s and the CPU time
//! of render_frame, which includes SHM uploads, every 2 s while frames are being drawn).

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            format::FormatSet,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmAccessError, DrmDevice, DrmDeviceFd, DrmError, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode,
            NodeType,
            compositor::{FrameFlags, PrimaryPlaneElement},
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDevice, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, ImportMemWl, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{all_gpus, primary_gpu},
    },
    desktop::utils::OutputPresentationFeedback,
    input::keyboard::LedState,
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::{
            EventLoop, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{ModeTypeFlags, connector, crtc},
        input::{self as libinput, DeviceCapability, Libinput},
        rustix::fs::OFlags,
        wayland_protocols::wp::{
            linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1, presentation_time::server::wp_presentation_feedback,
        },
    },
    utils::{DeviceFd, Monotonic, Time, Transform},
    wayland::{dmabuf::DmabufFeedbackBuilder, presentation::Refresh},
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner, SimpleCrtcMapper};
use tracing::{debug, error, info, warn};

use super::Backend;
use crate::{
    render::{self, SurfaceDmabufFeedback},
    state::State,
};

const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];

type GbmDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, Option<OutputPresentationFeedback>, DrmDeviceFd>;
type GbmDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

/// Frame scheduling for the output. See `queue_redraw`.
#[derive(Debug)]
enum RedrawState {
    /// Nothing to do; no timers armed.
    Idle,
    /// A render is scheduled on the event loop.
    Queued,
    /// A frame was queued to KMS; wait for its vblank. `redraw_needed`: render again right after.
    WaitingForVBlank { redraw_needed: bool },
    /// The last render produced no damage; a timer stands in for the vblank so clients that keep
    /// committing are paced at the refresh rate.
    WaitingForEstimatedVBlank { token: RegistrationToken, queued: bool },
}

struct OutputSurface {
    output: Output,
    drm_output: GbmDrmOutput,
    frame_duration: Duration,
    redraw: RedrawState,
    dmabuf_feedback: Option<SurfaceDmabufFeedback>,
    /// Last logged direct-scanout state, to log changes only.
    scanout: Option<bool>,
    stats: Option<FrameStats>,
    /// The output is off (idle blank): nothing is rendered until it is turned on again.
    off: bool,
    /// Turn it off at the next vblank (a page flip was pending when blanking was asked for).
    clear_at_vblank: bool,
}

/// RTSHELL_DEBUG_FPS: counted in render(), logged from there (no timer, so idle stays idle).
struct FrameStats {
    since: Instant,
    frames: u32,
    empty: u32,
    total: Duration,
    max: Duration,
    last: Instant,
}

impl FrameStats {
    fn new() -> Self {
        FrameStats {
            since: Instant::now(),
            frames: 0,
            empty: 0,
            total: Duration::ZERO,
            max: Duration::ZERO,
            last: Instant::now(),
        }
    }

    fn record(&mut self, took: Duration, queued: bool) {
        if queued {
            self.frames += 1;
        } else {
            self.empty += 1;
        }
        self.total += took;
        self.max = self.max.max(took);
        let elapsed = self.since.elapsed();
        if elapsed >= Duration::from_secs(2) {
            let n = (self.frames + self.empty).max(1);
            info!(
                fps = format!("{:.1}", self.frames as f64 / elapsed.as_secs_f64()),
                empty = self.empty,
                avg_ms = format!("{:.2}", self.total.as_secs_f64() * 1000.0 / n as f64),
                max_ms = format!("{:.2}", self.max.as_secs_f64() * 1000.0),
                "frame stats"
            );
            *self = FrameStats::new();
        }
    }
}

pub struct UdevData {
    session: LibSeatSession,
    libinput: Libinput,
    renderer: GlesRenderer,
    drm: GbmDrmOutputManager,
    surface: Option<OutputSurface>,
    keyboards: Vec<libinput::Device>,
    /// Every input device, for settings changed at runtime (`reconfigure_input`).
    devices: Vec<libinput::Device>,
    frame_flags: FrameFlags,
}

impl UdevData {
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    pub fn screenshot_target(&mut self) -> Option<(&mut GlesRenderer, Output)> {
        if !self.session.is_active() {
            return None;
        }
        let output = self.surface.as_ref()?.output.clone();
        Some((&mut self.renderer, output))
    }

    pub fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            warn!(vt, ?err, "VT switch failed");
        }
    }

    pub fn update_led_state(&mut self, leds: LedState) {
        for kb in &mut self.keyboards {
            kb.led_update(leds.into());
        }
    }
}

fn drm_device_path(seat: &str) -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("RTSHELL_DRM_DEVICE") {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = primary_gpu(seat).map_err(|e| e.to_string())? {
        return Ok(p);
    }
    all_gpus(seat)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "no DRM device found".to_string())
}

/// Seat name for the State's wl_seat, without opening anything yet.
pub fn new_session() -> Result<(LibSeatSession, smithay::backend::session::libseat::LibSeatSessionNotifier), String> {
    LibSeatSession::new().map_err(|e| format!("cannot open a libseat session (run from a VT as a logged-in user): {e}"))
}

pub fn init(
    state: &mut State,
    event_loop: &mut EventLoop<'static, State>,
    session: LibSeatSession,
    notifier: smithay::backend::session::libseat::LibSeatSessionNotifier,
) -> Result<(), String> {
    let seat = session.seat();
    let mut session = session;

    // --- DRM device, GBM, EGL, renderer
    let path = drm_device_path(&seat)?;
    info!(path = %path.display(), "opening DRM device");
    let fd = session
        .open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let node = DrmNode::from_file(&fd).map_err(|e| e.to_string())?;
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true).map_err(|e| e.to_string())?;
    let gbm = GbmDevice::new(fd).map_err(|e| e.to_string())?;

    let display = unsafe { EGLDisplay::new(gbm.clone()) }.map_err(|e| format!("EGL display: {e}"))?;
    let render_node = EGLDevice::device_for_display(&display)
        .ok()
        .and_then(|d| d.try_get_render_node().ok().flatten())
        .or_else(|| node.node_with_type(NodeType::Render).and_then(|n| n.ok()))
        .unwrap_or(node);
    let context = EGLContext::new(&display).map_err(|e| format!("EGL context: {e}"))?;
    let renderer = unsafe { GlesRenderer::new(context) }.map_err(|e| format!("GLES renderer: {e}"))?;
    info!(?node, ?render_node, "renderer ready");

    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    // NodeFilter::All: Smithay refuses to scan out a client dmabuf whose recorded node differs from the
    // exporter's. Our scanout feedback tranche names the primary node (card0) while rendering uses the
    // render node; both are the same Tegra device, so accept either.
    let exporter = GbmFramebufferExporter::new(gbm.clone(), smithay::backend::drm::exporter::gbm::NodeFilter::All);
    // `None` for the cursor GBM device: no cursor plane (it shows nothing on tegra, see module docs).
    let drm = DrmOutputManager::new(drm, allocator, exporter, None, COLOR_FORMATS, render_formats);

    // SCANOUT_ANY: Chromium's buffers are ABGR8888 while the swapchain is ARGB8888; the primary plane takes
    // both (modetest), and the plain flag only allows the swapchain's format.
    let mut frame_flags = FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT
        | FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY
        | FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT;
    if std::env::var_os("RTSHELL_NO_SCANOUT").is_some() {
        info!("direct scanout disabled (RTSHELL_NO_SCANOUT)");
        frame_flags = FrameFlags::empty();
    }

    let mut data = UdevData {
        session: session.clone(),
        libinput: Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into()),
        renderer,
        drm,
        surface: None,
        keyboards: Vec::new(),
        devices: Vec::new(),
        frame_flags,
    };

    // --- the output
    let surface = setup_output(state, &mut data, render_node)?;
    data.surface = Some(surface);

    // --- linux-dmabuf v4 with the render node as main device
    let default_feedback = DmabufFeedbackBuilder::new(render_node.dev_id(), data.renderer.dmabuf_formats())
        .build()
        .map_err(|e| e.to_string())?;
    let global = state
        .dmabuf_state
        .create_global_with_default_feedback::<State>(&state.display_handle, &default_feedback);
    state.dmabuf_global = Some(global);
    state.shm_state.update_formats(data.renderer.shm_formats());

    // --- input
    data.libinput
        .udev_assign_seat(&seat)
        .map_err(|_| format!("libinput: cannot assign seat {seat}"))?;
    let input_backend = LibinputInputBackend::new(data.libinput.clone());
    event_loop
        .handle()
        .insert_source(input_backend, |mut event, _, state| {
            use smithay::backend::input::InputEvent;
            match &mut event {
                InputEvent::DeviceAdded { device } => configure_device(state, device),
                InputEvent::DeviceRemoved { device } => {
                    if let Backend::Udev(d) = &mut state.backend {
                        d.keyboards.retain(|k| k != device);
                        d.devices.retain(|k| k != device);
                    }
                }
                _ => {}
            }
            state.process_input_event(event);
        })
        .map_err(|e| e.to_string())?;

    // --- session (VT switches)
    event_loop
        .handle()
        .insert_source(notifier, |event, _, state| on_session_event(state, event))
        .map_err(|e| e.to_string())?;

    // --- vblanks
    event_loop
        .handle()
        .insert_source(drm_notifier, |event, meta, state| match event {
            DrmEvent::VBlank(crtc) => on_vblank(state, crtc, meta),
            DrmEvent::Error(err) => error!(?err, "DRM error"),
        })
        .map_err(|e| e.to_string())?;

    state.backend = Backend::Udev(data);
    state.queue_redraw();
    Ok(())
}

fn setup_output(state: &mut State, data: &mut UdevData, render_node: DrmNode) -> Result<OutputSurface, String> {
    let device = data.drm.device();
    let mut scanner = DrmScanner::<SimpleCrtcMapper>::new();
    let scan = scanner.scan_connectors(device).map_err(|e| e.to_string())?;
    let mut candidates: Vec<(connector::Info, crtc::Handle)> = scan
        .into_iter()
        .filter_map(|e| match e {
            DrmScanEvent::Connected { connector, crtc: Some(crtc) } => Some((connector, crtc)),
            _ => None,
        })
        .collect();
    // Internal panel first (LVDS on the Surface RT); HDMI is a non-goal for now.
    candidates.sort_by_key(|(c, _)| {
        !matches!(
            c.interface(),
            connector::Interface::LVDS | connector::Interface::EmbeddedDisplayPort | connector::Interface::DSI
        )
    });
    for (c, _) in candidates.iter().skip(1) {
        info!(connector = %format!("{}-{}", c.interface().as_str(), c.interface_id()), "ignoring extra connector (single output)");
    }
    let (connector, crtc) = candidates.into_iter().next().ok_or("no connected display")?;

    let name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
    let mode = connector
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| connector.modes().first())
        .copied()
        .ok_or("connector has no modes")?;
    let wl_mode = WlMode::from(mode);
    let (phys_w, phys_h) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: connector.subpixel().into(),
            make: "Microsoft".into(),
            model: "Surface RT panel".into(),
            serial_number: "Unknown".into(),
        },
    );
    let _global = output.create_global::<State>(&state.display_handle);
    output.set_preferred(wl_mode);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        Some(smithay::output::Scale::Integer(1)),
        Some((0, 0).into()),
    );
    state.space.map_output(&output, (0, 0));
    info!(%name, ?crtc, size = ?wl_mode.size, refresh_mhz = wl_mode.refresh, "output");

    let mut planes = device.planes(&crtc).map_err(|e| e.to_string())?;
    planes.cursor.clear();
    if std::env::var_os("RTSHELL_NO_OVERLAY").is_some() {
        info!("overlay planes disabled (RTSHELL_NO_OVERLAY)");
        planes.overlay.clear();
    }

    let drm_output = data
        .drm
        .lock()
        .initialize_output::<_, render::OutputElement>(
            crtc,
            mode,
            &[connector.handle()],
            &output,
            Some(planes),
            &mut data.renderer,
            &DrmOutputRenderElements::default(),
        )
        .map_err(|e| format!("cannot initialise the output: {e}"))?;
    info!(format = ?drm_output.format(), "swapchain");

    let dmabuf_feedback = surface_dmabuf_feedback(&data.renderer, render_node, &drm_output);
    let frame_duration = Duration::from_secs_f64(1000.0 / wl_mode.refresh.max(1) as f64);

    Ok(OutputSurface {
        output,
        drm_output,
        frame_duration,
        redraw: RedrawState::Idle,
        dmabuf_feedback,
        scanout: None,
        stats: std::env::var_os("RTSHELL_DEBUG_FPS").is_some().then(FrameStats::new),
        off: false,
        clear_at_vblank: false,
    })
}

/// Scanout tranche: formats the primary/overlay planes can show and we can also render from, so there is
/// always a fallback when a buffer cannot be scanned out.
fn surface_dmabuf_feedback(
    renderer: &GlesRenderer,
    render_node: DrmNode,
    drm_output: &GbmDrmOutput,
) -> Option<SurfaceDmabufFeedback> {
    let render_formats = renderer.dmabuf_formats();
    let (planes, plane_formats) = drm_output.with_compositor(|c| {
        let s = c.surface();
        (s.planes().clone(), s.plane_info().formats.clone())
    });
    let scanout_formats = plane_formats
        .iter()
        .copied()
        .chain(planes.overlay.into_iter().flat_map(|p| p.formats))
        .collect::<FormatSet>()
        .intersection(&render_formats)
        .copied()
        .collect::<FormatSet>();
    let builder = DmabufFeedbackBuilder::new(render_node.dev_id(), render_formats.clone());
    let render = builder.clone().build().ok()?;
    let scanout_dev = drm_output.with_compositor(|c| c.surface().device_fd().dev_id().ok())?;
    let scanout = builder
        .add_preference_tranche(scanout_dev, zwp_linux_dmabuf_feedback_v1::TrancheFlags::Scanout, scanout_formats, 4u32..=6)
        .build()
        .ok()?;
    Some(SurfaceDmabufFeedback { render, scanout })
}

/// Touchpad / pointer settings from `[input]` (at device add and when Settings changes them).
fn apply_input(device: &mut libinput::Device, cfg: &crate::config::Input) {
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(cfg.tap_to_click);
    }
    if device.config_scroll_has_natural_scroll() {
        let _ = device.config_scroll_set_natural_scroll_enabled(cfg.natural_scroll);
    }
    if device.config_accel_is_available() {
        let _ = device.config_accel_set_speed(cfg.pointer_accel);
    }
}

impl UdevData {
    /// `[input]` changed at runtime: apply it to every device.
    pub fn reconfigure_input(&mut self, cfg: &crate::config::Input) {
        for d in &mut self.devices {
            apply_input(d, cfg);
        }
    }
}

fn configure_device(state: &mut State, device: &mut libinput::Device) {
    let cfg = state.config.input.clone();
    let caps: Vec<&str> = [
        (DeviceCapability::Keyboard, "keyboard"),
        (DeviceCapability::Pointer, "pointer"),
        (DeviceCapability::Touch, "touch"),
        (DeviceCapability::Switch, "switch"),
        (DeviceCapability::Gesture, "gesture"),
    ]
    .into_iter()
    .filter(|(c, _)| device.has_capability(*c))
    .map(|(_, n)| n)
    .collect();
    info!(name = %device.name(), ?caps, "libinput device");
    apply_input(device, &cfg);
    if let Backend::Udev(d) = &mut state.backend {
        d.devices.push(device.clone());
    }
    if device.has_capability(DeviceCapability::Keyboard) {
        let leds = state.seat.get_keyboard().map(|k| k.led_state());
        if let Backend::Udev(d) = &mut state.backend {
            if let Some(leds) = leds {
                device.led_update(leds.into());
            }
            d.keyboards.push(device.clone());
        }
    }
}

fn on_session_event(state: &mut State, event: SessionEvent) {
    let Backend::Udev(d) = &mut state.backend else { return };
    match event {
        SessionEvent::PauseSession => {
            info!("session paused (VT switch away)");
            d.libinput.suspend();
            d.drm.pause();
            if let Some(s) = d.surface.as_mut() {
                if let RedrawState::WaitingForEstimatedVBlank { token, .. } = s.redraw {
                    state.loop_handle.remove(token);
                }
                s.redraw = RedrawState::Idle;
            }
            state.idle_pause();
        }
        SessionEvent::ActivateSession => {
            info!("session active again");
            if d.libinput.resume().is_err() {
                error!("cannot resume libinput");
            }
            if let Err(err) = d.drm.lock().activate(false) {
                error!(?err, "cannot re-activate DRM");
            }
            if let Some(s) = d.surface.as_mut() {
                s.drm_output.reset_buffers();
                s.redraw = RedrawState::Idle;
            }
            state.queue_redraw();
            // Back on this VT counts as activity (turns a blank output on again), timer from now.
            state.idle_resume();
        }
    }
}

pub fn queue_redraw(state: &mut State) {
    let Backend::Udev(d) = &mut state.backend else { return };
    let Some(s) = d.surface.as_mut() else { return };
    if s.off {
        return;
    }
    match &mut s.redraw {
        RedrawState::Idle => {
            s.redraw = RedrawState::Queued;
            state.loop_handle.insert_idle(render);
        }
        RedrawState::Queued => {}
        RedrawState::WaitingForVBlank { redraw_needed } => *redraw_needed = true,
        RedrawState::WaitingForEstimatedVBlank { queued, .. } => *queued = true,
    }
}

fn is_inactive_error(err: &DrmError) -> bool {
    matches!(err, DrmError::DeviceInactive)
        || matches!(err, DrmError::Access(DrmAccessError { source, .. }) if source.kind() == std::io::ErrorKind::PermissionDenied)
}

fn render(state: &mut State) {
    let decorations = state.decoration_elements();
    let Backend::Udev(d) = &mut state.backend else { return };
    let Some(s) = d.surface.as_mut() else { return };
    if !matches!(s.redraw, RedrawState::Queued) {
        return;
    }
    if !d.session.is_active() || s.off {
        s.redraw = RedrawState::Idle;
        return;
    }

    let output = s.output.clone();
    let started = Instant::now();
    state.frame_clock.refresh = s.frame_duration;
    let present_at = state.frame_clock.next_present(state.clock.now().into());
    let size = crate::rotate::ui_size(&output);
    let locked = state.lock.locked;
    let (ui, panel) = render::prepare_ui(&mut state.switch, &mut state.overview, &mut state.overlay, &mut d.renderer, size, present_at, locked);
    let panel_shown = !panel.is_empty();
    let lock_drawn = locked && panel_shown;
    let ui_done = started.elapsed();
    state.last_present = present_at;
    let elements = render::output_elements(
        &mut d.renderer,
        &state.space,
        &output,
        &mut state.cursor,
        state.pointer.current_location(),
        ui,
        panel,
        state.switch.as_ref(),
        state.overview.as_ref(),
        decorations,
        locked,
    );
    // Smithay puts the bottom element on the primary plane only if it is opaque and covers the output, or
    // if the clear colour is black (the CRTC background). Chromium's ABGR8888 buffers are not reported
    // opaque, so clear with black whenever a window covers the output: the theme colour would be hidden
    // behind it anyway, and this is what enables direct scanout of a fullscreen client.
    let covered = !locked && state
        .space
        .output_geometry(&output)
        .is_some_and(|geo| state.space.elements_for_output(&output).any(|w| {
            state.space.element_geometry(w).is_some_and(|g| g.contains_rect(geo))
        }));
    let clear = if covered { [0.0, 0.0, 0.0, 1.0] } else { state.theme.colors.background.premultiplied() };
    let n_elements = elements.len();
    let result = s.drm_output.render_frame(&mut d.renderer, &elements, clear, d.frame_flags);

    let (is_empty, states) = match result {
        Ok(r) => {
            let scanout = matches!(r.primary_element, PrimaryPlaneElement::Element(_));
            if s.scanout != Some(scanout) {
                info!(direct_scanout = scanout, overlays = r.overlay_elements.len(), "primary plane");
                s.scanout = Some(scanout);
            }
            (r.is_empty, r.states)
        }
        Err(err) => {
            warn!(%err, "render failed");
            s.redraw = RedrawState::Idle;
            return;
        }
    };
    drop(elements);

    let mut queued = false;
    if !is_empty {
        let feedback = render::take_presentation_feedback(&state.space, &output, &states);
        match s.drm_output.queue_frame(Some(feedback)) {
            Ok(()) => queued = true,
            Err(err) => {
                let inactive = matches!(&err, smithay::backend::drm::compositor::FrameError::DrmError(e) if is_inactive_error(e));
                if inactive {
                    debug!("frame not queued: session inactive");
                    s.redraw = RedrawState::Idle;
                    return;
                }
                warn!(%err, "queue_frame failed");
            }
        }
    }

    if let Some(sw) = state.switch.as_mut() {
        let took = started.elapsed();
        sw.max_render = sw.max_render.max(took);
        if s.stats.is_some() && took > slow_frame() {
            let ms = |d: Duration| format!("{:.1}", d.as_secs_f32() * 1000.0);
            info!(frame = sw.frames, ui_ms = ms(ui_done), total_ms = ms(took), "slow switch frame");
        }
    }
    if let Some(ov) = state.overview.as_mut() {
        let took = started.elapsed();
        ov.max_render = ov.max_render.max(took);
        if s.stats.is_some() && took > slow_frame() {
            let ms = |d: Duration| format!("{:.1}", d.as_secs_f32() * 1000.0);
            info!(frame = ov.frames, ui_ms = ms(ui_done), total_ms = ms(took), elements = n_elements, open = ov.open.value, "slow overview frame");
        }
    }
    if panel_shown {
        let took = started.elapsed();
        let pf = state.panel_frames.get_or_insert_default();
        let now = Instant::now();
        if let Some(gap) = pf.last.map(|l| now - l).filter(|g| *g < Duration::from_millis(100)) {
            pf.max_gap = pf.max_gap.max(gap);
            if gap > s.frame_duration * 3 / 2 {
                pf.late += 1;
            }
        }
        pf.last = Some(now);
        pf.frames += 1;
        pf.max_render = pf.max_render.max(took);
        if s.stats.is_some() && took > slow_frame() {
            let ms = |d: Duration| format!("{:.1}", d.as_secs_f32() * 1000.0);
            info!(frame = pf.frames, ui_ms = ms(ui_done), total_ms = ms(took), elements = n_elements, "slow top layer frame");
        }
    } else if let Some(pf) = state.panel_frames.take() {
        let ms = |d: Duration| format!("{:.1}", d.as_secs_f32() * 1000.0);
        info!(frames = pf.frames, late = pf.late, max_gap_ms = ms(pf.max_gap), max_render_ms = ms(pf.max_render), "top layer frames");
    }
    if let Some(stats) = s.stats.as_mut() {
        if stats.last.elapsed() > Duration::from_millis(500) {
            // Drawing resumed after a pause: start a fresh window so idle time does not dilute fps.
            *stats = FrameStats::new();
        }
        stats.last = Instant::now();
        stats.record(started.elapsed(), queued);
    }
    if lock_drawn {
        // Reported at its vblank (or right away below if nothing was queued: the screen already shows it).
        state.lock.frame_queued = true;
    }
    if queued {
        s.redraw = RedrawState::WaitingForVBlank { redraw_needed: false };
    } else {
        let token = state
            .loop_handle
            .insert_source(Timer::from_duration(s.frame_duration), |_, _, state| {
                on_estimated_vblank(state);
                TimeoutAction::Drop
            })
            .expect("timer");
        s.redraw = RedrawState::WaitingForEstimatedVBlank { token, queued: false };
    }
    let feedback = s.dmabuf_feedback.clone();

    render::update_primary_scanout_output(&state.space, &output, &state.cursor, &states);
    if !locked {
        // Apps hidden by the lock draw nothing until it is gone.
        render::send_frame_callbacks(&state.space, &output, &state.cursor, state.clock.now(), state.switch.as_ref());
    }
    if let Some(fb) = feedback {
        render::send_dmabuf_feedback(&state.space, &output, &fb, &states);
    }
    if !queued && lock_drawn {
        state.lock_frame_presented();
    }
    state.after_overlay_frame();
}

/// Turn the output off (idle blank) or on again. Off = Smithay's `DrmCompositor::clear` (planes off,
/// CRTC inactive); the next queued frame after `on` does the modeset that brings it back, from fully reset
/// buffers (every element redrawn).
pub fn set_output_power(state: &mut State, on: bool) {
    let Backend::Udev(d) = &mut state.backend else { return };
    let Some(s) = d.surface.as_mut() else { return };
    if on {
        if !s.off {
            return;
        }
        s.off = false;
        s.clear_at_vblank = false;
        s.drm_output.reset_buffers();
        info!("output on");
        state.queue_redraw();
        return;
    }
    if s.off {
        return;
    }
    s.off = true;
    match s.redraw {
        // A flip is in flight: clearing now would race it; done at its vblank (`on_vblank`).
        RedrawState::WaitingForVBlank { .. } => {
            s.clear_at_vblank = true;
            return;
        }
        RedrawState::WaitingForEstimatedVBlank { token, .. } => state.loop_handle.remove(token),
        // A render scheduled on the loop sees `off` and does nothing.
        RedrawState::Queued | RedrawState::Idle => {}
    }
    s.redraw = RedrawState::Idle;
    if d.session.is_active() {
        clear_output(s);
    }
}

fn clear_output(s: &mut OutputSurface) {
    match s.drm_output.with_compositor(|c| c.clear()) {
        Ok(()) => info!("output off"),
        Err(err) => warn!(%err, "cannot turn the output off"),
    }
}

fn on_estimated_vblank(state: &mut State) {
    let Backend::Udev(d) = &mut state.backend else { return };
    let Some(s) = d.surface.as_mut() else { return };
    if let RedrawState::WaitingForEstimatedVBlank { queued, .. } = s.redraw {
        if queued {
            s.redraw = RedrawState::Queued;
            render(state);
        } else {
            s.redraw = RedrawState::Idle;
        }
    }
}

fn on_vblank(state: &mut State, crtc: crtc::Handle, meta: &mut Option<DrmEventMetadata>) {
    let Backend::Udev(d) = &mut state.backend else { return };
    let Some(s) = d.surface.as_mut() else { return };
    if s.drm_output.crtc() != crtc {
        return;
    }
    let (time, flags): (Time<Monotonic>, _) = match meta.as_ref().map(|m| m.time) {
        Some(DrmEventTime::Monotonic(tp)) if !tp.is_zero() => (
            tp.into(),
            wp_presentation_feedback::Kind::Vsync
                | wp_presentation_feedback::Kind::HwClock
                | wp_presentation_feedback::Kind::HwCompletion,
        ),
        _ => (state.clock.now(), wp_presentation_feedback::Kind::Vsync),
    };
    let seq = meta.as_ref().map(|m| m.sequence).unwrap_or(0);
    state.frame_clock.vblank(time.into());
    match s.drm_output.frame_submitted() {
        Ok(Some(Some(mut feedback))) => {
            feedback.presented(time, Refresh::fixed(s.frame_duration), seq as u64, flags);
        }
        Ok(_) => {}
        Err(err) => warn!(%err, "frame_submitted"),
    }
    if state.lock.frame_queued {
        state.lock_frame_presented();
    }
    let Backend::Udev(d) = &mut state.backend else { return };
    let Some(s) = d.surface.as_mut() else { return };
    if std::mem::take(&mut s.clear_at_vblank) {
        s.redraw = RedrawState::Idle;
        clear_output(s);
        return;
    }
    match s.redraw {
        RedrawState::WaitingForVBlank { redraw_needed: true } => {
            s.redraw = RedrawState::Queued;
            render(state);
        }
        RedrawState::WaitingForVBlank { redraw_needed: false } => s.redraw = RedrawState::Idle,
        _ => {}
    }
}

/// Frames slower than this (CPU time in `render`) are logged during switches and the Overview with
/// `RTSHELL_DEBUG_FPS=1` (default 14 ms; `RTSHELL_SLOW_MS=0` logs every frame).
fn slow_frame() -> Duration {
    static MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    Duration::from_millis(*MS.get_or_init(|| std::env::var("RTSHELL_SLOW_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(14)))
}
