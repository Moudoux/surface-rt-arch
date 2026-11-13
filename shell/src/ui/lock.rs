//! Lock screen (DESKTOP-SHELL.md Phase 8 design), default look: the Windows 8/RT lock screen.
//!
//! A full-screen "curtain" (the wallpaper with a large clock, the date, battery and Wi-Fi) covers the
//! sign-in screen. A swipe up (the curtain follows the finger; distance or flick decides), a tap, a click or
//! any key lifts it; the sign-in screen shows the user's name and a password box (Type Cover or the built-in
//! keyboard) or the PIN pad (`[lock] entry = "pin"` in config.toml, for a numeric password). The curtain
//! drops back after `curtain_timeout_ms` without input, and the typed text is wiped.
//!
//! The scene is drawn by `ShellUi` as its own top layer while locked; the core then draws nothing else
//! (lock.rs). Unlocking: the password goes to `services::auth` (PAM on a worker thread); on success the
//! scene sends `Request::Unlock`. logind's Lock / Unlock / PrepareForSleep arrive through the services
//! thread (`Services::take_session_events`); before sleep the scene lets the sleep go ahead once a frame
//! with the lock has been presented (`presented`).
//!
//! Secrets: the typed text is a `SecretBuf` (wiped on every edit, on curtain drop and on unlock); only
//! bullets are drawn; nothing typed is logged. Every look value comes from the `[lock]` tokens.

use std::time::Duration;

use smithay::{
    input::keyboard::keysyms as k,
    utils::{Clock, Monotonic},
};
use tracing::info;

use super::{Align, Layout, Node, NodeId, Paint, Size, Tree, home::local_time, shell::Scene};
use crate::{
    anim::{Spring, SpringParams, VelocityTracker},
    overlay::{CANCEL_ALL, KeyEvent, LockReason, Phase, PointerEvent, Request},
    render::{batch::Rgba, gl::Texture, text::TextStyle},
    services::{
        Charge, Command, Link, SessionEvent, Services,
        auth::{Authenticator, Backend, SecretBuf, User},
        greetd::SessionCmd,
    },
    theme::Theme,
};

/// Finger travel before a touch counts as a drag (px).
const SLOP: f32 = 10.0;
const BULLET: char = '\u{25cf}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PadKey {
    Digit(u8),
    Back,
    Ok,
}

impl PadKey {
    /// Name for scripted tests (`lock_point`).
    fn name(self) -> String {
        match self {
            PadKey::Digit(d) => d.to_string(),
            PadKey::Back => "back".into(),
            PadKey::Ok => "ok".into(),
        }
    }
}

const PAD: [PadKey; 12] = [
    PadKey::Digit(1),
    PadKey::Digit(2),
    PadKey::Digit(3),
    PadKey::Digit(4),
    PadKey::Digit(5),
    PadKey::Digit(6),
    PadKey::Digit(7),
    PadKey::Digit(8),
    PadKey::Digit(9),
    PadKey::Back,
    PadKey::Digit(0),
    PadKey::Ok,
];

struct Nodes {
    signin: NodeId,
    /// The user's name above the box.
    name: NodeId,
    /// The box + arrow row, the PIN pad, "Switch user" (greeter with several users).
    row: NodeId,
    pad: NodeId,
    switch: NodeId,
    /// Greeter: the user tiles (one per `GreeterState::users`) and their row.
    picker: NodeId,
    tiles: Vec<NodeId>,
    /// The sign-in column (moves up while the built-in keyboard covers the bottom).
    col: NodeId,
    field_outer: NodeId,
    field_text: NodeId,
    submit: NodeId,
    message: NodeId,
    keys: Vec<(NodeId, PadKey)>,
    curtain: NodeId,
    time: NodeId,
    date: NodeId,
    battery: NodeId,
    wifi: NodeId,
}

/// What a touch is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Grab {
    /// Moving the curtain (it started while the curtain was down or moving).
    Curtain { y0: f32, lift0: f32 },
    /// On the sign-in screen, on this node (a pad key, the box, the arrow) or nothing.
    Signin { x0: f32, y0: f32, on: Option<NodeId> },
}

struct Touch {
    id: i32,
    grab: Grab,
    moved: bool,
    vt: VelocityTracker,
}

/// Greeter mode (`rtshell --greeter`): who can sign in and who is picked.
struct GreeterState {
    users: Vec<User>,
    /// The picked user (or the highlighted tile while picking).
    chosen: usize,
    /// The user tiles are up instead of the box (several users, none picked yet, or "Switch user").
    picking: bool,
    /// greetd accepted the session: "Signing in…", input ignored, waiting for the core to quit.
    done: bool,
}

