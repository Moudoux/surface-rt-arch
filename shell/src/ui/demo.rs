//! `--demo`: a test scene with every render-kit primitive (DESKTOP-SHELL.md Phase 2): the wallpaper, a large
//! clock, a grid of cards (solid and glass, with shadows, icons and labels), a dim layer and a glass side
//! panel that slides in on a spring, plus a live readout of frame costs.
//!
//! Touch (or the mouse when nested): drag a card, it follows 1:1 and springs back with the release
//! velocity; tap empty space to open/close the panel; drag the panel sideways to move it. `--demo-auto`
//! additionally cycles the panel and bounces the cards on its own (for fps measurements); without it the
//! scene is idle until touched (no frames, except the clock's minute tick).

use std::time::Duration;

use super::{Align, Layout, Node, NodeId, Paint, Shadow, Size, Tree, shell::{FrameCost, Scene}};
use crate::{
    anim::{Spring, SpringParams, VelocityTracker},
    overlay::{Phase, PointerEvent},
    render::{gl::Texture, text::TextStyle},
    theme::Theme,
};

const CARDS: [(&str, &str); 8] = [
    ("Terminal", "utilities-terminal"),
    ("Browser", "internet-web-browser"),
    ("Files", "system-file-manager"),
    ("Editor", "accessories-text-editor"),
    ("Settings", "preferences-system"),
    ("Music", "elisa"),
    ("Photos", "org.kde.gwenview"),
    ("Mail", "internet-mail"),
];

struct Card {
    node: NodeId,
    x: Spring,
    y: Spring,
    scale: Spring,
}

struct Drag {
    id: i32,
    target: Target,
    start: (f32, f32),
    origin: (f32, f32),
    moved: bool,
    vel: VelocityTracker,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Card(usize),
    Panel,
    Background,
}

pub struct Demo {
    tree: Tree,
    /// Panel width (theme `[demo]`), for its slide distance and drag.
    panel_w: f32,
    clock: NodeId,
    date: NodeId,
    stats: NodeId,
    dim: NodeId,
    panel: NodeId,
    /// Rounded image (the wallpaper as a thumbnail): the textured rounded-rect primitive.
    thumb: NodeId,
    cards: Vec<Card>,
    progress: Spring,
    drag: Option<Drag>,
    auto: bool,
    /// Auto mode: next time something is kicked.
    next_auto: Duration,
    auto_step: u32,
    clock_minute: i64,
    next_minute: Option<Duration>,
    cost: FrameCost,
    stats_at: Duration,
    frames: u32,
    background: [f32; 4],
    now: Duration,
}

fn local_time() -> (String, String, i64, u64) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
        (
            format!("{:02}:{:02}", tm.tm_hour, tm.tm_min),
            format!("{}, {} {}", DAYS[tm.tm_wday as usize % 7], tm.tm_mday, MONTHS[tm.tm_mon as usize % 12]),
            t as i64 / 60,
            60 - tm.tm_sec as u64,
        )
    }
}

