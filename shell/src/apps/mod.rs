//! Starting apps: commands from the config and from `.desktop` files (`desktop.rs`), each in its own
//! process group, with the session environment and (for launches from the shell UI) an xdg-activation token.

pub mod desktop;

use std::{
    collections::BTreeMap,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use std::sync::Mutex;

use tracing::{info, warn};

/// Process groups of apps we started that may still have members (each app gets its own group). A group
/// stays listed after its leader exits while other members are alive (launchers, Electron helpers).
static RUNNING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// How long apps get to quit after SIGTERM before SIGKILL. Must stay below run.sh's `timeout -k 3`, which
/// SIGKILLs the compositor itself 3 s after its SIGTERM.
const QUIT_GRACE: Duration = Duration::from_secs(2);

/// Whether any process is left in process group `pgid` (signal 0 only checks).
fn group_alive(pgid: u32) -> bool {
    unsafe { libc::kill(-(pgid as i32), 0) == 0 }
}

/// End every app we started, as when a session ends: SIGTERM to each process group, then SIGKILL to groups
/// still alive after QUIT_GRACE. Needed because some clients keep running after the compositor goes away,
/// and VSCodium even spins at 100 % CPU on a failed shutdown, holding its profile lock (notes/13).
pub fn terminate_all() {
    let mut groups = std::mem::take(&mut *RUNNING.lock().unwrap());
    groups.retain(|&pgid| group_alive(pgid));
    for &pgid in &groups {
        info!(pgid, "asking app to quit (SIGTERM)");
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGTERM);
        }
    }
    let deadline = Instant::now() + QUIT_GRACE;
    while !groups.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        groups.retain(|&pgid| group_alive(pgid));
    }
    for pgid in groups {
        warn!(pgid, grace_s = QUIT_GRACE.as_secs(), "app did not quit; SIGKILL");
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGKILL);
        }
    }
}

/// Environment every app gets.
#[derive(Debug, Clone)]
pub struct AppEnv {
    pub wayland_display: String,
    pub extra: BTreeMap<String, String>,
    /// Xwayland's display number once it is ready (`DISPLAY=:N` for X11 apps); None = no DISPLAY.
    pub x11_display: Option<u32>,
}

/// Set once in greeter mode: nothing may start apps (the login screen runs as greetd's user).
static NO_APPS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn disable() {
    NO_APPS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Started from a text VT (or greetd), PATH lacks ~/.local/bin, where the codium and chromium wrappers live.
/// Prepend it for the whole process, so apps, `xdg-mime` and `xdg-open` all resolve those wrappers. Without it,
/// `xdg-mime query default` skips a default whose Exec is not on PATH (chromium) and reports Firefox instead.
///
/// Must run first thing in `main`, before any thread exists (`set_var` is only sound single-threaded).
pub fn add_local_bin_to_path() {
    let (Some(home), Some(path)) = (std::env::var_os("HOME"), std::env::var_os("PATH")) else { return };
    let local = std::path::Path::new(&home).join(".local/bin");
    if std::env::split_paths(&path).any(|p| p == local) {
        return;
    }
    if let Ok(joined) = std::env::join_paths(std::iter::once(local).chain(std::env::split_paths(&path))) {
        // SAFETY: called at the top of `main`, before any other thread is started.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

pub fn spawn(cmd: &[String], env: &AppEnv) {
    spawn_with(cmd, env, None, None);
}

/// Start `cmd` in `cwd` (if it exists); `token` is an xdg-activation token handed over as
/// XDG_ACTIVATION_TOKEN and DESKTOP_STARTUP_ID so the app can activate itself when its window appears.
pub fn spawn_with(cmd: &[String], env: &AppEnv, cwd: Option<&std::path::Path>, token: Option<&str>) {
    if NO_APPS.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(?cmd, "greeter mode: no apps are started");
        return;
    }
    let Some((prog, args)) = cmd.split_first() else {
        return;
    };
    // PATH already holds ~/.local/bin: `add_local_bin_to_path` runs at start-up.
    let mut command = Command::new(prog);
    command
        .args(args)
        .envs(&env.extra)
        .env("WAYLAND_DISPLAY", &env.wayland_display)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", desktop::DESKTOP_NAME)
        .env_remove("DISPLAY")
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID")
        .stdin(Stdio::null())
        // Own process group: a signal to the compositor's group (e.g. Ctrl+C on the VT) does not hit apps.
        .process_group(0);
    if let Some(dir) = cwd.filter(|d| d.is_dir()) {
        command.current_dir(dir);
    }
    if let Some(t) = token {
        command.env("XDG_ACTIVATION_TOKEN", t).env("DESKTOP_STARTUP_ID", t);
    }
    if let Some(n) = env.x11_display {
        command.env("DISPLAY", format!(":{n}"));
    }
    // The compositor blocks SIGTERM/SIGINT (calloop signal source) and the mask is inherited across exec:
    // unblock everything in the child, or apps could not be stopped with kill.
    unsafe {
        command.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            info!(cmd = ?cmd, pid, "started app");
            {
                let mut running = RUNNING.lock().unwrap();
                running.retain(|&pgid| group_alive(pgid));
                running.push(pid);
            }
            // Reap it so it does not stay a zombie. One small thread per running app is fine for a handful.
            let name = prog.clone();
            let spawned = std::thread::Builder::new()
                .name("reap".into())
                .stack_size(32 * 1024)
                .spawn(move || {
                    match child.wait() {
                        Ok(status) => info!(app = %name, %status, "app exited"),
                        Err(err) => warn!(app = %name, %err, "wait failed"),
                    }
                    // Forget the group only once it is empty (children may outlive the leader).
                    if !group_alive(pid) {
                        RUNNING.lock().unwrap().retain(|p| *p != pid);
                    }
                });
            if let Err(err) = spawned {
                warn!(%err, "could not start reaper thread");
            }
        }
        Err(err) => warn!(cmd = ?cmd, %err, "could not start app"),
    }
}
