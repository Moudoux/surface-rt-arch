//! The pointer cursor, drawn with GL as an ordinary element (DESKTOP-SHELL.md Log 2026-11-09: the tegra cursor
//! plane 87 shows nothing via Smithay). Hidden while touch is in use, shown again on pointer input.
//! Animated xcursors show their first frame only, so the cursor never forces redraws (idle CPU 0 %).

use std::{collections::HashMap, io::Read};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::render_elements_from_surface_tree,
            },
            gles::GlesRenderer,
        },
    },
    input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    utils::{IsAlive, Logical, Physical, Point, Scale, Transform},
    wayland::compositor::with_states,
};
use tracing::{debug, warn};
use xcursor::{CursorTheme, parser::parse_xcursor};

use super::OutputElement;
use crate::theme::CursorTokens;

/// anvil's fallback arrow (Smithay, MIT): 64x64, Argb8888 bytes, hotspot (1, 1).
static FALLBACK: &[u8] = include_bytes!("../../assets/cursor-fallback.rgba");

#[derive(Clone)]
struct CursorImage {
    buffer: MemoryRenderBuffer,
    hotspot: Point<i32, Logical>,
}

pub struct CursorState {
    /// What the focused client asked for (named shape, its own surface, or hidden).
    pub status: CursorImageStatus,
    /// False while the user works with touch: the cursor is only drawn after pointer input.
    pub visible: bool,
    themes: Vec<CursorTheme>,
    size: u32,
    cache: HashMap<CursorIcon, CursorImage>,
    fallback: CursorImage,
}

impl CursorState {
    pub fn new(tokens: &CursorTokens) -> Self {
        let mut names = vec![tokens.theme.clone()];
        for n in ["default", "Adwaita", "breeze_cursors"] {
            if !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        }
        CursorState {
            status: CursorImageStatus::default_named(),
            visible: false,
            themes: names.iter().map(|n| CursorTheme::load(n)).collect(),
            size: tokens.size,
            cache: HashMap::new(),
            fallback: CursorImage {
                buffer: MemoryRenderBuffer::from_slice(FALLBACK, Fourcc::Argb8888, (64, 64), 1, Transform::Normal, None),
                hotspot: (1, 1).into(),
            },
        }
    }

    fn load(&self, icon: CursorIcon) -> Option<CursorImage> {
        let names = std::iter::once(icon.name()).chain(icon.alt_names().iter().copied());
        for name in names {
            for theme in &self.themes {
                let Some(path) = theme.load_icon(name) else { continue };
                let mut data = Vec::new();
                if std::fs::File::open(&path).and_then(|mut f| f.read_to_end(&mut data)).is_err() {
                    continue;
                }
                let Some(images) = parse_xcursor(&data) else { continue };
                let Some(img) = images
                    .iter()
                    .min_by_key(|i| (i.size as i64 - self.size as i64).abs())
                else {
                    continue;
                };
                debug!(?icon, path = %path.display(), size = img.size, "cursor loaded");
                // xcursor pixels are little-endian ARGB32 (= DRM Argb8888), premultiplied.
                return Some(CursorImage {
                    buffer: MemoryRenderBuffer::from_slice(
                        &img.pixels_rgba,
                        Fourcc::Argb8888,
                        (img.width as i32, img.height as i32),
                        1,
                        Transform::Normal,
                        None,
                    ),
                    hotspot: (img.xhot as i32, img.yhot as i32).into(),
                });
            }
        }
        None
    }

    fn named(&mut self, icon: CursorIcon) -> CursorImage {
        if let Some(img) = self.cache.get(&icon) {
            return img.clone();
        }
        let img = self.load(icon).or_else(|| (icon != CursorIcon::Default).then(|| self.named(CursorIcon::Default)));
        let img = img.unwrap_or_else(|| {
            warn!("no xcursor theme found, using the built-in arrow");
            self.fallback.clone()
        });
        self.cache.insert(icon, img.clone());
        img
    }

    /// Elements for the cursor at `location` (output-local, logical). Empty when hidden.
    pub fn render_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        location: Point<f64, Logical>,
        scale: Scale<f64>,
    ) -> Vec<OutputElement> {
        if !self.visible {
            return Vec::new();
        }
        if let CursorImageStatus::Surface(s) = &self.status
            && !s.alive()
        {
            self.status = CursorImageStatus::default_named();
        }
        match self.status.clone() {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Named(icon) => {
                let img = self.named(icon);
                let pos: Point<f64, Physical> = (location - img.hotspot.to_f64()).to_physical(scale);
                // Kind::Unspecified: never offer the cursor to the (broken) cursor plane.
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    pos,
                    &img.buffer,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ) {
                    Ok(e) => vec![OutputElement::Memory(e)],
                    Err(err) => {
                        warn!(?err, "cursor upload failed");
                        Vec::new()
                    }
                }
            }
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|d| d.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let pos = (location - hotspot.to_f64()).to_physical(scale).to_i32_round();
                render_elements_from_surface_tree(renderer, &surface, pos, scale, 1.0, Kind::Unspecified)
                    .into_iter()
                    .map(OutputElement::Surface)
                    .collect()
            }
        }
    }
}