pub struct LockScreen {
    theme: Theme,
    /// Some in greeter mode: never unlocks; a successful sign-in ends rtshell (`Request::Quit`).
    greeter: Option<GreeterState>,
    pin: bool,
    tree: Tree,
    n: Nodes,
    services: Option<Services>,
    auth: Authenticator,
    locked: bool,
    /// 0 = curtain down, 1 = lifted (sign-in screen uncovered).
    lift: Spring,
    springs: SpringParams,
    w: f32,
    h: f32,
    touch: Option<Touch>,
    pressed: Option<NodeId>,
    pw: SecretBuf,
    /// Shown under the box: PAM's words or "Checking…", and whether it is an error.
    message: Option<(String, bool)>,
    /// The password box has the keyboard (the built-in keyboard comes up for it).
    focus: bool,
    hw_keyboard: bool,
    wallpaper: Option<Texture>,
    now: Duration,
    last_input: Duration,
    next_minute: Option<Duration>,
    sys_gen: u64,
    requests: Vec<Request>,
    /// A sleep waits for the lock to be on the panel.
    sleep_pending: bool,
    /// Height the built-in keyboard covers at the bottom (the sign-in column centres above it).
    inset: Spring,
    clock: Clock<Monotonic>,
}

fn text(s: &str, size: f32, weight: u16, color: Rgba) -> Paint {
    Paint::Text { text: s.into(), style: TextStyle { size, weight, ..TextStyle::default() }, color, wrap: false, layout: None }
}

fn mono(s: &str, size: f32, weight: u16, color: Rgba) -> Paint {
    Paint::Text { text: s.into(), style: TextStyle { size, weight, mono: true, ..TextStyle::default() }, color, wrap: false, layout: None }
}

fn set_color(tree: &mut Tree, id: NodeId, c: Rgba) {
    match &mut tree[id].paint {
        Paint::Text { color, .. } | Paint::Rect { color, .. } => *color = c,
        _ => {}
    }
}

/// An optional colour token: empty = the accent colour.
fn or_accent(token: &str, theme: &Theme) -> Rgba {
    crate::theme::Color::parse(token).unwrap_or(theme.colors.accent).premultiplied()
}

fn align_of(s: &str) -> Align {
    match s {
        "left" | "top" => Align::Start,
        "right" | "bottom" => Align::End,
        _ => Align::Center,
    }
}

pub(super) fn battery_icon(percent: f32, charge: Charge) -> String {
    let level = match percent {
        p if p >= 80.0 => "full",
        p if p >= 45.0 => "good",
        p if p >= 15.0 => "low",
        _ => "caution",
    };
    match charge {
        Charge::Charging => format!("battery-{level}-charging-symbolic"),
        // Breeze has no "-charged" variant.
        Charge::Full => "battery-full-charging-symbolic".into(),
        _ => format!("battery-{level}-symbolic"),
    }
}

impl LockScreen {
    pub fn new(theme: &Theme, config: &crate::config::Lock, services: Option<Services>, ping: smithay::reexports::calloop::ping::Ping) -> LockScreen {
        let pin = config.entry == "pin";
        let auth = Authenticator::new(&config.pam_service, ping);
        info!(entry = if pin { "pin" } else { "password" }, service = %config.pam_service, "lock screen ready");
        Self::with(theme, pin, auth, None, services)
    }

    /// The login screen (`rtshell --greeter`): people from /etc/passwd, sign-in through greetd at `sock`.
    pub fn greeter(
        theme: &Theme,
        config: &crate::config::Lock,
        greeter: &crate::config::Greeter,
        sock: Option<std::path::PathBuf>,
        services: Option<Services>,
        ping: smithay::reexports::calloop::ping::Ping,
    ) -> LockScreen {
        let pin = config.entry == "pin";
        if sock.is_none() {
            tracing::error!("greeter: GREETD_SOCK is not set; nobody can sign in");
        }
        let backend = Backend::Greetd {
            sock: sock.unwrap_or_default(),
            session: SessionCmd { cmd: greeter.session.clone(), env: greeter.env.clone() },
            wrong_password: "The password is incorrect. Try again.".into(),
        };
        let mut auth = Authenticator::with_backend(backend, ping);
        let users = User::list();
        let chosen = users.iter().position(|u| u.name == greeter.default_user).unwrap_or(0);
        auth.set_user(users.get(chosen).cloned());
        info!(users = users.len(), entry = if pin { "pin" } else { "password" }, session = ?greeter.session, "greeter ready");
        let g = GreeterState { picking: users.len() > 1, users, chosen, done: false };
        Self::with(theme, pin, auth, Some(g), services)
    }

