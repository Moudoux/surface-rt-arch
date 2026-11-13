//! Control Center (DESKTOP-SHELL.md 3.2, Phase 5): a glass panel that slides in from the right edge under the
//! finger (or with Super+A), drawn above everything, apps included.
//!
//! Contents, top to bottom: clock and date with the battery; quick toggles (Wi-Fi, Bluetooth, rotation lock);
//! the brightness and volume sliders (tap the speaker = mute); a "Networks" row; paired Bluetooth devices (tap = connect / disconnect); Lock /
//! Sleep / Restart / Power off. The "Networks" row opens a second page (like the Windows 8 Networks pane):
//! networks in range, tap one to expand it with Connect / Disconnect / Forget and, for a new WPA2 network, a
//! password box typed on the keyboard (shown as bullets only; the text goes to NetworkManager once and is
//! wiped here). Values come from the
//! services thread (`services::Services` snapshots); taps send `services::Command`s and show the new state at
//! once ("pending") until the service confirms it or `PENDING` passes.
//!
//! Touch while open: tap outside (or Escape) closes, drag the panel to the right to close it (follows the
//! finger, release by velocity / distance like the edge swipe), drag the slider. Restart and Power off
//! need a second tap within `confirm_ms`.
//!
//! The volume popup (volume keys, `volume_key`) lives in this layer too, like the notification banners: the
//! level as a number, a bar and the speaker icon, at `[volume]`'s anchor; it fades out `timeout_ms` after
//! the last change or touch. Drag its bar to set the level, tap the icon to mute.
//!
//! The panel is a separate scene with its own UI layer (`ShellUi` draws it as a second element on top), so
//! moving it never repaints Home's layer. All sizes and colours come from the theme (`[control]`, colours,
//! fonts, shape, shadow).

use std::time::Duration;

use smithay::{
    input::keyboard::keysyms as k,
    utils::{Clock, Monotonic},
};
use tracing::info;

use super::{Align, Layout, Node, NodeId, Paint, Shadow, Size, Tree, home::local_time, shell::Scene};
use crate::{
    anim::{Spring, SpringParams, VelocityTracker},
    overlay::{CANCEL_ALL, EdgePhase, KeyEvent, Phase, PointerEvent, Request, VolumeKey},
    render::{batch::Rgba, text::TextStyle},
    rotate::Orientation,
    services::{Charge, Command, JoinState, Link, MIN_BRIGHTNESS, Network, Note, NoteEvent, Secret, Security, Services, Sys, Volume},
    theme::Theme,
};

/// Finger travel before a touch counts as a drag (px).
const SLOP: f32 = 10.0;
/// The id of the shell's own screenshot banner (apps' ids count up from 1; services use u32::MAX, MAX - 1).
const SCREENSHOT_ID: u32 = u32::MAX - 2;
/// How long a toggle shows the state it was switched to while the service has not confirmed it.
const PENDING: Duration = Duration::from_secs(4);
/// The same for a Bluetooth device connecting or disconnecting (BlueZ gives up on an LE device that is off
/// after ~40 s).
const BT_PENDING: Duration = Duration::from_secs(45);
/// Ask for a new Wi-Fi scan this often while the Networks page is open.
const SCAN_EVERY: Duration = Duration::from_secs(30);
/// WPA2 passphrases are 8-63 characters.
const PSK_MIN: usize = 8;
const PSK_MAX_BYTES: usize = 63;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    Networks,
    Notifications,
}

/// A notification kept for the Notifications page.
struct Past {
    note: Note,
    /// Local time it arrived ("15:21").
    at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Wifi,
    Bluetooth,
    Rotation,
}
const TOGGLES: [Toggle; 3] = [Toggle::Wifi, Toggle::Bluetooth, Toggle::Rotation];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Power {
    Lock,
    Sleep,
    Restart,
    Off,
}
const POWER: [Power; 4] = [Power::Lock, Power::Sleep, Power::Restart, Power::Off];

impl Power {
    fn label(self) -> &'static str {
        match self {
            Power::Lock => "Lock",
            Power::Sleep => "Sleep",
            Power::Restart => "Restart",
            Power::Off => "Power off",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Power::Lock => "system-lock-screen",
            Power::Sleep => "system-suspend",
            Power::Restart => "system-reboot",
            Power::Off => "system-shutdown",
        }
    }
    fn needs_confirm(self) -> bool {
        matches!(self, Power::Restart | Power::Off)
    }
}

/// The sliders: the panel's brightness and volume rows, the volume popup's bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slider {
    Bright,
    Volume,
    Popup,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Target {
    Outside,
    Panel,
    Toggle(usize),
    Slider(Slider),
    /// The speaker icon of the panel's volume row or of the popup (tap = mute / unmute).
    Mute { popup: bool },
    Power(usize),
    /// The "Networks" row on the main page.
    NetsLink,
    Back,
    /// A paired Bluetooth device row.
    Device(usize),
    /// A network row (expand / collapse), its Connect-or-Disconnect and Forget buttons, the password box.
    Net(usize),
    NetConnect(usize),
    NetForget(usize),
    Field,
    /// A notification banner (index into `banners`).
    Banner(usize),
    /// The "Notifications" row, a notification on its page (tap = remove it), "Clear all".
    NotesLink,
    /// "All settings": opens the Settings app (ShellUi picks it up with `take_open_settings`).
    SettingsLink,
    Past(usize),
    ClearAll,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Down, not moved past the slop yet.
    Pending,
    /// Dragging the panel (closing it by hand).
    Slide { p0: f32 },
    Slider(Slider),
    /// Dragging a notification banner away.
    Banner,
    /// Moved, but not a drag we use.
    Done,
}

struct Touch {
    id: i32,
    start: (f32, f32),
    target: Target,
    mode: Mode,
    vel: VelocityTracker,
}

struct ToggleNodes {
    node: NodeId,
    icon: NodeId,
    label: NodeId,
    status: NodeId,
}

struct PowerNodes {
    node: NodeId,
    circle: NodeId,
    label: NodeId,
}

/// A slider row of the panel: icon (a touch target), the slider with its track, level fill and knob.
struct SliderNodes {
    row: NodeId,
    icon_box: NodeId,
    icon: NodeId,
    slider: NodeId,
    fill: NodeId,
    knob: NodeId,
}

/// The volume popup: the level bar is laid out by weights (`spacer` Fill(1 - level), the knob, `level`
/// Fill(level)), so it needs no measured sizes.
struct PopNodes {
    node: NodeId,
    number: NodeId,
    bar: NodeId,
    spacer: NodeId,
    level: NodeId,
    icon_box: NodeId,
    icon: NodeId,
}

/// A list row: icon, name with a status line under it.
struct RowNodes {
    node: NodeId,
    icon: NodeId,
    name: NodeId,
    status: NodeId,
}

/// One network on the Networks page (rebuilt when the list changes).
struct NetRow {
    head: NodeId,
    connect: Option<NodeId>,
    forget: Option<NodeId>,
    field: Option<NodeId>,
}

/// A notification banner on screen (or sliding in / out).
struct Banner {
    note: Note,
    node: Option<NodeId>,
    /// 0 = out of view, 1 = shown.
    p: Spring,
    /// How long it stays once shown (None = until dismissed), and when that ends (set when first drawn).
    stay: Option<Duration>,
    until: Option<Duration>,
    closing: bool,
    /// Finger offset while dragged.
    drag: (f32, f32),
}

struct Nodes {
    dim: NodeId,
    /// Column holding the notification banners.
    banner_col: NodeId,
    panel: NodeId,
    main: NodeId,
    nets_link: RowNodes,
    notes_link: RowNodes,
    settings_link: RowNodes,
    notes_page: NodeId,
    notes_back: NodeId,
    clear_all: NodeId,
    notes_list: NodeId,
    notes_hint: NodeId,
    time: NodeId,
    date: NodeId,
    battery: NodeId,
    battery_status: NodeId,
    toggles: Vec<ToggleNodes>,
    bright: SliderNodes,
    volume: SliderNodes,
    pop: PopNodes,
    /// Full-screen screenshot flash (topmost).
    flash: NodeId,
    devices: NodeId,
    device_rows: Vec<RowNodes>,
    power: Vec<PowerNodes>,
    nets_page: NodeId,
    back: NodeId,
    net_list: NodeId,
    nets_hint: NodeId,
}

pub struct ControlCenter {
    theme: Theme,
    tree: Tree,
    n: Nodes,
    w: f32,
    h: f32,
    services: Services,
    generation: u64,
    sys: Sys,
    /// 0 = closed, 1 = open.
    p: Spring,
    /// The right-edge swipe is holding the panel.
    edge_held: bool,
    touch: Option<Touch>,
    /// Shown while the service has not confirmed a toggle: (state, since).
    wifi_pending: Option<(bool, Duration)>,
    bt_pending: Option<(bool, Duration)>,
    /// Rotation lock: the screen stays in the orientation it has.
    rotation_locked: bool,
    /// A hardware keyboard (Type Cover) is attached: the core keeps the screen in normal orientation.
    hw_keyboard: bool,
    /// The accelerometer is claimed from iio-sensor-proxy (only while auto-rotate can act).
    accel_claimed: bool,
    /// The orientation last asked of the core.
    sent_orientation: Option<Orientation>,
    requests: Vec<Request>,
    /// The slider's value while dragged (and shortly after, until the service reports it).
    bright: Option<(f32, Duration)>,
    /// The volume shown while the audio thread has not reported it yet: (level, since); the same for mute.
    vol: Option<(f32, Duration)>,
    mute_pending: Option<(bool, Duration)>,
    /// The volume popup: wanted on screen, its opacity (fades), and when it goes (None = not timed yet).
    pop_shown: bool,
    pop_o: f32,
    pop_until: Option<Duration>,
    /// "All settings" was tapped (taken by ShellUi).
    open_settings: bool,
    /// The screenshot flash's strength (1 -> 0 over `[screenshot] flash_ms`).
    flash_o: f32,
    /// Volume keys arrive between frames: `now` is read from the clock then.
    clock: Clock<Monotonic>,
    /// Restart / Power off armed: (button, until).
    confirm: Option<(usize, Duration)>,
    page: Page,
    /// The expanded network (SSID).
    sel: Option<String>,
    /// The password being typed for `sel` (never logged or drawn; wiped when dropped from here).
    pw: String,
    net_rows: Vec<NetRow>,
    /// What the network rows were built from (the password only by length).
    nets_key: String,
    next_scan: Option<Duration>,
    /// A Bluetooth device connecting (true) or disconnecting: (path, connect, since).
    bt_dev_pending: Option<(String, bool, Duration)>,
    banners: Vec<Banner>,
    /// The session is locked: the panel stays shut and banners are hidden (their text is private); only
    /// the volume popup shows.
    locked: bool,
    /// Notifications for the Notifications page, newest first.
    history: Vec<Past>,
    /// Row nodes of the Notifications page, and whether they need rebuilding.
    past_rows: Vec<NodeId>,
    history_dirty: bool,
    /// The banner nodes need rebuilding (one came or went, or its text changed).
    banners_dirty: bool,
    now: Duration,
    next_minute: Option<Duration>,
    springs: SpringParams,
}

fn text(s: &str, size: f32, weight: u16, color: Rgba) -> Paint {
    Paint::Text { text: s.into(), style: TextStyle { size, weight, ..TextStyle::default() }, color, wrap: false, layout: None }
}

fn set_color(tree: &mut Tree, id: NodeId, c: Rgba) {
    match &mut tree[id].paint {
        Paint::Text { color, .. } | Paint::Rect { color, .. } => *color = c,
        _ => {}
    }
}

fn set_icon(tree: &mut Tree, id: NodeId, icon: &str) {
    if let Paint::Icon { name, .. } = &mut tree[id].paint
        && name != icon
    {
        *name = icon.to_string();
    }
}

