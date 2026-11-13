//! Icons: freedesktop icon-theme lookup (index.theme directories, Inherits, hicolor last), rasterized with
//! resvg (SVG) or decoded (PNG) at the size asked for, kept in an LRU cell atlas texture.
//!
//! Lookup and rasterizing run on a worker thread (an SVG icon at 64 px takes ~1-3 ms on the Surface, but
//! VSCodium's 1024 px PNG ~270 ms): the first `draw` of an icon queues it and returns `Drawn::Later`; the
//! worker pings the event loop when results are in (`collect`), and the next frame uploads them into the
//! atlas. Results and failures are cached per (name, size); the atlas evicts the least recently drawn icon
//! when all cells are taken.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc,
};

use smithay::reexports::calloop::ping::Ping;

use smithay::backend::renderer::gles::ffi::Gles2;
use tracing::{debug, info};

use super::{
    atlas::CellAtlas,
    batch::{DrawList, Rect},
    gl::{self, Format, Texture},
};

const ATLAS: u32 = 1024;
/// Largest icon edge in pixels (one atlas cell).
pub const MAX_ICON: u32 = 128;

struct ThemeDir {
    path: String,
    size: u32,
    min: u32,
    max: u32,
    scalable: bool,
}

struct Theme {
    roots: Vec<PathBuf>,
    dirs: Vec<ThemeDir>,
    inherits: Vec<String>,
}

fn base_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        v.push(Path::new(&home).join(".local/share/icons"));
        v.push(Path::new(&home).join(".icons"));
    }
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    v.extend(data.split(':').filter(|s| !s.is_empty()).map(|d| Path::new(d).join("icons")));
    v.push("/usr/share/pixmaps".into());
    v
}

fn load_theme(name: &str) -> Option<Theme> {
    let roots: Vec<PathBuf> = base_dirs().into_iter().map(|b| b.join(name)).filter(|p| p.is_dir()).collect();
    let index = roots.iter().find_map(|r| std::fs::read_to_string(r.join("index.theme")).ok())?;
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut cur = String::new();
    for line in index.lines() {
        let line = line.trim();
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            cur = s.to_string();
        } else if let Some((k, v)) = line.split_once('=') {
            sections.entry(cur.clone()).or_default().insert(k.trim().into(), v.trim().into());
        }
    }
    let main = sections.get("Icon Theme")?;
    let list = |k: &str| -> Vec<String> {
        main.get(k).map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
    };
    let mut dirs = Vec::new();
    for d in list("Directories").into_iter().chain(list("ScaledDirectories")) {
        let Some(s) = sections.get(&d) else { continue };
        // Only scale-1 directories: the shell renders at scale 1.
        if s.get("Scale").is_some_and(|v| v != "1") {
            continue;
        }
        let num = |k: &str| s.get(k).and_then(|v| v.parse::<u32>().ok());
        let size = num("Size").unwrap_or(0);
        let ty = s.get("Type").map(String::as_str).unwrap_or("Threshold");
        let threshold = num("Threshold").unwrap_or(2);
        let (min, max) = match ty {
            "Fixed" => (size, size),
            "Scalable" => (num("MinSize").unwrap_or(size), num("MaxSize").unwrap_or(size)),
            _ => (size.saturating_sub(threshold), size + threshold),
        };
        dirs.push(ThemeDir { path: d, size, min, max, scalable: ty == "Scalable" });
    }
    Some(Theme { roots, dirs, inherits: list("Inherits") })
}

/// One atlas texture of equal cells.
struct Bank {
    atlas: CellAtlas,
    tex: Texture,
    /// Which (name, size) each cell holds.
    owner: Vec<Option<(String, u32)>>,
}

impl Bank {
    unsafe fn new(gl: &Gles2, cell: u32) -> Bank {
        let n = (ATLAS / cell) * (ATLAS / cell);
        Bank {
            atlas: CellAtlas::new(ATLAS, cell),
            tex: unsafe { gl::texture(gl, ATLAS as i32, ATLAS as i32, Format::Rgba, true, None) },
            owner: vec![None; n as usize],
        }
    }
}

/// Result of `Icons::draw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drawn {
    Yes,
    /// No such icon (draw a fallback).
    Missing,
    /// Not rasterized yet (queued on the worker; the loop is woken when it is ready).
    Later,
}

