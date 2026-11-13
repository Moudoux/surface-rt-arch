//! Directory watching with inotify on a worker thread (core utility).
//!
//! `spawn` watches the directories `dirs()` returns and calls `changed()` on the worker thread once things
//! have been quiet for `DEBOUNCE` after a burst of events (package updates touch many files at once). The
//! directory list is recomputed after every change, so directories created later get watched too (watch
//! the parent of a directory that may not exist yet). No thread wakes while nothing changes.

use std::{ffi::CString, os::unix::ffi::OsStrExt, path::PathBuf, time::Duration};

use tracing::{debug, warn};

const DEBOUNCE: Duration = Duration::from_millis(300);

const MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_CLOSE_WRITE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF;

struct Inotify {
    fd: i32,
    watches: Vec<i32>,
}

impl Inotify {
    fn new() -> Option<Inotify> {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        (fd >= 0).then_some(Inotify { fd, watches: Vec::new() })
    }

    fn rewatch(&mut self, dirs: &[PathBuf]) {
        for wd in self.watches.drain(..) {
            unsafe { libc::inotify_rm_watch(self.fd, wd) };
        }
        for d in dirs {
            let Ok(c) = CString::new(d.as_os_str().as_bytes()) else { continue };
            let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), MASK) };
            if wd >= 0 {
                self.watches.push(wd);
            }
        }
    }

    /// Wait up to `timeout` (None = forever) for events and drain them. True if there were any.
    fn wait(&self, timeout: Option<Duration>) -> bool {
        let mut pfd = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
        let ms = timeout.map(|t| t.as_millis() as i32).unwrap_or(-1);
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n <= 0 {
            return false;
        }
        let mut buf = [0u8; 4096];
        unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
        true
    }
}

impl Drop for Inotify {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

pub fn spawn(
    name: &str,
    dirs: impl Fn() -> Vec<PathBuf> + Send + 'static,
    mut changed: impl FnMut() + Send + 'static,
) {
    let name = name.to_string();
    let spawned = std::thread::Builder::new().name(format!("watch-{name}")).stack_size(256 * 1024).spawn(move || {
        let Some(mut ino) = Inotify::new() else {
            warn!(%name, "inotify unavailable; no live refresh");
            return;
        };
        let mut watched: Vec<PathBuf> = Vec::new();
        loop {
            let mut list: Vec<PathBuf> = dirs().into_iter().filter(|d| d.is_dir()).collect();
            list.sort();
            list.dedup();
            if list != watched {
                ino.rewatch(&list);
                // Removing the old watches queued IN_IGNORED events: drop them, or they would wake us at once.
                while ino.wait(Some(Duration::ZERO)) {}
                debug!(%name, dirs = list.len(), "watching");
                watched = list;
            }
            if !ino.wait(None) {
                continue;
            }
            while ino.wait(Some(DEBOUNCE)) {}
            changed();
        }
    });
    if let Err(err) = spawned {
        warn!(%err, "could not start watch thread");
    }
}