/// "3 h 20 min" / "45 min".
fn duration_text(secs: i64) -> String {
    let m = (secs + 30) / 60;
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// Overwrite a password in place, then empty it.
fn wipe(s: &mut String) {
    // SAFETY: zero bytes keep the String valid UTF-8.
    unsafe { s.as_bytes_mut() }.fill(0);
    s.clear();
}

/// Signal-strength icon for a network.
pub(super) fn signal_icon(strength: u8) -> &'static str {
    match strength {
        0..=19 => "network-wireless-signal-none",
        20..=39 => "network-wireless-signal-weak",
        40..=59 => "network-wireless-signal-ok",
        60..=79 => "network-wireless-signal-good",
        _ => "network-wireless-signal-excellent",
    }
}

/// Icon for a BlueZ device `Icon` value (freedesktop names, e.g. "input-keyboard", "audio-headset").
fn bt_icon(icon: &str) -> String {
    if icon.is_empty() { "network-bluetooth".into() } else { icon.to_string() }
}

/// A network that needs a password typed before it can be joined.
fn needs_password(n: &Network) -> bool {
    n.security == Security::Psk && !n.saved && !n.active
}

/// A list row (icon, name, status under it; `chevron` adds a "go on" arrow at the right end).
fn list_row(tree: &mut Tree, parent: NodeId, theme: &Theme, chevron: bool) -> RowNodes {
    let t = &theme.control;
    let f = &theme.font;
    let unit = theme.spacing.unit as f32;
    let node = tree.add(
        parent,
        Node::new()
            .size(Size::Fill(1.0), Size::Fixed(t.row_height))
            .layout(Layout::Row { gap: 1.5 * unit })
            .paint(Paint::Rect { color: [0.0; 4], radius: t.row_radius })
            .clip()
            .hit(),
    );
    let icon = tree.add(node, Node::new().align(Align::Start, Align::Center).paint(Paint::Icon { name: String::new(), size: t.row_icon }));
    let col = tree.add(node, Node::new().layout(Layout::Column { gap: 0.0 }).align(Align::Start, Align::Center));
    let name = tree.add(col, Node::new().paint(text("", f.size_body, f.weight_body, theme.colors.text.premultiplied())));
    let status = tree.add(col, Node::new().paint(text("", f.size_caption, f.weight_body, theme.colors.text_dim.premultiplied())));
    if chevron {
        tree.add(node, Node::new().size(Size::Fill(1.0), Size::Fixed(1.0)));
        tree.add(node, Node::new().align(Align::Start, Align::Center).paint(Paint::Icon { name: "go-next".into(), size: t.row_icon }));
    }
    RowNodes { node, icon, name, status }
}

/// A button (label centred; `primary` = accent fill).
fn button(tree: &mut Tree, parent: NodeId, theme: &Theme, label: &str, primary: bool) -> NodeId {
    let t = &theme.control;
    let c = &theme.colors;
    let (fill, fg) = if primary { (c.accent, c.on_accent) } else { (t.toggle_off, c.text) };
    let b = tree.add(
        parent,
        Node::new()
            .size(Size::Fill(1.0), Size::Fixed(t.button_height))
            .paint(Paint::Rect { color: fill.premultiplied(), radius: t.button_radius })
            .hit(),
    );
    tree.add(b, Node::new().align(Align::Center, Align::Center).paint(text(label, theme.font.size_body, theme.font.weight_title, fg.premultiplied())));
    b
}

/// Shorten a label to `max` characters with an ellipsis.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// A panel slider row: icon box (a touch target), then the slider (track, level fill, knob). Geometry from
/// `[control]` tokens.
fn slider_row(tree: &mut Tree, parent: NodeId, theme: &Theme, icon: &str) -> SliderNodes {
    let t = &theme.control;
    let c = &theme.colors;
    let unit = theme.spacing.unit as f32;
    let row = tree.add(parent, Node::new().size(Size::Fill(1.0), Size::Fixed(t.slider_height)).layout(Layout::Row { gap: unit * 1.5 }));
    let icon_box = tree.add(row, Node::new().size(Size::Fixed(t.toggle_icon as f32), Size::Fill(1.0)).hit());
    let icon = tree.add(icon_box, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: icon.into(), size: t.toggle_icon }));
    let slider = tree.add(row, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).hit());
    let track_r = t.slider_radius;
    tree.add(
        slider,
        Node::new()
            .size(Size::Fill(1.0), Size::Fixed(t.slider_track))
            .align(Align::Start, Align::Center)
            .paint(Paint::Rect { color: t.toggle_off.premultiplied(), radius: track_r }),
    );
    let fill = tree.add(
        slider,
        Node::new()
            .size(Size::Fixed(t.slider_knob), Size::Fixed(t.slider_track))
            .align(Align::Start, Align::Center)
            .paint(Paint::Rect { color: c.accent.premultiplied(), radius: track_r }),
    );
    let mut knob = Node::new()
        .size(Size::Fixed(t.knob_width), Size::Fixed(t.slider_knob))
        .align(Align::Start, Align::Center)
        .paint(Paint::Rect { color: t.knob.premultiplied(), radius: t.knob_radius });
    if t.shadow {
        knob = knob.shadow(Shadow { sigma: theme.shadow.small_sigma, dx: 0.0, dy: theme.shadow.small_offset_y, color: theme.shadow.color.premultiplied() });
    }
    let knob = tree.add(slider, knob);
    SliderNodes { row, icon_box, icon, slider, fill, knob }
}

/// Place a panel slider's knob and fill at `f` (0-1); the slider's width follows from the tokens.
fn place_slider(tree: &mut Tree, n: &SliderNodes, f: f32, theme: &Theme) {
    let t = &theme.control;
    let unit = theme.spacing.unit as f32;
    let slider_w = t.width - 2.0 * t.padding - t.toggle_icon as f32 - 1.5 * unit;
    let x = f.clamp(0.0, 1.0) * (slider_w - t.knob_width).max(0.0);
    tree[n.knob].translate = (x, 0.0);
    let fill_w = x + t.knob_width * 0.5;
    if tree[n.fill].width != Size::Fixed(fill_w) {
        tree[n.fill].width = Size::Fixed(fill_w);
        tree.mark_layout();
    }
}

/// The volume popup's nodes (hidden), from `[volume]` tokens.
fn popup(tree: &mut Tree, root: NodeId, theme: &Theme) -> PopNodes {
    let v = &theme.volume;
    let vertical = v.orientation != "horizontal";
    let al = |s: &str, start: &str, end: &str| {
        if s == start {
            Align::Start
        } else if s == end {
            Align::End
        } else {
            Align::Center
        }
    };
    let boxed = tree.add(root, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).padding(v.margin));
    let fill = if v.glass { Paint::Glass { radius: v.radius, tint: v.fill.premultiplied() } } else { Paint::Rect { color: v.fill.premultiplied(), radius: v.radius } };
    let mut node = Node::new()
        .size(Size::Fixed(v.width), Size::Fixed(v.height))
        .align(al(&v.anchor, "left", "right"), al(&v.anchor_y, "top", "bottom"))
        .layout(if vertical { Layout::Column { gap: v.gap } } else { Layout::Row { gap: v.gap } })
        .padding(v.padding)
        .paint(fill)
        .hit();
    if v.shadow {
        node = node.shadow(Shadow { sigma: theme.shadow.sigma, dx: 0.0, dy: theme.shadow.offset_y, color: theme.shadow.color.premultiplied() });
    }
    let node = tree.add(boxed, node);
    tree[node].visible = false;
    let number_node = |tree: &mut Tree| {
        // A fixed box: the text changing width ("5" -> "100") must not move the bar.
        let b = tree.add(
            node,
            Node::new()
                .size(if vertical { Size::Fill(1.0) } else { Size::Fixed(v.number_size * 1.9) }, if vertical { Size::Fixed(v.number_size * 1.3) } else { Size::Fill(1.0) }),
        );
        tree.add(b, Node::new().align(Align::Center, Align::Center).paint(text("", v.number_size, v.number_weight, v.text.premultiplied())))
    };
    let icon_node = |tree: &mut Tree| {
        let side = (v.icon as f32 + v.padding).max(v.icon as f32);
        let b = tree.add(
            node,
            Node::new().size(if vertical { Size::Fill(1.0) } else { Size::Fixed(side) }, if vertical { Size::Fixed(side) } else { Size::Fill(1.0) }).hit(),
        );
        let i = tree.add(b, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: "audio-volume-medium".into(), size: v.icon }));
        (b, i)
    };
    let mut number = 0;
    let mut icon = (0, 0);
    if vertical {
        number = number_node(tree);
    } else {
        icon = icon_node(tree);
    }
    // The bar: track, and on top of it the knob and the level, placed by weights.
    let bar = tree.add(node, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).hit());
    let sz = |len: Size, th: f32| if vertical { (Size::Fixed(th), len) } else { (len, Size::Fixed(th)) };
    let (w, h) = sz(Size::Fill(1.0), v.track);
    tree.add(bar, Node::new().size(w, h).align(Align::Center, Align::Center).paint(Paint::Rect { color: v.track_fill.premultiplied(), radius: v.track_radius }));
    let (w, h) = sz(Size::Fill(1.0), v.knob_thickness.max(v.track));
    let lane = tree.add(
        bar,
        Node::new().size(w, h).align(Align::Center, Align::Center).layout(if vertical { Layout::Column { gap: 0.0 } } else { Layout::Row { gap: 0.0 } }),
    );
    let (w, h) = sz(Size::Fill(1.0), v.track);
    let spacer = tree.add(lane, Node::new().size(w, h).align(Align::Center, Align::Center));
    let mut knob_n = None;
    let mut level_n = None;
    let mut add_level = |tree: &mut Tree| {
        let (w, h) = sz(Size::Fill(0.0), v.track);
        level_n = Some(tree.add(lane, Node::new().size(w, h).align(Align::Center, Align::Center).paint(Paint::Rect { color: theme.colors.accent.premultiplied(), radius: v.track_radius })));
    };
    let mut add_knob = |tree: &mut Tree| {
        if v.knob_length > 0.0 {
            let (w, h) = if vertical { (Size::Fixed(v.knob_thickness), Size::Fixed(v.knob_length)) } else { (Size::Fixed(v.knob_length), Size::Fixed(v.knob_thickness)) };
            knob_n = Some(tree.add(lane, Node::new().size(w, h).align(Align::Center, Align::Center).paint(Paint::Rect { color: v.knob.premultiplied(), radius: v.knob_radius })));
        }
    };
    // Vertical: empty space on top, then knob, then the level down to the bottom; horizontal: the level
    // from the left, the knob, then the empty space. The spacer node was added first, so in the horizontal
    // case it is moved to the end below.
    if vertical {
        add_knob(tree);
        add_level(tree);
    } else {
        add_level(tree);
        add_knob(tree);
        let ch = &mut tree[lane].children;
        ch.retain(|&c| c != spacer);
        ch.push(spacer);
    }
    if vertical {
        icon = icon_node(tree);
    } else {
        number = number_node(tree);
    }
    PopNodes { node, number, bar, spacer, level: level_n.unwrap(), icon_box: icon.0, icon: icon.1 }
}

/// The speaker icon for a volume.
fn volume_icon(v: Volume) -> &'static str {
    match v.level {
        _ if v.muted => "audio-volume-muted",
        l if l <= 0.0 => "audio-volume-muted",
        l if l < 0.34 => "audio-volume-low",
        l if l < 0.67 => "audio-volume-medium",
        _ => "audio-volume-high",
    }
}

/// One volume key step up or down from `cur`, landing on multiples of `step` (0.10 with step 0.04 goes to
/// 0.12 or 0.08), within 0-1.
fn step_level(cur: f32, step: f32, up: bool) -> f32 {
    let step = step.max(0.005);
    let q = cur / step;
    let n = if up { (q + 1e-3).floor() + 1.0 } else { (q - 1e-3).ceil() - 1.0 };
    (n * step).clamp(0.0, 1.0)
}