/// Icons up to this size go to the bank of small cells (4x as many fit).
const SMALL: u32 = 64;

type Key = (String, u32);
type Pixels = Option<(Vec<u8>, u32, u32)>;

/// The worker's side: themes and the lookup cache.
struct Lookup {
    themes: Vec<Theme>,
    found: HashMap<Key, Option<PathBuf>>,
}

pub struct Icons {
    jobs: mpsc::Sender<Key>,
    results: mpsc::Receiver<(Key, Pixels)>,
    /// Queued, not back yet.
    pending: HashSet<Key>,
    /// Back from the worker, not in the atlas yet.
    ready: HashMap<Key, Pixels>,
    /// (name, size) -> (bank, cell, w, h), or None if it could not be loaded.
    cells: HashMap<Key, Option<(usize, usize, u32, u32)>>,
    /// Small (<= 64 px) and large cells; each 1024x1024 texture (4 MB) is made on first use.
    banks: [Option<Bank>; 2],
    frame: u64,
}

impl Icons {
    /// `ping` wakes the event loop when rasterized icons are ready (then call `collect`). `prepared` is the
    /// preloaded theme + disk cache (`render::preload`) if it was started for this theme.
    pub unsafe fn new(_gl: &Gles2, theme: &str, ping: Ping, prepared: Option<std::thread::JoinHandle<Prepared>>) -> Icons {
        let (jobs, job_rx) = mpsc::channel::<Key>();
        let (res_tx, results) = mpsc::channel();
        let prepared = prepared.and_then(|h| h.join().ok()).unwrap_or_else(|| prepare(theme));
        let Prepared { lookup, disk, hits } = prepared;
        let revalidate: Vec<Key> = hits.keys().cloned().collect();
        let spawned = std::thread::Builder::new().name("icons".into()).spawn(move || worker(lookup, disk, revalidate, job_rx, res_tx, ping));
        if let Err(err) = spawned {
            tracing::warn!(%err, "no icon thread: icons will not show");
        }
        let ready = hits;
        Icons {
            jobs,
            results,
            pending: HashSet::new(),
            ready,
            cells: HashMap::new(),
            banks: [None, None],
            frame: 0,
        }
    }

    #[allow(dead_code)] // theme reload
    pub unsafe fn delete(self, gl: &Gles2) {
        for b in self.banks.into_iter().flatten() {
            unsafe { gl::delete_texture(gl, b.tex) };
        }
    }

    /// Icons queued on the worker and not back yet.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.collect();
    }

    /// Take finished icons from the worker. True if there were any (a redraw shows them).
    pub fn collect(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, px)) = self.results.try_recv() {
            self.pending.remove(&key);
            // A changed icon (background re-check of a cached one): drop the old cell, the next draw uploads.
            if let Some(Some((b, cell, ..))) = self.cells.remove(&key) {
                if let Some(bank) = self.banks[b].as_mut() {
                    bank.owner[cell] = None;
                }
            }
            self.ready.insert(key, px);
            any = true;
        }
        any
    }

    /// Draw icon `name` centred in a `size` x `size` box at (x, y).
    pub fn draw(&mut self, gl: &Gles2, list: &mut DrawList, name: &str, x: f32, y: f32, size: u32, opacity: f32) -> Drawn {
        let size = size.min(MAX_ICON);
        let key = (name.to_string(), size);
        let entry = match self.cells.get(&key) {
            Some(e) => *e,
            None => {
                let Some(raster) = self.ready.remove(&key) else {
                    if self.pending.insert(key.clone()) {
                        let _ = self.jobs.send(key);
                    }
                    return Drawn::Later;
                };
                let Some((px, w, h)) = raster else {
                    self.cells.insert(key, None);
                    return Drawn::Missing;
                };
                let b = usize::from(size > SMALL);
                let bank = self.banks[b].get_or_insert_with(|| unsafe { Bank::new(gl, if b == 0 { SMALL } else { MAX_ICON }) });
                // Every cell is on screen this frame: skip the icon (kept, so it comes back later).
                let Some((cell, _)) = bank.atlas.alloc(self.frame) else {
                    self.ready.insert(key, Some((px, w, h)));
                    return Drawn::Missing;
                };
                if let Some(old) = bank.owner[cell].take() {
                    self.cells.remove(&old);
                }
                let (cx, cy) = bank.atlas.origin(cell);
                unsafe { gl::upload(gl, bank.tex, cx as i32, cy as i32, w as i32, h as i32, Format::Rgba, &px) };
                bank.owner[cell] = Some(key.clone());
                let e = Some((b, cell, w, h));
                self.cells.insert(key, e);
                e
            }
        };
        let Some((b, cell, w, h)) = entry else { return Drawn::Missing };
        let Some(bank) = self.banks[b].as_mut() else { return Drawn::Missing };
        bank.atlas.touch(cell, self.frame);
        let src = bank.atlas.rect(cell, w, h);
        let s = ATLAS as f32;
        let dst = Rect::new((x + (size - w) as f32 * 0.5).round(), (y + (size - h) as f32 * 0.5).round(), w as f32, h as f32);
        list.texture(bank.tex, dst, [src.x / s, src.y / s, src.right() / s, src.bottom() / s], 0.0, [opacity; 4], [0.0; 4]);
        Drawn::Yes
    }
}

