//! Configuring windows of both kinds the same way (core; DESKTOP-SHELL.md 3.6): xdg toplevels and, with the
//! `xwayland` feature, X11 windows. Policies set a `Pending` state (size, bounds, maximized, fullscreen,
//! tiled, suspended) and place windows; this module turns that into xdg configures, or for X11 into a
//! ConfigureWindow at the placed position plus the _NET_WM_STATE hints.
//!
//! X11 has no pending/ack cycle: the pending state lives in the window's user data and is sent when the
//! window is placed (`place`) or when `send` is asked for a window that was placed before.

#[cfg(feature = "xwayland")]
use std::cell::RefCell;

use smithay::{
    desktop::Window,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Size},
    wayland::{
        compositor::with_states,
        shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData},
    },
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;

const TILED: [XdgState; 4] = [XdgState::TiledLeft, XdgState::TiledRight, XdgState::TiledTop, XdgState::TiledBottom];

/// The configure state a policy decides, the same for every kind of window.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pending {
    pub size: Option<Size<i32, Logical>>,
    pub bounds: Option<Size<i32, Logical>>,
    pub maximized: bool,
    pub fullscreen: bool,
    /// Tiled on all four sides (split halves, maximized apps).
    pub tiled: bool,
    pub suspended: bool,
}

/// X11 windows: the pending state and where the window was last placed.
#[cfg_attr(not(feature = "xwayland"), allow(dead_code))]
#[derive(Debug, Default)]
struct X11Conf {
    pending: Pending,
    loc: Option<Point<i32, Logical>>,
}

#[cfg(feature = "xwayland")]
fn x11_conf<R>(w: &Window, f: impl FnOnce(&mut X11Conf) -> R) -> R {
    w.user_data().insert_if_missing(|| RefCell::new(X11Conf::default()));
    let cell = w.user_data().get::<RefCell<X11Conf>>().expect("just inserted");
    f(&mut cell.borrow_mut())
}

/// Read and change the pending state.
pub fn with_pending<R>(w: &Window, f: impl FnOnce(&mut Pending) -> R) -> R {
    if let Some(tl) = w.toplevel() {
        return tl.with_pending_state(|s| {
            let mut p = Pending {
                size: s.size,
                bounds: s.bounds,
                maximized: s.states.contains(XdgState::Maximized),
                fullscreen: s.states.contains(XdgState::Fullscreen),
                tiled: TILED.iter().all(|t| s.states.contains(*t)),
                suspended: s.states.contains(XdgState::Suspended),
            };
            let r = f(&mut p);
            s.size = p.size;
            s.bounds = p.bounds;
            let mut set = |st: XdgState, on: bool| {
                if on {
                    s.states.set(st);
                } else {
                    s.states.unset(st);
                }
            };
            set(XdgState::Maximized, p.maximized);
            set(XdgState::Fullscreen, p.fullscreen);
            set(XdgState::Suspended, p.suspended);
            for t in TILED {
                set(t, p.tiled);
            }
            r
        });
    }
    #[cfg(feature = "xwayland")]
    if w.x11_surface().is_some() {
        return x11_conf(w, |c| f(&mut c.pending));
    }
    f(&mut Pending::default())
}

/// The window has had its first configure (xdg) / is managed (X11).
pub fn initial_sent(w: &Window) -> bool {
    match w.toplevel() {
        Some(tl) => tl.is_initial_configure_sent(),
        None => true,
    }
}

/// Send the pending state if it changed (xdg), or re-place an X11 window that was placed before.
pub fn send(w: &Window) {
    if let Some(tl) = w.toplevel() {
        if tl.is_initial_configure_sent() {
            tl.send_pending_configure();
        }
        return;
    }
    #[cfg(feature = "xwayland")]
    if w.x11_surface().is_some()
        && let Some(loc) = x11_conf(w, |c| c.loc)
    {
        place(w, loc);
    }
}

/// Send a configure even if nothing changed (the protocol wants an answer to a request).
pub fn send_always(w: &Window) {
    match w.toplevel() {
        Some(tl) if tl.is_initial_configure_sent() => {
            tl.send_configure();
        }
        Some(_) => {}
        None => send(w),
    }
}