/// Popup and banner touches are not the panel's.
fn floating(target: Target) -> bool {
    matches!(target, Target::Banner(_) | Target::Slider(Slider::Popup) | Target::Mute { popup: true })
}

impl ControlCenter {
    pub fn new(theme: &Theme, services: Services) -> ControlCenter {
        let springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        let (tree, n) = Self::build(theme);
        let mut cc = ControlCenter {
            theme: theme.clone(),
            tree,
            n,
            w: 1366.0,
            h: 768.0,
            services,
            generation: u64::MAX,
            sys: Sys::default(),
            p: Spring::new(0.0, springs).with_eps(0.001),
            edge_held: false,
            touch: None,
            wifi_pending: None,
            bt_pending: None,
            rotation_locked: false,
            hw_keyboard: false,
            accel_claimed: false,
            sent_orientation: None,
            requests: Vec::new(),
            bright: None,
            vol: None,
            mute_pending: None,
            pop_shown: false,
            pop_o: 0.0,
            pop_until: None,
            flash_o: 0.0,
            open_settings: false,
            clock: Clock::new(),
            confirm: None,
            page: Page::Main,
            sel: None,
            pw: String::new(),
            net_rows: Vec::new(),
            nets_key: String::new(),
            next_scan: None,
            bt_dev_pending: None,
            banners: Vec::new(),
            locked: false,
            history: Vec::new(),
            past_rows: Vec::new(),
            history_dirty: true,
            banners_dirty: false,
            now: Duration::ZERO,
            next_minute: None,
            springs,
        };
        cc.poll();
        cc.apply();
        cc
    }

    fn build(theme: &Theme) -> (Tree, Nodes) {
        let t = &theme.control;
        let c = &theme.colors;
        let f = &theme.font;
        let unit = theme.spacing.unit as f32;
        let fg = c.text.premultiplied();
        let dim_text = c.text_dim.premultiplied();
        let full = || Node::new().size(Size::Fill(1.0), Size::Fill(1.0));
        let mut tree = Tree::new(full());
        let root = tree.root;

        let dim = tree.add(root, full().paint(Paint::Rect { color: t.dim.premultiplied(), radius: 0.0 }));
        // A fully transparent dim is not drawn at all (it would still cost a full-screen blend).
        tree[dim].visible = t.dim.a > 0.0;
        let frame = tree.add(root, full().padding(t.margin));
        let fill = if t.glass {
            Paint::Glass { radius: t.radius, tint: t.fill.premultiplied() }
        } else {
            Paint::Rect { color: t.fill.premultiplied(), radius: t.radius }
        };
        let mut panel = Node::new()
            .size(Size::Fixed(t.width), Size::Fill(1.0))
            .align(Align::End, Align::Start)
            .layout(Layout::Column { gap: t.gap })
            .padding(t.padding)
            .paint(fill)
            .hit();
        if t.shadow {
            panel = panel.shadow(Shadow { sigma: theme.shadow.sigma, dx: 0.0, dy: theme.shadow.offset_y, color: theme.shadow.color.premultiplied() });
        }
        let panel = tree.add(frame, panel);
        let main = tree.add(panel, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).layout(Layout::Column { gap: t.gap }));