    fn with(theme: &Theme, pin: bool, auth: Authenticator, greeter: Option<GreeterState>, services: Option<Services>) -> LockScreen {
        let display = auth.user().map(|u| u.display.clone()).unwrap_or_default();
        let users: &[User] = greeter.as_ref().map_or(&[], |g| &g.users);
        let (tree, n) = Self::build(theme, pin, &display, None, users);
        let springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        let mut me = LockScreen {
            greeter,
            theme: theme.clone(),
            pin,
            tree,
            n,
            services,
            auth,
            locked: false,
            lift: Spring::new(0.0, springs).with_eps(0.002),
            springs,
            w: 1.0,
            h: 1.0,
            touch: None,
            pressed: None,
            pw: SecretBuf::new(),
            message: None,
            focus: false,
            hw_keyboard: false,
            wallpaper: None,
            now: Duration::ZERO,
            last_input: Duration::ZERO,
            next_minute: None,
            sys_gen: 0,
            requests: Vec::new(),
            sleep_pending: false,
            inset: Spring::new(0.0, springs).with_eps(0.5),
            clock: Clock::new(),
        };
        if me.greeter.as_ref().is_some_and(|g| g.users.is_empty()) {
            me.message = Some((me.theme.lock.label_no_users.clone(), true));
        }
        me.apply();
        me
    }

    fn build(theme: &Theme, pin: bool, display: &str, wallpaper: Option<Texture>, users: &[User]) -> (Tree, Nodes) {
        let l = &theme.lock;
        let fg = l.text.premultiplied();
        let sfg = l.signin_text.premultiplied();
        let mut tree = Tree::new(Node::new().size(Size::Fill(1.0), Size::Fill(1.0)));
        let root = tree.root;

        // Sign-in screen (under the curtain).
        let fill = l.signin_fill.premultiplied();
        let signin_paint = if l.signin_glass { Paint::Glass { radius: 0.0, tint: fill } } else { Paint::Rect { color: fill, radius: 0.0 } };
        let signin = tree.add(root, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(signin_paint).hit());
        let col = tree.add(signin, Node::new().layout(Layout::Column { gap: l.gap }).align(Align::Center, Align::Center));
        // Greeter: the user tiles (a square with the initial, the name under it).
        let picker = tree.add(col, Node::new().layout(Layout::Row { gap: l.user_gap }).align(Align::Center, Align::Start));
        let mut tiles = Vec::new();
        for u in users {
            let tile = tree.add(picker, Node::new().layout(Layout::Column { gap: l.gap * 0.5 }).hit());
            let square = tree.add(
                tile,
                Node::new()
                    .size(Size::Fixed(l.user_tile), Size::Fixed(l.user_tile))
                    .paint(Paint::Rect { color: l.user_tile_fill.premultiplied(), radius: l.user_tile_radius }),
            );
            let initial: String = u.display.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            tree.add(square, Node::new().align(Align::Center, Align::Center).paint(text(&initial, l.user_initial_size, l.user_initial_weight, sfg)));
            tree.add(tile, Node::new().align(Align::Center, Align::Start).paint(text(&u.display, l.user_name_size, l.name_weight, sfg)));
            tiles.push(tile);
        }
        let name = tree.add(col, Node::new().align(Align::Center, Align::Start).paint(text(display, l.name_size, l.name_weight, sfg)));
        let row = tree.add(col, Node::new().layout(Layout::Row { gap: 0.0 }).align(Align::Center, Align::Start));
        let field_outer = tree.add(
            row,
            Node::new()
                .size(Size::Fixed(l.field_width), Size::Fixed(l.field_height))
                .padding(l.field_border)
                .paint(Paint::Rect { color: l.field_fill.premultiplied(), radius: l.field_radius })
                .hit(),
        );
        let field_inner = tree.add(
            field_outer,
            Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(Paint::Rect { color: l.field_fill.premultiplied(), radius: l.field_radius }),
        );
        let field_text = tree.add(
            field_inner,
            Node::new().align(Align::Start, Align::Center).offset(l.field_height * 0.25, 0.0).paint(text("", l.message_size, 400, l.field_placeholder.premultiplied())),
        );
        let submit = tree.add(
            row,
            Node::new()
                .size(Size::Fixed(l.field_height), Size::Fixed(l.field_height))
                .paint(Paint::Rect { color: or_accent(&l.submit_fill, theme), radius: l.field_radius })
                .hit(),
        );
        tree.add(submit, Node::new().align(Align::Center, Align::Center).paint(mono(&l.label_submit, l.message_size * 1.4, 400, l.submit_text.premultiplied())));
        let message = tree.add(
            col,
            Node::new().size(Size::Fixed(l.field_width + l.field_height), Size::Fit).paint(Paint::Text {
                text: String::new(),
                style: TextStyle { size: l.message_size, weight: 400, ..TextStyle::default() },
                color: l.message_text.premultiplied(),
                wrap: true,
                layout: None,
            }),
        );
        let pad_w = 3.0 * l.pin_key + 2.0 * l.pin_gap;
        let pad = tree.add(col, Node::new().size(Size::Fixed(pad_w), Size::Fit).layout(Layout::Grid { columns: 3, gap: l.pin_gap }).align(Align::Center, Align::Start));
        let mut keys = Vec::new();
        for key in PAD {
            let node = tree.add(
                pad,
                Node::new().size(Size::Fixed(l.pin_key), Size::Fixed(l.pin_key)).paint(Paint::Rect { color: l.pin_fill.premultiplied(), radius: l.pin_radius }).hit(),
            );
            let label = match key {
                PadKey::Digit(d) => text(&d.to_string(), l.pin_size, l.pin_weight, l.pin_text.premultiplied()),
                PadKey::Back => mono(&l.label_back, l.pin_size * 0.8, 400, l.pin_text.premultiplied()),
                PadKey::Ok => text(&l.label_ok, l.pin_size * 0.7, l.pin_weight, l.pin_text.premultiplied()),
            };
            tree.add(node, Node::new().align(Align::Center, Align::Center).paint(label));
            keys.push((node, key));
        }
        tree.set_visible(pad, pin);
        tree.set_visible(submit, !pin);
        let switch = tree.add(col, Node::new().align(Align::Center, Align::Start).padding(l.gap * 0.5).hit());
        tree.add(switch, Node::new().paint(text(&l.label_switch_user, l.message_size, 400, sfg)));
        tree.set_visible(switch, false);
        tree.set_visible(picker, false);

        // The curtain (on top): wallpaper, scrim, clock block.
        let curtain_paint = match wallpaper {
            Some(tex) => Paint::Backdrop { tex },
            None => Paint::Rect { color: theme.colors.background.premultiplied(), radius: 0.0 },
        };
        let curtain = tree.add(root, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(curtain_paint).hit());
        let mut scrim = l.curtain_scrim.premultiplied();
        // A black scrim darkens the curtain's own paint (DrawList::darken); keep only its alpha.
        scrim[0] = 0.0;
        scrim[1] = 0.0;
        scrim[2] = 0.0;
        tree.add(curtain, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(Paint::Scrim { color: scrim }));
        let (ax, ay) = (align_of(&l.anchor), align_of(&l.anchor_y));
        let ox = match ax {
            Align::Start => l.margin_x,
            Align::End => -l.margin_x,
            Align::Center => 0.0,
        };
        let oy = match ay {
            Align::Start => l.margin_y,
            Align::End => -l.margin_y,
            Align::Center => 0.0,
        };
        let block = tree.add(curtain, Node::new().layout(Layout::Column { gap: 0.0 }).align(ax, ay).offset(ox, oy));
        let time = tree.add(block, Node::new().align(ax, Align::Start).paint(text("", l.clock_size, l.clock_weight, fg)));
        let date = tree.add(block, Node::new().align(ax, Align::Start).paint(text("", l.date_size, l.date_weight, fg)));
        let status = tree.add(block, Node::new().layout(Layout::Row { gap: l.status_gap }).align(ax, Align::Start).offset(0.0, l.status_gap));
        let battery = tree.add(status, Node::new().paint(Paint::Icon { name: String::new(), size: l.status_icon }));
        let wifi = tree.add(status, Node::new().paint(Paint::Icon { name: String::new(), size: l.status_icon }));
        tree.set_visible(battery, false);
        tree.set_visible(wifi, false);
        tree.set_visible(status, l.status_icon > 0);

        (tree, Nodes { signin, name, row, pad, switch, picker, tiles, col, field_outer, field_text, submit, message, keys, curtain, time, date, battery, wifi })
    }

