//! Nested backend for development on the host: rtshell runs in a window of the host's Wayland/X11 session.
//! `cargo run --no-default-features --features winit -- --nested`
//!
//! Matches the Surface where it can: a 1366x768 window and a GLES 2.0 context (catches GLES3-only paths).
//! Vsync is off (a swap would block forever while the host hides the window and sends no frame callbacks);
//! frames are paced by a ~60 Hz timer instead.

use std::time::{Duration, Instant};

use smithay::{
    backend::{
        egl::{EGLDevice, context::GlAttributes},
        renderer::{ImportDma, ImportMemWl, damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop,
            timer::{TimeoutAction, Timer},
        },
        winit::{dpi::LogicalSize, window::WindowAttributes},
    },
    utils::Transform,
    wayland::dmabuf::DmabufFeedbackBuilder,
};
use tracing::{info, warn};

use super::Backend;
use crate::{policy::FocusChange, render, state::State};

const FRAME: Duration = Duration::from_micros(16_667);

pub struct WinitData {
    backend: WinitGraphicsBackend<GlesRenderer>,
    damage_tracker: OutputDamageTracker,
    output: Output,
    /// A redraw timer is armed (or a Redraw requested); further requests coalesce into it.
    redraw_pending: bool,
    last_frame: Option<Instant>,
}

impl WinitData {
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        self.backend.renderer()
    }

    pub fn output(&self) -> &Output {
        &self.output
    }
}

pub fn init(state: &mut State, event_loop: &mut EventLoop<'static, State>) -> Result<(), String> {
    let attributes = WindowAttributes::default()
        .with_surface_size(LogicalSize::new(1366.0, 768.0))
        .with_title("rtshell (nested)")
        .with_visible(true);
    let gl = GlAttributes {
        version: (2, 0),
        profile: None,
        debug: false,
        vsync: false,
    };
    let (mut backend, winit) = winit::init_from_attributes_with_gl_attr::<GlesRenderer>(attributes, gl)
        .map_err(|e| format!("winit: {e}"))?;
    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    let output = Output::new(
        "winit".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "rtshell".into(),
            model: "nested".into(),
            serial_number: "Unknown".into(),
        },
    );
    let _global = output.create_global::<State>(&state.display_handle);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));

    let formats = backend.renderer().dmabuf_formats();
    let node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
        .ok()
        .and_then(|d| d.try_get_render_node().ok().flatten());
    let global = match node.and_then(|n| DmabufFeedbackBuilder::new(n.dev_id(), formats.clone()).build().ok()) {
        Some(fb) => state
            .dmabuf_state
            .create_global_with_default_feedback::<State>(&state.display_handle, &fb),
        None => {
            warn!("no render node: linux-dmabuf v3 only");
            state.dmabuf_state.create_global::<State>(&state.display_handle, formats)
        }
    };
    state.dmabuf_global = Some(global);
    state.shm_state.update_formats(backend.renderer().shm_formats());

    event_loop
        .handle()
        .insert_source(winit, |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                if let Backend::Winit(d) = &mut state.backend {
                    let mode = Mode { size, refresh: 60_000 };
                    d.output.change_current_state(Some(mode), None, None, None);
                    d.output.set_preferred(mode);
                }
                state.with_policy(|p, ctx| {
                    p.area_changed(ctx);
                    FocusChange::Keep
                });
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => render(state),
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            _ => {}
        })
        .map_err(|e| e.to_string())?;

    info!(size = ?mode.size, "nested output");
    state.backend = Backend::Winit(WinitData {
        damage_tracker: OutputDamageTracker::from_output(&output),
        backend,
        output,
        redraw_pending: false,
        last_frame: None,
    });
    state.queue_redraw();
    Ok(())
}

pub fn queue_redraw(state: &mut State) {
    let Backend::Winit(d) = &mut state.backend else { return };
    if d.redraw_pending {
        return;
    }
    d.redraw_pending = true;
    let wait = d.last_frame.map(|t| FRAME.saturating_sub(t.elapsed())).unwrap_or_default();
    let _ = state.loop_handle.insert_source(Timer::from_duration(wait), |_, _, state| {
        if let Backend::Winit(d) = &mut state.backend {
            d.backend.window().request_redraw();
        }
        TimeoutAction::Drop
    });
}

fn render(state: &mut State) {
    let decorations = state.decoration_elements();
    let Backend::Winit(d) = &mut state.backend else { return };
    d.redraw_pending = false;
    d.last_frame = Some(Instant::now());
    let output = d.output.clone();
    let age = d.backend.buffer_age().unwrap_or(0);
    let clear = state.theme.colors.background.premultiplied();
    let states = {
        let Ok((renderer, mut fb)) = d.backend.bind() else { return };
        let present_at = state.frame_clock.next_present(state.clock.now().into());
        let size = output.current_mode().map(|m| m.size).unwrap_or_default();
        let (ui, panel) = render::prepare_ui(&mut state.switch, &mut state.overview, &mut state.overlay, renderer, size, present_at, state.lock.locked);
        state.lock.frame_queued = state.lock.locked && !panel.is_empty();
        state.last_present = present_at;
        let elements = render::output_elements(
            renderer,
            &state.space,
            &output,
            &mut state.cursor,
            state.pointer.current_location(),
            ui,
            panel,
            state.switch.as_ref(),
            state.overview.as_ref(),
            decorations,
            state.lock.locked,
        );
        match d.damage_tracker.render_output(renderer, &mut fb, age, &elements, clear) {
            Ok(r) => Some((r.damage.cloned(), r.states)),
            Err(err) => {
                warn!(?err, "render failed");
                None
            }
        }
    };
    let Some((damage, states)) = states else { return };
    if let Some(damage) = damage
        && let Err(err) = d.backend.submit(Some(&damage))
    {
        warn!(?err, "submit failed");
    }
    render::update_primary_scanout_output(&state.space, &output, &state.cursor, &states);
    if !state.lock.locked {
        render::send_frame_callbacks(&state.space, &output, &state.cursor, state.clock.now(), state.switch.as_ref());
    }
    state.lock_frame_presented();
    state.after_overlay_frame();
}
