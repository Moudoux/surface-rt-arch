//! Compositor state and Wayland protocol handlers (core layer, DESKTOP-SHELL.md 3.6).
//!
//! Protocols (3.7, Phase 1-2 list): wl_compositor + wl_subcompositor, wl_shm, linux-dmabuf v4 (+ feedback,
//! global created by the backend), xdg-shell (v6, suspended), xdg-decoration, wl_seat (keyboard, pointer,
//! touch), wl_data_device + primary selection, viewporter, fractional-scale (preferred 1.0), presentation-time,
//! xdg-output, cursor-shape, single-pixel-buffer, text-input-v3 (the compositor itself is the input method:
//! the shell's on-screen keyboard, input/osk.rs).

use std::sync::Arc;

use smithay::{
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    delegate_dispatch2,
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy, Space, Window,
        WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        keyboard::{Keycode, LedState, XkbConfig},
        pointer::{CursorImageStatus, CursorImageSurfaceData, Focus, PointerHandle},
        tablet::TabletSeatHandler,
    },
    output::Output,
    reexports::{
        calloop::{Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface},
        },
    },
    utils::{Clock, Logical, Monotonic, Point, Rectangle, SERIAL_COUNTER, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes, get_parent,
            is_sync_subsurface, with_states,
        },
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale},
        output::{OutputHandler, OutputManagerState},
        presentation::PresentationState,
        selection::{
            SelectionHandler,
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus},
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState, set_primary_focus},
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        text_input::{TextInputActivation, TextInputManagerState, TextInputSeat},
        viewporter::ViewporterState,
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
    },
};
use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{ContentHint, ContentPurpose};
use tracing::{info, warn};

use std::time::Duration;

use smithay::reexports::calloop::{
    RegistrationToken,
    ping::{Ping, make_ping},
    timer::{TimeoutAction, Timer},
};

use crate::{
    anim::{FrameClock, SpringParams},
    overlay::{Overlay, Request},
    render::batch::Rect,
    switch::{Backdrop, Switch},
    apps::{self, AppEnv},
    backend::Backend,
    config::Config,
    policy::{FocusChange, WindowPolicy, WmCtx},
    render::cursor::CursorState,
    theme::Theme,
};

#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// Every xdg toplevel the core knows about, shown or not.
pub struct Toplevel {
    pub window: Window,
    /// Has content (committed a buffer) and was handed to the policy.
    pub mapped: bool,
}

/// Set by `Request::Restart`: `main` exits with `RESTART_EXIT` after a clean shutdown.
pub static RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Exit status that asks `rtshell-session` for a restart (not counted as a crash). EX_TEMPFAIL.
pub const RESTART_EXIT: i32 = 75;

// Several protocol states are only held to keep their globals alive.
#[allow(dead_code)]
pub struct State {
    pub backend: Backend,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, State>,
    pub loop_signal: LoopSignal,
    pub clock: Clock<Monotonic>,
    pub socket_name: String,
    pub config: Config,
    pub theme: Theme,
    pub policy: Box<dyn WindowPolicy>,

    pub space: Space<Window>,
    pub toplevels: Vec<Toplevel>,
    /// Xwayland (xwayland.rs): the X window manager once Xwayland is ready, its display number, the
    /// xwayland-shell global, mapped override-redirect windows (menus, tooltips), and whether it was started.
    #[cfg(feature = "xwayland")]
    pub xwm: Option<smithay::xwayland::X11Wm>,
    #[cfg(feature = "xwayland")]
    pub xdisplay: Option<u32>,
    #[cfg(feature = "xwayland")]
    pub xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState,
    #[cfg(feature = "xwayland")]
    pub x11_override: Vec<Window>,
    #[cfg(feature = "xwayland")]
    pub xwayland_started: bool,
    pub popups: PopupManager,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub output_manager_state: OutputManagerState,
    pub viewporter_state: ViewporterState,
    pub presentation_state: PresentationState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub cursor_shape_state: CursorShapeManagerState,
    pub activation_state: XdgActivationState,
    pub text_input_state: TextInputManagerState,
    pub idle_inhibit_state: smithay::wayland::idle_inhibit::IdleInhibitManagerState,

    pub seat: Seat<State>,
    pub pointer: PointerHandle<State>,
    pub cursor: CursorState,
    /// Keys whose press we consumed as a shortcut; their release is consumed too.
    pub suppressed_keys: Vec<Keycode>,
    /// The power button is down (its keycode) and nothing else was pressed since: its release sleeps.
    pub power_held: Option<Keycode>,
    /// User screenshots report here from their writer thread (`save_screenshot`).
    pub shot_tx: calloop::channel::Sender<Result<std::path::PathBuf, String>>,
    /// A held volume key repeating (input/mod.rs `volume_press`): its keycode (None from a script) and the
    /// press number its timer belongs to (the timer drops itself once that press is over).
    pub volume_repeat: Option<(Option<Keycode>, u64)>,
    pub volume_presses: u64,
    pub dnd_icon: Option<WlSurface>,
    /// RTSHELL_DEBUG_KEYS=1: log modifier keys and modified key presses (input/mod.rs).
    pub debug_keys: bool,
    /// SIGUSR2 burst (render/screenshot.rs): commits left to dump, running sequence number, start time,
    /// last commit time and the sequence number of each surface's current (not yet released) buffer.
    pub burst: Burst,
    /// The shell UI drawn above windows (`--demo` for now), fed input first. See overlay.rs.
    pub overlay: Option<Box<dyn Overlay>>,
    /// Whether any app window is mapped (told to the overlay; Home hides behind apps).
    pub apps_shown: bool,
    /// The Super key went down with nothing else pressed since: its release toggles Home.
    pub super_tap: bool,
    /// Hardware keyboards attached (the Type Cover): rotation stays normal while any is (rotate.rs).
    pub hw_keyboards: Vec<String>,
    /// The orientation the shell UI asked for (from the accelerometer, unless rotation is locked).
    pub want_orientation: crate::rotate::Orientation,
    /// Vblank timestamps -> predicted presentation time for animations.
    pub frame_clock: FrameClock,
    /// Timer for the overlay's next timed redraw (clock tick).
    pub overlay_wakeup: Option<(RegistrationToken, Duration)>,
    /// Presentation time the overlay was last built for (screenshots reuse it).
    pub last_present: Duration,
    /// The running Home <-> app or app -> app animation (switch.rs).
    pub switch: Option<Switch>,
    /// Edge gesture recognizer (gesture.rs) and the gesture a finger is driving now (input/edges.rs).
    pub gestures: crate::gesture::Recognizer,
    pub gesture: Option<crate::input::EdgeRun>,
    /// The Overview (overview.rs), while open or fading out.
    pub overview: Option<crate::overview::Overview>,
    /// Fires when the finger of a bottom swipe has rested (opens the Overview; input/edges.rs).
    pub pause_timer: Option<smithay::reexports::calloop::RegistrationToken>,
    /// Open "not responding" questions on Home (id, the app's window) and the last id handed out.
    pub prompts: Vec<(u64, Window)>,
    /// Frame timing while the side panel (Control Center) is drawn, logged when it goes away.
    pub panel_frames: Option<PanelFrames>,
    pub next_prompt: u64,
    /// Clock floors raised during interaction (boost.rs) and the timer that lowers them again.
    pub boost: crate::boost::Boost,
    pub boost_timer: Option<RegistrationToken>,
    /// A touch event reached a client since the last wl_touch.frame.
    pub touch_frame_due: bool,
    /// The focused app has text-input-v3 enabled (with the field's purpose): the on-screen keyboard's
    /// text goes there with commit_string.
    pub app_text_input: Option<crate::overlay::TextPurpose>,
    /// Pixels at the bottom kept free of apps (the on-screen keyboard), from `Request::Reserve`.
    pub osk_reserve: i32,
    /// The touch slot (or `MOUSE`) holding a policy handle (split divider), see `input/handles.rs`.
    pub handle_slot: Option<i32>,
    /// Render element ids of the policy's decorations, by role (and grip), so damage tracking sees them move.
    pub deco_ids: std::collections::HashMap<(crate::policy::DecoRole, bool), smithay::backend::renderer::element::Id>,
    /// Session lock (lock.rs): while locked only the lock screen is drawn and gets input.
    pub lock: crate::lock::LockState,
    /// `--greeter`: the login screen under greetd. Always locked (never unlocks), no apps.
    pub greeter: bool,
    /// Idle stages and inhibitors (idle.rs).
    pub idle: crate::idle::IdleState,
}

