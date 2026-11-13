//! Screenshots. User ones (`save`, Phase 9) go to the Pictures folder as compressed PNGs. Debug ones (SIGUSR1): re-render the output's current elements into an offscreen texture, read it
//! back with plain `glReadPixels` and write `/tmp/rtshell-shot-<n>.png`.
//!
//! Why not Smithay's `ExportMem::copy_framebuffer`: it reads through a pixel-pack buffer, `glReadBuffer` and
//! `glMapBufferRange`, which are GLES 3; grate is GLES 2.0.
//!
//! What it shows: the same elements, imported textures and shaders as the real frame, rendered again with
//! full damage. It does not see the KMS planes (a directly scanned-out buffer is sampled here instead) or
//! anything that differs between the swapchain buffer and an offscreen texture. Costs nothing until a
//! signal arrives: no timer, no per-frame work.
//!
//! Each client dmabuf on screen is also dumped as it is stored (`-buf<k>.png`): bound as a render target
//! and read with glReadPixels, without sampling it. A wrong composite with a correct buffer dump means
//! the sampling (import/texture descriptor) is at fault, a wrong dump means the producer.
//!
//! Burst (SIGUSR2, `State::start_burst`): the next `RTSHELL_BURST` (default 12) client commits that attach a
//! dmabuf dump that buffer as stored *at commit time* (`/tmp/rtshell-burst-<seq>.png`, before Smithay
//! imports it or releases the previous one) and re-read the surface's previous buffer
//! (`-<prevseq>-late.png`, still unreleased). early(k) vs early(k+1) shows frame-to-frame changes the
//! panel flickers with; early(k) vs late(k) differing means the client's GPU was still writing the buffer
//! after the commit (missing producer->consumer sync).

use std::{
    io::Write,
    sync::atomic::{AtomicU32, Ordering},
    time::Instant,
};

use smithay::{
    backend::{
        allocator::{Buffer, Fourcc, dmabuf::Dmabuf},
        renderer::{
            Bind, Frame, Offscreen, Renderer,
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture, ffi},
        },
    },
    output::Output,
    utils::{Size, Transform},
};
use tracing::{debug, info, warn};

use super::OutputElement;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Dump client dmabufs as stored, next to screenshot number `COUNTER` (call after `take`).
pub fn dump_dmabufs(renderer: &mut GlesRenderer, buffers: Vec<Dmabuf>) {
    let n = COUNTER.load(Ordering::Relaxed);
    for (k, buf) in buffers.into_iter().enumerate() {
        dump_dmabuf(renderer, buf, format!("/tmp/rtshell-shot-{n}-buf{k}.png"), "screenshot dmabuf");
    }
}

/// One dmabuf as stored (bound as a render target and read with glReadPixels, not sampled) -> PNG at `path`
/// (written on a thread, logged with `what`). ~65 ms of GL readback on the Tegra for 1366x768.
pub fn dump_dmabuf(renderer: &mut GlesRenderer, mut buf: Dmabuf, path: String, what: &'static str) {
    let (w, h) = (buf.width() as i32, buf.height() as i32);
    let desc = format!(
        "{:?} {}x{} stride={} offset={} mod={:?}",
        buf.format().code,
        w,
        h,
        buf.strides().next().unwrap_or(0),
        buf.offsets().next().unwrap_or(0),
        buf.format().modifier
    );
    let result = (|| -> Result<Vec<u8>, String> {
        let mut fb = renderer.bind(&mut buf).map_err(|e| format!("bind: {e}"))?;
        let size = Size::from((w, h));
        let mut frame = renderer
            .render(&mut fb, size, Transform::Normal)
            .map_err(|e| format!("frame: {e}"))?;
        let pixels = read_pixels(&mut frame, w, h);
        let _ = frame.finish();
        pixels
    })();
    match result {
        Ok(rgba) => {
            std::thread::spawn(move || match write_png(&path, w as u32, h as u32, &rgba) {
                Ok(()) => info!(%path, buffer = %desc, what),
                Err(err) => warn!(%path, %err, "screenshot write failed"),
            });
        }
        Err(err) => warn!(buffer = %desc, %err, "screenshot dmabuf failed"),
    }
}

fn read_pixels(frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>, w: i32, h: i32) -> Result<Vec<u8>, String> {
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let err = frame
        .with_context(|gl| unsafe {
            gl.GetError();
            gl.PixelStorei(ffi::PACK_ALIGNMENT, 4);
            gl.ReadPixels(0, 0, w, h, ffi::RGBA, ffi::UNSIGNED_BYTE, rgba.as_mut_ptr().cast());
            gl.GetError()
        })
        .map_err(|e| format!("context: {e}"))?;
    if err != ffi::NO_ERROR {
        return Err(format!("glReadPixels: GL error 0x{err:x}"));
    }
    Ok(rgba)
}