    /// Drawn now (locked).
    pub fn active(&self) -> bool {
        self.locked
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Lock (from the core). Always succeeds.
    pub fn lock(&mut self, reason: LockReason) -> bool {
        self.now = self.now.max(self.clock.now().into());
        if !self.locked {
            info!(?reason, "lock screen: locked");
            self.locked = true;
            if let Some(s) = self.services.as_ref().filter(|_| self.greeter.is_none()) {
                s.send(Command::SetLockedHint(true));
            }
            self.update_clock();
            self.refresh_status(true);
        }
        // Always starts (or drops back) to the curtain, at once for sleep.
        self.drop_curtain(reason == LockReason::Sleep || reason == LockReason::Idle);
        true
    }

    fn unlock(&mut self, why: &str) {
        if !self.locked {
            return;
        }
        if self.greeter.is_some() {
            tracing::warn!(why, "greeter: unlock ignored");
            return;
        }
        info!(why, "lock screen: unlocked");
        self.locked = false;
        self.pw.clear();
        self.message = None;
        self.focus = false;
        self.touch = None;
        self.pressed = None;
        self.lift.set(0.0, 0.0);
        self.requests.push(Request::Unlock);
        if let Some(s) = self.services.as_ref() {
            s.send(Command::SetLockedHint(false));
        }
        self.apply();
    }

    /// The screen went blank (idle): back to the curtain at once, typed text wiped.
    pub fn blanked(&mut self) {
        if self.locked {
            self.drop_curtain(true);
        }
    }

    /// Back to the curtain, typed text wiped.
    fn drop_curtain(&mut self, at_once: bool) {
        self.pw.clear();
        if !self.auth.busy() && !self.greeter.as_ref().is_some_and(|g| g.users.is_empty() || g.done) {
            self.message = None;
        }
        self.focus = false;
        self.pressed = None;
        if at_once {
            self.lift.set(0.0, 0.0);
        } else {
            self.lift.set_target(0.0);
        }
        self.apply();
    }

    fn lift_curtain(&mut self) {
        self.lift.set_target(1.0);
        // The box takes the keyboard at once (the built-in keyboard comes up for it on a tablet).
        self.focus = !self.pin && !self.picking();
        self.apply();
    }

    fn curtain_up(&self) -> bool {
        self.lift.target >= 1.0
    }

    /// Greeter: the user tiles are up.
    fn picking(&self) -> bool {
        self.greeter.as_ref().is_some_and(|g| g.picking)
    }

    /// Greeter: signed in, waiting to quit (input ignored).
    fn done(&self) -> bool {
        self.greeter.as_ref().is_some_and(|g| g.done)
    }

    /// Greeter: sign in as user `i` next (the box comes up for it).
    fn pick(&mut self, i: usize) {
        let Some(g) = self.greeter.as_mut() else { return };
        let Some(u) = g.users.get(i).cloned() else { return };
        if self.auth.busy() {
            return;
        }
        info!(user = %u.name, "greeter: user picked");
        g.chosen = i;
        g.picking = false;
        self.tree.set_text(self.n.name, &u.display);
        self.auth.set_user(Some(u));
        self.pw.clear();
        self.message = None;
        self.focus = !self.pin;
        self.apply();
    }

    /// Greeter: back to the user tiles ("Switch user", Escape). False if there is only one person.
    fn to_picker(&mut self) -> bool {
        let busy = self.auth.busy();
        let Some(g) = self.greeter.as_mut().filter(|g| g.users.len() > 1 && !g.done && !busy) else { return false };
        g.picking = true;
        self.pw.clear();
        self.message = None;
        self.focus = false;
        self.apply();
        true
    }

    /// The built-in keyboard covers this much of the bottom now (0 = none). True if that changed.
    pub fn set_bottom_inset(&mut self, px: f32) -> bool {
        if (self.inset.target - px).abs() < 0.5 {
            return false;
        }
        self.inset.set_target(px);
        true
    }

    pub fn set_hw_keyboard(&mut self, attached: bool) {
        self.hw_keyboard = attached;
    }

    /// A text field wants the on-screen keyboard (the password box, once the curtain is up).
    pub fn wants_text(&self) -> bool {
        self.locked && !self.pin && self.focus && self.curtain_up() && !self.picking() && !self.done()
    }

    /// The core presented a frame with the lock on it: a waiting sleep may go ahead.
    pub fn presented(&mut self) {
        if std::mem::take(&mut self.sleep_pending) {
            info!("lock screen: on the panel, sleep may go ahead");
            if let Some(s) = self.services.as_ref() {
                s.send(Command::SleepReady(None));
            }
        }
    }

    /// Where control `name` is (for scripted tests): a pad key ("0"-"9", "back", "ok"), "field", "submit",
    /// and in greeter mode "user:<login>" (a tile) and "switch" (back to the tiles).
    pub fn point(&self, name: &str) -> Option<(f32, f32)> {
        if !self.locked || !self.curtain_up() {
            return None;
        }
        let tile = |user: &str| {
            let g = self.greeter.as_ref().filter(|g| g.picking)?;
            g.users.iter().position(|u| u.name == user).map(|i| self.n.tiles[i])
        };
        let node = match name {
            "field" => Some(self.n.field_outer),
            "submit" => Some(self.n.submit),
            "switch" => Some(self.n.switch),
            _ if name.starts_with("user:") => tile(&name[5..]),
            _ => self.n.keys.iter().find(|(_, k)| k.name() == name).map(|(n, _)| *n),
        }?;
        if !self.tree[node].visible {
            return None;
        }
        let r = self.tree.screen_rect(node);
        Some((r.x + r.w * 0.5, r.y + r.h * 0.5))
    }

    fn update_clock(&mut self) {
        let (time, date, _, secs) = local_time();
        self.tree.set_text(self.n.time, &time);
        self.tree.set_text(self.n.date, &date);
        self.next_minute = Some(self.now + Duration::from_secs(secs));
    }

    /// Battery and Wi-Fi icons from the services snapshot (when it changed, or `force`).
    fn refresh_status(&mut self, force: bool) -> bool {
        let Some(s) = self.services.as_ref() else { return false };
        let (g, sys) = s.snapshot();
        if g == self.sys_gen && !force {
            return false;
        }
        self.sys_gen = g;
        let bat = sys.battery.as_ref().map(|b| battery_icon(b.percent, b.charge));
        let wifi = sys.wifi.as_ref().filter(|w| w.enabled && w.link == Link::Connected).map(|w| super::control::signal_icon(w.strength).to_string());
        for (node, icon) in [(self.n.battery, bat), (self.n.wifi, wifi)] {
            self.tree.set_visible(node, icon.is_some());
            if let (Paint::Icon { name, .. }, Some(icon)) = (&mut self.tree[node].paint, icon)
                && *name != icon
            {
                *name = icon;
                self.tree.mark_layout();
            }
        }
        true
    }

    fn submit(&mut self) {
        if self.pw.is_empty() || self.auth.busy() || self.picking() || self.done() {
            return;
        }
        info!("lock screen: checking");
        let secret = self.pw.take();
        self.auth.start(secret);
        self.message = Some((self.theme.lock.label_checking.clone(), false));
        self.apply();
    }

    fn type_text(&mut self, t: &str) {
        if self.auth.busy() || self.picking() || self.done() {
            return;
        }
        if self.pin && !t.chars().all(|c| c.is_ascii_digit()) {
            return;
        }
        if self.pw.push_str(t) && matches!(self.message, Some((_, true))) {
            // A new try: the old error goes away.
            self.message = None;
        }
        self.apply();
    }

    fn backspace(&mut self) {
        if !self.auth.busy() {
            self.pw.pop();
            self.apply();
        }
    }

    fn pad(&mut self, key: PadKey) {
        match key {
            PadKey::Digit(d) => self.type_text(&d.to_string()),
            PadKey::Back => self.backspace(),
            PadKey::Ok => self.submit(),
        }
    }

    /// Write the state into the tree.
    fn apply(&mut self) {
        let l = &self.theme.lock;
        let lift = self.lift.value.clamp(0.0, 1.0);
        self.tree.set_visible(self.n.curtain, lift < 0.999);
        self.tree[self.n.curtain].translate = (0.0, -lift * self.h);
        self.tree.set_visible(self.n.signin, lift > 0.001);
        self.tree[self.n.col].translate = (0.0, -self.inset.value * 0.5);
        let picking = self.picking();
        let several = self.greeter.as_ref().is_some_and(|g| g.users.len() > 1);
        let no_users = self.greeter.as_ref().is_some_and(|g| g.users.is_empty());
        self.tree.set_visible(self.n.picker, picking);
        self.tree.set_visible(self.n.name, !picking && !no_users);
        self.tree.set_visible(self.n.row, !picking && !self.pin && !no_users);
        self.tree.set_visible(self.n.pad, !picking && self.pin && !no_users);
        self.tree.set_visible(self.n.switch, !picking && several && !self.done());
        if let Some(g) = self.greeter.as_ref() {
            let sel = or_accent(&l.user_tile_selected, &self.theme);
            for (i, &tile) in self.n.tiles.iter().enumerate() {
                let square = self.tree[tile].children[0];
                set_color(&mut self.tree, square, if i == g.chosen { sel } else { l.user_tile_fill.premultiplied() });
            }
        }
        // The box: bullets, or the placeholder word.
        let n = self.pw.chars();
        let (label, color, size) = if n == 0 {
            let word = if self.pin { &l.label_pin } else { &l.label_password };
            (word.clone(), l.field_placeholder.premultiplied(), l.message_size)
        } else {
            (std::iter::repeat_n(BULLET, n).collect::<String>(), l.field_text.premultiplied(), l.bullet_size)
        };
        let ft = self.n.field_text;
        self.tree.set_text(ft, &label);
        set_color(&mut self.tree, ft, color);
        if let Paint::Text { style, .. } = &mut self.tree[ft].paint
            && style.size != size
        {
            style.size = size;
            self.tree.mark_layout();
        }
        let border = if self.focus { or_accent(&l.field_focus, &self.theme) } else { l.field_fill.premultiplied() };
        set_color(&mut self.tree, self.n.field_outer, border);
        let (msg, err) = self.message.clone().unwrap_or_default();
        self.tree.set_text(self.n.message, &msg);
        set_color(&mut self.tree, self.n.message, if err { l.error_text.premultiplied() } else { l.message_text.premultiplied() });
        let pressed = or_accent(&l.pin_pressed, &self.theme);
        for (node, _) in self.n.keys.clone() {
            set_color(&mut self.tree, node, if self.pressed == Some(node) { pressed } else { l.pin_fill.premultiplied() });
        }
    }

    fn curtain_deadline(&self) -> Option<Duration> {
        (self.locked && self.curtain_up() && !self.auth.busy() && !self.done()).then(|| self.last_input + Duration::from_millis(self.theme.lock.curtain_timeout_ms as u64))
    }

    fn handle_session(&mut self, ev: SessionEvent) {
        if self.greeter.is_some() && matches!(ev, SessionEvent::Lock | SessionEvent::Unlock) {
            return; // the greeter's own session: nothing to lock or unlock
        }
        match ev {
            SessionEvent::Lock => {
                if !self.locked {
                    self.requests.push(Request::Lock(LockReason::User));
                }
            }
            SessionEvent::Unlock => self.unlock("logind"),
            SessionEvent::PrepareForSleep(true) => {
                self.sleep_pending = true;
                if self.locked {
                    self.drop_curtain(true);
                } else {
                    self.requests.push(Request::Lock(LockReason::Sleep));
                }
            }
            SessionEvent::PrepareForSleep(false) => {
                self.sleep_pending = false;
                self.last_input = self.clock.now().into();
                // Resumed: the screen comes back on (the core's idle state may still be blank).
                self.requests.push(Request::Wake);
            }
        }
    }
}

impl Scene for LockScreen {
    fn tree(&mut self) -> &mut Tree {
        &mut self.tree
    }