#[derive(Default)]
pub struct Burst {
    pub left: u32,
    pub seq: u32,
    pub started: Option<std::time::Instant>,
    pub last: Option<std::time::Instant>,
    pub prev: std::collections::HashMap<WlSurface, u32>,
}

impl State {
    pub fn new(
        display: Display<State>,
        loop_handle: LoopHandle<'static, State>,
        loop_signal: LoopSignal,
        config: Config,
        theme: Theme,
        policy: Box<dyn WindowPolicy>,
        seat_name: &str,
    ) -> State {
        let dh = display.handle();
        let clock = Clock::new();

        // Without a socket (e.g. no XDG_RUNTIME_DIR) no app can connect, but the shell itself (the greeter,
        // the lock screen) still works, so it is not fatal.
        let socket_name = match ListeningSocketSource::new_auto() {
            Ok(socket) => {
                let name = socket.socket_name().to_string_lossy().into_owned();
                loop_handle
                    .insert_source(socket, |stream, _, state| {
                        if let Err(err) = state.display_handle.insert_client(stream, Arc::new(ClientState::default())) {
                            warn!(%err, "cannot add Wayland client");
                        }
                    })
                    .expect("cannot listen on the Wayland socket");
                name
            }
            Err(err) => {
                tracing::error!(%err, "cannot create a Wayland socket; no app can connect");
                String::new()
            }
        };
        loop_handle
            .insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
                // Safety: the display is not dropped while the source exists.
                unsafe { display.get_mut().dispatch_clients(state).unwrap() };
                Ok(PostAction::Continue)
            })
            .expect("cannot watch the Wayland display");
        info!(socket = %socket_name, "listening");
        let (shot_tx, shot_rx) = calloop::channel::channel();
        loop_handle
            .insert_source(shot_rx, |ev, _, state| {
                if let calloop::channel::Event::Msg(result) = ev {
                    state.screenshot_done(result);
                }
            })
            .expect("cannot watch the screenshot channel");

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let presentation_state = PresentationState::new::<Self>(&dh, clock.id() as u32);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(&dh);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let cursor_shape_state = CursorShapeManagerState::new::<Self>(&dh);
        let activation_state = XdgActivationState::new::<Self>(&dh);
        let text_input_state = TextInputManagerState::new::<Self>(&dh);
        let idle_inhibit_state = smithay::wayland::idle_inhibit::IdleInhibitManagerState::new::<Self>(&dh);

        let mut seat = seat_state.new_wl_seat(&dh, seat_name);
        let pointer = seat.add_pointer();
        let kb = &config.keyboard;
        let xkb = XkbConfig {
            rules: &kb.rules,
            model: &kb.model,
            layout: &kb.layout,
            variant: &kb.variant,
            options: (!kb.options.is_empty()).then(|| kb.options.clone()),
        };
        if let Err(err) = seat.add_keyboard(xkb, kb.repeat_delay, kb.repeat_rate) {
            warn!(?err, "bad xkb settings in config, using the xkb defaults");
            seat.add_keyboard(XkbConfig::default(), kb.repeat_delay, kb.repeat_rate)
                .expect("xkb default keymap");
        }
        // Touch is added when a touch device shows up (input/).
        // No input-method-v2 client: the compositor answers text-input-v3 itself (the on-screen keyboard).
        seat.text_input().set_compositor_input_method(true);

        #[cfg(feature = "xwayland")]
        let xwayland_shell_state = smithay::wayland::xwayland_shell::XWaylandShellState::new::<State>(&dh);
        let cursor = CursorState::new(&theme.cursor);
        let mut gestures = crate::gesture::Recognizer::default();
        gestures.zone = config.input.edge_zone.clamp(2.0, 120.0);
        gestures.lock = config.input.edge_lock.clamp(2.0, 80.0);

        State {
            backend: Backend::Uninit,
            display_handle: dh,
            loop_handle,
            loop_signal,
            clock,
            socket_name,
            config,
            theme,
            policy,
            space: Space::default(),
            toplevels: Vec::new(),
            popups: PopupManager::default(),
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            shm_state,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            seat_state,
            data_device_state,
            primary_selection_state,
            output_manager_state,
            viewporter_state,
            presentation_state,
            fractional_scale_state,
            single_pixel_buffer_state,
            cursor_shape_state,
            activation_state,
            text_input_state,
            idle_inhibit_state,
            seat,
            pointer,
            cursor,
            suppressed_keys: Vec::new(),
            power_held: None,
            #[cfg(feature = "xwayland")]
            xwm: None,
            #[cfg(feature = "xwayland")]
            xdisplay: None,
            #[cfg(feature = "xwayland")]
            xwayland_shell_state,
            #[cfg(feature = "xwayland")]
            x11_override: Vec::new(),
            #[cfg(feature = "xwayland")]
            xwayland_started: false,
            shot_tx,
            volume_repeat: None,
            volume_presses: 0,
            dnd_icon: None,
            debug_keys: std::env::var_os("RTSHELL_DEBUG_KEYS").is_some(),
            burst: Burst::default(),
            overlay: None,
            apps_shown: false,
            super_tap: false,
            hw_keyboards: Vec::new(),
            want_orientation: crate::rotate::Orientation::Normal,
            frame_clock: FrameClock::new(Duration::from_micros(16_667)),
            overlay_wakeup: None,
            last_present: Duration::ZERO,
            switch: None,
            gestures,
            gesture: None,
            touch_frame_due: false,
            overview: None,
            pause_timer: None,
            boost: crate::boost::Boost::new(),
            boost_timer: None,
            prompts: Vec::new(),
            panel_frames: None,
            next_prompt: 0,
            app_text_input: None,
            osk_reserve: 0,
            handle_slot: None,
            deco_ids: std::collections::HashMap::new(),
            lock: Default::default(),
            greeter: false,
            idle: Default::default(),
        }
    }

    /// Install the shell UI. `make` gets a ping that worker threads (wallpaper loading) use to wake the loop.
    pub fn set_overlay(&mut self, make: impl FnOnce(Ping) -> Box<dyn Overlay>) {
        let (ping, source) = make_ping().expect("ping source");
        self.loop_handle
            .insert_source(source, |_, _, state| {
                if state.overlay.as_mut().is_some_and(|o| o.poll()) {
                    state.queue_redraw();
                }
                // A services update may carry a request (e.g. a new orientation) or a new power source.
                state.handle_overlay_requests();
                state.idle_power_source();
            })
            .expect("insert ping");
        let mut o = make(ping);
        if let Some(size) = self.output().and_then(|out| Some(out.current_transform().transform_size(out.current_mode()?.size))) {
            o.size_hint(size.w, size.h);
        }
        o.set_apps_shown(self.apps_shown);
        self.overlay = Some(o);
        self.queue_redraw();
    }

    /// The overlay, if it is on screen now (Home is not while an app is shown).
    pub fn visible_overlay(&mut self) -> Option<&mut Box<dyn Overlay>> {
        self.overlay.as_mut().filter(|o| o.visible())
    }

    /// Carry out what the overlay asked for (launches).
    pub fn handle_overlay_requests(&mut self) {
        let Some(o) = self.overlay.as_mut() else { return };
        for req in o.take_requests() {
            match req {
                Request::Launch { cmd, app_ids, cwd, terminal, from } => self.launch(cmd, &app_ids, cwd, terminal, from),
                Request::ForceQuit { id, kill } => self.answer_prompt(id, kill),
                Request::Rotate(o) => {
                    self.want_orientation = o;
                    self.apply_rotation();
                }
                Request::OskKey(ev) => self.osk_key(ev),
                Request::Reserve(px) => self.reserve_bottom(px),
                Request::Lock(reason) => self.lock_session(reason),
                Request::Unlock => self.unlock_session(),
                Request::Wake => self.idle_wake(),
                Request::ConfigChanged(config) => self.apply_config(*config),
                Request::Restart => {
                    info!("restart requested by the shell UI");
                    RESTART.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.loop_signal.stop();
                    self.loop_signal.wakeup();
                }
                Request::Quit => {
                    info!("quit requested by the shell UI");
                    self.loop_signal.stop();
                    self.loop_signal.wakeup();
                }
            }
        }
    }

    /// Rotate the output to `want_orientation` (normal while a hardware keyboard is attached). Waits while a
    /// switch or the Overview runs (they hold geometry of the old size); called again after each frame.
    pub fn apply_rotation(&mut self) {
        if !self.backend.is_drm() {
            return; // the nested winit window keeps its own transform
        }
        let Some(output) = self.output() else { return };
        let target = if self.hw_keyboards.is_empty() { self.want_orientation.transform() } else { smithay::utils::Transform::Normal };
        if output.current_transform() == target || self.switch.is_some() || self.overview.is_some() {
            return;
        }
        info!(?target, orientation = ?self.want_orientation, keyboards = self.hw_keyboards.len(), "rotate output");
        self.touch_cancel();
        output.change_current_state(None, Some(target), None, None);
        self.space.map_output(&output, (0, 0));
        self.with_policy(|p, ctx| {
            p.area_changed(ctx);
            FocusChange::Keep
        });
        self.queue_redraw();
    }

    /// Bring a running app forward (by xdg app_id), or start it with an xdg-activation token.
    fn launch(&mut self, cmd: Vec<String>, app_ids: &[String], cwd: Option<std::path::PathBuf>, terminal: bool, from: Option<Rect>) {
        let running = self
            .toplevels
            .iter()
            .filter(|t| t.mapped)
            .find(|t| window_app_id(&t.window).is_some_and(|id| app_ids.iter().any(|a| *a == id)));
        if let Some(t) = running {
            let w = t.window.clone();
            info!(?app_ids, "launch: app already running, activating it");
            self.switch_with(from, |p, ctx| p.activate(ctx, &w));
            return;
        }
        let cmd = if terminal {
            let mut t = self.config.apps.terminal.clone();
            t.extend(cmd);
            t
        } else {
            cmd
        };
        self.activation_state.retain_tokens(|_, d| d.timestamp.elapsed() < Duration::from_secs(60));
        let (token, _) = self.activation_state.create_external_token(None);
        let token = token.as_str().to_string();
        apps::spawn_with(&cmd, &self.app_env(), cwd.as_deref(), Some(&token));
    }

    /// Home key: show Home, or (from Home) the most recent app. During a Home <-> app animation it reverses
    /// the animation.
    pub fn toggle_home(&mut self) {
        if self.overview_takes_touches() {
            return self.leave_overview();
        }
        let home = match self.switch.as_ref().filter(|s| s.backdrop == Backdrop::Home) {
            Some(s) => s.entering(),
            None => self.space.elements().next().is_some(),
        };
        self.switch_with(None, |p, ctx| p.set_home(ctx, home));
    }

    /// Next/previous app (Alt+Tab), animated.
    pub fn cycle(&mut self, forward: bool) {
        self.switch_with(None, |p, ctx| p.cycle(ctx, forward));
    }

    /// The windows shown now (an app and its dialogs), bottom to top, at their space locations.
    pub(crate) fn shown_windows(&self) -> Vec<(Window, Point<i32, Logical>)> {
        self.space
            .elements()
            .filter_map(|w| Some((w.clone(), self.space.element_location(w)?)))
            .collect()
    }

    /// Run a policy hook that may change which app is shown, and animate the change: the app shrinking away
    /// to Home (into its tile if Home shows one), growing back in (from `from`, the tile that was tapped, or
    /// its tile), or growing over the previous app. A running animation of the same app is reversed in place.
    pub fn switch_with(&mut self, from: Option<Rect>, f: impl FnOnce(&mut dyn WindowPolicy, &mut WmCtx) -> FocusChange) {
        self.kick_boost();
        let before = self.shown_windows();
        self.with_policy_raw(f);
        let after = self.shown_windows();
        let first = |v: &[(Window, Point<i32, Logical>)]| v.first().map(|(w, _)| w.clone());
        let (b, a) = (first(&before), first(&after));
        let area = self.output().and_then(|o| self.space.output_geometry(&o));
        let params = SpringParams { stiffness: self.theme.motion.spring_stiffness, damping: self.theme.motion.spring_damping };
        // Reverse a Home switch of the same app.
        let same = |s: &Switch, w: &Option<Window>| s.backdrop == Backdrop::Home && s.windows.first().map(|x| &x.0) == w.as_ref();
        match (b, a, area) {
            (Some(_), None, Some(area)) => {
                if let Some(s) = self.switch.as_mut().filter(|s| same(s, &first(&before))) {
                    s.head(false);
                } else {
                    let id = first(&before).and_then(|w| window_app_id(&w));
                    self.switch = Some(Switch::new(before, Backdrop::Home, id, area, params, 0.0, 1.0));
                }
            }
            (None, Some(w), Some(area)) => {
                if let Some(s) = self.switch.as_mut().filter(|s| same(s, &Some(w.clone()))) {
                    s.windows = after;
                    s.head(true);
                    if from.is_some() {
                        s.tile_hint = from;
                    }
                } else {
                    let mut s = Switch::new(after, Backdrop::Home, window_app_id(&w), area, params, 1.0, 0.0);
                    s.tile_hint = from;
                    self.switch = Some(s);
                }
            }
            // A different unit (an app or a split pair) came up; focus moving inside a pair is no switch.
            (Some(b), Some(a), Some(area)) if !after.iter().any(|(w, _)| *w == b) => {
                let mut s = Switch::new(after, Backdrop::App, window_app_id(&a), area, params, 1.0, 0.0);
                s.under = before;
                self.switch = Some(s);
            }
            _ => {}
        }
        self.sync_apps_shown();
        self.queue_redraw();
    }

    /// Tell the overlay whether an app is shown now. While an app grows in over Home, Home stays shown (it is
    /// drawn underneath) until the app has fully grown.
    pub(crate) fn sync_apps_shown(&mut self) {
        let over_home = self.switch.as_ref().is_some_and(|s| s.backdrop == Backdrop::Home);
        let shown = self.space.elements().next().is_some() && !over_home;
        if shown != self.apps_shown {
            self.apps_shown = shown;
            if let Some(o) = self.overlay.as_mut() {
                o.set_apps_shown(shown);
            }
        }
    }

    /// After a frame: keep rendering while the overlay animates, and arm a timer for its next timed change.
    pub fn after_overlay_frame(&mut self) {
        match self.switch.as_ref().map(|s| s.is_done()) {
            Some(true) => {
                if let Some(s) = self.switch.take() {
                    info!(
                        to = match (s.entering(), s.backdrop) {
                            (true, _) => "app",
                            (false, Backdrop::Home) => "home",
                            (false, Backdrop::App) => "back",
                        },
                        backdrop = ?s.backdrop,
                        app = s.app_id.as_deref().unwrap_or("?"),
                        tile = s.tile.is_some(),
                        frames = s.frames,
                        held_frames = s.held_frames,
                        ms = s.duration().as_millis() as u64,
                        max_gap_ms = format!("{:.1}", s.max_gap.as_secs_f32() * 1000.0),
                        max_render_ms = format!("{:.1}", s.max_render.as_secs_f32() * 1000.0),
                        late = ?s.late,
                        "switch done"
                    );
                }
                self.sync_apps_shown();
                // One more frame: drawn normally again (direct scanout back on).
                self.queue_redraw();
            }
            // A finger holding the app redraws when it moves.
            Some(false) if self.switch.as_ref().is_some_and(|s| s.held) => {}
            Some(false) => self.queue_redraw(),
            None => {}
        }
        self.overview_after_frame();
        self.handle_overlay_requests();
        // A rotation that waited for a switch or the Overview to end.
        self.apply_rotation();
        let Some(o) = self.overlay.as_ref() else { return };
        let (animating, wake) = (o.animating(), o.next_wakeup());
        if animating {
            self.queue_redraw();
        }
        if self.overlay_wakeup.as_ref().map(|w| w.1) == wake {
            return;
        }
        if let Some((token, _)) = self.overlay_wakeup.take() {
            self.loop_handle.remove(token);
        }
        if let Some(at) = wake {
            let now: Duration = self.clock.now().into();
            let token = self
                .loop_handle
                .insert_source(Timer::from_duration(at.saturating_sub(now)), |_, _, state| {
                    state.overlay_wakeup = None;
                    state.queue_redraw();
                    TimeoutAction::Drop
                })
                .expect("timer");
            self.overlay_wakeup = Some((token, at));
        }
    }

    pub fn app_env(&self) -> AppEnv {
        AppEnv {
            wayland_display: self.socket_name.clone(),
            extra: self.config.env.clone(),
            #[cfg(feature = "xwayland")]
            x11_display: self.xdisplay,
            #[cfg(not(feature = "xwayland"))]
            x11_display: None,
        }
    }

    pub fn spawn(&self, cmd: &[String]) {
        apps::spawn(cmd, &self.app_env());
    }


    /// The output's area in space coordinates (where apps are laid out).
    pub fn area(&self) -> Option<smithay::utils::Rectangle<i32, Logical>> {
        self.output().and_then(|o| self.space.output_geometry(&o))
    }

    pub fn output(&self) -> Option<Output> {
        self.space.outputs().next().cloned()
    }

    /// Run a window-policy hook with the context it may touch, then apply the focus it returns.
    pub fn with_policy(&mut self, f: impl FnOnce(&mut dyn WindowPolicy, &mut WmCtx) -> FocusChange) {
        self.with_policy_raw(f);
        self.sync_apps_shown();
        self.queue_redraw();
    }

    /// `with_policy` without telling the overlay (switch_with decides that after looking at the result).
    pub(crate) fn with_policy_raw(&mut self, f: impl FnOnce(&mut dyn WindowPolicy, &mut WmCtx) -> FocusChange) {
        if let Some(change) = self.policy_ctx(f) {
            self.apply_focus(change);
        }
    }

    /// The policy's decorations (split divider) as render elements; none while a switch or the Overview runs.
    pub fn decoration_elements(&mut self) -> Vec<crate::render::OutputElement> {
        if self.switch.is_some() || self.overview.is_some() {
            return Vec::new();
        }
        let Some(output) = self.output() else { return Vec::new() };
        let Some(geo) = self.space.output_geometry(&output) else { return Vec::new() };
        let scale = smithay::utils::Scale::from(output.current_scale().fractional_scale());
        let decos = self.policy_ctx(|p, ctx| p.decorations(ctx)).unwrap_or_default();
        crate::render::decoration_elements(&decos, &self.theme.split, &mut self.deco_ids, geo, scale)
    }

    /// Run `f` with the policy and the context it may touch (None without an output).
    pub(crate) fn policy_ctx<R>(&mut self, f: impl FnOnce(&mut dyn WindowPolicy, &mut WmCtx) -> R) -> Option<R> {
        let output = self.output()?;
        let output_geometry = self.space.output_geometry(&output)?;
        // The on-screen keyboard's strip at the bottom (the status strip only shows outside apps).
        let mut area = output_geometry;
        area.size.h = (area.size.h - self.osk_reserve).max(area.size.h / 3);
        let mut ctx = WmCtx {
            space: &mut self.space,
            output: &output,
            area,
            output_geometry,
        };
        Some(f(self.policy.as_mut(), &mut ctx))
    }

    fn apply_focus(&mut self, change: FocusChange) {
        match change {
            FocusChange::Keep => return,
            FocusChange::Focus(w) => self.set_focus(Some(w)),
            FocusChange::Clear => self.set_focus(None),
        }
        self.refresh_pointer_focus();
    }

    fn set_focus(&mut self, window: Option<Window>) {
        for t in &self.toplevels {
            let active = Some(&t.window) == window.as_ref();
            if t.window.set_activated(active) && crate::winconf::initial_sent(&t.window) && t.window.toplevel().is_some() {
                crate::winconf::send(&t.window);
            }
        }
        let x11 = window.clone().filter(crate::winconf::is_x11);
        let surface = window.and_then(|w| crate::winconf::surface(&w));
        if self.lock.locked {
            // No app gets the keyboard while locked; this one gets it on unlock.
            self.lock.set_focus_after(surface.as_ref().and_then(|s| self.window_for_surface(s)));
            return;
        }
        let keyboard = self.seat.get_keyboard().unwrap();
        if keyboard.current_focus() != surface {
            keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        }
        if let Some(w) = x11 {
            crate::winconf::focused(&w);
        }
    }

    pub fn focused_window(&self) -> Option<Window> {
        let focus = self.seat.get_keyboard()?.current_focus()?;
        self.window_for_surface(&focus)
    }

    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.toplevels
            .iter()
            .find(|t| crate::winconf::surface(&t.window).is_some_and(|s| s == *surface))
            .map(|t| t.window.clone())
    }

    fn window_for_toplevel(&self, surface: &ToplevelSurface) -> Option<Window> {
        self.window_for_surface(surface.wl_surface())
    }

    /// Topmost surface at `pos` (space coordinates) and its origin.
    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space.element_under(pos).and_then(|(window, loc)| {
            window
                .surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
                .map(|(s, p)| (s, (p + loc).to_f64()))
        })
    }

    /// After the layout changed under a still pointer, tell clients what is under it now.
    pub fn refresh_pointer_focus(&mut self) {
        if !self.cursor.visible {
            return;
        }
        let pos = self.pointer.current_location();
        let under = self.surface_under(pos);
        let pointer = self.pointer.clone();
        let event = smithay::input::pointer::MotionEvent {
            location: pos,
            serial: SERIAL_COUNTER.next_serial(),
            time: smithay::backend::input::InputTime::now(),
        };
        pointer.motion(self, under, &event);
        pointer.frame(self);
    }

    /// Bring a window forward (touch on it, xdg-activation); a switch to another app animates.
    pub fn activate_window(&mut self, window: &Window) {
        let window = window.clone();
        self.switch_with(None, |p, ctx| p.activate(ctx, &window));
    }

    /// True while an app grows in: new touches and clicks are dropped (the app is not where it will be).
    pub fn switch_blocks_input(&self) -> bool {
        self.switch.as_ref().is_some_and(|s| s.entering())
    }

    /// Interaction: raise the clock floors now and keep them while input continues or anything animates.
    pub fn kick_boost(&mut self) {
        self.boost.kick();
        if !self.boost.available() || self.boost_timer.is_some() {
            return;
        }
        let res = self.loop_handle.insert_source(Timer::from_duration(crate::boost::HOLD), |_, _, state| {
            let now = std::time::Instant::now();
            let busy = state.switch.is_some() || state.overview.is_some() || state.gesture.is_some();
            match state.boost.until {
                Some(until) if until > now => TimeoutAction::ToDuration(until - now),
                _ if busy => TimeoutAction::ToDuration(Duration::from_millis(100)),
                _ => {
                    state.boost.release();
                    state.boost_timer = None;
                    TimeoutAction::Drop
                }
            }
        });
        match res {
            Ok(token) => self.boost_timer = Some(token),
            Err(err) => tracing::warn!(%err, "boost timer"),
        }
    }

    pub fn queue_redraw(&mut self) {
        crate::backend::queue_redraw(self);
    }

    /// Settings changed config.toml: apply the parts that can change while running. Style, theme (the UI
    /// reloads that itself), lock, greeter and env are left as they were started.
    pub fn apply_config(&mut self, mut new: crate::config::Config) {
        new.keyboard = new.keyboard.resolved();
        let (old_kb, kb) = (&self.config.keyboard, &new.keyboard);
        let keymap_changed = (&old_kb.rules, &old_kb.model, &old_kb.layout, &old_kb.variant, &old_kb.options)
            != (&kb.rules, &kb.model, &kb.layout, &kb.variant, &kb.options);
        let repeat_changed = (old_kb.repeat_delay, old_kb.repeat_rate) != (kb.repeat_delay, kb.repeat_rate);
        if let Some(keyboard) = self.seat.get_keyboard() {
            if keymap_changed {
                let xkb = XkbConfig {
                    rules: &kb.rules,
                    model: &kb.model,
                    layout: &kb.layout,
                    variant: &kb.variant,
                    options: (!kb.options.is_empty()).then(|| kb.options.clone()),
                };
                match keyboard.set_xkb_config(self, xkb) {
                    Ok(()) => info!(layout = %kb.layout, variant = %kb.variant, "config: keyboard layout changed"),
                    Err(err) => {
                        warn!(?err, layout = %kb.layout, "config: bad keyboard layout; keeping the old one");
                        new.keyboard = self.config.keyboard.clone();
                    }
                }
            }
            if repeat_changed {
                keyboard.change_repeat_info(new.keyboard.repeat_rate, new.keyboard.repeat_delay);
            }
        }
        self.gestures.zone = new.input.edge_zone.clamp(2.0, 120.0);
        self.gestures.lock = new.input.edge_lock.clamp(2.0, 80.0);
        self.backend.reconfigure_input(&new.input);
        let idle_changed = self.config.idle != new.idle;
        self.config.keyboard = new.keyboard;
        self.config.input = new.input;
        self.config.idle = new.idle;
        self.config.apps = new.apps;
        self.config.osk.resize_apps = new.osk.resize_apps;
        if idle_changed {
            info!(idle = ?self.config.idle, "config: idle times changed");
            self.idle_arm();
        }
    }

    /// SIGUSR1: re-render the output's current elements offscreen and save a PNG (render/screenshot.rs).
    pub fn take_screenshot(&mut self) {
        self.shoot(false);
    }

    /// Super+Shift+S, Power + Volume-down: the same picture as a compressed PNG in the Pictures folder; the
    /// writer thread reports back through `shot_tx` (-> `screenshot_done`, the overlay's feedback).
    pub fn save_screenshot(&mut self) {
        self.shoot(true);
    }

    /// A user screenshot was written (or failed).
    fn screenshot_done(&mut self, result: Result<std::path::PathBuf, String>) {
        let shown = result.map(|p| {
            // "~/Pictures/x.png" reads better than the full path.
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            match home.as_deref().and_then(|h| p.strip_prefix(h).ok()) {
                Some(rel) => format!("~/{}", rel.display()),
                None => p.display().to_string(),
            }
        });
        if let Some(o) = self.overlay.as_mut() {
            o.screenshot_taken(&shown);
        }
        self.queue_redraw();
    }

    fn shoot(&mut self, user: bool) {
        // A window mapped since the last loop iteration is not yet assigned to the output (Space::refresh).
        self.space.refresh();
        let clear = self.theme.colors.background.premultiplied();
        let pointer = self.pointer.current_location();
        let decorations = self.decoration_elements();
        let Some((renderer, output)) = self.backend.screenshot_target() else {
            tracing::warn!("screenshot: no active output");
            return;
        };
        let size = crate::rotate::ui_size(&output);
        // While the Overview shows its Home snapshot, the live Home is not drawn (as in render::prepare_ui).
        let snapshot = self.overview.as_ref().is_some_and(|ov| ov.home.is_some());
        let locked = self.lock.locked;
        let ui = self
            .overlay
            .as_mut()
            .filter(|o| o.visible() && !snapshot && !locked)
            .and_then(|o| o.element(renderer, size, self.last_present));
        let panel = self.overlay.as_mut().map(|o| o.top_elements(renderer, size, self.last_present)).unwrap_or_default();
        if let Some(sw) = self.switch.as_ref() {
            info!(pose = ?sw.pose(), style = ?sw.style, held = sw.held, p = sw.p.value, windows = sw.windows.len(), under = sw.under.len(), ui = ui.is_some(), "screenshot: switch");
        }
        let elements = crate::render::output_elements(renderer, &self.space, &output, &mut self.cursor, pointer, ui, panel, self.switch.as_ref(), self.overview.as_ref(), decorations, locked);
        if user {
            // The flash and the banner come once the file is written (`screenshot_done`).
            match crate::render::screenshot::pictures_dir() {
                Some(dir) => {
                    let tx = self.shot_tx.clone();
                    crate::render::screenshot::save(renderer, &output, &elements, clear, dir, move |r| {
                        let _ = tx.send(r);
                    });
                }
                None => {
                    let _ = self.shot_tx.send(Err("no home folder (HOME unset)".into()));
                }
            }
            return;
        }
        crate::render::screenshot::take(renderer, &output, &elements, clear);
        drop(elements);
        if locked {
            // No app buffers in debug dumps while locked either.
            return;
        }
        // Client dmabufs as stored (not sampled), to tell producer from importer bugs.
        let mut buffers = Vec::new();
        for window in self.space.elements() {
            window.with_surfaces(|_, data| {
                let Some(rs) = data.data_map.get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
                else {
                    return;
                };
                if let Some(buffer) = rs.lock().unwrap().buffer()
                    && let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(buffer)
                {
                    buffers.push(dmabuf.clone());
                }
            });
        }
        crate::render::screenshot::dump_dmabufs(renderer, buffers);
    }

    /// SIGUSR2: dump the next `n` dmabuf commits as stored (render/screenshot.rs, burst).
    pub fn start_burst(&mut self, n: u32) {
        info!(commits = n, "burst: armed");
        self.burst.left = n;
        self.burst.started = Some(std::time::Instant::now());
        self.burst.last = None;
        self.burst.prev.clear();
    }

    /// Before Smithay sees the commit: the new buffer is in the surface's current state, the previous one
    /// is still held by the renderer state (not released to the client yet).
    fn burst_commit(&mut self, surface: &WlSurface) {
        use std::os::fd::AsRawFd;

        use smithay::{wayland::compositor::BufferAssignment, wayland::dmabuf::get_dmabuf};
        let new = with_states(surface, |states| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            match &guard.current().buffer {
                Some(BufferAssignment::NewBuffer(b)) => get_dmabuf(b).ok().map(|d| d.clone()),
                _ => None,
            }
        });
        let Some(new) = new else { return };
        let prev = with_renderer_surface_state(surface, |s| s.buffer().and_then(|b| get_dmabuf(b).ok().map(|d| d.clone())))
            .flatten();
        let prev_seq = self.burst.prev.remove(surface);
        let Some(renderer) = self.backend.renderer() else { return };
        // The previous buffer of a dumped commit, re-read one commit later (still unreleased).
        if let (Some(prev), Some(ps)) = (prev.clone(), prev_seq) {
            crate::render::screenshot::dump_dmabuf(renderer, prev, format!("/tmp/rtshell-burst-{ps}-late.png"), "burst late");
        }
        if self.burst.left == 0 {
            return;
        }
        let now = std::time::Instant::now();
        let seq = self.burst.seq + 1;
        self.burst.seq = seq;
        self.burst.left -= 1;
        let t_ms = self.burst.started.map(|t| now.duration_since(t).as_secs_f64() * 1000.0).unwrap_or(0.0);
        let dt_ms = self.burst.last.map(|t| now.duration_since(t).as_secs_f64() * 1000.0).unwrap_or(0.0);
        self.burst.last = Some(now);
        self.burst.prev.insert(surface.clone(), seq);
        info!(
            seq,
            surface = %surface.id(),
            t_ms = format!("{t_ms:.1}"),
            dt_ms = format!("{dt_ms:.1}"),
            fd = new.handles().next().map(|f| f.as_raw_fd()).unwrap_or(-1),
            prev_fd = prev.as_ref().and_then(|p| p.handles().next().map(|f| f.as_raw_fd())).unwrap_or(-1),
            prev_seq,
            left = self.burst.left,
            "burst: commit"
        );
        crate::render::screenshot::dump_dmabuf(renderer, new, format!("/tmp/rtshell-burst-{seq}.png"), "burst early");
        if self.burst.left == 0 {
            info!("burst: done");
        }
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else { return };
        let Some(window) = self.window_for_surface(&root) else { return };
        let Some(window_geo) = self.space.element_geometry(&window) else { return };
        let Some(output_geo) = self.output().and_then(|o| self.space.output_geometry(&o)) else { return };
        // The target rectangle is relative to the popup's parent geometry.
        let mut target: Rectangle<i32, Logical> = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