/// Render `elements` (front to back, as for the real frame) and save them as a PNG. Pixel reading happens
/// here (GL), PNG encoding and writing on a thread.
pub fn take(renderer: &mut GlesRenderer, output: &Output, elements: &[OutputElement], clear: [f32; 4]) {
    let started = Instant::now();
    {
        use smithay::backend::renderer::element::Element;
        let scale = output.current_scale().fractional_scale();
        for (i, e) in elements.iter().enumerate() {
            debug!(i, id = ?e.id(), geometry = ?e.geometry(scale.into()), "screenshot element");
        }
    }
    match read_back(renderer, output, elements, clear) {
        Ok((w, h, rgba)) => {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            let path = format!("/tmp/rtshell-shot-{n}.png");
            let read_ms = started.elapsed().as_secs_f64() * 1000.0;
            let count = elements.len();
            std::thread::spawn(move || match write_png(&path, w, h, &rgba) {
                Ok(()) => info!(%path, w, h, elements = count, read_ms = format!("{read_ms:.1}"), "screenshot"),
                Err(err) => warn!(%path, %err, "screenshot write failed"),
            });
        }
        Err(err) => warn!(%err, "screenshot failed"),
    }
}

/// A user screenshot (Super+Shift+S, Power + Volume-down): like `take`, but saved as a compressed PNG into
/// `dir` (created if missing) with a local date-time name. The readback happens here (~65 ms of GL on the
/// Tegra for 1366x768), encoding and writing on a thread, which reports the saved path or the error to `done`.
pub fn save(
    renderer: &mut GlesRenderer,
    output: &Output,
    elements: &[OutputElement],
    clear: [f32; 4],
    dir: std::path::PathBuf,
    done: impl FnOnce(Result<std::path::PathBuf, String>) + Send + 'static,
) {
    let started = Instant::now();
    let (w, h, rgba) = match read_back(renderer, output, elements, clear) {
        Ok(x) => x,
        Err(err) => {
            warn!(%err, "screenshot failed");
            done(Err(err));
            return;
        }
    };
    let read_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stamp = local_stamp();
    let spawned = std::thread::Builder::new().name("screenshot".into()).spawn(move || {
        let t = Instant::now();
        let res = (|| -> Result<std::path::PathBuf, String> {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = free_name(&dir, &stamp);
            let part = path.with_extension("png.part");
            let file = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
            let res = write_png_compressed(std::io::BufWriter::new(file), w, h, &rgba);
            if let Err(e) = res {
                let _ = std::fs::remove_file(&part);
                return Err(format!("{}: {e}", path.display()));
            }
            std::fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(path)
        })();
        match &res {
            Ok(path) => info!(path = %path.display(), w, h, read_ms = format!("{read_ms:.1}"), write_ms = t.elapsed().as_millis() as u64, "screenshot saved"),
            Err(err) => warn!(%err, "screenshot write failed"),
        }
        done(res);
    });
    if let Err(err) = spawned {
        warn!(%err, "screenshot: no writer thread");
    }
}