    fn update(&mut self, now: Duration, dt: f32) -> bool {
        self.now = now;
        let moving = self.lift.step(dt) | self.inset.step(dt);
        if self.next_minute.is_some_and(|t| now >= t) {
            self.update_clock();
        }
        if self.curtain_deadline().is_some_and(|t| now >= t) && self.touch.is_none() {
            info!("lock screen: curtain back (no input)");
            self.drop_curtain(false);
        }
        self.apply();
        moving || !self.lift.is_resting() || !self.inset.is_resting()
    }

    fn input(&mut self, ev: PointerEvent) -> bool {
        self.now = self.now.max(ev.time);
        self.last_input = self.now;
        match ev.phase {
            Phase::Down => {
                if self.touch.is_some() {
                    return true; // one finger at a time
                }
                let grab = if self.curtain_up() && self.lift.is_resting() {
                    let on = self.tree.hit_test(ev.x, ev.y).filter(|&n| n != self.n.signin);
                    if let Some(on) = on.filter(|n| self.n.keys.iter().any(|(k, _)| k == n)) {
                        self.pressed = Some(on);
                    }
                    Grab::Signin { x0: ev.x, y0: ev.y, on }
                } else {
                    Grab::Curtain { y0: ev.y, lift0: self.lift.value }
                };
                let mut vt = VelocityTracker::default();
                vt.add(ev.time, ev.x, ev.y);
                self.touch = Some(Touch { id: ev.id, grab, moved: false, vt });
            }
            Phase::Motion => {
                let h = self.h;
                let Some(t) = self.touch.as_mut().filter(|t| t.id == ev.id) else { return true };
                t.vt.add(ev.time, ev.x, ev.y);
                match t.grab {
                    Grab::Curtain { y0, lift0 } => {
                        if (y0 - ev.y).abs() > SLOP {
                            t.moved = true;
                        }
                        if t.moved {
                            let v = -t.vt.velocity().1 / h;
                            self.lift.set((lift0 + (y0 - ev.y) / h).clamp(0.0, 1.0), v);
                        }
                    }
                    Grab::Signin { x0, y0, .. } => {
                        if (ev.x - x0).hypot(ev.y - y0) > SLOP * 2.0 {
                            t.moved = true;
                            self.pressed = None;
                        }
                    }
                }
            }
            Phase::Up => {
                let Some(t) = self.touch.take_if(|t| t.id == ev.id) else { return true };
                match t.grab {
                    Grab::Curtain { .. } => {
                        let vy = t.vt.velocity().1;
                        let l = &self.theme.lock;
                        if !t.moved || vy < -l.lift_flick || (vy <= l.lift_flick && self.lift.value > l.lift_commit) {
                            self.lift_curtain();
                        } else {
                            self.drop_curtain(false);
                        }
                    }
                    Grab::Signin { on, .. } => {
                        let pressed = self.pressed.take();
                        if !t.moved
                            && let Some(on) = on
                        {
                            if let Some((_, key)) = self.n.keys.iter().find(|(n, _)| *n == on && pressed == Some(on)).copied() {
                                self.pad(key);
                            } else if on == self.n.submit {
                                self.submit();
                            } else if on == self.n.switch {
                                self.to_picker();
                            } else if let Some(i) = self.n.tiles.iter().position(|&n| n == on) {
                                self.pick(i);
                            } else if on == self.n.field_outer && !self.pin {
                                self.focus = true;
                            }
                        }
                    }
                }
            }
            Phase::Cancel => {
                if ev.id == CANCEL_ALL || self.touch.as_ref().is_some_and(|t| t.id == ev.id) {
                    self.touch = None;
                    self.pressed = None;
                    if !self.curtain_up() || !self.lift.is_resting() {
                        let up = self.lift.value > self.theme.lock.lift_commit;
                        self.lift.set_target(if up { 1.0 } else { 0.0 });
                    }
                }
            }
        }
        self.apply();
        true
    }

