//! X11 apps through Xwayland (core; `xwayland` feature). Xwayland runs rootless and is started once after
//! the first frame (not in greeter mode); apps rtshell starts get `DISPLAY`. X11 windows become ordinary
//! `Toplevel`s handed to the window policy, configured through `winconf` (an X ConfigureWindow where the
//! policy places them); override-redirect windows (menus, tooltips) go straight into the space at their own
//! position. X keyboard focus follows the seat focus (`winconf::focused`).
//!
//! Not yet: clipboard between X11 and Wayland apps (the XWM selection hooks), X11 cursors (Xwayland draws
//! through wl_pointer.set_cursor), scaling.

use std::process::Stdio;

use smithay::{
    desktop::Window,
    utils::{Logical, Rectangle},
    wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    xwayland::{
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
        xwm::{Reorder, ResizeEdge, XwmId},
    },
};
use tracing::{debug, info, warn};

use crate::{
    policy::FocusChange,
    state::{State, Toplevel},
    winconf,
};

impl State {
    /// Start Xwayland (once). The X display becomes known when it is ready (`xdisplay`).
    pub fn start_xwayland(&mut self) {
        if self.xwayland_started {
            return;
        }
        self.xwayland_started = true;
        let started = std::time::Instant::now();
        let res = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            std::iter::empty::<String>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        );
        let (xwayland, client) = match res {
            Ok(x) => x,
            Err(err) => return warn!(%err, "xwayland: cannot start /usr/bin/Xwayland"),
        };
        let dh = self.display_handle.clone();
        let res = self.loop_handle.insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => match X11Wm::start_wm(state.loop_handle.clone(), &dh, x11_socket, client.clone()) {
                Ok(wm) => {
                    info!(display = display_number, ms = started.elapsed().as_millis() as u64, "xwayland: ready");
                    state.xwm = Some(wm);
                    state.xdisplay = Some(display_number);
                }
                Err(err) => warn!(?err, "xwayland: window manager failed"),
            },
            XWaylandEvent::Error => warn!("xwayland: exited during startup"),
        });
        if let Err(err) = res {
            warn!(%err, "xwayland: cannot watch it");
        }
    }

    fn x11_index(&self, w: &X11Surface) -> Option<usize> {
        self.toplevels.iter().position(|t| t.window.x11_surface() == Some(w))
    }

    fn x11_window(&self, w: &X11Surface) -> Option<Window> {
        self.x11_index(w).map(|i| self.toplevels[i].window.clone())
    }

    /// Gone (unmapped or destroyed): out of the policy and the lists, or out of the space (override-redirect).
    fn x11_forget(&mut self, w: &X11Surface) {
        if let Some(i) = self.x11_override.iter().position(|o| o.x11_surface() == Some(w)) {
            let o = self.x11_override.remove(i);
            self.space.unmap_elem(&o);
            self.queue_redraw();
        }
        if let Some(i) = self.x11_index(w) {
            debug!(class = %w.class(), "x11: window gone");
            self.forget_toplevel(i);
        }
    }
}

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

impl XwmHandler for State {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("xwm events only come while it runs")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if self.x11_index(&window).is_some() {
            return;
        }
        if let Err(err) = window.set_mapped(true) {
            return warn!(%err, "x11: cannot map");
        }
        info!(class = %window.class(), title = %window.title(), dialog = window.is_transient_for().is_some(), "x11: new window");
        let w = Window::new_x11_window(window);
        // Handed to the policy on its first buffer (toplevel_root_commit), like an xdg toplevel; configure it
        // now so it draws at the right size from the start.
        self.toplevels.push(Toplevel { window: w.clone(), mapped: false });
        let ww = w.clone();
        self.with_policy(|p, ctx| {
            p.configure_new(ctx, &ww);
            FocusChange::Keep
        });
        if let Some(area) = self.area() {
            winconf::place(&w, area.loc);
        }
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let loc = window.geometry().loc;
        let w = Window::new_x11_window(window);
        self.space.map_element(w.clone(), loc, true);
        self.x11_override.push(w);
        self.queue_redraw();
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_forget(&window);
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_forget(&window);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let Some(managed) = self.x11_window(&window) else {
            // Not ours yet (before its map request): as asked.
            let mut geo = window.geometry();
            if let Some(x) = x {
                geo.loc.x = x;
            }
            if let Some(y) = y {
                geo.loc.y = y;
            }
            if let Some(w) = w {
                geo.size.w = w as i32;
            }
            if let Some(h) = h {
                geo.size.h = h as i32;
            }
            let _ = window.configure(geo);
            return;
        };
        // Apps keep the policy's rectangle; dialogs may pick their size (the policy keeps them centred and
        // within their app). Either way the window gets the policy's answer.
        if winconf::has_parent(&managed) {
            let cur = window.geometry().size;
            let size = (w.map_or(cur.w, |w| w as i32), h.map_or(cur.h, |h| h as i32)).into();
            winconf::with_pending(&managed, |p| p.size = Some(size));
            self.with_policy(|p, ctx| {
                p.committed(ctx, &managed);
                FocusChange::Keep
            });
        }
        winconf::send(&managed);
    }

    fn configure_notify(&mut self, _xwm: XwmId, window: X11Surface, geometry: Rectangle<i32, Logical>, _above: Option<u32>) {
        if let Some(o) = self.x11_override.iter().find(|o| o.x11_surface() == Some(&window)).cloned() {
            self.space.map_element(o, geometry.loc, true);
            self.queue_redraw();
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.x11_window(&window) {
            self.with_policy(|p, ctx| {
                p.maximize_request(ctx, &w, true);
                FocusChange::Keep
            });
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.x11_window(&window) {
            self.with_policy(|p, ctx| {
                p.maximize_request(ctx, &w, false);
                FocusChange::Keep
            });
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.x11_window(&window) {
            self.with_policy(|p, ctx| {
                p.fullscreen_request(ctx, &w, true);
                FocusChange::Keep
            });
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.x11_window(&window) {
            self.with_policy(|p, ctx| {
                p.fullscreen_request(ctx, &w, false);
                FocusChange::Keep
            });
        }
    }

    // Windows are placed by the policy: interactive move/resize do nothing (FloatingPolicy, Phase 10).
    fn resize_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32, _edge: ResizeEdge) {}
    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}
}
