//! Text: cosmic-text shapes and lays out, swash rasterizes, glyphs live in a shared atlas texture
//! (`SharedAtlas`, which also holds 9-slice shadow images).
//!
//! Subpixel positioning is off: glyph origins are rounded to whole pixels, so every glyph is rasterized
//! once per size and weight and the atlas stays small. Fonts are loaded by family name through `fc-match`
//! (only the faces we use, not all of /usr/share/fonts: the Surface has 700 font files).

use std::{
    collections::HashMap,
    process::{Command, Stdio},
};

use cosmic_text::{Attrs, Buffer, CacheKey, CacheKeyFlags, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight, Wrap};
use smithay::backend::renderer::gles::ffi::Gles2;
use tracing::{info, warn};

use super::{
    atlas::ShelfAtlas,
    batch::{DrawList, Rect, Rgba, scale},
    gl::{self, Format, Texture},
};

pub const ATLAS_SIZE: u32 = 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub weight: u16,
    pub mono: bool,
    /// Line height as a multiple of the size.
    pub line_height: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle { size: 16.0, weight: 400, mono: false, line_height: 1.25 }
    }
}

/// Laid-out text, ready to draw at any origin.
pub struct TextLayout {
    pub w: f32,
    pub h: f32,
    /// Distance from the top to the first baseline (for aligning text of different sizes, Phase 3).
    #[allow(dead_code)]
    pub baseline: f32,
    glyphs: Vec<(CacheKey, i32, i32)>,
    key: (String, TextStyle, Option<u32>),
}

impl TextLayout {
    pub fn is_for(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> bool {
        self.key.0 == text && self.key.1 == *style && self.key.2 == max_width.map(|w| w as u32)
    }
}

pub struct Text {
    fonts: FontSystem,
    swash: SwashCache,
    ui_family: String,
    mono_family: String,
}

/// Font files for `patterns` through `fc-match`, all at once (one process each, ~85 ms apiece on the Surface).
fn fc_match_all(patterns: &[&str]) -> Vec<Option<String>> {
    let children: Vec<_> = patterns.iter().map(|p| Command::new("fc-match").args(["-f", "%{file}", p]).stdout(Stdio::piped()).spawn().ok()).collect();
    children
        .into_iter()
        .map(|c| {
            let out = c?.wait_with_output().ok()?;
            let path = String::from_utf8(out.stdout).ok()?;
            (!path.is_empty()).then_some(path)
        })
        .collect()
}

/// `fc-match` answers from the last start (`~/.cache/rtshell/fonts.tsv`: pattern TAB file), used when every
/// file still exists; a background thread asks fontconfig again a while after startup and rewrites the cache
/// if anything changed (takes effect on the next start).
fn resolve_fonts(patterns: &[&str]) -> Vec<Option<String>> {
    let file = super::preload::cache_dir().map(|d| d.join("fonts.tsv"));
    let cached: HashMap<String, String> = file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|t| t.lines().filter_map(|l| l.split_once('\t')).map(|(a, b)| (a.to_string(), b.to_string())).collect())
        .unwrap_or_default();
    let hit: Option<Vec<Option<String>>> =
        patterns.iter().map(|p| cached.get(*p).filter(|f| std::path::Path::new(f).is_file()).map(|f| Some(f.clone()))).collect();
    let save = move |file: &Option<std::path::PathBuf>, patterns: &[String], found: &[Option<String>]| {
        let Some(f) = file else { return };
        let text: String = patterns.iter().zip(found).filter_map(|(p, f)| Some(format!("{p}\t{}\n", f.as_ref()?))).collect();
        let _ = super::preload::write_atomic(f, text.as_bytes());
    };
    let owned: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
    match hit {
        Some(found) => {
            let before = found.clone();
            let _ = std::thread::Builder::new().name("fc-recheck".into()).spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(20));
                let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
                let now = fc_match_all(&refs);
                if now != before {
                    info!(?now, "fonts changed since the cache was written; used from the next start");
                    save(&file, &owned, &now);
                }
            });
            found
        }
        None => {
            let found = fc_match_all(patterns);
            save(&file, &owned, &found);
            found
        }
    }
}