impl Demo {
    pub fn new(theme: &Theme, auto: bool) -> Demo {
        let c = &theme.colors;
        let f = &theme.font;
        let sh = &theme.shadow;
        let springs = SpringParams { stiffness: theme.motion.spring_stiffness, damping: theme.motion.spring_damping };
        let unit = theme.spacing.unit as f32;
        let (card_w, card_h, panel_w) = (theme.demo.card_width, theme.demo.card_height, theme.demo.panel_width);
        let card_shadow = Shadow { sigma: sh.sigma, dx: 0.0, dy: sh.offset_y, color: sh.color.premultiplied() };
        let small_shadow = Shadow { sigma: sh.small_sigma, dx: 0.0, dy: sh.small_offset_y, color: sh.color.premultiplied() };
        let text = |s: &str, size: f32, weight: u16, color: [f32; 4]| Paint::Text {
            text: s.into(),
            style: TextStyle { size, weight, ..TextStyle::default() },
            color,
            wrap: false,
            layout: None,
        };

        let background = c.background.premultiplied();
        let mut t = Tree::new(Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(Paint::Rect { color: background, radius: 0.0 }));
        let root = t.root;

        // Clock and date, top left.
        let clock_col = t.add(root, Node::new().layout(Layout::Column { gap: 0.0 }).offset(6.0 * unit, 4.0 * unit));
        let clock = t.add(clock_col, Node::new().paint(text("00:00", f.size_display, f.weight_display, c.text.premultiplied())));
        let date = t.add(clock_col, Node::new().paint(text("", f.size_title, f.weight_body, c.text_dim.premultiplied())));

        // Frame-cost readout, top right (mono).
        let stats = t.add(
            root,
            Node::new()
                .align(Align::End, Align::Start)
                .offset(-3.0 * unit, 3.0 * unit)
                .paint(Paint::Text {
                    text: "rtshell render kit demo".into(),
                    style: TextStyle { size: f.size_caption, weight: f.weight_body, mono: true, ..TextStyle::default() },
                    color: c.text_dim.premultiplied(),
                    wrap: false,
                    layout: None,
                }),
        );

        // Cards: a 4x2 grid, bottom centre. Odd cards are glass, even ones solid.
        let grid_w = 4.0 * card_w + 3.0 * 3.0 * unit;
        let grid = t.add(
            root,
            Node::new()
                .size(Size::Fixed(grid_w), Size::Fit)
                .layout(Layout::Grid { columns: 4, gap: 3.0 * unit })
                .align(Align::Center, Align::End)
                .offset(0.0, -6.0 * unit),
        );
        let radius = theme.shape.radius_card as f32;
        let mut cards = Vec::new();
        for (i, (label, icon)) in CARDS.iter().enumerate() {
            let paint = if i % 2 == 1 {
                Paint::Glass { radius, tint: theme.glass.tint.premultiplied() }
            } else {
                Paint::Rect { color: c.surface.premultiplied(), radius }
            };
            let card = t.add(
                grid,
                Node::new()
                    .size(Size::Fixed(card_w), Size::Fixed(card_h))
                    .layout(Layout::Column { gap: unit })
                    .padding(2.0 * unit)
                    .paint(paint)
                    .shadow(card_shadow)
                    .hit(),
            );
            t.add(card, Node::new().size(Size::Fill(1.0), Size::Fixed(72.0)).paint(Paint::Icon { name: icon.to_string(), size: 64 }));
            t.add(card, Node::new().align(Align::Center, Align::Start).paint(text(label, f.size_body, f.weight_body, c.text.premultiplied())));
            cards.push(Card {
                node: card,
                x: Spring::new(0.0, springs).with_eps(0.2),
                y: Spring::new(0.0, springs).with_eps(0.2),
                scale: Spring::new(1.0, springs).with_eps(0.0005),
            });
        }

        // Dim layer, then the glass panel on the right (slides in from off-screen).
        let mut dim_node = Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).paint(Paint::Scrim { color: c.scrim.premultiplied() });
        dim_node.opacity = 0.0;
        let dim = t.add(root, dim_node);
        let lane = t.add(root, Node::new().size(Size::Fill(1.0), Size::Fill(1.0)).padding(2.0 * unit));
        let panel = t.add(
            lane,
            Node::new()
                .size(Size::Fixed(panel_w), Size::Fill(1.0))
                .align(Align::End, Align::Start)
                .layout(Layout::Column { gap: 1.5 * unit })
                .padding(3.0 * unit)
                .paint(Paint::Glass { radius: theme.shape.radius_panel as f32, tint: theme.glass.tint.premultiplied() })
                .shadow(card_shadow)
                .hit(),
        );
        t.add(panel, Node::new().paint(text("Quick settings", f.size_title, f.weight_title, c.text.premultiplied())));
        let rows = [("Wi-Fi", "network-wireless"), ("Bluetooth", "preferences-system-bluetooth"), ("Rotation lock", "rotation-locked-portrait"), ("Brightness", "brightness-high")];
        for (i, (label, icon)) in rows.iter().enumerate() {
            let on = i % 2 == 0;
            let row = t.add(
                panel,
                Node::new()
                    .size(Size::Fill(1.0), Size::Fixed(theme.spacing.touch_target as f32 + unit))
                    .layout(Layout::Row { gap: 1.5 * unit })
                    .padding(1.5 * unit)
                    .paint(Paint::Rect {
                        color: if on { c.accent.premultiplied() } else { c.surface.premultiplied() },
                        radius: theme.shape.radius_small as f32 * 1.5,
                    })
                    .shadow(small_shadow),
            );
            t.add(row, Node::new().align(Align::Start, Align::Center).paint(Paint::Icon { name: icon.to_string(), size: 24 }));
            let fg = if on { c.on_accent } else { c.text };
            t.add(row, Node::new().align(Align::Start, Align::Center).paint(text(label, f.size_body, f.weight_body, fg.premultiplied())));
        }
        // A rounded image with a caption.
        let media = t.add(panel, Node::new().layout(Layout::Row { gap: 1.5 * unit }).size(Size::Fill(1.0), Size::Fit));
        let thumb = t.add(
            media,
            Node::new()
                .size(Size::Fixed(128.0), Size::Fixed(72.0))
                .paint(Paint::Rect { color: c.surface.premultiplied(), radius: theme.shape.radius_small as f32 * 1.5 })
                .shadow(small_shadow),
        );
        t.add(media, Node::new().align(Align::Start, Align::Center).paint(text("Wallpaper", f.size_body, f.weight_body, c.text.premultiplied())));
        // A slider: track + filled part.
        let track = t.add(
            panel,
            Node::new()
                .size(Size::Fill(1.0), Size::Fixed(unit))
                .layout(Layout::Row { gap: 0.0 })
                .paint(Paint::Rect { color: c.text_dim.premultiplied().map(|v| v * 0.35), radius: unit * 0.5 }),
        );
        t.add(track, Node::new().size(Size::Fixed(panel_w * 0.45), Size::Fill(1.0)).paint(Paint::Rect { color: c.accent.premultiplied(), radius: unit * 0.5 }));
        t.add(
            panel,
            Node::new().paint(Paint::Text {
                text: "Glass: the wallpaper blurred once on the GPU, sampled at screen position. Drag the cards, tap to toggle this panel.".into(),
                style: TextStyle { size: f.size_caption, weight: f.weight_body, ..TextStyle::default() },
                color: c.text_dim.premultiplied(),
                wrap: true,
                layout: None,
            })
            .size(Size::Fill(1.0), Size::Fit),
        );