    fn next_wakeup(&self) -> Option<Duration> {
        if !self.locked {
            return None;
        }
        [self.next_minute, self.curtain_deadline()].into_iter().flatten().min()
    }

    fn set_wallpaper(&mut self, tex: Option<Texture>) {
        self.wallpaper = tex;
        self.tree[self.n.curtain].paint = match tex {
            Some(tex) => Paint::Backdrop { tex },
            None => Paint::Rect { color: self.theme.colors.background.premultiplied(), radius: 0.0 },
        };
    }

    fn opaque(&self) -> bool {
        // The curtain (an opaque image) or a solid sign-in fill covers the whole output.
        self.locked && (self.lift.value < 0.001 || (!self.theme.lock.signin_glass && self.theme.lock.signin_fill.a >= 1.0 && self.lift.value > 0.999))
    }

    fn set_size(&mut self, w: f32, h: f32) {
        self.w = w;
        self.h = h;
        self.apply();
    }

    fn poll(&mut self) -> bool {
        let mut redraw = false;
        if let Some(events) = self.services.as_ref().map(|s| s.take_session_events()) {
            for ev in events {
                self.handle_session(ev);
                redraw = true;
            }
        }
        if let Some(out) = self.auth.poll() {
            redraw = true;
            if out.ok && self.greeter.is_some() {
                info!("greeter: signed in, quitting so greetd starts the session");
                if let Some(g) = self.greeter.as_mut() {
                    g.done = true;
                }
                self.message = Some((self.theme.lock.label_signing_in.clone(), false));
                self.focus = false;
                self.requests.push(Request::Quit);
                self.apply();
            } else if out.ok {
                self.unlock("password");
            } else {
                info!("lock screen: not accepted");
                // PAM's own words (e.g. how long the account stays locked) are shown, not logged.
                self.message = Some((out.messages.join("\n"), true));
                self.pw.clear();
                self.last_input = self.clock.now().into();
                self.apply();
            }
        }
        if self.locked && self.refresh_status(false) {
            redraw = true;
        }
        if redraw && self.sleep_pending && self.locked {
            // Make sure a frame comes (it reports the lock as presented).
            return true;
        }
        redraw && self.locked
    }