impl Text {
    pub fn new(ui: &str, mono: &str) -> Text {
        let started = std::time::Instant::now();
        let mut db = cosmic_text::fontdb::Database::new();
        let mut loaded = Vec::new();
        let patterns = [ui, mono, "sans-serif", "monospace", "emoji"];
        for (pattern, file) in patterns.iter().zip(resolve_fonts(&patterns)) {
            match file {
                Some(path) if !loaded.contains(&path) => {
                    if let Err(err) = db.load_font_file(&path) {
                        warn!(%path, %err, "font not loaded");
                    }
                    loaded.push(path);
                }
                Some(_) => {}
                None => warn!(%pattern, "fc-match found no font"),
            }
        }
        if db.is_empty() {
            warn!("no fonts via fc-match; scanning /usr/share/fonts");
            db.load_fonts_dir("/usr/share/fonts");
        }
        // Family names as the files report them (fc-match may have substituted).
        let family_of = |want: &str| {
            db.faces()
                .flat_map(|f| f.families.iter().map(|(n, _)| n.clone()))
                .find(|n| n.eq_ignore_ascii_case(want))
                .unwrap_or_else(|| db.faces().next().map(|f| f.families[0].0.clone()).unwrap_or_default())
        };
        let ui_family = family_of(ui);
        let mono_family = family_of(mono);
        info!(faces = db.len(), files = ?loaded, %ui_family, %mono_family, ms = started.elapsed().as_millis() as u64, "fonts loaded");
        let fonts = FontSystem::new_with_locale_and_db("en-US".into(), db);
        Text { fonts, swash: SwashCache::new(), ui_family, mono_family }
    }

    pub fn layout(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> TextLayout {
        let metrics = Metrics::new(style.size, (style.size * style.line_height).round());
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        let family = if style.mono { &self.mono_family } else { &self.ui_family };
        let attrs = Attrs::new().family(Family::Name(family)).weight(Weight(style.weight));
        {
            let mut b = buffer.borrow_with(&mut self.fonts);
            b.set_wrap(if max_width.is_some() { Wrap::WordOrGlyph } else { Wrap::None });
            b.set_size(max_width, None);
            b.set_text(text, &attrs, Shaping::Advanced, None);
            b.shape_until_scroll(true);
        }
        let mut glyphs = Vec::new();
        let (mut w, mut h, mut baseline) = (0.0f32, 0.0f32, None);
        for run in buffer.layout_runs() {
            w = w.max(run.line_w);
            h = h.max(run.line_top + run.line_height);
            baseline.get_or_insert(run.line_y);
            for g in run.glyphs {
                let x = (g.x + g.font_size * g.x_offset).round();
                let y = (run.line_y - g.font_size * g.y_offset).round();
                let (key, xi, yi) = CacheKey::new(g.font_id, g.glyph_id, g.font_size, (x, y), g.font_weight, CacheKeyFlags::empty());
                glyphs.push((key, xi, yi));
            }
        }
        TextLayout {
            w: w.ceil(),
            h: h.ceil(),
            baseline: baseline.unwrap_or(0.0),
            glyphs,
            key: (text.to_string(), style.clone(), max_width.map(|w| w as u32)),
        }
    }
}

#[derive(Clone, Copy)]
struct Slot {
    rect: Rect,
    left: i32,
    top: i32,
    color: bool,
}

/// One atlas texture for glyphs and shadow slices. When it fills up it is cleared and `generation` bumps;
/// a frame that saw that happen must be rebuilt (`take_overflow`), since its earlier glyphs were dropped.
pub struct SharedAtlas {
    pub tex: Texture,
    shelf: ShelfAtlas,
    glyphs: HashMap<CacheKey, Option<Slot>>,
    /// (radius, sigma*4) -> (atlas rect, corner size k, blur extent)
    shadows: HashMap<(u32, u32), (Rect, f32, f32)>,
    pub generation: u64,
    overflow: bool,
    scratch: Vec<u8>,
}

impl SharedAtlas {
    pub unsafe fn new(gl: &Gles2) -> SharedAtlas {
        let tex = unsafe { gl::texture(gl, ATLAS_SIZE as i32, ATLAS_SIZE as i32, Format::Rgba, true, None) };
        SharedAtlas {
            tex,
            shelf: ShelfAtlas::new(ATLAS_SIZE, ATLAS_SIZE),
            glyphs: HashMap::new(),
            shadows: HashMap::new(),
            generation: 0,
            overflow: false,
            scratch: Vec::new(),
        }
    }