        let progress = Spring::new(0.0, springs).with_eps(0.0005);
        let mut d = Demo {
            tree: t,
            panel_w,
            clock,
            date,
            stats,
            dim,
            panel,
            thumb,
            cards,
            progress,
            drag: None,
            auto,
            next_auto: Duration::ZERO,
            auto_step: 0,
            clock_minute: -1,
            next_minute: None,
            cost: FrameCost::default(),
            stats_at: Duration::ZERO,
            frames: 0,
            background,
            now: Duration::ZERO,
        };
        d.apply();
        d
    }

    /// Copy spring values into the tree.
    fn apply(&mut self) {
        let p = self.progress.value;
        self.tree[self.panel].translate = ((1.0 - p) * (self.panel_w + 48.0), 0.0);
        self.tree[self.panel].opacity = (p * 3.0).clamp(0.0, 1.0);
        self.tree[self.dim].opacity = p.clamp(0.0, 1.0);
        for c in &self.cards {
            let n = &mut self.tree[c.node];
            n.translate = (c.x.value, c.y.value);
            n.scale = c.scale.value;
        }
    }

    fn toggle_panel(&mut self) {
        let open = self.progress.target < 0.5;
        self.progress.set_target(if open { 1.0 } else { 0.0 });
    }

    fn update_clock(&mut self, now: Duration) {
        let (hm, date, minute, secs_left) = local_time();
        if minute != self.clock_minute {
            self.clock_minute = minute;
            self.tree.set_text(self.clock, &hm);
            self.tree.set_text(self.date, &date);
        }
        self.next_minute = Some(now + Duration::from_secs(secs_left));
    }

    fn update_stats(&mut self, now: Duration) {
        self.frames += 1;
        let since = now.saturating_sub(self.stats_at);
        if since < Duration::from_millis(500) {
            return;
        }
        let fps = self.frames as f32 / since.as_secs_f32();
        let l = self.cost.layer;
        let s = format!(
            "{fps:4.1} fps   ui {:4.1} ms   {} draws   {} quads   damage {:.0}k px",
            self.cost.build_ms,
            l.draws,
            l.quads,
            l.damaged_px as f32 / 1000.0
        );
        self.tree.set_text(self.stats, &s);
        self.stats_at = now;
        self.frames = 0;
    }

    fn resting(&self) -> bool {
        self.progress.is_resting() && self.cards.iter().all(|c| c.x.is_resting() && c.y.is_resting() && c.scale.is_resting())
    }

    /// Auto mode: kick the next motion as soon as the last one settles (continuous animation, for fps).
    fn auto_kick(&mut self, now: Duration) {
        if !self.auto || self.drag.is_some() || !self.resting() {
            return;
        }
        self.auto_step += 1;
        // Alternate: panel in/out, then a wave over the cards (scale bounce + lift).
        if self.auto_step % 2 == 1 {
            self.toggle_panel();
        } else {
            for (i, c) in self.cards.iter_mut().enumerate() {
                c.scale.velocity = 1.2 + 0.15 * (i % 4) as f32;
                c.y.velocity = -500.0 - 60.0 * i as f32;
            }
        }
        self.next_auto = now;
    }
}