// ---------------------------------------------------------------------------------------------------------
// wl_compositor, wl_shm, linux-dmabuf

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        #[cfg(feature = "xwayland")]
        if let Some(x) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &x.compositor_state;
        }
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        if self.burst.left > 0 || !self.burst.prev.is_empty() {
            self.burst_commit(surface);
        }
        on_commit_buffer_handler::<Self>(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(idx) = self
                .toplevels
                .iter()
                .position(|t| crate::winconf::surface(&t.window).is_some_and(|s| s == root))
            {
                let window = self.toplevels[idx].window.clone();
                window.on_commit();
                if &root == surface {
                    self.toplevel_root_commit(idx, &window);
                }
            }
        }

        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            // The initial configure is always allowed.
            let _ = popup.send_configure();
        }

        if matches!(&self.cursor.status, CursorImageStatus::Surface(s) if s == surface) {
            with_states(surface, |states| {
                let delta = states.cached_state.get::<SurfaceAttributes>().current().buffer_delta.take();
                if let (Some(delta), Some(attrs)) = (delta, states.data_map.get::<CursorImageSurfaceData>()) {
                    attrs.lock().unwrap().hotspot -= delta;
                }
            });
        }

        self.queue_redraw();
    }
}

impl State {
    /// A toplevel (xdg or X11) is gone: drop it from every list, the Overview, a running switch and the policy.
    pub fn forget_toplevel(&mut self, idx: usize) {
        let t = self.toplevels.remove(idx);
        self.forget_prompts(&t.window);
        if let Some(ov) = self.overview.as_mut() {
            ov.forget(&t.window);
            if ov.live_cards() == 0 {
                ov.leave();
            }
        }
        if self.switch.as_mut().is_some_and(|s| s.forget(&t.window)) {
            self.switch = None;
        }
        if t.mapped {
            self.with_policy(|p, ctx| p.unmap(ctx, &t.window));
        }
        self.space.unmap_elem(&t.window);
        self.queue_redraw();
    }