/// The window is shown at `loc` (space coordinates): X11 windows are moved and sized there (an X client
/// positions its menus from it); xdg toplevels need nothing.
pub fn place(w: &Window, loc: Point<i32, Logical>) {
    #[cfg(feature = "xwayland")]
    if let Some(x) = w.x11_surface() {
        let p = x11_conf(w, |c| {
            c.loc = Some(loc);
            c.pending
        });
        let size = p.size.unwrap_or_else(|| x.geometry().size);
        if let Err(err) = x.configure(smithay::utils::Rectangle::new(loc, size)) {
            tracing::warn!(%err, "x11: configure");
        }
        let _ = x.set_maximized(p.maximized);
        let _ = x.set_fullscreen(p.fullscreen);
    }
    #[cfg(not(feature = "xwayland"))]
    let _ = (w, loc);
}

/// The client's minimum size (0 = none).
pub fn min_size(w: &Window) -> Size<i32, Logical> {
    if let Some(tl) = w.toplevel() {
        return with_states(tl.wl_surface(), |states| states.cached_state.get::<SurfaceCachedState>().current().min_size);
    }
    #[cfg(feature = "xwayland")]
    if let Some(x) = w.x11_surface() {
        return x.min_size().unwrap_or_default();
    }
    Size::default()
}

/// Is `w` a dialog of `parent` (xdg parent / X11 WM_TRANSIENT_FOR)?
pub fn is_child_of(w: &Window, parent: &Window) -> bool {
    if let (Some(tl), Some(ptl)) = (w.toplevel(), parent.toplevel()) {
        return tl.parent().as_ref() == Some(ptl.wl_surface());
    }
    #[cfg(feature = "xwayland")]
    if let (Some(x), Some(px)) = (w.x11_surface(), parent.x11_surface()) {
        return x.is_transient_for() == Some(px.window_id());
    }
    false
}

/// Has a parent at all (known to us or not).
pub fn has_parent(w: &Window) -> bool {
    if let Some(tl) = w.toplevel() {
        return tl.parent().is_some();
    }
    #[cfg(feature = "xwayland")]
    if let Some(x) = w.x11_surface() {
        return x.is_transient_for().is_some();
    }
    false
}

/// xdg app_id, or the X11 WM_CLASS class, lowercased.
pub fn app_id(w: &Window) -> Option<String> {
    if let Some(tl) = w.toplevel() {
        return with_states(tl.wl_surface(), |st| st.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.app_id.clone()))
            .map(|id| id.to_lowercase());
    }
    #[cfg(feature = "xwayland")]
    if let Some(x) = w.x11_surface() {
        let c = x.class();
        return (!c.is_empty()).then(|| c.to_lowercase());
    }
    None
}

/// Ask the window to close (xdg_toplevel.close / WM_DELETE_WINDOW).
pub fn close(w: &Window) {
    if let Some(tl) = w.toplevel() {
        tl.send_close();
        return;
    }
    #[cfg(feature = "xwayland")]
    if let Some(x) = w.x11_surface() {
        let _ = x.close();
    }
}

/// The window's main wl_surface (an X11 window has one only once Xwayland associated it).
pub fn surface(w: &Window) -> Option<WlSurface> {
    use smithay::wayland::seat::WaylandFocus;
    w.wl_surface().map(|s| s.into_owned())
}

/// After keyboard focus went to `w`'s surface: X11 windows also need the X input focus.
pub fn focused(w: &Window) {
    #[cfg(feature = "xwayland")]
    if let Some(x) = w.x11_surface() {
        x.take_input_focus();
    }
    #[cfg(not(feature = "xwayland"))]
    let _ = w;
}

/// The process to kill for a force quit: an X11 window's _NET_WM_PID (killing Xwayland, the wl client,
/// would take every X app with it); None for Wayland clients (the caller uses the client's credentials).
pub fn x11_pid(w: &Window) -> Option<i32> {
    #[cfg(feature = "xwayland")]
    if let Some(x) = w.x11_surface() {
        return x.pid().map(|p| p as i32);
    }
    let _ = w;
    None
}

pub fn is_x11(w: &Window) -> bool {
    #[cfg(feature = "xwayland")]
    return w.x11_surface().is_some();
    #[cfg(not(feature = "xwayland"))]
    {
        let _ = w;
        false
    }
}