        // Title ("Settings"; empty = none).
        if !t.title.is_empty() {
            tree.add(main, Node::new().paint(text(&t.title, t.title_size, t.title_weight, fg)));
        }
        // Clock and date, battery on the right.
        let header = tree.add(main, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Row { gap: unit }));
        let clock = tree.add(header, Node::new().layout(Layout::Column { gap: 0.0 }));
        let time = tree.add(clock, Node::new().paint(text("", t.time_size, t.time_weight, fg)));
        let date = tree.add(clock, Node::new().paint(text("", f.size_caption, f.weight_body, dim_text)));
        tree.add(header, Node::new().size(Size::Fill(1.0), Size::Fixed(1.0)));
        let batt = tree.add(header, Node::new().layout(Layout::Column { gap: 0.0 }).align(Align::Start, Align::End));
        let battery = tree.add(batt, Node::new().align(Align::End, Align::Start).paint(text("", f.size_title, f.weight_title, fg)));
        let battery_status =
            tree.add(batt, Node::new().align(Align::End, Align::Start).paint(text("", f.size_caption, f.weight_body, dim_text)));

        // Quick toggles: "column" = icon on top, label and status at the bottom (square tiles); "row" = icon
        // left of the text (wide pills / cards).
        let vertical = t.toggle_layout == "column";
        let grid = tree.add(
            main,
            Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Grid { columns: t.toggle_columns.max(1), gap: t.toggle_gap }),
        );
        let toggles = TOGGLES
            .iter()
            .map(|_| {
                let node = tree.add(
                    grid,
                    Node::new()
                        .size(Size::Fill(1.0), Size::Fixed(t.toggle_height))
                        .layout(if vertical { Layout::Column { gap: 0.0 } } else { Layout::Row { gap: 1.5 * unit } })
                        .padding(t.toggle_padding)
                        .paint(Paint::Rect { color: t.toggle_off.premultiplied(), radius: t.toggle_radius })
                        .clip()
                        .hit(),
                );
                let icon = tree.add(
                    node,
                    Node::new().align(Align::Start, Align::Center).paint(Paint::Icon { name: String::new(), size: t.toggle_icon }),
                );
                if vertical {
                    tree.add(node, Node::new().size(Size::Fixed(1.0), Size::Fill(1.0)));
                }
                let col = tree.add(node, Node::new().layout(Layout::Column { gap: 0.0 }).align(Align::Start, Align::Center));
                let label = tree.add(col, Node::new().paint(text("", t.toggle_label_size, f.weight_title, fg)));
                let status = tree.add(col, Node::new().paint(text("", f.size_caption, f.weight_body, dim_text)));
                ToggleNodes { node, icon, label, status }
            })
            .collect();

        // Brightness and volume.
        let bright = slider_row(&mut tree, main, theme, "brightness-high");
        let volume = slider_row(&mut tree, main, theme, "audio-volume-medium");

        // "Networks" and "Notifications" (each opens its page).
        let links = tree.add(main, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: unit * 0.5 }));
        let nets_link = list_row(&mut tree, links, theme, true);
        tree.set_text(nets_link.name, "Networks");
        let notes_link = list_row(&mut tree, links, theme, true);
        tree.set_text(notes_link.name, "Notifications");
        set_icon(&mut tree, notes_link.icon, "notifications");
        // Windows 8: "Change PC settings" at the bottom of the Settings charm.
        let settings_link = list_row(&mut tree, links, theme, true);
        tree.set_text(settings_link.name, "All settings");
        set_icon(&mut tree, settings_link.icon, "preferences-system");

        // Paired Bluetooth devices.
        let devices = tree.add(main, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: unit * 0.5 }));
        tree.add(devices, Node::new().paint(text("Bluetooth devices", t.section_size, f.weight_title, dim_text)));
        let device_rows = (0..t.devices_max).map(|_| list_row(&mut tree, devices, theme, false)).collect();

        // Power buttons at the bottom.
        tree.add(main, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)));
        let row = tree.add(main, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Row { gap: t.toggle_gap }));
        let power = POWER
            .iter()
            .map(|p| {
                let node = tree.add(row, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: unit * 0.75 }).hit());
                let circle = tree.add(
                    node,
                    Node::new()
                        .size(if t.power_fill { Size::Fill(1.0) } else { Size::Fixed(t.power_button) }, Size::Fixed(t.power_button))
                        .align(Align::Center, Align::Start)
                        .paint(Paint::Rect { color: t.toggle_off.premultiplied(), radius: t.power_radius }),
                );
                tree.add(circle, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: p.icon().into(), size: t.power_icon }));
                let label = tree.add(node, Node::new().align(Align::Center, Align::Start).paint(text(p.label(), f.size_caption, f.weight_body, fg)));
                PowerNodes { node, circle, label }
            })
            .collect();

        // A sub-page: back arrow and title on top (hidden until its row is tapped). Returns (page, header
        // row, back button).
        let sub_page = |tree: &mut Tree, title: &str| {
            let page = tree.add(panel, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).layout(Layout::Column { gap: t.gap }));
            tree[page].visible = false;
            let head = tree.add(page, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Row { gap: unit }));
            let back = tree.add(
                head,
                Node::new()
                    .size(Size::Fixed(t.row_height), Size::Fixed(t.row_height))
                    .align(Align::Start, Align::Center)
                    .paint(Paint::Rect { color: [0.0; 4], radius: t.button_radius })
                    .hit(),
            );
            tree.add(back, Node::new().align(Align::Center, Align::Center).paint(Paint::Icon { name: "go-previous".into(), size: t.row_icon }));
            tree.add(head, Node::new().align(Align::Start, Align::Center).paint(text(title, t.title_size, t.title_weight, fg)));
            (page, head, back)
        };

        // The Networks page.
        let (nets_page, _, back) = sub_page(&mut tree, "Networks");
        let net_list = tree.add(nets_page, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: unit * 0.5 }));
        let nets_hint = tree.add(nets_page, Node::new().paint(text("", f.size_caption, f.weight_body, dim_text)));

        // The Notifications page: newest first, "Clear all" under the title.
        let (notes_page, _, notes_back) = sub_page(&mut tree, "Notifications");
        let clear_all = tree.add(
            notes_page,
            Node::new()
                .size(Size::Fit, Size::Fixed(t.button_height))
                .padding(unit)
                .paint(Paint::Rect { color: t.toggle_off.premultiplied(), radius: t.button_radius })
                .hit(),
        );
        tree.add(clear_all, Node::new().align(Align::Center, Align::Center).paint(text("Clear all", f.size_body, f.weight_title, fg)));
        let notes_list = tree.add(notes_page, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: unit * 0.5 }));
        let notes_hint = tree.add(notes_page, Node::new().paint(text("No notifications", f.size_caption, f.weight_body, dim_text)));

        // Notification banners: a column at the top, anchored left, centre or right (above the panel).
        let b = &theme.banner;
        let banner_box = tree.add(root, full().padding(b.margin));
        let anchor = match b.anchor.as_str() {
            "left" => Align::Start,
            "center" => Align::Center,
            _ => Align::End,
        };
        let banner_col = tree.add(
            banner_box,
            Node::new().size(Size::Fixed(b.width), Size::Fit).layout(Layout::Column { gap: b.gap }).align(anchor, Align::Start),
        );

        // The volume popup (on top of the banners).
        let pop = popup(&mut tree, root, theme);
        // The screenshot flash, over everything of this layer.
        let flash = tree.add(
            root,
            Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(Paint::Rect { color: theme.screenshot.flash.premultiplied(), radius: 0.0 }),
        );
        tree[flash].visible = false;

        let n = Nodes {
            dim,
            banner_col,
            panel,
            main,
            nets_link,
            notes_link,
            settings_link,
            notes_page,
            notes_back,
            clear_all,
            notes_list,
            notes_hint,
            time,
            date,
            battery,
            battery_status,
            toggles,
            bright,
            volume,
            pop,
            flash,
            devices,
            device_rows,
            power,
            nets_page,
            back,
            net_list,
            nets_hint,
        };
        (tree, n)
    }

    /// The panel is on screen or moving.
    fn panel_shown(&self) -> bool {
        self.edge_held || self.touch.as_ref().is_some_and(|t| t.mode != Mode::Banner && !floating(t.target)) || self.p.target > 0.0 || self.p.value > 0.001
    }

    /// Something of this layer is on screen: the panel, a notification banner or the volume popup.
    pub fn active(&self) -> bool {
        if self.locked {
            return self.pop_up();
        }
        self.panel_shown() || !self.banners.is_empty() || self.pop_up() || self.flash_o > 0.0
    }

    /// The session was locked or unlocked (see `locked`).
    pub fn set_locked(&mut self, locked: bool) {
        if self.locked == locked {
            return;
        }
        self.locked = locked;
        if locked {
            self.open(false);
            self.p.set(0.0, 0.0);
            self.edge_held = false;
            self.touch = None;
        }
        self.tree.set_visible(self.n.banner_col, !locked);
        self.apply();
    }

    /// The volume popup is wanted or still fading out.
    fn pop_up(&self) -> bool {
        self.pop_shown || self.pop_o > 0.0
    }

    fn popup_at(&self, x: f32, y: f32) -> bool {
        self.pop_shown && self.tree[self.n.pop.node].visible && self.tree.screen_rect(self.n.pop.node).contains(x, y)
    }

    /// A touch the core should give this layer although the panel is not open: one it is already
    /// tracking, or one that starts on a banner.
    pub fn wants(&self, id: i32, x: f32, y: f32) -> bool {
        if self.locked {
            return self.touch.as_ref().is_some_and(|t| t.id == id) || self.popup_at(x, y);
        }
        self.touch.as_ref().is_some_and(|t| t.id == id) || self.banner_at(x, y).is_some() || self.popup_at(x, y)
    }

    fn banner_at(&self, x: f32, y: f32) -> Option<usize> {
        self.banners.iter().position(|b| !b.closing && b.node.is_some_and(|n| self.tree.screen_rect(n).contains(x, y)))
    }

    /// A notification arrived or its app closed it.
    fn note_event(&mut self, ev: NoteEvent) {
        let default_ms = self.theme.banner.timeout_ms;
        match ev {
            NoteEvent::Show(note) => {
                self.remember(&note);
                let stay = match note.timeout_ms {
                    Some(0) => None,
                    Some(ms) => Some(Duration::from_millis(ms.clamp(1500, 30_000) as u64)),
                    None => Some(Duration::from_millis(default_ms as u64)),
                };
                if let Some(old) = self.banners.iter_mut().find(|x| x.note.id == note.id && !x.closing) {
                    old.note = note;
                    old.stay = stay;
                    old.until = None;
                } else {
                    self.banners.push(Banner { note, node: None, p: Spring::new(0.0, self.springs).with_eps(0.002), stay, until: None, closing: false, drag: (0.0, 0.0) });
                    let last = self.banners.len() - 1;
                    self.banners[last].p.set_target(1.0);
                    // Too many: the oldest goes.
                    let open: Vec<usize> = (0..self.banners.len()).filter(|&i| !self.banners[i].closing).collect();
                    if open.len() > self.theme.banner.max_shown.max(1) {
                        self.dismiss(open[0], false);
                    }
                }
                self.banners_dirty = true;
            }
            NoteEvent::Close(id) => {
                // Closed by its app: gone from the list too.
                let before = self.history.len();
                self.history.retain(|p| p.note.id != id);
                self.history_dirty |= self.history.len() != before;
                if let Some(x) = self.banners.iter_mut().find(|x| x.note.id == id && !x.closing) {
                    x.closing = true;
                    x.p.set_target(0.0);
                }
            }
        }
    }

    /// Slide banner `i` out and tell its app (by the user, or timed out).
    fn dismiss(&mut self, i: usize, by_user: bool) {
        let Some(b) = self.banners.get_mut(i).filter(|b| !b.closing) else { return };
        b.closing = true;
        b.p.set_target(0.0);
        self.services.send(Command::NoteClosed(b.note.id, by_user));
    }

    /// Rebuild the banner nodes (after one came or went).
    fn build_banners(&mut self) {
        self.banners_dirty = false;
        let theme = &self.theme;
        let b = &theme.banner;
        let tree = &mut self.tree;
        tree.clear_children(self.n.banner_col);
        for ban in &mut self.banners {
            let fill = if b.glass {
                Paint::Glass { radius: b.radius, tint: b.fill.premultiplied() }
            } else {
                Paint::Rect { color: b.fill.premultiplied(), radius: b.radius }
            };
            let mut node = Node::new()
                .size(Size::Fill(1.0), Size::Fit)
                .layout(Layout::Row { gap: b.padding })
                .padding(b.padding)
                .paint(fill)
                .hit();
            if b.shadow {
                node = node.shadow(Shadow { sigma: theme.shadow.sigma, dx: 0.0, dy: theme.shadow.offset_y, color: theme.shadow.color.premultiplied() });
            }
            let node = tree.add(self.n.banner_col, node);
            let icon = if ban.note.icon.is_empty() { "dialog-information".to_string() } else { ban.note.icon.clone() };
            tree.add(node, Node::new().paint(Paint::Icon { name: icon, size: b.icon }));
            let col = tree.add(node, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: 2.0 }));
            let title = if ban.note.summary.is_empty() { ban.note.app.clone() } else { ban.note.summary.clone() };
            let wrapped = |s: &str, size: f32, weight: u16, color: Rgba| Paint::Text {
                text: ellipsize(s, 160),
                style: TextStyle { size, weight, ..TextStyle::default() },
                color,
                wrap: true,
                layout: None,
            };
            tree.add(col, Node::new().size(Size::Fill(1.0), Size::Fit).paint(wrapped(&title, b.title_size, theme.font.weight_title, b.text.premultiplied())));
            if !ban.note.body.is_empty() {
                tree.add(col, Node::new().size(Size::Fill(1.0), Size::Fit).paint(wrapped(&ban.note.body, b.body_size, theme.font.weight_body, b.text_dim.premultiplied())));
            }
            ban.node = Some(node);
        }
    }

    /// Banners: slide (spring + finger), drop the ones that finished sliding out. True while moving.
    fn step_banners(&mut self, dt: f32) -> bool {
        let mut moving = false;
        let now = self.now;
        let dragged = self.touch.as_ref().and_then(|t| if let Target::Banner(i) = t.target { Some(i) } else { None });
        let mut expired = Vec::new();
        for (i, b) in self.banners.iter_mut().enumerate() {
            if !b.closing && b.until.is_none() {
                b.until = b.stay.map(|s| now + s);
            }
            if !b.closing && dragged != Some(i) && b.until.is_some_and(|u| now >= u) {
                expired.push(i);
            }
        }
        for i in expired {
            self.dismiss(i, false);
        }
        for b in &mut self.banners {
            moving |= b.p.step(dt);
        }
        let before = self.banners.len();
        self.banners.retain(|b| !(b.closing && b.p.value < 0.01));
        if self.banners.len() != before {
            // Indices shift: a finger on a banner lets go of it.
            if self.touch.as_ref().is_some_and(|t| matches!(t.target, Target::Banner(_))) {
                self.touch = None;
            }
            self.banners_dirty = true;
            moving = true;
        }
        moving
    }

    /// Place the banners for this frame.
    fn apply_banners(&mut self) {
        if self.banners_dirty {
            self.build_banners();
        }
        let b = &self.theme.banner;
        let from_top = b.slide_from == "top";
        let shadow_room = if b.shadow { self.theme.shadow.sigma * 3.0 } else { 0.0 };
        for ban in &self.banners {
            let Some(node) = ban.node else { continue };
            let q = 1.0 - ban.p.value.clamp(0.0, 1.0);
            let h = self.tree[node].frame.h;
            let (dx, dy) = ban.drag;
            self.tree[node].translate = if from_top {
                (0.0, -q * (h + b.margin + shadow_room) + dy.min(0.0))
            } else {
                (q * (b.width + b.margin + shadow_room) + dx.max(0.0), 0.0)
            };
        }
    }

    /// Warm-up pose for `ShellUi::warm_panel`: shown fully open (off-screen quads are culled, which would
    /// skip loading their glyphs), or back to the real state.
    pub fn warm_pose(&mut self, on: bool) {
        if on {
            let root = self.tree.root;
            self.tree[root].visible = true;
            self.tree[self.n.panel].translate = (0.0, 0.0);
            // The volume popup too, with every digit (its first show would otherwise shape them).
            let pop = &self.n.pop;
            self.tree[pop.node].visible = true;
            self.tree[pop.node].opacity = 1.0;
            self.tree.set_text(pop.number, "0123456789–");
        } else {
            self.apply();
        }
    }

    /// The services snapshot this panel shows (to warm up again once real values are in).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Where the panel covers the screen opaquely (a solid, square, fully opaque fill), so the compositor
    /// skips what is under it. Empty for glass, rounded or see-through panels.
    pub fn opaque(&self) -> Vec<crate::render::batch::Rect> {
        let t = &self.theme.control;
        if t.glass || t.radius > 0.0 || t.fill.a < 1.0 || !self.panel_shown() {
            return Vec::new();
        }
        let r = self.tree.screen_rect(self.n.panel).round_out().inset(1.0);
        if r.w <= 0.0 || r.h <= 0.0 { Vec::new() } else { vec![r] }
    }

    /// Open, held or tracking a touch: input goes here first. A panel that is sliding out after being
    /// closed is still drawn (`active`) but no longer takes new touches.
    /// Being pulled in or open (not just a touch on a banner).
    pub fn coming_in(&self) -> bool {
        self.edge_held || self.p.target > 0.0
    }

    pub fn takes_input(&self) -> bool {
        if self.locked {
            return false;
        }
        self.edge_held || self.touch.is_some() || self.p.target > 0.0
    }

    /// Open (true) or close the panel, from where it is now.
    pub fn open(&mut self, open: bool) {
        if open && !self.panel_shown() {
            info!("control center: open");
            self.page = Page::Main;
            // Fresh values when it comes in.
            self.poll();
            self.update_clock();
        } else if !open && self.p.target > 0.0 {
            info!("control center: close");
        }
        self.p.set_target(if open { 1.0 } else { 0.0 });
        if !open {
            self.confirm = None;
            // Nothing typed survives closing.
            self.select(None);
            self.next_scan = None;
        }
        self.apply();
    }

    pub fn toggle(&mut self) {
        let open = self.p.target < 0.5;
        self.open(open);
    }

    /// Distance over which the panel comes in (its width and the margin).
    fn travel(&self) -> f32 {
        self.theme.control.width + self.theme.control.margin
    }

    /// The right-edge swipe: the panel's left edge follows the finger.
    pub fn edge(&mut self, phase: EdgePhase, x: f32, vx: f32) {
        let travel = self.travel();
        let p = ((self.w - x) / travel).clamp(0.0, 1.0);
        let v = -vx / travel;
        match phase {
            EdgePhase::Begin => {
                if !self.panel_shown() {
                    self.page = Page::Main;
                    self.poll();
                    self.update_clock();
                }
                self.edge_held = true;
                self.p.set(p, v);
            }
            EdgePhase::Move => self.p.set(p, v),
            EdgePhase::End | EdgePhase::Cancel => {
                self.edge_held = false;
                let t = &self.theme.control;
                let open = phase == EdgePhase::End && (-vx > t.open_flick || (vx < t.open_flick && p > t.open_commit));
                info!(open, p = format!("{p:.2}"), vx = vx as i32, "control center: edge swipe released");
                self.p.set_target(if open { 1.0 } else { 0.0 });
            }
        }
        self.apply();
    }

    fn update_clock(&mut self) {
        let (time, date, _, secs) = local_time();
        self.tree.set_text(self.n.time, &time);
        self.tree.set_text(self.n.date, &date);
        self.next_minute = Some(self.now + Duration::from_secs(secs));
    }

    fn wifi_on(&self) -> Option<bool> {
        let real = self.sys.wifi.as_ref().map(|w| w.enabled)?;
        Some(self.wifi_pending.map(|(on, _)| on).unwrap_or(real))
    }

    fn bt_on(&self) -> Option<bool> {
        let real = self.sys.bluetooth.as_ref().map(|b| b.powered)?;
        Some(self.bt_pending.map(|(on, _)| on).unwrap_or(real))
    }


    /// Write the current state into the tree.
    fn apply(&mut self) {
        let theme = &self.theme;
        let t = &theme.control;
        let c = &theme.colors;
        let fg = c.text.premultiplied();
        let dim_text = c.text_dim.premultiplied();
        let on_accent = c.on_accent.premultiplied();
        let accent = c.accent.premultiplied();
        let off = t.toggle_off.premultiplied();
        let p = self.p.value.clamp(0.0, 1.0);
        let travel = t.width + t.margin;
        let shown = self.p.value > 0.001 || self.edge_held || !self.banners.is_empty() || self.pop_up() || self.flash_o > 0.0;
        let volume = self.shown_volume();
        let tree = &mut self.tree;

        // Slide and dim.
        let shadow_room = if t.shadow { theme.shadow.sigma * 3.0 } else { 0.0 };
        tree[self.n.panel].translate = ((1.0 - p) * (travel + shadow_room), 0.0);
        tree[self.n.dim].opacity = p;
        let root = tree.root;
        tree[root].visible = shown;

        // Battery.
        let (pct, status) = match &self.sys.battery {
            Some(b) => {
                let status = match b.charge {
                    Charge::Charging if b.time_to_full > 0 => format!("Charging · full in {}", duration_text(b.time_to_full)),
                    Charge::Charging => "Charging".into(),
                    Charge::Discharging if b.time_to_empty > 0 => format!("{} left", duration_text(b.time_to_empty)),
                    Charge::Discharging => "On battery".into(),
                    Charge::Full => "Fully charged".into(),
                    Charge::Pending => "Plugged in".into(),
                    Charge::Empty => "Empty".into(),
                    Charge::Unknown => String::new(),
                };
                (format!("{:.0}%", b.percent), status)
            }
            None => (String::new(), "No battery".into()),
        };
        tree.set_text(self.n.battery, &pct);
        tree.set_text(self.n.battery_status, &status);

        // Toggles.
        // Status text is cut to what fits a toggle (roughly: average glyph ~0.55 em).
        let cols = t.toggle_columns.max(1) as f32;
        let tile_w = (t.width - 2.0 * t.padding - (cols - 1.0) * t.toggle_gap) / cols - 2.0 * t.toggle_padding;
        let text_w = if t.toggle_layout == "column" { tile_w } else { tile_w - t.toggle_icon as f32 - 1.5 * theme.spacing.unit as f32 };
        let max_chars = ((text_w / (theme.font.size_caption * 0.55)) as usize).max(4);
        let wifi = self.sys.wifi.clone();
        let wifi_on = self.wifi_pending.map(|(on, _)| on).or(wifi.as_ref().map(|w| w.enabled));
        let bt = self.sys.bluetooth.clone();
        let bt_on = self.bt_pending.map(|(on, _)| on).or(bt.as_ref().map(|b| b.powered));
        for (i, which) in TOGGLES.iter().enumerate() {
            let (label, status, icon, on, avail) = match which {
                Toggle::Wifi => {
                    let on = wifi_on.unwrap_or(false);
                    let status = match wifi.as_ref() {
                        None => "Unavailable".to_string(),
                        Some(w) if !w.hw_enabled => "Blocked".into(),
                        Some(_) if !on => "Off".into(),
                        Some(_) if self.wifi_pending.is_some() => "Turning on…".into(),
                        Some(w) => match (w.link, w.ssid.as_deref()) {
                            (Link::Connected, Some(s)) => ellipsize(s, max_chars),
                            (Link::Connected, None) => "Connected".into(),
                            (Link::Connecting, _) => "Connecting…".into(),
                            (Link::Disconnected, _) => "Not connected".into(),
                        },
                    };
                    let icon = match wifi.as_ref() {
                        Some(w) if on && w.link == Link::Connected => {
                            let level = match w.strength {
                                0..=12 => "00",
                                13..=37 => "25",
                                38..=62 => "50",
                                63..=87 => "75",
                                _ => "100",
                            };
                            format!("network-wireless-connected-{level}")
                        }
                        Some(_) if on => "network-wireless".into(),
                        _ => "network-wireless-disconnected".into(),
                    };
                    ("Wi-Fi", status, icon, on, wifi.is_some())
                }
                Toggle::Bluetooth => {
                    let on = bt_on.unwrap_or(false);
                    let status = match bt.as_ref() {
                        None => "Unavailable".to_string(),
                        Some(_) if !on => "Off".into(),
                        Some(b) => match b.devices.iter().find(|d| d.connected) {
                            Some(d) => ellipsize(&d.name, max_chars),
                            None => "On".into(),
                        },
                    };
                    let icon = if on { "network-bluetooth-activated" } else { "network-bluetooth-inactive" };
                    ("Bluetooth", status, icon.to_string(), on, bt.is_some())
                }
                Toggle::Rotation => {
                    let on = self.rotation_locked;
                    let portrait = matches!(self.sent_orientation, Some(Orientation::LeftUp | Orientation::RightUp));
                    let icon = match (on, portrait) {
                        (true, true) => "rotation-locked-portrait",
                        (true, false) => "rotation-locked-landscape",
                        _ => "rotation-allowed",
                    };
                    let avail = self.sys.orientation.is_some();
                    let status = if !avail {
                        "Unavailable"
                    } else if on {
                        "Locked"
                    } else if self.hw_keyboard {
                        "Keyboard attached"
                    } else {
                        "Auto"
                    };
                    ("Rotation", ellipsize(status, max_chars), icon.to_string(), on, avail)
                }
            };
            let nodes = &self.n.toggles[i];
            tree.set_text(nodes.label, label);
            tree.set_text(nodes.status, &status);
            set_icon(tree, nodes.icon, &icon);
            set_color(tree, nodes.node, if on { accent } else { off });
            set_color(tree, nodes.label, if on { on_accent } else { fg });
            set_color(tree, nodes.status, if on { on_accent } else { dim_text });
            tree[nodes.node].opacity = if avail { 1.0 } else { 0.5 };
        }

        // Brightness and volume sliders.
        let bright = self.bright.map(|(b, _)| b).or(self.sys.brightness);
        tree.set_visible(self.n.bright.row, bright.is_some());
        if let Some(b) = bright {
            place_slider(tree, &self.n.bright, b, theme);
        }
        tree.set_visible(self.n.volume.row, volume.is_some());
        if let Some(v) = volume {
            place_slider(tree, &self.n.volume, v.level, theme);
            set_icon(tree, self.n.volume.icon, volume_icon(v));
            set_color(tree, self.n.volume.fill, if v.muted { theme.volume.muted_fill.premultiplied() } else { accent });
        }

        // Paired Bluetooth devices (while Bluetooth is on): tap = connect / disconnect.
        let devs = match bt.as_ref() {
            Some(b) if bt_on == Some(true) => b.devices.clone(),
            _ => Vec::new(),
        };
        let failed = bt.as_ref().and_then(|b| b.failed.clone());
        tree.set_visible(self.n.devices, !devs.is_empty());
        let row_chars = ((t.width - 2.0 * t.padding - t.row_icon as f32 - 1.5 * theme.spacing.unit as f32) / (theme.font.size_body * 0.55)) as usize;
        for (i, row) in self.n.device_rows.iter().enumerate() {
            match devs.get(i) {
                Some(d) => {
                    let pending = self.bt_dev_pending.as_ref().filter(|(p, _, _)| *p == d.path).map(|(_, on, _)| *on);
                    let status = match pending {
                        Some(true) => "Connecting…",
                        Some(false) => "Disconnecting…",
                        None if failed.as_deref() == Some(d.path.as_str()) => "Couldn't connect",
                        None if d.connected => "Connected",
                        None => "Not connected",
                    };
                    tree.set_text(row.name, &ellipsize(&d.name, row_chars.max(4)));
                    tree.set_text(row.status, status);
                    set_icon(tree, row.icon, &bt_icon(&d.icon));
                    tree.set_visible(row.node, true);
                }
                None => tree.set_visible(row.node, false),
            }
        }

        // "Networks" row: what Wi-Fi is doing.
        let link_status = match wifi.as_ref() {
            None => "Unavailable".to_string(),
            Some(_) if !wifi_on.unwrap_or(false) => "Wi-Fi is off".into(),
            Some(w) => match (w.link, w.ssid.as_deref()) {
                (Link::Connected, Some(s)) => format!("Connected to {s}"),
                (Link::Connecting, Some(s)) => format!("Connecting to {s}…"),
                _ => match w.networks.len() {
                    0 => "No networks found".into(),
                    1 => "1 network available".into(),
                    n => format!("{n} networks available"),
                },
            },
        };
        tree.set_text(self.n.nets_link.status, &ellipsize(&link_status, row_chars.max(4)));
        let link_icon = match wifi.as_ref() {
            Some(w) if wifi_on == Some(true) && w.link == Link::Connected => signal_icon(w.strength),
            Some(_) if wifi_on == Some(true) => "network-wireless",
            _ => "network-wireless-disconnected",
        };
        set_icon(tree, self.n.nets_link.icon, link_icon);

        // Pages.
        tree.set_visible(self.n.main, self.page == Page::Main);
        tree.set_visible(self.n.nets_page, self.page == Page::Networks);
        tree.set_visible(self.n.notes_page, self.page == Page::Notifications);
        let notes_status = match self.history.len() {
            0 => "No notifications".to_string(),
            1 => "1 notification".into(),
            n => format!("{n} notifications"),
        };
        tree.set_text(self.n.notes_link.status, &notes_status);

        // Power buttons (Restart / Power off ask for a second tap).
        let armed = self.confirm.filter(|(_, until)| self.now < *until).map(|(i, _)| i);
        for (i, which) in POWER.iter().enumerate() {
            let nodes = &self.n.power[i];
            let is_armed = armed == Some(i);
            tree.set_text(nodes.label, if is_armed { "Tap again" } else { which.label() });
            set_color(tree, nodes.circle, if is_armed { accent } else { off });
        }
        self.apply_networks();
        self.apply_history();
        self.apply_banners();
        self.apply_popup(volume);
        let flash = self.n.flash;
        self.tree.set_visible(flash, self.flash_o > 0.0);
        self.tree[flash].opacity = self.flash_o.clamp(0.0, 1.0);
    }

    /// The volume popup's level, number, icon and fade.
    fn apply_popup(&mut self, volume: Option<Volume>) {
        let theme = &self.theme;
        let v = &theme.volume;
        let tree = &mut self.tree;
        let n = &self.n.pop;
        let up = self.pop_shown || self.pop_o > 0.0;
        tree.set_visible(n.node, up);
        if !up {
            return;
        }
        tree[n.node].opacity = self.pop_o.clamp(0.0, 1.0);
        let vol = volume.unwrap_or_default();
        let l = vol.level.clamp(0.0, 1.0);
        let vertical = v.orientation != "horizontal";
        let set_len = |tree: &mut Tree, id: NodeId, w: f32| {
            let s = Size::Fill(w);
            let cur = if vertical { tree[id].height } else { tree[id].width };
            if cur != s {
                if vertical {
                    tree[id].height = s;
                } else {
                    tree[id].width = s;
                }
                tree.mark_layout();
            }
        };
        set_len(tree, n.spacer, 1.0 - l);
        set_len(tree, n.level, l);
        let level_fill = if vol.muted {
            v.muted_fill
        } else {
            crate::theme::Color::parse(&v.level_fill).unwrap_or(theme.colors.accent)
        };
        set_color(tree, n.level, level_fill.premultiplied());
        tree.set_text(n.number, &if volume.is_some() { format!("{:.0}", l * 100.0) } else { "–".into() });
        set_icon(tree, n.icon, volume_icon(vol));
    }

    /// The volume as shown: the audio thread's, with changes made here that it has not reported yet.
    fn shown_volume(&self) -> Option<Volume> {
        let mut v = self.sys.volume?;
        if let Some((l, _)) = self.vol {
            v.level = l;
        }
        if let Some((m, _)) = self.mute_pending {
            v.muted = m;
        }
        Some(v)
    }

    fn set_volume(&mut self, level: f32) {
        let level = level.clamp(0.0, 1.0);
        if self.shown_volume().map(|v| v.level) != Some(level) {
            self.services.send(Command::SetVolume(level));
        }
        self.vol = Some((level, self.now));
    }

    fn set_mute(&mut self, muted: bool) {
        self.services.send(Command::SetMute(Some(muted)));
        self.mute_pending = Some((muted, self.now));
    }

    /// "All settings" was tapped since the last call.
    pub fn take_open_settings(&mut self) -> bool {
        std::mem::take(&mut self.open_settings)
    }

    /// A user screenshot was saved (`Ok(path as shown)`) or failed: flash and/or a banner (`[screenshot]`).
    pub fn screenshot_taken(&mut self, result: &Result<String, String>) {
        let tk = &self.theme.screenshot;
        let note = match result {
            Ok(path) => {
                if tk.flash_ms > 0 {
                    self.flash_o = 1.0;
                }
                tk.banner.then(|| Note {
                    id: SCREENSHOT_ID,
                    app: "Screenshot".into(),
                    icon: tk.icon.clone(),
                    summary: "Screenshot saved".into(),
                    body: path.clone(),
                    urgency: 0,
                    timeout_ms: None,
                    transient: false,
                })
            }
            Err(err) => Some(Note {
                id: SCREENSHOT_ID,
                app: "Screenshot".into(),
                icon: "dialog-warning".into(),
                summary: "Couldn't save the screenshot".into(),
                body: err.clone(),
                urgency: 1,
                timeout_ms: None,
                transient: false,
            }),
        };
        if let Some(note) = note {
            self.note_event(NoteEvent::Show(note));
        }
        self.apply();
    }

    /// Flash fade-out. True while fading.
    fn step_flash(&mut self, dt: f32) -> bool {
        if self.flash_o <= 0.0 {
            return false;
        }
        let ms = self.theme.screenshot.flash_ms.max(1) as f32;
        self.flash_o = (self.flash_o - dt * 1000.0 / ms).max(0.0);
        true
    }

    /// Show the popup (again) for `timeout_ms` from its next frame.
    fn show_popup(&mut self) {
        self.pop_shown = true;
        self.pop_until = None;
    }

    /// A volume key (from the core; repeats come as more presses).
    pub fn volume_key(&mut self, key: VolumeKey) {
        // Called between frames: a fresh time for the pending values' timeouts.
        self.now = self.now.max(self.clock.now().into());
        let Some(cur) = self.shown_volume() else {
            info!(?key, "volume key: no audio output");
            return;
        };
        let step = self.theme.volume.step;
        match key {
            VolumeKey::Up => {
                self.set_volume(step_level(cur.level, step, true));
                if cur.muted {
                    self.set_mute(false);
                }
            }
            VolumeKey::Down => self.set_volume(step_level(cur.level, step, false)),
            VolumeKey::Mute => self.set_mute(!cur.muted),
        }
        let now = self.shown_volume().unwrap_or(cur);
        info!(?key, level = format!("{:.2}", now.level), muted = now.muted, "volume key");
        self.show_popup();
        self.apply();
    }

    /// Popup: time out (not while touched), fade. True while fading.
    fn step_popup(&mut self, dt: f32) -> bool {
        let v = &self.theme.volume;
        let held = self.touch.as_ref().is_some_and(|t| matches!(t.target, Target::Slider(Slider::Popup) | Target::Mute { popup: true }));
        if held {
            self.pop_until = None;
        } else if self.pop_shown {
            let until = *self.pop_until.get_or_insert(self.now + Duration::from_millis(v.timeout_ms as u64));
            if self.now >= until {
                self.pop_shown = false;
                self.pop_until = None;
            }
        }
        let target = if self.pop_shown { 1.0 } else { 0.0 };
        if self.pop_o == target {
            return false;
        }
        let ms = if self.pop_shown { v.fade_in_ms } else { v.fade_out_ms };
        let d = if ms == 0 { 1.0 } else { dt * 1000.0 / ms as f32 };
        self.pop_o = if target > self.pop_o { (self.pop_o + d).min(1.0) } else { (self.pop_o - d).max(0.0) };
        true
    }

    /// The Notifications page rows (rebuilt when the history changed and the page is shown).
    fn apply_history(&mut self) {
        if self.page != Page::Notifications || !self.history_dirty {
            return;
        }
        self.history_dirty = false;
        let theme = &self.theme;
        let t = &theme.control;
        let f = &theme.font;
        let c = &theme.colors;
        let unit = theme.spacing.unit as f32;
        let tree = &mut self.tree;
        tree.clear_children(self.n.notes_list);
        self.past_rows.clear();
        let shown = self.history.len().min(t.notes_max);
        tree.set_visible(self.n.notes_hint, self.history.is_empty());
        tree.set_visible(self.n.clear_all, !self.history.is_empty());
        let line = |s: &str, size: f32, weight: u16, color: Rgba, wrap: bool| Paint::Text {
            text: s.into(),
            style: TextStyle { size, weight, ..TextStyle::default() },
            color,
            wrap,
            layout: None,
        };
        for past in &self.history[..shown] {
            let n = &past.note;
            let row = tree.add(
                self.n.notes_list,
                Node::new()
                    .size(Size::Fill(1.0), Size::Fit)
                    .layout(Layout::Row { gap: 1.5 * unit })
                    .padding(t.row_padding)
                    .paint(Paint::Rect { color: [0.0; 4], radius: t.row_radius })
                    .hit(),
            );
            let icon = if n.icon.is_empty() { "dialog-information" } else { n.icon.as_str() };
            tree.add(row, Node::new().paint(Paint::Icon { name: icon.into(), size: t.row_icon }));
            let col = tree.add(row, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: 1.0 }));
            let title = if n.summary.is_empty() { n.app.as_str() } else { n.summary.as_str() };
            tree.add(col, Node::new().size(Size::Fill(1.0), Size::Fit).paint(line(&ellipsize(title, 80), f.size_body, f.weight_title, c.text.premultiplied(), true)));
            if !n.body.is_empty() {
                tree.add(col, Node::new().size(Size::Fill(1.0), Size::Fit).paint(line(&ellipsize(&n.body, 120), f.size_caption, f.weight_body, c.text.premultiplied(), true)));
            }
            let meta = if n.app.is_empty() { past.at.clone() } else { format!("{} · {}", n.app, past.at) };
            tree.add(col, Node::new().paint(line(&meta, f.size_caption, f.weight_body, c.text_dim.premultiplied(), false)));
            self.past_rows.push(row);
        }
    }

    /// Keep a notification for the Notifications page (newest first; one entry per id).
    fn remember(&mut self, note: &Note) {
        self.history.retain(|p| p.note.id != note.id);
        if note.transient {
            return;
        }
        self.history.insert(0, Past { note: note.clone(), at: local_time().0 });
        self.history.truncate(self.theme.control.notes_max.max(1) * 4);
        self.history_dirty = true;
    }

    /// The Networks page: rebuild the rows when what they show changed.
    fn apply_networks(&mut self) {
        if self.page != Page::Networks {
            return;
        }
        let theme = &self.theme;
        let t = &theme.control;
        let c = &theme.colors;
        let f = &theme.font;
        let unit = theme.spacing.unit as f32;
        let wifi = self.sys.wifi.clone();
        let wifi_on = self.wifi_on().unwrap_or(false);
        let nets: Vec<Network> = match wifi.as_ref() {
            Some(w) if wifi_on => w.networks.iter().take(t.networks_max).cloned().collect(),
            _ => Vec::new(),
        };
        let join = wifi.as_ref().and_then(|w| w.join.clone());
        let link = wifi.as_ref().map(|w| w.link);
        // The password enters the key only by its length.
        let key = format!("{nets:?}|{:?}|{}|{join:?}|{link:?}", self.sel, self.pw.chars().count());
        let hint = match wifi.as_ref() {
            None => "Wi-Fi is not available",
            Some(_) if !wifi_on => "Wi-Fi is off",
            Some(_) if nets.is_empty() => "Looking for networks…",
            Some(_) => "",
        };
        self.tree.set_text(self.n.nets_hint, hint);
        self.tree.set_visible(self.n.nets_hint, !hint.is_empty());
        if key == self.nets_key {
            return;
        }
        self.nets_key = key;
        let tree = &mut self.tree;
        tree.clear_children(self.n.net_list);
        self.net_rows.clear();
        let fg = c.text.premultiplied();
        let dim_text = c.text_dim.premultiplied();
        let name_chars = ((t.width - 2.0 * t.padding - t.row_icon as f32 - 1.5 * unit) / (f.size_body * 0.55)) as usize;
        for net in &nets {
            let selected = self.sel.as_deref() == Some(net.ssid.as_str());
            let joining = join.as_ref().filter(|j| j.ssid == net.ssid);
            let connecting = joining.is_some_and(|j| j.state == JoinState::Connecting) || (net.active && link == Some(Link::Connecting));
            let entry = tree.add(
                self.n.net_list,
                Node::new()
                    .size(Size::Fill(1.0), Size::Fit)
                    .layout(Layout::Column { gap: 0.0 })
                    .padding(t.row_padding)
                    .paint(Paint::Rect { color: if selected { t.row_selected.premultiplied() } else { [0.0; 4] }, radius: t.row_radius }),
            );
            let row = list_row(tree, entry, theme, false);
            let status = if connecting {
                "Connecting…"
            } else if net.active && link == Some(Link::Connected) {
                "Connected"
            } else {
                match net.security {
                    Security::Unsupported => "Not supported (WPA3, Enterprise or WEP)",
                    _ if net.saved => "Saved",
                    Security::Psk => "Secured",
                    Security::Open => "Open",
                }
            };
            tree.set_text(row.name, &ellipsize(&net.ssid, name_chars.max(4)));
            tree.set_text(row.status, status);
            set_icon(tree, row.icon, signal_icon(net.strength));
            let mut nr = NetRow { head: row.node, connect: None, forget: None, field: None };
            if selected {
                let detail = tree.add(entry, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Column { gap: unit }).padding(unit));
                if needs_password(net) && !connecting {
                    let field = tree.add(
                        detail,
                        Node::new()
                            .size(Size::Fill(1.0), Size::Fixed(t.field_height))
                            .layout(Layout::Row { gap: 0.0 })
                            .padding(unit)
                            .paint(Paint::Rect { color: t.field_fill.premultiplied(), radius: t.field_radius })
                            .hit(),
                    );
                    let n = self.pw.chars().count();
                    let (shown, color) = if n == 0 {
                        // Placeholder: the text colour at half opacity (premultiplied, so every channel halves).
                        ("Network security key".to_string(), t.field_text.premultiplied().map(|v| v * 0.5))
                    } else {
                        ("•".repeat(n), t.field_text.premultiplied())
                    };
                    tree.add(field, Node::new().align(Align::Start, Align::Center).paint(text(&shown, f.size_body, f.weight_body, color)));
                    nr.field = Some(field);
                }
                let error = match joining.map(|j| j.state) {
                    Some(JoinState::WrongPassword) => Some("That didn't work. Check the password and try again."),
                    Some(JoinState::Failed) => Some("Couldn't connect to this network."),
                    _ => None,
                };
                if let Some(e) = error {
                    tree.add(detail, Node::new().paint(text(e, f.size_caption, f.weight_body, fg)));
                }
                if !connecting && net.security != Security::Unsupported {
                    let buttons = tree.add(detail, Node::new().size(Size::Fill(1.0), Size::Fit).layout(Layout::Row { gap: unit }));
                    let connected = net.active && link == Some(Link::Connected);
                    let b = button(tree, buttons, theme, if connected { "Disconnect" } else { "Connect" }, !connected);
                    if needs_password(net) && self.pw.len() < PSK_MIN {
                        tree[b].opacity = 0.5;
                    }
                    nr.connect = Some(b);
                    if net.saved {
                        nr.forget = Some(button(tree, buttons, theme, "Forget", false));
                    }
                } else if connecting {
                    tree.add(detail, Node::new().paint(text("Connecting…", f.size_caption, f.weight_body, dim_text)));
                }
            }
            self.net_rows.push(nr);
        }
    }

    fn target_at(&self, x: f32, y: f32) -> Target {
        if self.popup_at(x, y) {
            let hit = self.tree.hit_test(x, y);
            return if hit == Some(self.n.pop.icon_box) { Target::Mute { popup: true } } else { Target::Slider(Slider::Popup) };
        }
        if let Some(i) = self.banner_at(x, y) {
            return Target::Banner(i);
        }
        if !self.tree.screen_rect(self.n.panel).contains(x, y) {
            return Target::Outside;
        }
        let Some(hit) = self.tree.hit_test(x, y) else { return Target::Panel };
        if hit == self.n.bright.slider {
            return Target::Slider(Slider::Bright);
        }
        if hit == self.n.volume.slider {
            return Target::Slider(Slider::Volume);
        }
        if hit == self.n.volume.icon_box {
            return Target::Mute { popup: false };
        }
        if let Some(i) = self.n.toggles.iter().position(|t| t.node == hit) {
            return Target::Toggle(i);
        }
        if let Some(i) = self.n.power.iter().position(|p| p.node == hit) {
            return Target::Power(i);
        }
        if hit == self.n.nets_link.node {
            return Target::NetsLink;
        }
        if hit == self.n.notes_link.node {
            return Target::NotesLink;
        }
        if hit == self.n.settings_link.node {
            return Target::SettingsLink;
        }
        if hit == self.n.notes_back {
            return Target::Back;
        }
        if hit == self.n.clear_all {
            return Target::ClearAll;
        }
        if let Some(i) = self.past_rows.iter().position(|&r| r == hit) {
            return Target::Past(i);
        }
        if hit == self.n.back {
            return Target::Back;
        }
        if let Some(i) = self.n.device_rows.iter().position(|r| r.node == hit) {
            return Target::Device(i);
        }
        for (i, r) in self.net_rows.iter().enumerate() {
            if r.head == hit {
                return Target::Net(i);
            }
            if r.connect == Some(hit) {
                return Target::NetConnect(i);
            }
            if r.forget == Some(hit) {
                return Target::NetForget(i);
            }
            if r.field == Some(hit) {
                return Target::Field;
            }
        }
        Target::Panel
    }

    fn slide_to(&mut self, which: Slider, x: f32, y: f32) {
        let t = &self.theme.control;
        match which {
            Slider::Bright => {
                let r = self.tree.screen_rect(self.n.bright.slider);
                let span = (r.w - t.knob_width).max(1.0);
                let f = ((x - r.x - t.knob_width * 0.5) / span).clamp(MIN_BRIGHTNESS, 1.0);
                if self.bright.map(|(b, _)| b) != Some(f) {
                    self.services.send(Command::SetBrightness(f));
                }
                self.bright = Some((f, self.now));
            }
            Slider::Volume => {
                let r = self.tree.screen_rect(self.n.volume.slider);
                let span = (r.w - t.knob_width).max(1.0);
                let f = ((x - r.x - t.knob_width * 0.5) / span).clamp(0.0, 1.0);
                self.drag_volume(f);
            }
            Slider::Popup => {
                let v = &self.theme.volume;
                let r = self.tree.screen_rect(self.n.pop.bar);
                let k = v.knob_length;
                let f = if v.orientation == "horizontal" {
                    (x - r.x - k * 0.5) / (r.w - k).max(1.0)
                } else {
                    (r.y + r.h - y - k * 0.5) / (r.h - k).max(1.0)
                };
                self.drag_volume(f.clamp(0.0, 1.0));
                self.show_popup();
            }
        }
    }

    /// A slider sets the volume; moving it up unmutes.
    fn drag_volume(&mut self, f: f32) {
        let Some(cur) = self.shown_volume() else { return };
        // Whole percents: no flood of tiny changes.
        let f = (f * 100.0).round() / 100.0;
        self.set_volume(f);
        if cur.muted && f > cur.level {
            self.set_mute(false);
        }
    }

    fn tap(&mut self, target: Target) {
        match target {
            Target::Outside => self.open(false),
            Target::Panel | Target::Slider(_) | Target::Field => {}
            Target::Mute { popup } => {
                if let Some(v) = self.shown_volume() {
                    info!(muted = !v.muted, "control center: mute");
                    self.set_mute(!v.muted);
                    if popup {
                        self.show_popup();
                    }
                }
            }
            Target::Banner(i) => self.dismiss(i, true),
            Target::NetsLink => self.show_page(Page::Networks),
            Target::Back => self.show_page(Page::Main),
            Target::NotesLink => self.show_page(Page::Notifications),
            Target::SettingsLink => {
                self.open_settings = true;
                self.open(false);
            }
            Target::Past(i) => {
                if i < self.history.len() {
                    self.history.remove(i);
                    self.history_dirty = true;
                }
            }
            Target::ClearAll => {
                info!(n = self.history.len(), "control center: clear notifications");
                self.history.clear();
                self.history_dirty = true;
            }
            Target::Device(i) => {
                let dev = self.sys.bluetooth.as_ref().and_then(|b| b.devices.get(i)).cloned();
                if let Some(d) = dev
                    && self.bt_dev_pending.is_none()
                {
                    info!(device = %d.name, connect = !d.connected, "control center: Bluetooth device");
                    self.bt_dev_pending = Some((d.path.clone(), !d.connected, self.now));
                    self.services.send(Command::BtConnect(d.path, !d.connected));
                }
            }
            Target::Net(i) => {
                let ssid = self.shown_network(i).map(|n| n.ssid);
                if ssid.is_some() && ssid == self.sel {
                    self.select(None);
                } else {
                    self.select(ssid);
                }
            }
            Target::NetConnect(i) => self.connect(i),
            Target::NetForget(i) => {
                if let Some(n) = self.shown_network(i) {
                    info!(ssid = %n.ssid, "control center: forget network");
                    self.services.send(Command::WifiForget(n.ssid));
                }
            }
            Target::Toggle(i) => match TOGGLES[i] {
                Toggle::Wifi => {
                    if let Some(on) = self.wifi_on() {
                        info!(on = !on, "control center: Wi-Fi");
                        self.wifi_pending = Some((!on, self.now));
                        self.services.send(Command::SetWifi(!on));
                    }
                }
                Toggle::Bluetooth => {
                    if let Some(on) = self.bt_on() {
                        info!(on = !on, "control center: Bluetooth");
                        self.bt_pending = Some((!on, self.now));
                        self.services.send(Command::SetBluetooth(!on));
                    }
                }
                Toggle::Rotation => {
                    self.rotation_locked = !self.rotation_locked;
                    info!(locked = self.rotation_locked, "control center: rotation lock");
                    self.sync_rotation();
                }
            },
            Target::Power(i) => {
                let which = POWER[i];
                let armed = self.confirm.is_some_and(|(j, until)| j == i && self.now < until);
                if which.needs_confirm() && !armed {
                    self.confirm = Some((i, self.now + Duration::from_millis(self.theme.control.confirm_ms as u64)));
                    self.apply();
                    return;
                }
                info!(action = which.label(), "control center: power");
                if which == Power::Lock {
                    // Locked here directly (logind's Lock signal is for other callers).
                    self.requests.push(Request::Lock(crate::overlay::LockReason::User));
                    self.open(false);
                    return;
                }
                self.services.send(match which {
                    Power::Lock => Command::Lock,
                    Power::Sleep => Command::Suspend,
                    Power::Restart => Command::Reboot,
                    Power::Off => Command::PowerOff,
                });
                self.open(false);
            }
        }
        self.apply();
    }

    /// A hardware keyboard came or went (from the core).
    pub fn set_hw_keyboard(&mut self, attached: bool) {
        self.hw_keyboard = attached;
        self.sync_rotation();
        self.apply();
    }

    /// Auto-rotate: claim the accelerometer while it can act (unlocked, no keyboard), and ask the core for
    /// each new orientation it reports. Locked keeps whatever orientation the screen has.
    fn sync_rotation(&mut self) {
        let want_claim = self.sys.orientation.is_some() && !self.rotation_locked && !self.hw_keyboard;
        if want_claim != self.accel_claimed {
            self.accel_claimed = want_claim;
            self.services.send(Command::Accelerometer(want_claim));
        }
        if self.hw_keyboard {
            // The core keeps it normal anyway; forget the last one so the next reading after the keyboard
            // is removed counts as new.
            self.sent_orientation = Some(Orientation::Normal);
            return;
        }
        if !want_claim {
            return;
        }
        if let Some(o) = self.sys.orientation.as_deref().and_then(Orientation::parse)
            && self.sent_orientation != Some(o)
        {
            info!(orientation = ?o, "control center: rotate");
            self.sent_orientation = Some(o);
            self.requests.push(Request::Rotate(o));
        }
    }

    fn show_page(&mut self, page: Page) {
        if page == self.page {
            return;
        }
        self.select(None);
        self.page = page;
        self.nets_key.clear();
        self.history_dirty = true;
        if page == Page::Networks {
            info!("control center: networks");
            self.services.send(Command::WifiScan);
            self.next_scan = Some(self.now + SCAN_EVERY);
        } else {
            self.next_scan = None;
        }
    }

    /// Expand a network (or none); any password typed so far is wiped.
    fn select(&mut self, ssid: Option<String>) {
        wipe(&mut self.pw);
        self.sel = ssid;
    }

    /// The network shown in row `i` of the Networks page.
    fn shown_network(&self, i: usize) -> Option<Network> {
        let w = self.sys.wifi.as_ref()?;
        w.networks.iter().take(self.theme.control.networks_max).nth(i).cloned()
    }

    /// The Connect / Disconnect button of row `i` (or Return in the password box).
    fn connect(&mut self, i: usize) {
        let Some(n) = self.shown_network(i) else { return };
        let link = self.sys.wifi.as_ref().map(|w| w.link);
        if n.active && link == Some(Link::Connected) {
            info!(ssid = %n.ssid, "control center: disconnect");
            self.services.send(Command::WifiDisconnect);
            return;
        }
        if n.security == Security::Unsupported {
            return;
        }
        let psk = if needs_password(&n) {
            if self.pw.len() < PSK_MIN {
                return;
            }
            // Moves the buffer: no copy of the password stays here.
            Some(Secret::new(std::mem::take(&mut self.pw)))
        } else {
            None
        };
        info!(ssid = %n.ssid, new = psk.is_some(), "control center: connect");
        self.services.send(Command::WifiConnect { ssid: n.ssid, psk });
    }

    /// Typing into the password box of the expanded network.
    fn type_password(&mut self, key: &KeyEvent) -> bool {
        let Some(i) = self.net_rows.iter().position(|r| r.field.is_some()) else { return false };
        match key.sym {
            k::KEY_BackSpace => {
                if let Some(c) = self.pw.pop() {
                    // pop() leaves the character's bytes in the spare capacity: zero them.
                    // SAFETY: writes only initialised zeros into spare capacity, length unchanged.
                    unsafe { self.pw.as_mut_vec() }.spare_capacity_mut()[..c.len_utf8()].fill(std::mem::MaybeUninit::new(0));
                }
            }
            k::KEY_Return | k::KEY_KP_Enter => self.connect(i),
            _ => match key.text.as_deref() {
                Some(t) if !key.ctrl && !key.alt => {
                    if self.pw.capacity() < PSK_MAX_BYTES + 4 {
                        // Room for the longest passphrase up front, so the buffer is never reallocated (a
                        // reallocation would leave a copy behind that `wipe` cannot reach).
                        self.pw.reserve_exact(PSK_MAX_BYTES + 4);
                    }
                    if self.pw.len() + t.len() <= PSK_MAX_BYTES {
                        self.pw.push_str(t);
                    }
                }
                _ => return false,
            },
        }
        true
    }

    /// Drop optimistic states the service has confirmed or that timed out.
    fn settle_pending(&mut self) {
        let now = self.now;
        let expired = |since: Duration| now.saturating_sub(since) > PENDING;
        if let Some((on, since)) = self.wifi_pending
            && (self.sys.wifi.as_ref().is_some_and(|w| w.enabled == on && (!on || w.link != Link::Disconnected)) || expired(since))
        {
            self.wifi_pending = None;
        }
        if let Some((on, since)) = self.bt_pending
            && (self.sys.bluetooth.as_ref().is_some_and(|b| b.powered == on) || expired(since))
        {
            self.bt_pending = None;
        }
        if let Some((path, on, since)) = self.bt_dev_pending.as_ref() {
            let b = self.sys.bluetooth.as_ref();
            let done = b.is_some_and(|b| b.failed.as_deref() == Some(path.as_str()) || b.devices.iter().any(|d| d.path == *path && d.connected == *on));
            if done || now.saturating_sub(*since) > BT_PENDING {
                self.bt_dev_pending = None;
            }
        }
        let dragging = self.touch.as_ref().is_some_and(|t| t.mode == Mode::Slider(Slider::Bright));
        if let Some((b, since)) = self.bright
            && !dragging
            && (self.sys.brightness.is_some_and(|s| (s - b).abs() < 0.01) || now.saturating_sub(since) > Duration::from_secs(1))
        {
            self.bright = None;
        }
        // Volume: the audio thread reports two decimals, and takes ~70 ms per call (more while dragging).
        let dragging = self.touch.as_ref().is_some_and(|t| matches!(t.mode, Mode::Slider(Slider::Volume | Slider::Popup)));
        let real = self.sys.volume;
        if let Some((l, since)) = self.vol
            && !dragging
            && (real.is_some_and(|r| (r.level - l).abs() < 0.006) || now.saturating_sub(since) > Duration::from_millis(1500))
        {
            self.vol = None;
        }
        if let Some((m, since)) = self.mute_pending
            && (real.is_some_and(|r| r.muted == m) || now.saturating_sub(since) > Duration::from_millis(1500))
        {
            self.mute_pending = None;
        }
    }
}