    fn toplevel_root_commit(&mut self, idx: usize, window: &Window) {
        if let Some(toplevel) = window.toplevel().cloned()
            && !toplevel.is_initial_configure_sent()
        {
            let w = window.clone();
            self.with_policy(|p, ctx| {
                p.configure_new(ctx, &w);
                FocusChange::Keep
            });
            toplevel.send_configure();
            return;
        }
        // xdg toplevels and X11 windows (configured when Xwayland asked to map them) alike from here.
        let Some(root) = crate::winconf::surface(window) else { return };
        let has_buffer = with_renderer_surface_state(&root, |s| s.buffer().is_some()).unwrap_or(false);
        let mapped = self.toplevels[idx].mapped;
        let w = window.clone();
        if has_buffer && !mapped {
            self.toplevels[idx].mapped = true;
            self.with_policy(|p, ctx| p.map(ctx, &w));
        } else if !has_buffer && mapped {
            // A null buffer unmaps the toplevel; it may map again later.
            self.toplevels[idx].mapped = false;
            self.with_policy(|p, ctx| p.unmap(ctx, &w));
        } else if mapped {
            self.with_policy(|p, ctx| {
                p.committed(ctx, &w);
                FocusChange::Keep
            });
        }
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: smithay::backend::allocator::dmabuf::Dmabuf, notifier: ImportNotifier) {
        use smithay::backend::renderer::ImportDma;
        let ok = self
            .backend
            .renderer()
            .is_some_and(|r| r.import_dmabuf(&dmabuf, None).is_ok());
        if ok {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// xdg-shell, xdg-decoration

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // The initial configure goes out on the first commit (toplevel_root_commit), when the client has
        // set its parent, app_id and size hints.
        self.toplevels.push(Toplevel {
            window: Window::new_wayland_window(surface),
            mapped: false,
        });
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(idx) = self
            .toplevels
            .iter()
            .position(|t| t.window.toplevel() == Some(&surface))
        else {
            return;
        };
        self.forget_toplevel(idx);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            warn!(%err, "cannot track popup");
        }
    }


    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        let Some(seat) = Seat::<State>::from_resource(&seat) else { return };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let Ok(mut grab) = self.popups.grab_popup(root, kind, &seat, serial) else { return };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        if let Some(w) = self.window_for_toplevel(&surface) {
            self.with_policy(|p, ctx| {
                p.fullscreen_request(ctx, &w, true);
                FocusChange::Keep
            });
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(w) = self.window_for_toplevel(&surface) {
            self.with_policy(|p, ctx| {
                p.fullscreen_request(ctx, &w, false);
                FocusChange::Keep
            });
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(w) = self.window_for_toplevel(&surface) {
            self.with_policy(|p, ctx| {
                p.maximize_request(ctx, &w, true);
                FocusChange::Keep
            });
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(w) = self.window_for_toplevel(&surface) {
            self.with_policy(|p, ctx| {
                p.maximize_request(ctx, &w, false);
                FocusChange::Keep
            });
        }
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        if let Some(w) = self.window_for_toplevel(&surface) {
            self.with_policy(|p, ctx| {
                p.move_request(ctx, &w);
                FocusChange::Keep
            });
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        if let Some(w) = self.window_for_toplevel(&surface) {
            self.with_policy(|p, ctx| {
                p.resize_request(ctx, &w);
                FocusChange::Keep
            });
        }
    }

    fn parent_changed(&mut self, _surface: ToplevelSurface) {
        // App vs dialog may have changed: let the policy re-lay out everything.
        self.with_policy(|p, ctx| {
            p.area_changed(ctx);
            FocusChange::Keep
        });
    }
}

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        let mode = self.policy.decoration_mode();
        toplevel.with_pending_state(|s| s.decoration_mode = Some(mode));
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        // The policy decides; with FullscreenPolicy that is "server side", and the server draws nothing.
        self.new_decoration(toplevel.clone());
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.request_mode(toplevel, DecorationMode::ServerSide);
    }
}

// ---------------------------------------------------------------------------------------------------------
// wl_seat, selections, drag and drop, cursor-shape

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
        // text-input `leave` disables the old field; the new one sends enable itself if it wants text.
        self.set_app_text_input(None);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor.status = image;
        if self.cursor.visible {
            self.queue_redraw();
        }
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        self.backend.update_led_state(led_state);
    }
}