impl Lookup {
    fn new(theme: &str) -> Lookup {
        // The theme, everything it inherits (breadth first), then hicolor.
        let mut names = vec![theme.to_string()];
        let mut themes = Vec::new();
        let mut i = 0;
        while i < names.len() {
            if let Some(t) = load_theme(&names[i]) {
                for inh in &t.inherits {
                    if !names.contains(inh) {
                        names.push(inh.clone());
                    }
                }
                themes.push(t);
            }
            i += 1;
        }
        if !names.iter().any(|n| n == "hicolor")
            && let Some(t) = load_theme("hicolor")
        {
            themes.push(t);
        }
        info!(chain = ?names, loaded = themes.len(), "icon themes");
        Lookup { themes, found: HashMap::new() }
    }

    /// Freedesktop lookup: in each theme, an exact size match first, else the closest directory.
    fn find(&mut self, name: &str, size: u32) -> Option<PathBuf> {
        let key = (name.to_string(), size);
        if let Some(f) = self.found.get(&key) {
            return f.clone();
        }
        let found = (|| {
            if name.starts_with('/') {
                return Path::new(name).is_file().then(|| PathBuf::from(name));
            }
            for t in &self.themes {
                let mut best: Option<(u32, PathBuf)> = None;
                for d in &t.dirs {
                    let dist = if size < d.min {
                        d.min - size
                    } else if size > d.max {
                        size - d.max
                    } else {
                        0
                    };
                    if best.as_ref().is_some_and(|(b, _)| *b <= dist) {
                        continue;
                    }
                    for root in &t.roots {
                        for ext in ["svg", "png"] {
                            let p = root.join(&d.path).join(format!("{name}.{ext}"));
                            if p.is_file() {
                                // Prefer an SVG in a scalable dir over a small bitmap.
                                let dist = if d.scalable && ext == "svg" { 0 } else { dist };
                                if best.as_ref().is_none_or(|(b, _)| dist < *b) {
                                    best = Some((dist, p));
                                }
                            }
                        }
                    }
                    let _ = d.size;
                }
                if let Some((_, p)) = best {
                    return Some(p);
                }
            }
            for ext in ["svg", "png"] {
                let p = Path::new("/usr/share/pixmaps").join(format!("{name}.{ext}"));
                if p.is_file() {
                    return Some(p);
                }
            }
            None
        })();
        debug!(%name, size, file = ?found, "icon lookup");
        self.found.insert(key, found.clone());
        found
    }

    fn rasterize(path: &Path, size: u32) -> Option<(Vec<u8>, u32, u32)> {
        let data = std::fs::read(path).ok()?;
        if path.extension().is_some_and(|e| e == "svg") {
            let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
            let ts = tree.size();
            let k = size as f32 / ts.width().max(ts.height());
            let (w, h) = ((ts.width() * k).round().max(1.0) as u32, (ts.height() * k).round().max(1.0) as u32);
            let mut pm = resvg::tiny_skia::Pixmap::new(w, h)?;
            resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
            Some((pm.take(), w, h))
        } else {
            let img = image::load_from_memory(&data).ok()?.into_rgba8();
            let k = size as f32 / img.width().max(img.height()) as f32;
            let (w, h) = ((img.width() as f32 * k).round().max(1.0) as u32, (img.height() as f32 * k).round().max(1.0) as u32);
            let img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
            let mut px = img.into_raw();
            for p in px.chunks_exact_mut(4) {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = (*c as u32 * a / 255) as u8;
                }
            }
            Some((px, w, h))
        }
    }
}