    fn visible(&self) -> bool {
        self.locked
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        self.now = self.now.max(self.clock.now().into());
        self.last_input = self.now;
        if !self.curtain_up() {
            self.lift_curtain();
            // The key that lifted it types too (Windows), unless it is a control key.
            if key.sym == k::KEY_Escape || key.sym == k::KEY_Return || key.sym == k::KEY_KP_Enter {
                return true;
            }
        }
        if self.done() {
            return true;
        }
        if self.picking() {
            let g = self.greeter.as_mut().expect("picking implies greeter");
            let n = g.users.len();
            match key.sym {
                k::KEY_Left | k::KEY_Up if n > 0 => g.chosen = (g.chosen + n - 1) % n,
                k::KEY_Right | k::KEY_Down | k::KEY_Tab if n > 0 => g.chosen = (g.chosen + 1) % n,
                k::KEY_Return | k::KEY_KP_Enter => {
                    let i = g.chosen;
                    self.pick(i);
                }
                k::KEY_Escape => self.drop_curtain(false),
                _ => {}
            }
            self.apply();
            return true;
        }
        match key.sym {
            k::KEY_Escape => {
                if !self.to_picker() {
                    self.drop_curtain(false);
                }
            }
            k::KEY_Return | k::KEY_KP_Enter => self.submit(),
            k::KEY_BackSpace => self.backspace(),
            _ => {
                if let Some(t) = key.text.as_deref().filter(|_| !key.ctrl && !key.alt) {
                    self.focus = !self.pin;
                    self.type_text(t);
                }
            }
        }
        true
    }

    fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        self.lift.params = self.springs;
        self.inset.params = self.springs;
        let display = self.auth.user().map(|u| u.display.clone()).unwrap_or_default();
        let users: &[User] = self.greeter.as_ref().map_or(&[], |g| &g.users);
        let (tree, n) = Self::build(theme, self.pin, &display, self.wallpaper, users);
        self.tree = tree;
        self.n = n;
        self.update_clock();
        self.refresh_status(true);
        self.apply();
    }

    fn text_field(&self) -> bool {
        self.wants_text()
    }
}