// text-input-v3 with the compositor as the input method: enable/disable show and hide the on-screen keyboard.
impl TextInputActivation for State {
    fn activated(&mut self, content_type: Option<(ContentHint, ContentPurpose)>) {
        use crate::overlay::TextPurpose as P;
        let purpose = match content_type.map(|c| c.1) {
            Some(ContentPurpose::Password | ContentPurpose::Pin) => P::Password,
            Some(ContentPurpose::Digits | ContentPurpose::Number | ContentPurpose::Phone) => P::Number,
            Some(ContentPurpose::Terminal) => P::Terminal,
            _ => P::Text,
        };
        // A sensitive-data hint counts as a password too (no key previews).
        let sensitive = content_type.is_some_and(|c| c.0.contains(ContentHint::SensitiveData) || c.0.contains(ContentHint::HiddenText));
        self.set_app_text_input(Some(if sensitive { P::Password } else { purpose }));
    }

    fn deactivated(&mut self) {
        self.set_app_text_input(None);
    }
}

impl State {
    /// The focused app's text input state changed: tell the shell UI (which shows or hides the keyboard).
    pub fn set_app_text_input(&mut self, purpose: Option<crate::overlay::TextPurpose>) {
        let changed = self.app_text_input != purpose;
        self.app_text_input = purpose;
        // Every enable counts (a field focused again after the keyboard was hidden by hand shows it again).
        if (changed || purpose.is_some()) && let Some(o) = self.overlay.as_mut() {
            info!(?purpose, "text input");
            o.set_text_input(purpose);
            self.handle_overlay_requests();
            self.queue_redraw();
        }
    }

