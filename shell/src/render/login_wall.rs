//! The login screen's wallpaper. The greeter runs as greetd's user and cannot read the user's home, so the
//! session keeps a copy of its lock-screen image (else the desktop's) in `DIR`, a directory the user owns and
//! everyone may read (made once by root, see build/shell/rtshell-login-wallpaper.sh), plus the scaled RGBA
//! cache for both orientations, so the greeter neither decodes nor blocks. No `DIR` (or not writable): nothing
//! happens, and the greeter shows its own config's wallpaper.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

pub const DIR: &str = "/var/lib/rtshell";
const NAME: &str = "login-wallpaper";

/// The scaled-image cache base the greeter reads (`wallpaper::set_cache_dir`).
pub fn cache_dir() -> PathBuf {
    Path::new(DIR).join("cache")
}

/// The copy for the greeter, if the session left one.
pub fn image() -> Option<PathBuf> {
    std::fs::read_dir(DIR).ok()?.flatten().map(|e| e.path()).find(|p| is_ours(p))
}

fn is_ours(p: &Path) -> bool {
    p.file_stem().is_some_and(|s| s == NAME) && p.extension().is_some_and(|e| e != "tmp")
}

/// Bring `DIR` in line with `src` (None = no image: remove the copy) on a worker thread. `size` is the output
/// size; the cache is made for it and its rotation.
pub fn sync(src: Option<PathBuf>, size: (u32, u32)) {
    let _ = std::thread::Builder::new().name("login-wall".into()).spawn(move || {
        if let Err(err) = sync_now(src.as_deref(), size) {
            warn!(%err, "login wallpaper not updated");
        }
    });
}

fn sync_now(src: Option<&Path>, (w, h): (u32, u32)) -> Result<(), String> {
    let dir = Path::new(DIR);
    if !nix_access_w(dir) {
        debug!("no writable {DIR}: login wallpaper not shared");
        return Ok(());
    }
    let src = src.filter(|p| p.is_file());
    let current = image();
    let Some(src) = src else {
        if let Some(c) = current {
            std::fs::remove_file(&c).map_err(|e| e.to_string())?;
            clear_cache();
            info!("login wallpaper removed");
        }
        return Ok(());
    };
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    let dest = dir.join(format!("{NAME}.{ext}"));
    let sm = std::fs::metadata(src).map_err(|e| e.to_string())?;
    let same = current.as_deref() == Some(dest.as_path())
        && std::fs::metadata(&dest).is_ok_and(|d| d.len() == sm.len() && d.modified().ok() == sm.modified().ok());
    if !same {
        let tmp = dir.join(format!("{NAME}.tmp"));
        std::fs::copy(src, &tmp).map_err(|e| format!("copy: {e}"))?;
        // Same mtime as the source: the next sync sees it is up to date.
        if let Ok(t) = sm.modified() {
            let _ = std::fs::File::options().write(true).open(&tmp).and_then(|f| f.set_modified(t));
        }
        if let Some(c) = current.filter(|c| *c != dest) {
            let _ = std::fs::remove_file(c);
        }
        std::fs::rename(&tmp, &dest).map_err(|e| format!("rename: {e}"))?;
        clear_cache();
        info!(src = %src.display(), "login wallpaper updated");
    }
    let base = cache_dir();
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    for (cw, ch) in [(w, h), (h, w)] {
        crate::render::wallpaper::prepare_cache(&dest, cw, ch, &base)?;
    }
    Ok(())
}

fn clear_cache() {
    let _ = std::fs::remove_dir_all(cache_dir());
}

/// access(2) W_OK: the permission bits alone do not say whether this user may write.
fn nix_access_w(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}