    pub fn take_overflow(&mut self) -> bool {
        std::mem::take(&mut self.overflow)
    }

    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if let Some(p) = self.shelf.alloc(w, h) {
            return Some(p);
        }
        info!(glyphs = self.glyphs.len(), shadows = self.shadows.len(), "atlas full, clearing");
        self.shelf.clear();
        self.glyphs.clear();
        self.shadows.clear();
        self.generation += 1;
        self.overflow = true;
        self.shelf.alloc(w, h)
    }

    fn glyph(&mut self, gl: &Gles2, text: &mut Text, key: CacheKey) -> Option<Slot> {
        if let Some(s) = self.glyphs.get(&key) {
            return *s;
        }
        let slot = (|| {
            let img = text.swash.get_image_uncached(&mut text.fonts, key)?;
            let (w, h) = (img.placement.width, img.placement.height);
            if w == 0 || h == 0 {
                return None;
            }
            let color = matches!(img.content, SwashContent::Color);
            self.scratch.clear();
            match img.content {
                SwashContent::Mask => self.scratch.extend(img.data.iter().flat_map(|&a| [a, a, a, a])),
                SwashContent::Color => self.scratch.extend(img.data.chunks_exact(4).flat_map(|p| {
                    let a = p[3] as u32;
                    [(p[0] as u32 * a / 255) as u8, (p[1] as u32 * a / 255) as u8, (p[2] as u32 * a / 255) as u8, p[3]]
                })),
                SwashContent::SubpixelMask => self.scratch.extend(img.data.chunks_exact(4).flat_map(|p| [p[1]; 4])),
            }
            let (x, y) = self.alloc(w, h)?;
            unsafe { gl::upload(gl, self.tex, x as i32, y as i32, w as i32, h as i32, Format::Rgba, &self.scratch) };
            Some(Slot {
                rect: Rect::new(x as f32, y as f32, w as f32, h as f32),
                left: img.placement.left,
                top: img.placement.top,
                color,
            })
        })();
        self.glyphs.insert(key, slot);
        slot
    }

    /// Draw a layout with its top-left at (x, y) (rounded to whole pixels: glyphs are pixel-aligned).
    pub fn draw_text(&mut self, gl: &Gles2, text: &mut Text, list: &mut DrawList, layout: &TextLayout, x: f32, y: f32, color: Rgba, opacity: f32) {
        let (ox, oy) = (x.round() as i32, y.round() as i32);
        let s = ATLAS_SIZE as f32;
        let tint = scale(color, opacity);
        for &(key, gx, gy) in &layout.glyphs {
            let Some(slot) = self.glyph(gl, text, key) else { continue };
            let r = Rect::new((ox + gx + slot.left) as f32, (oy + gy - slot.top) as f32, slot.rect.w, slot.rect.h);
            let uv = [slot.rect.x / s, slot.rect.y / s, slot.rect.right() / s, slot.rect.bottom() / s];
            let mul = if slot.color { [opacity; 4] } else { tint };
            list.texture(self.tex, r, uv, 0.0, mul, [0.0; 4]);
        }
    }

