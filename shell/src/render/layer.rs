//! The shell UI as one Smithay render element (`UiElement`) drawn with the render kit.
//!
//! `UiLayer` persists across frames: it diffs each new draw list against the previous one to report only
//! the changed area as damage (so a ticking clock or a moving card redraws that region, not the screen),
//! and keeps a short damage history for Smithay's buffer-age based tracking.

use std::{collections::VecDeque, rc::Rc};

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesError, GlesFrame, GlesRenderer},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer as BufferCoords, Physical, Rectangle, Scale, Size},
};

use super::{
    batch::{self, Batch, DrawList, Quad, Rect, Vertex},
    kit::Kit,
};

const HISTORY: usize = 8;

/// RTSHELL_UI_TIMING=1: bracket the UI draw with glFinish and accumulate its GPU time here (microseconds,
/// draws). Costs a pipeline stall per frame, so only for measurements.
pub static GPU_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static GPU_DRAWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RTSHELL_UI_TIMING").is_some())
}

pub struct UiLayer {
    id: Id,
    commit: CommitCounter,
    prev: Vec<Quad>,
    /// (commit after the change, damaged rect) for the last few changes.
    history: VecDeque<(CommitCounter, Rectangle<i32, Physical>)>,
    size: Size<i32, Physical>,
    pub stats: LayerStats,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct LayerStats {
    pub quads: usize,
    pub draws: usize,
    pub damaged_px: i64,
}

fn to_physical(r: Rect) -> Rectangle<i32, Physical> {
    Rectangle::new((r.x as i32, r.y as i32).into(), (r.w as i32, r.h as i32).into())
}

impl UiLayer {
    pub fn new() -> UiLayer {
        UiLayer {
            id: Id::new(),
            commit: CommitCounter::default(),
            prev: Vec::new(),
            history: VecDeque::new(),
            size: Size::from((0, 0)),
            stats: LayerStats::default(),
        }
    }

    /// Turn this frame's draw list into an element. `full`: textures the list samples changed (atlas
    /// rebuilt, wallpaper reloaded), so everything is damaged. `opaque`: regions the scene covers opaquely.
    pub fn element(&mut self, kit: &Rc<Kit>, list: &DrawList, size: Size<i32, Physical>, full: bool, opaque: &[Rect]) -> UiElement {
        let screen = Rectangle::from_size(size);
        let damage = if full || size != self.size {
            Some(screen)
        } else {
            batch::diff_damage(&self.prev, &list.quads).and_then(|r| to_physical(r).intersection(screen))
        };
        if let Some(d) = damage {
            self.commit.increment();
            self.history.push_front((self.commit, d));
            self.history.truncate(HISTORY);
            self.stats.damaged_px = d.size.w as i64 * d.size.h as i64;
        } else {
            self.stats.damaged_px = 0;
        }
        self.size = size;
        self.prev.clear();
        self.prev.extend_from_slice(&list.quads);

        let mut vertices = Vec::new();
        let batches = batch::batches(&list.quads, &mut vertices);
        self.stats.quads = list.quads.len();
        self.stats.draws = batches.len();
        if std::env::var_os("RTSHELL_UI_DEBUG").is_some() {
            tracing::info!(?batches, "ui batches");
        }
        UiElement {
            id: self.id.clone(),
            commit: self.commit,
            history: self.history.iter().copied().collect(),
            size,
            opaque: opaque.iter().map(|r| to_physical(*r)).collect(),
            frame: Rc::new(UiFrame { kit: kit.clone(), vertices, batches }),
        }
    }
}

struct UiFrame {
    kit: Rc<Kit>,
    vertices: Vec<Vertex>,
    batches: Vec<Batch>,
}

#[derive(Clone)]
pub struct UiElement {
    id: Id,
    commit: CommitCounter,
    history: Vec<(CommitCounter, Rectangle<i32, Physical>)>,
    size: Size<i32, Physical>,
    opaque: Vec<Rectangle<i32, Physical>>,
    frame: Rc<UiFrame>,
}

impl std::fmt::Debug for UiElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiElement")
            .field("commit", &self.commit)
            .field("batches", &self.frame.batches.len())
            .finish()
    }
}

impl Element for UiElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        Rectangle::from_size((self.size.w as f64, self.size.h as f64).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::from_size(self.size)
    }

    fn damage_since(&self, _scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        let full = || DamageSet::from_slice(&[Rectangle::from_size(self.size)]);
        let Some(since) = commit else { return full() };
        match self.commit.distance(Some(since)) {
            Some(0) => DamageSet::default(),
            Some(n) if n <= self.history.len() => self.history[..n].iter().map(|(_, r)| *r).collect(),
            _ => full(),
        }
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.opaque.iter().copied().collect()
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for UiElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        let proj = *frame.projection();
        let instancing = frame.capabilities().contains(&smithay::backend::renderer::gles::Capability::Instancing);
        let f = &self.frame;
        frame.with_context(|gl| unsafe {
            // Damage is relative to dst; map each rect through the projection to framebuffer pixels.
            let vp = super::gl::viewport(gl);
            let to_win = |x: f32, y: f32| {
                let cx = proj[0] * x + proj[3] * y + proj[6];
                let cy = proj[1] * x + proj[4] * y + proj[7];
                ((cx + 1.0) * 0.5 * vp[2] as f32 + vp[0] as f32, (cy + 1.0) * 0.5 * vp[3] as f32 + vp[1] as f32)
            };
            let scissors: Vec<[i32; 4]> = damage
                .iter()
                .map(|d| {
                    let x0 = (dst.loc.x + d.loc.x) as f32;
                    let y0 = (dst.loc.y + d.loc.y) as f32;
                    let a = to_win(x0, y0);
                    let b = to_win(x0 + d.size.w as f32, y0 + d.size.h as f32);
                    let (l, r) = (a.0.min(b.0).round() as i32, a.0.max(b.0).round() as i32);
                    let (t, u) = (a.1.min(b.1).round() as i32, a.1.max(b.1).round() as i32);
                    [l, t, r - l, u - t]
                })
                .collect();
            let t0 = timing().then(|| {
                gl.Finish();
                std::time::Instant::now()
            });
            f.kit.draw(gl, &proj, &f.vertices, &f.batches, &scissors, instancing);
            if let Some(t0) = t0 {
                gl.Finish();
                use std::sync::atomic::Ordering::Relaxed;
                GPU_US.fetch_add(t0.elapsed().as_micros() as u64, Relaxed);
                GPU_DRAWS.fetch_add(1, Relaxed);
            }
            // Smithay's frame-wide scissor box (gles/mod.rs: the whole output).
            gl.Scissor(vp[0], vp[1], vp[2], vp[3]);
        })
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
