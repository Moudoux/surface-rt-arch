//! Wallpaper loading on a worker thread: decode, crop to the output's aspect ratio and scale to its size
//! ("cover"), cache the result as raw RGBA under ~/.cache/rtshell/ keyed by path, mtime and size, and send it
//! to the event loop. Decoding the 5120x2880 PNG takes seconds on the Surface; the cache makes later starts
//! instant. With no wallpaper configured (or a broken file), a gradient from theme colours is generated.
//!
//! Regenerated when the output size changes (rotation): `State` asks again with the new size.

use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::Instant,
};

use tracing::{info, warn};

pub struct WallpaperImage {
    pub px: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// Start loading; `done` runs on the worker thread with the result (send it over a calloop channel).
pub fn spawn(path: Option<PathBuf>, w: u32, h: u32, colors: [[f32; 4]; 3], done: impl FnOnce(WallpaperImage) + Send + 'static) {
    std::thread::Builder::new()
        .name("wallpaper".into())
        .spawn(move || {
            let started = Instant::now();
            let img = path
                .as_deref()
                .and_then(|p| match load(p, w, h) {
                    Ok(img) => Some(img),
                    Err(err) => {
                        warn!(path = %p.display(), %err, "wallpaper not loaded, using a gradient");
                        None
                    }
                })
                .unwrap_or_else(|| gradient(w, h, colors));
            info!(w, h, ms = started.elapsed().as_millis() as u64, "wallpaper ready");
            done(img);
        })
        .expect("spawn wallpaper thread");
}

/// Where the scaled RGBA cache lives: `set_cache_dir`'s directory (the greeter: the login image's shared
/// cache, read-only for it), else $XDG_CACHE_HOME or ~/.cache.
static CACHE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_cache_dir(dir: PathBuf) {
    let _ = CACHE_DIR.set(dir);
}

fn cache_file(path: &Path, w: u32, h: u32) -> Option<PathBuf> {
    let base = CACHE_DIR.get().cloned().or_else(|| {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
    })?;
    cache_file_in(path, w, h, &base)
}

fn cache_file_in(path: &Path, w: u32, h: u32, base: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hs);
    meta.modified().ok()?.hash(&mut hs);
    meta.len().hash(&mut hs);
    Some(base.join("rtshell").join(format!("wallpaper-{:016x}-{w}x{h}.rgba", hs.finish())))
}

fn load(path: &Path, w: u32, h: u32) -> Result<WallpaperImage, String> {
    load_cached(path, w, h, cache_file(path, w, h))
}

/// Make sure the scaled cache of `path` at w x h exists under `base` (decodes only if it does not).
pub fn prepare_cache(path: &Path, w: u32, h: u32, base: &Path) -> Result<(), String> {
    let cache = cache_file_in(path, w, h, base).ok_or("no cache file name")?;
    if std::fs::metadata(&cache).is_ok_and(|m| m.len() == (w * h * 4) as u64) {
        return Ok(());
    }
    load_cached(path, w, h, Some(cache)).map(|_| ())
}

fn load_cached(path: &Path, w: u32, h: u32, cache: Option<PathBuf>) -> Result<WallpaperImage, String> {
    if let Some(c) = &cache
        && let Ok(px) = std::fs::read(c)
        && px.len() == (w * h * 4) as usize
    {
        info!(cache = %c.display(), "wallpaper from cache");
        return Ok(WallpaperImage { px, w, h });
    }
    let img = image::open(path).map_err(|e| e.to_string())?.into_rgba8();
    let (sw, sh) = img.dimensions();
    // Cover: crop the source to the output's aspect ratio, centred.
    let (cw, ch) = if sw as u64 * h as u64 > sh as u64 * w as u64 {
        ((sh as u64 * w as u64 / h as u64) as u32, sh)
    } else {
        (sw, (sw as u64 * h as u64 / w as u64) as u32)
    };
    let cropped = image::imageops::crop_imm(&img, (sw - cw) / 2, (sh - ch) / 2, cw, ch).to_image();
    drop(img);
    // A fast box reduction to about twice the size, then a proper filter for the last step.
    let pre = if cw > w * 2 { image::imageops::thumbnail(&cropped, w * 2, h * 2) } else { cropped };
    let out = image::imageops::resize(&pre, w, h, image::imageops::FilterType::Triangle);
    let px = out.into_raw();
    if let Some(c) = &cache {
        let _ = std::fs::create_dir_all(c.parent().unwrap());
        if let Err(err) = std::fs::write(c, &px) {
            warn!(%err, "wallpaper cache not written");
        }
    }
    Ok(WallpaperImage { px, w, h })
}

/// A soft diagonal gradient between two colours with a glow of the third (the accent), opaque.
/// Per-pixel work is one add per channel outside the glow and a sqrt inside it (~5x faster than the plain loop).
fn gradient(w: u32, h: u32, [a, b, glow]: [[f32; 4]; 3]) -> WallpaperImage {
    let mut px = vec![255u8; (w * h * 4) as usize];
    let (gx, gy) = (w as f32 * 0.72, h as f32 * 0.3);
    let gr = w.max(h) as f32 * 0.55;
    let tx: Vec<f32> = (0..w).map(|x| x as f32 / w as f32 * 0.35).collect();
    let dx2: Vec<f32> = (0..w).map(|x| (x as f32 - gx).powi(2)).collect();
    for (y, row) in px.chunks_exact_mut((w * 4) as usize).enumerate() {
        let ty = y as f32 / h as f32 * 0.65;
        let dy2 = (y as f32 - gy).powi(2);
        let in_glow = dy2 < gr * gr;
        for (x, p) in row.chunks_exact_mut(4).enumerate() {
            let t = (tx[x] + ty).min(1.0);
            let g = if in_glow && dx2[x] + dy2 < gr * gr {
                let d = (dx2[x] + dy2).sqrt() / gr;
                (1.0 - d) * (1.0 - d) * 0.55
            } else {
                0.0
            };
            for c in 0..3 {
                let v = a[c] + (b[c] - a[c]) * t;
                let v = v + (glow[c] - v) * g;
                p[c] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
    }
    WallpaperImage { px, w, h }
}