    /// A drop shadow under the rounded rect `r` (radius `radius`): a Gaussian of `sigma` px, offset by
    /// (dx, dy), drawn as a 9-slice from a pre-rendered slice image.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_shadow(&mut self, gl: &Gles2, list: &mut DrawList, r: Rect, radius: f32, sigma: f32, dx: f32, dy: f32, color: Rgba) {
        if sigma <= 0.0 || color[3] <= 0.0 {
            return;
        }
        let key = (radius.round() as u32, (sigma * 4.0).round() as u32);
        let (src, k, extent) = match self.shadows.get(&key) {
            Some(v) => *v,
            None => {
                let (img, size, k, extent) = shadow_slice(key.0 as f32, key.1 as f32 / 4.0);
                let Some((x, y)) = self.alloc(size, size) else { return };
                unsafe { gl::upload(gl, self.tex, x as i32, y as i32, size as i32, size as i32, Format::Rgba, &img) };
                let v = (Rect::new(x as f32, y as f32, size as f32, size as f32), k as f32, extent as f32);
                self.shadows.insert(key, v);
                v
            }
        };
        let outer = r.translate(dx, dy).inset(-extent);
        list.nine_slice(self.tex, src, k, outer, color, true);
    }
}

/// The slice image for a shadow: a rounded square of radius r, blurred with sigma, on a (2k+1)^2 canvas.
/// k = extent + m, where the shape reaches m + 0.5 from the centre and m >= extent, so the centre texel
/// (stretched over the middle) sees no blur from the far edges. Returns RGBA (coverage in every channel),
/// size, k and the blur extent (how far the shadow reaches outside the shape).
fn shadow_slice(r: f32, sigma: f32) -> (Vec<u8>, u32, u32, u32) {
    let extent = (sigma * 2.5).ceil() as u32;
    let k = extent + (r as u32).max(extent);
    let n = 2 * k + 1;
    let nn = n as usize;
    // Shape coverage: square [extent, n - extent) with rounded corners, analytic 1 px edge.
    let lo = extent as f32;
    let hi = (n - extent) as f32;
    let (c, half) = ((lo + hi) * 0.5, (hi - lo) * 0.5);
    let mut a = vec![0f32; nn * nn];
    for y in 0..nn {
        for x in 0..nn {
            let qx = ((x as f32 + 0.5) - c).abs() - (half - r);
            let qy = ((y as f32 + 0.5) - c).abs() - (half - r);
            let d = (qx.max(0.0).hypot(qy.max(0.0))) + qx.max(qy).min(0.0) - r;
            a[y * nn + x] = (0.5 - d).clamp(0.0, 1.0);
        }
    }
    // Separable Gaussian.
    let taps = extent as i32;
    let w: Vec<f32> = (-taps..=taps).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = w.iter().sum();
    let w: Vec<f32> = w.iter().map(|v| v / sum).collect();
    let mut tmp = vec![0f32; nn * nn];
    for pass in 0..2 {
        let (src, dst) = if pass == 0 { (&a, &mut tmp) } else { (&tmp, &mut a) };
        for y in 0..nn {
            for x in 0..nn {
                let mut s = 0.0;
                for (j, wj) in w.iter().enumerate() {
                    let o = j as i32 - taps;
                    let (sx, sy) = if pass == 0 { (x as i32 + o, y as i32) } else { (x as i32, y as i32 + o) };
                    if sx >= 0 && sy >= 0 && (sx as usize) < nn && (sy as usize) < nn {
                        s += wj * src[sy as usize * nn + sx as usize];
                    }
                }
                dst[y * nn + x] = s;
            }
        }
    }
    let img = a.iter().flat_map(|&v| [(v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8; 4]).collect();
    (img, n, k, extent)
}

#[cfg(test)]
mod tests {
    #[test]
    fn shadow_slice_is_symmetric_and_solid_in_the_middle() {
        let (img, n, k, _) = super::shadow_slice(16.0, 8.0);
        assert_eq!(n, 2 * k + 1);
        let at = |x: u32, y: u32| img[((y * n + x) * 4) as usize];
        assert!(at(k, k) > 240, "centre {}", at(k, k));
        assert!(at(0, 0) < 8);
        assert_eq!(at(3, k), at(n - 4, k));
    }
}