impl Scene for ControlCenter {
    fn tree(&mut self) -> &mut Tree {
        &mut self.tree
    }

    fn update(&mut self, now: Duration, dt: f32) -> bool {
        self.now = now;
        if self.next_minute.is_none_or(|t| now >= t) {
            self.update_clock();
        }
        if self.confirm.is_some_and(|(_, until)| now >= until) {
            self.confirm = None;
        }
        if let Some(at) = self.next_scan
            && now >= at
            && self.p.target > 0.0
        {
            self.services.send(Command::WifiScan);
            self.next_scan = Some(now + SCAN_EVERY);
        }
        self.settle_pending();
        let moving = if self.edge_held || matches!(self.touch.as_ref().map(|t| t.mode), Some(Mode::Slide { .. })) {
            false
        } else {
            self.p.step(dt)
        };
        let moving = self.step_banners(dt) || moving;
        let moving = self.step_popup(dt) || moving;
        let moving = self.step_flash(dt) || moving;
        if !self.panel_shown() {
            // Closed: forget transient state.
            self.confirm = None;
        }
        self.apply();
        moving
    }

    fn input(&mut self, ev: PointerEvent) -> bool {
        match ev.phase {
            Phase::Down => {
                if self.touch.is_some() {
                    return true; // one finger at a time
                }
                let target = self.target_at(ev.x, ev.y);
                if !self.takes_input() && !floating(target) {
                    return false; // only banners / the volume popup are up: everything else goes on to Home / apps
                }
                let mut vel = VelocityTracker::default();
                vel.add(ev.time, ev.x, ev.y);
                let mode = if let Target::Slider(s) = target { Mode::Slider(s) } else { Mode::Pending };
                self.touch = Some(Touch { id: ev.id, start: (ev.x, ev.y), target, mode, vel });
                if let Target::Slider(s) = target {
                    self.slide_to(s, ev.x, ev.y);
                    self.apply();
                }
                true
            }
            Phase::Motion => {
                let travel = self.travel();
                let p_now = self.p.value;
                let Some(t) = self.touch.as_mut().filter(|t| t.id == ev.id) else { return false };
                t.vel.add(ev.time, ev.x, ev.y);
                let (dx, dy) = (ev.x - t.start.0, ev.y - t.start.1);
                if t.mode == Mode::Pending && dx.abs().max(dy.abs()) > SLOP {
                    // A banner follows the finger; dragging right (anywhere else) pushes the panel out; other
                    // drags do nothing.
                    t.mode = if matches!(t.target, Target::Banner(_)) {
                        Mode::Banner
                    } else if floating(t.target) {
                        Mode::Done
                    } else if dx > 0.0 && dx.abs() > dy.abs() {
                        Mode::Slide { p0: p_now }
                    } else {
                        Mode::Done
                    };
                }
                if let (Mode::Banner, Target::Banner(i)) = (t.mode, t.target) {
                    if let Some(b) = self.banners.get_mut(i) {
                        b.drag = (dx, dy);
                    }
                    self.apply();
                    return true;
                }
                match t.mode {
                    Mode::Slide { p0 } => {
                        let (vx, _) = t.vel.velocity();
                        let p = (p0 - (ev.x - t.start.0) / travel).clamp(0.0, 1.0);
                        self.p.set(p, -vx / travel);
                        self.apply();
                    }
                    Mode::Slider(s) => {
                        self.slide_to(s, ev.x, ev.y);
                        self.apply();
                    }
                    _ => {}
                }
                true
            }
            Phase::Up | Phase::Cancel => {
                let all = ev.phase == Phase::Cancel && ev.id == CANCEL_ALL;
                let Some(t) = self.touch.take_if(|t| all || t.id == ev.id) else { return false };
                let cancel = ev.phase == Phase::Cancel;
                match t.mode {
                    Mode::Pending if !cancel => self.tap(t.target),
                    Mode::Slide { .. } => {
                        let (vx, _) = t.vel.velocity();
                        let tk = &self.theme.control;
                        let close = !cancel && (vx > tk.open_flick || (vx > -tk.open_flick && self.p.value < 1.0 - tk.open_commit));
                        self.open(!close);
                    }
                    Mode::Slider(Slider::Bright) => {
                        self.bright = self.bright.map(|(b, _)| (b, self.now));
                    }
                    Mode::Slider(s) => {
                        self.vol = self.vol.map(|(l, _)| (l, self.now));
                        if s == Slider::Popup {
                            self.show_popup();
                        }
                    }
                    Mode::Banner => {
                        if let Target::Banner(i) = t.target {
                            let (dx, dy) = self.banners.get(i).map(|b| b.drag).unwrap_or_default();
                            let (vx, vy) = t.vel.velocity();
                            let away = if self.theme.banner.slide_from == "top" {
                                dy < -40.0 || vy < -self.theme.control.open_flick
                            } else {
                                dx > 80.0 || vx > self.theme.control.open_flick || dy < -40.0
                            };
                            if let Some(b) = self.banners.get_mut(i) {
                                b.drag = (0.0, 0.0);
                                if away && !cancel {
                                    // Continue from where the finger left it.
                                    let span = if self.theme.banner.slide_from == "top" { 80.0 } else { self.theme.banner.width };
                                    b.p.value = (1.0 - dx.max(-dy).max(0.0) / span).clamp(0.0, 1.0);
                                }
                            }
                            if away && !cancel {
                                self.dismiss(i, true);
                            }
                        }
                    }
                    _ => {}
                }
                self.apply();
                true
            }
        }
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        if key.sym == k::KEY_Escape {
            // Collapse the open network, then leave the Networks page, then close.
            if self.sel.is_some() {
                self.select(None);
            } else if self.page != Page::Main {
                self.show_page(Page::Main);
            } else {
                self.open(false);
            }
            self.apply();
            return true;
        }
        if self.page == Page::Networks && self.type_password(key) {
            self.apply();
            return true;
        }
        false
    }