impl Scene for Demo {
    fn tree(&mut self) -> &mut Tree {
        &mut self.tree
    }

    fn update(&mut self, now: Duration, dt: f32) -> bool {
        self.now = now;
        if self.next_minute.is_none_or(|t| now >= t) {
            self.update_clock(now);
        }
        self.auto_kick(now);
        let mut moving = self.progress.step(dt);
        for c in &mut self.cards {
            moving |= c.x.step(dt);
            moving |= c.y.step(dt);
            moving |= c.scale.step(dt);
        }
        self.apply();
        if moving || self.auto {
            self.update_stats(now);
        } else {
            self.frames = 0;
            self.stats_at = now;
        }
        moving || self.drag.is_some() || self.auto
    }

    fn input(&mut self, ev: PointerEvent) -> bool {
        match ev.phase {
            Phase::Down => {
                if self.drag.is_some() {
                    return true; // one finger at a time
                }
                let hit = self.tree.hit_test(ev.x, ev.y);
                let target = match hit {
                    Some(n) if n == self.panel => Target::Panel,
                    Some(n) => self.cards.iter().position(|c| c.node == n).map(Target::Card).unwrap_or(Target::Background),
                    None => Target::Background,
                };
                let origin = match target {
                    Target::Card(i) => {
                        let c = &mut self.cards[i];
                        c.scale.set_target(1.06);
                        (c.x.value, c.y.value)
                    }
                    Target::Panel => (self.progress.value, 0.0),
                    Target::Background => (0.0, 0.0),
                };
                let mut vel = VelocityTracker::default();
                vel.add(ev.time, ev.x, ev.y);
                self.drag = Some(Drag { id: ev.id, target, start: (ev.x, ev.y), origin, moved: false, vel });
                true
            }
            Phase::Motion => {
                let Some(d) = self.drag.as_mut().filter(|d| d.id == ev.id) else { return false };
                d.vel.add(ev.time, ev.x, ev.y);
                let (dx, dy) = (ev.x - d.start.0, ev.y - d.start.1);
                if dx.abs() + dy.abs() > 8.0 {
                    d.moved = true;
                }
                match d.target {
                    Target::Card(i) => {
                        let (vx, vy) = d.vel.velocity();
                        let c = &mut self.cards[i];
                        c.x.set(d.origin.0 + dx, vx);
                        c.y.set(d.origin.1 + dy, vy);
                    }
                    Target::Panel => {
                        let p = (d.origin.0 - dx / self.panel_w).clamp(0.0, 1.0);
                        let v = -d.vel.velocity().0 / self.panel_w;
                        self.progress.set(p, v);
                    }
                    Target::Background => {}
                }
                true
            }
            Phase::Up | Phase::Cancel => {
                let all = ev.phase == Phase::Cancel && ev.id == crate::overlay::CANCEL_ALL;
                let Some(d) = self.drag.take_if(|d| all || d.id == ev.id) else { return false };
                match d.target {
                    Target::Card(i) => {
                        let c = &mut self.cards[i];
                        c.x.set_target(0.0);
                        c.y.set_target(0.0);
                        c.scale.set_target(1.0);
                    }
                    Target::Panel if d.moved => {
                        // Settle by where it would coast to.
                        let coast = self.progress.value + self.progress.velocity * 0.15;
                        self.progress.set_target(if coast > 0.5 { 1.0 } else { 0.0 });
                    }
                    Target::Panel => {}
                    Target::Background => {
                        if !d.moved && ev.phase == Phase::Up {
                            self.toggle_panel();
                        }
                    }
                }
                true
            }
        }
    }

    fn next_wakeup(&self) -> Option<Duration> {
        self.next_minute
    }

    fn set_wallpaper(&mut self, tex: Option<Texture>) {
        let root = self.tree.root;
        self.tree[root].paint = match tex {
            Some(tex) => Paint::Backdrop { tex },
            None => Paint::Rect { color: self.background, radius: 0.0 },
        };
        if let Some(tex) = tex {
            let radius = match self.tree[self.thumb].paint {
                Paint::Rect { radius, .. } | Paint::Image { radius, .. } => radius,
                _ => 0.0,
            };
            self.tree[self.thumb].paint = Paint::Image { tex, radius };
        }
    }

    fn set_cost(&mut self, cost: FrameCost) {
        self.cost = cost;
    }

    fn opaque(&self) -> bool {
        true
    }
}