    /// Keep `px` at the bottom free of (non-fullscreen) apps.
    fn reserve_bottom(&mut self, px: i32) {
        let px = if self.config.osk.resize_apps { px.max(0) } else { 0 };
        if px == self.osk_reserve {
            return;
        }
        info!(px, "reserve bottom of the screen");
        self.osk_reserve = px;
        self.with_policy(|p, ctx| {
            p.area_changed(ctx);
            FocusChange::Keep
        });
    }
}

// Required by cursor-shape; no tablet support yet.
impl TabletSeatHandler for State {
    type ToolFocus = WlSurface;
}

// Required by the pointer focus type; the pointer-constraints global itself comes later (3.7).
impl smithay::wayland::pointer_constraints::PointerConstraintsHandler for State {}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

impl WaylandDndGrabHandler for State {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        // TODO(Phase 2+): draw the drag icon.
        self.dnd_icon = icon;
        match type_ {
            GrabType::Pointer => {
                let pointer = seat.get_pointer().unwrap();
                let Some(start) = pointer.grab_start_data() else {
                    source.cancel();
                    return;
                };
                let grab = DnDGrab::new_pointer(&self.display_handle, start, source, seat);
                pointer.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => {
                let Some(touch) = seat.get_touch() else {
                    source.cancel();
                    return;
                };
                let Some(start) = touch.grab_start_data() else {
                    source.cancel();
                    return;
                };
                let grab = DnDGrab::new_touch(&self.display_handle, start, source, seat);
                touch.set_grab(self, grab, serial);
            }
        }
    }
}