    fn next_wakeup(&self) -> Option<Duration> {
        if !self.active() {
            return None;
        }
        let mut t = self.next_minute;
        let mut add = |at: Duration| t = Some(t.map_or(at, |x: Duration| x.min(at)));
        if let Some((_, until)) = self.confirm {
            add(until);
        }
        for since in [self.wifi_pending.map(|p| p.1), self.bt_pending.map(|p| p.1)].into_iter().flatten() {
            add(since + PENDING);
        }
        if let Some((_, _, since)) = self.bt_dev_pending {
            add(since + BT_PENDING);
        }
        if let Some(at) = self.next_scan {
            add(at);
        }
        if self.pop_shown {
            add(self.pop_until.unwrap_or(self.now));
        }
        for (_, since) in [self.vol.map(|v| ((), v.1)), self.mute_pending.map(|m| ((), m.1))].into_iter().flatten() {
            add(since + Duration::from_millis(1500));
        }
        for b in self.banners.iter().filter(|b| !b.closing) {
            // Not yet timed (first frame pending): soon.
            add(b.until.unwrap_or(self.now));
        }
        t
    }

    fn set_size(&mut self, w: f32, h: f32) {
        self.w = w;
        self.h = h;
        self.apply();
    }

    /// New services snapshot: true if the panel shows it now (needs a frame).
    fn poll(&mut self) -> bool {
        let notes = self.services.take_notes();
        let got_notes = !notes.is_empty();
        for ev in notes {
            self.note_event(ev);
        }
        if got_notes {
            self.apply();
        }
        let g = self.services.generation();
        if g == self.generation {
            return got_notes;
        }
        let (g, sys) = self.services.snapshot();
        self.generation = g;
        self.sys = sys;
        self.settle_pending();
        self.sync_rotation();
        self.apply();
        self.active()
    }

    fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// The Wi-Fi password box is open.
    fn text_field(&self) -> bool {
        self.page == Page::Networks && self.net_rows.iter().any(|r| r.field.is_some())
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        self.p.params = self.springs;
        let (tree, n) = Self::build(theme);
        self.tree = tree;
        self.n = n;
        self.touch = None;
        self.update_clock();
        self.apply();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration_text(45 * 60), "45 min");
        assert_eq!(duration_text(3 * 3600), "3 h");
        assert_eq!(duration_text(3 * 3600 + 20 * 60 + 10), "3 h 20 min");
        assert_eq!(ellipsize("abcdef", 4), "abc…");
        assert_eq!(ellipsize("abc", 4), "abc");
    }

    #[test]
    fn volume_steps() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(near(step_level(0.10, 0.04, true), 0.12));
        assert!(near(step_level(0.10, 0.04, false), 0.08));
        assert!(near(step_level(0.12, 0.04, true), 0.16));
        assert!(near(step_level(0.12, 0.04, false), 0.08));
        assert!(near(step_level(0.0, 0.04, false), 0.0));
        assert!(near(step_level(0.99, 0.04, true), 1.0));
        assert!(near(step_level(1.0, 0.04, true), 1.0));
        assert_eq!(volume_icon(Volume { level: 0.5, muted: true }), "audio-volume-muted");
        assert_eq!(volume_icon(Volume { level: 0.2, muted: false }), "audio-volume-low");
    }
}