/// The icon theme chain plus the cached rasterized icons still valid, built off the main thread (at process
/// start by `render::preload`, else in `Icons::new`).
pub struct Prepared {
    lookup: Lookup,
    disk: DiskCache,
    /// Cached icons whose source file is unchanged, ready for the first frame (None = known missing).
    hits: HashMap<Key, Pixels>,
}

pub fn prepare(theme: &str) -> Prepared {
    let started = std::time::Instant::now();
    let lookup = Lookup::new(theme);
    let disk = DiskCache::load(theme);
    let mut hits = HashMap::new();
    for (key, e) in &disk.entries {
        let valid = match &e.source {
            Some(p) => mtime_ns(p) == Some(e.mtime),
            None => true, // re-checked by the worker after startup
        };
        if valid {
            hits.insert(key.clone(), e.source.as_ref().map(|_| (e.px.clone(), e.w, e.h)));
        }
    }
    info!(cached = disk.entries.len(), valid = hits.len(), ms = started.elapsed().as_millis() as u64, "icons prepared");
    Prepared { lookup, disk, hits }
}

fn mtime_ns(p: &Path) -> Option<u64> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as u64)
}

/// The worker: answers jobs (lookup + rasterize, recorded in the disk cache); once startup is over (10 s
/// after the first frame's icons were served from the cache) looks the cached ones up again and sends any
/// that changed (new icon installed, theme updated, a missing one now present); saves the cache when idle.
fn worker(mut lookup: Lookup, mut disk: DiskCache, mut revalidate: Vec<Key>, jobs: mpsc::Receiver<Key>, results: mpsc::Sender<(Key, Pixels)>, ping: Ping) {
    use std::time::{Duration, Instant};
    let recheck_at = Instant::now() + Duration::from_secs(10);
    let mut dirty = false;
    let send = |key: Key, px: Pixels| {
        let ok = results.send((key, px)).is_ok();
        ping.ping();
        ok
    };
    loop {
        let wait = if !revalidate.is_empty() {
            Some(recheck_at.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)))
        } else if dirty {
            Some(Duration::from_secs(2))
        } else {
            None
        };
        let job = match wait {
            Some(w) => jobs.recv_timeout(w),
            None => jobs.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match job {
            Ok(key) => {
                let t0 = Instant::now();
                let path = lookup.find(&key.0, key.1);
                let px = path.as_ref().and_then(|p| Lookup::rasterize(p, key.1));
                let took = t0.elapsed();
                if took > Duration::from_millis(50) {
                    info!(name = %key.0, size = key.1, file = ?path, ms = took.as_millis() as u64, "slow icon");
                }
                disk.put(key.clone(), path.as_deref(), &px);
                dirty = true;
                if !send(key, px) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) if !revalidate.is_empty() && Instant::now() >= recheck_at => {
                let t0 = Instant::now();
                let mut changed = 0;
                for key in std::mem::take(&mut revalidate) {
                    let path = lookup.find(&key.0, key.1);
                    let same = disk.entries.get(&key).is_some_and(|e| e.source == path && path.as_deref().is_none_or(|p| mtime_ns(p) == Some(e.mtime)));
                    if same {
                        continue;
                    }
                    let px = path.as_ref().and_then(|p| Lookup::rasterize(p, key.1));
                    disk.put(key.clone(), path.as_deref(), &px);
                    dirty = true;
                    changed += 1;
                    if !send(key, px) {
                        return;
                    }
                }
                info!(changed, ms = t0.elapsed().as_millis() as u64, "icon cache re-checked");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dirty {
                    disk.save();
                    dirty = false;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if dirty {
        disk.save();
    }
}

struct CacheEntry {
    source: Option<PathBuf>,
    mtime: u64,
    w: u32,
    h: u32,
    px: Vec<u8>,
}

/// Rasterized icons on disk (`~/.cache/rtshell/icons-<theme>.bin`), keyed by (name, size), with the source
/// file and its mtime; None source = no such icon. Capped at `CACHE_MAX` bytes of pixels.
struct DiskCache {
    file: Option<PathBuf>,
    entries: HashMap<Key, CacheEntry>,
}

const CACHE_MAGIC: &[u8] = b"RTICONS1";
const CACHE_MAX: usize = 6 << 20;

impl DiskCache {
    fn load(theme: &str) -> DiskCache {
        let safe: String = theme.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let file = super::preload::cache_dir().map(|d| d.join(format!("icons-{safe}.bin")));
        let entries = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|d| Self::parse(&d)).unwrap_or_default();
        DiskCache { file, entries }
    }

    fn parse(d: &[u8]) -> Option<HashMap<Key, CacheEntry>> {
        let mut r = d.strip_prefix(CACHE_MAGIC)?;
        let mut take = |n: usize| -> Option<&[u8]> {
            let (a, b) = (r.get(..n)?, r.get(n..)?);
            r = b;
            Some(a)
        };
        let mut out = HashMap::new();
        loop {
            let Some(len) = take(4) else { break };
            let len = u32::from_le_bytes(len.try_into().ok()?) as usize;
            let name = String::from_utf8(take(len)?.to_vec()).ok()?;
            let size = u32::from_le_bytes(take(4)?.try_into().ok()?);
            let has = take(1)?[0];
            let e = if has == 1 {
                let plen = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
                let source = PathBuf::from(String::from_utf8(take(plen)?.to_vec()).ok()?);
                let mtime = u64::from_le_bytes(take(8)?.try_into().ok()?);
                let w = u32::from_le_bytes(take(4)?.try_into().ok()?);
                let h = u32::from_le_bytes(take(4)?.try_into().ok()?);
                if w > MAX_ICON || h > MAX_ICON {
                    return None;
                }
                let px = take((w * h * 4) as usize)?.to_vec();
                CacheEntry { source: Some(source), mtime, w, h, px }
            } else {
                CacheEntry { source: None, mtime: 0, w: 0, h: 0, px: Vec::new() }
            };
            out.insert((name, size), e);
        }
        Some(out)
    }

    fn put(&mut self, key: Key, source: Option<&Path>, px: &Pixels) {
        let e = match (source, px) {
            (Some(p), Some((px, w, h))) => CacheEntry { source: Some(p.to_path_buf()), mtime: mtime_ns(p).unwrap_or(0), w: *w, h: *h, px: px.clone() },
            // Found but not decodable, or not found: remember as missing.
            _ => CacheEntry { source: None, mtime: 0, w: 0, h: 0, px: Vec::new() },
        };
        self.entries.insert(key, e);
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        let mut out = CACHE_MAGIC.to_vec();
        let mut bytes = 0;
        for ((name, size), e) in &self.entries {
            bytes += e.px.len();
            if bytes > CACHE_MAX {
                break;
            }
            out.extend((name.len() as u32).to_le_bytes());
            out.extend(name.as_bytes());
            out.extend(size.to_le_bytes());
            match &e.source {
                Some(p) => {
                    let p = p.to_string_lossy();
                    out.push(1);
                    out.extend((p.len() as u32).to_le_bytes());
                    out.extend(p.as_bytes());
                    out.extend(e.mtime.to_le_bytes());
                    out.extend(e.w.to_le_bytes());
                    out.extend(e.h.to_le_bytes());
                    out.extend(&e.px);
                }
                None => out.push(0),
            }
        }
        match super::preload::write_atomic(file, &out) {
            Ok(()) => debug!(entries = self.entries.len(), bytes = out.len(), "icon cache saved"),
            Err(err) => tracing::warn!(%err, "icon cache not saved"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_cache_round_trip() {
        let dir = std::env::temp_dir().join(format!("rtshell-icons-test-{}", std::process::id()));
        let file = dir.join("icons-t.bin");
        let src = dir.join("a.svg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&src, b"x").unwrap();
        let mut c = DiskCache { file: Some(file.clone()), entries: HashMap::new() };
        c.put(("a".into(), 32), Some(&src), &Some((vec![7; 2 * 3 * 4], 2, 3)));
        c.put(("gone".into(), 16), None, &None);
        c.save();
        let back = DiskCache::parse(&std::fs::read(&file).unwrap()).unwrap();
        let a = &back[&("a".to_string(), 32)];
        assert_eq!((a.w, a.h, a.px.len(), a.source.as_deref()), (2, 3, 24, Some(src.as_path())));
        assert_eq!(a.mtime, mtime_ns(&src).unwrap());
        assert!(back[&("gone".to_string(), 16)].source.is_none());
        assert!(DiskCache::parse(b"RTICONS1\x05\x00\x00\x00ab").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