/// `Screenshot_YYYY-MM-DD_HH-MM-SS` in local time.
fn local_stamp() -> String {
    // Safety: plain libc calls on local values.
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    format!(
        "Screenshot_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// `<stamp>.png`, or `<stamp>-2.png`, ... if taken twice in one second.
fn free_name(dir: &std::path::Path, stamp: &str) -> std::path::PathBuf {
    let mut path = dir.join(format!("{stamp}.png"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stamp}-{n}.png"));
        n += 1;
    }
    path
}

/// The Pictures folder: `XDG_PICTURES_DIR` from `user-dirs.dirs` if set, else `~/Pictures`.
pub fn pictures_dir() -> Option<std::path::PathBuf> {
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    let config = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let from_dirs = std::fs::read_to_string(config.join("user-dirs.dirs")).ok().and_then(|text| parse_user_dir(&text, "XDG_PICTURES_DIR", &home));
    Some(from_dirs.unwrap_or_else(|| home.join("Pictures")))
}

/// One `NAME="$HOME/dir"` (or absolute) line of `user-dirs.dirs`. `$HOME` alone (a disabled folder) is ignored.
fn parse_user_dir(text: &str, name: &str, home: &std::path::Path) -> Option<std::path::PathBuf> {
    let line = text.lines().map(str::trim).find(|l| l.strip_prefix(name).is_some_and(|r| r.trim_start().starts_with('=')))?;
    let value = line.split_once('=')?.1.trim().trim_matches('"');
    let path = if let Some(rest) = value.strip_prefix("$HOME") {
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            return None;
        }
        home.join(rest)
    } else if value.starts_with('/') {
        std::path::PathBuf::from(value)
    } else {
        return None;
    };
    Some(path)
}

/// RGBA rows (top first) -> 8-bit RGB PNG, zlib-compressed (fast level; a desktop shot is ~0.3-1 MB).
fn write_png_compressed(out: impl Write, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    use image::{
        ExtendedColorType, ImageEncoder,
        codecs::png::{CompressionType, FilterType, PngEncoder},
    };
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect();
    PngEncoder::new_with_quality(out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(&rgb, w, h, ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())
}

fn read_back(
    renderer: &mut GlesRenderer,
    output: &Output,
    elements: &[OutputElement],
    clear: [f32; 4],
) -> Result<(u32, u32, Vec<u8>), String> {
    let mode = output.current_mode().ok_or("output has no mode")?;
    // The physical size as rendered (after the output transform).
    let size = output.current_transform().transform_size(mode.size);
    let scale = output.current_scale().fractional_scale();
    let (w, h) = (size.w, size.h);

    // ARGB8888 is grate's native render target format (BGRA); ABGR8888 needs mesa-grate >= 26.2.3-27.
    let mut texture: GlesTexture = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Argb8888, Size::from((w, h)))
        .or_else(|_| Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, Size::from((w, h))))
        .map_err(|e| format!("create_buffer: {e}"))?;
    let mut fb = renderer.bind(&mut texture).map_err(|e| format!("bind: {e}"))?;

    // Transform Normal: the texture is read as it lies in memory, row 0 = top of the panel.
    let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
    let result = tracker
        .render_output(renderer, &mut fb, 0, elements, clear)
        .map_err(|e| format!("render: {e:?}"))?;
    {
        use smithay::backend::renderer::element::Element;
        for e in elements {
            let st = result.states.element_render_state(e.id().clone());
            debug!(id = ?e.id(), state = ?st, "screenshot element state");
        }
        debug!(damage = ?result.damage, "screenshot damage");
    }

    // A second frame only to have the texture's FBO bound while reading.
    let mut frame = renderer
        .render(&mut fb, size, Transform::Normal)
        .map_err(|e| format!("frame: {e}"))?;
    let rgba = read_pixels(&mut frame, w, h);
    let _ = frame.finish();
    Ok((w as u32, h as u32, rgba?))
}

/// PNG, 8-bit RGB (alpha dropped: the panel has none), zlib "stored" blocks (no compression: simple and
/// fast on the Tegra; ~3 MB for 1366x768 on tmpfs).
fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((h * (1 + w * 3)) as usize);
    for row in rgba.chunks_exact((w * 4) as usize) {
        raw.push(0); // filter: none
        for px in row.chunks_exact(4) {
            raw.extend_from_slice(&px[..3]);
        }
    }

    let mut z = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 16);
    z.extend_from_slice(&[0x78, 0x01]);
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(block) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bit, RGB, deflate, no filter, no interlace

    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    out.write_all(b"\x89PNG\r\n\x1a\n")?;
    chunk(&mut out, b"IHDR", &ihdr)?;
    chunk(&mut out, b"IDAT", &z)?;
    chunk(&mut out, b"IEND", &[])?;
    out.flush()
}

fn chunk(out: &mut impl Write, kind: &[u8; 4], data: &[u8]) -> std::io::Result<()> {
    out.write_all(&(data.len() as u32).to_be_bytes())?;
    out.write_all(kind)?;
    out.write_all(data)?;
    let crc = crc32(crc32(0, kind), data);
    out.write_all(&crc.to_be_bytes())
}

fn crc32(crc: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    let mut c = !crc;
    for &b in data {
        c = table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for block in data.chunks(5552) {
        for &x in block {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs() {
        let home = std::path::Path::new("/home/a");
        let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\n";
        assert_eq!(parse_user_dir(text, "XDG_PICTURES_DIR", home), Some("/home/a/Bilder".into()));
        assert_eq!(parse_user_dir("XDG_PICTURES_DIR=\"$HOME/\"", "XDG_PICTURES_DIR", home), None);
        assert_eq!(parse_user_dir("XDG_PICTURES_DIR=\"/data/pics\"", "XDG_PICTURES_DIR", home), Some("/data/pics".into()));
        assert_eq!(parse_user_dir(text, "XDG_MUSIC_DIR", home), None);
    }

    #[test]
    fn png_roundtrip() {
        let (w, h) = (3u32, 2u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| [i as u8 * 40, 255 - i as u8, 7, 255]).collect();
        let mut buf = Vec::new();
        write_png_compressed(&mut buf, w, h, &rgba).unwrap();
        let img = image::load_from_memory_with_format(&buf, image::ImageFormat::Png).unwrap().to_rgb8();
        assert_eq!((img.width(), img.height()), (w, h));
        assert_eq!(img.get_pixel(2, 1).0, [5 * 40, 250, 7]);
    }

    #[test]
    fn checksums() {
        assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }
}
