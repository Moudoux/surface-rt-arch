//! The interface between the compositor core and window policies (DESKTOP-SHELL.md 3.6, layer 2).
//!
//! This file belongs to the core: the core calls a `dyn WindowPolicy` and never names a concrete policy.
//! Policies (`wm/`) decide where toplevels go, their size and configure states, stacking, which windows are
//! shown at all, and what gets focus. The core keeps the protocol plumbing: it sends the initial configure,
//! tracks which toplevels have content, hands out keyboard focus as the policy returns it, and draws whatever
//! is mapped in the `Space`, at whatever position and size, in stacking order.
//!
//! The `Space` holds the windows that are *shown*; a policy unmaps windows it hides (e.g. background apps in
//! `FullscreenPolicy`) and maps them again when they come back.

use smithay::{
    desktop::{Space, Window},
    output::Output,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
    utils::{Logical, Point, Rectangle},
};

/// What a policy may touch.
pub struct WmCtx<'a> {
    pub space: &'a mut Space<Window>,
    /// The output windows are laid out on (single-output for now).
    #[allow(dead_code)] // for policies that need output properties (scale, transform)
    pub output: &'a Output,
    /// Area available to windows, in space coordinates (the output minus reserved shell areas).
    pub area: Rectangle<i32, Logical>,
    /// The whole output, in space coordinates (for fullscreen).
    pub output_geometry: Rectangle<i32, Logical>,
}

/// Which half of the screen a window snaps to: `Start` = left (landscape) or top (portrait), `End` = right or
/// bottom. Split view in `FullscreenPolicy`; a floating policy would snap a window to that half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapSide {
    Start,
    End,
}

/// A policy-owned handle being dragged (the split divider now; title bars and resize edges later).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HandlePhase {
    Down,
    Motion,
    Up,
    Cancel,
}

/// What a decoration rectangle shows; the core picks its look from theme tokens (`[split]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecoRole {
    /// The bar between two split apps (opaque); the theme draws its grip in the middle. `DividerActive`
    /// while it is dragged.
    Divider,
    DividerActive,
    /// Where the divider will land when released (a divider-sized rectangle; the theme draws a mark in it).
    SnapMark,
    /// Area uncovered while the divider is dragged (the apps are resized only on release; opaque).
    Reveal,
    /// Covers the app that a release here would dismiss.
    Dismiss,
}

/// A rectangle the policy wants drawn above the windows (space coordinates), bottom to top.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoration {
    pub rect: Rectangle<i32, Logical>,
    pub role: DecoRole,
}

/// Returned by policy hooks: which window should get keyboard focus now.
#[derive(Debug, Clone, PartialEq)]
pub enum FocusChange {
    /// Leave focus as it is.
    Keep,
    /// Focus this window.
    Focus(Window),
    /// Nothing should be focused.
    Clear,
}

pub trait WindowPolicy {
    fn name(&self) -> &'static str;

    /// Called right before the initial configure of a new toplevel (its parent, app_id and size hints are
    /// known by then). Set the pending configure state (size, states, bounds); the core sends it.
    fn configure_new(&mut self, ctx: &mut WmCtx, window: &Window);

    /// The toplevel committed its first buffer: show it (map it into `ctx.space`) as the policy sees fit.
    fn map(&mut self, ctx: &mut WmCtx, window: &Window) -> FocusChange;

    /// The toplevel was destroyed or lost its buffer. Remove it from `ctx.space` and the policy's own lists.
    fn unmap(&mut self, ctx: &mut WmCtx, window: &Window) -> FocusChange;

    /// A mapped toplevel committed (size may have changed).
    fn committed(&mut self, _ctx: &mut WmCtx, _window: &Window) {}

    /// The user picked this window (tap, click, xdg-activation).
    fn activate(&mut self, ctx: &mut WmCtx, window: &Window) -> FocusChange;

    /// Switch to the next/previous app (temporary Alt+Tab until the Overview exists).
    fn cycle(&mut self, ctx: &mut WmCtx, forward: bool) -> FocusChange;

    /// The client asked for (un)fullscreen. The policy must send a configure in reply.
    fn fullscreen_request(&mut self, ctx: &mut WmCtx, window: &Window, fullscreen: bool);

    /// The client asked for (un)maximize. The policy must send a configure in reply.
    fn maximize_request(&mut self, ctx: &mut WmCtx, window: &Window, maximize: bool);

    /// Interactive move/resize requests (xdg_toplevel.move/resize). A floating policy will need pointer/touch
    /// grabs from the core for these; `WmCtx` grows when that policy is written.
    fn move_request(&mut self, _ctx: &mut WmCtx, _window: &Window) {}
    fn resize_request(&mut self, _ctx: &mut WmCtx, _window: &Window) {}

    /// Home on (hide every app: unmap, suspend, clear focus) or off (show the most recent app again).
    /// A new window, `activate` and `cycle` end Home by themselves.
    fn set_home(&mut self, _ctx: &mut WmCtx, _home: bool) -> FocusChange {
        FocusChange::Keep
    }

    /// The running apps for the Overview, most recently used first; each app with its dialogs (bottom to
    /// top) at the positions they have when shown.
    fn families(&self, _ctx: &WmCtx) -> Vec<Vec<(Window, Point<i32, Logical>)>> {
        Vec::new()
    }

    /// Snap `window`'s app to one half of the screen (`None`: back to the whole screen). In split view the
    /// partner is `with`'s app if given, else the most recent other app; with no other app this does nothing
    /// (returns `Keep`).
    fn snap(&mut self, _ctx: &mut WmCtx, _window: &Window, _side: Option<SnapSide>, _with: Option<&Window>) -> FocusChange {
        FocusChange::Keep
    }

    /// The apps (no dialogs) that form the unit `window` belongs to: the app itself, or both apps of a split
    /// pair. Closing a card or pulling the shown unit down closes all of them.
    fn unit_apps(&self, window: &Window) -> Vec<Window> {
        vec![window.clone()]
    }

    /// Does a policy handle (split divider, later title bars) sit under `pos`? If so, the core sends that
    /// touch or button press to `handle_drag` instead of the app.
    fn handle_at(&self, _ctx: &WmCtx, _pos: Point<f64, Logical>) -> bool {
        false
    }

    /// A handle grabbed by `handle_at` moves, is let go or cancelled. `time` is a monotonic clock (double taps).
    fn handle_drag(&mut self, _ctx: &mut WmCtx, _phase: HandlePhase, _pos: Point<f64, Logical>, _time: std::time::Duration) -> FocusChange {
        FocusChange::Keep
    }

    /// Rectangles to draw above the shown windows (split divider and its drag feedback). Not drawn while a
    /// switch animation or the Overview runs.
    fn decorations(&self, _ctx: &WmCtx) -> Vec<Decoration> {
        Vec::new()
    }

    /// The output size or the reserved areas changed (rotation, mode change).
    fn area_changed(&mut self, ctx: &mut WmCtx);

    /// xdg-decoration mode to tell clients.
    fn decoration_mode(&self) -> DecorationMode {
        DecorationMode::ServerSide
    }
}