impl DndGrabHandler for State {
    fn dropped(
        &mut self,
        _target: Option<smithay::input::dnd::DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.dnd_icon = None;
    }
}

// ---------------------------------------------------------------------------------------------------------
// outputs, fractional scale

impl OutputHandler for State {}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self
            .output()
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fs| fs.set_preferred_scale(scale));
        });
    }
}

// ---------------------------------------------------------------------------------------------------------
// xdg-activation: apps we launch get a token (apps/mod.rs); a window presenting a fresh one comes forward.

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }

    fn request_activation(&mut self, token: XdgActivationToken, data: XdgActivationTokenData, surface: WlSurface) {
        // Our launch tokens and client tokens made right after input; anything older is a focus steal.
        let fresh = data.timestamp.elapsed() < Duration::from_secs(30);
        let window = self.window_for_surface(&surface);
        info!(fresh, known = window.is_some(), "activation request");
        if fresh && let Some(w) = window {
            self.activate_window(&w);
        }
        self.activation_state.remove_token(&token);
    }
}

delegate_dispatch2!(State);

/// A toplevel's xdg app_id, lowercased.
pub fn window_app_id(w: &Window) -> Option<String> {
    crate::winconf::app_id(w)
}

/// Frames drawn with the side panel on screen (for the log line when it closes).
#[derive(Debug, Default)]
pub struct PanelFrames {
    pub frames: u32,
    /// Frames that came more than 1.5 refreshes after the previous one (gaps over 100 ms are idle, not late).
    pub late: u32,
    pub max_gap: Duration,
    pub max_render: Duration,
    pub last: Option<std::time::Instant>,
}
