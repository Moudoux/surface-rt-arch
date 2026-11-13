//! rtshell: a touch-first Wayland compositor and shell for the Surface RT. See DESKTOP-SHELL.md.
//!
//! Layers (3.6): compositor core (`backend/`, `state.rs`, `render/`, `input/`, `policy.rs` = the interface
//! to window policies) / window policy (`wm/`) / shell UI + theme tokens (`theme/`, later `ui/`, `anim/`).
//! This file is the composition root: it picks the backend, the policy and the theme and wires them up.

// The nested-only build does not use the udev-only paths (input config, presentation/dmabuf feedback).
#![cfg_attr(not(feature = "udev"), allow(dead_code))]

mod anim;
mod apps;
mod backend;
mod boost;
mod config;
mod gesture;
mod idle;
mod overview;
mod input;
mod lock;
mod overlay;
mod policy;
mod services;
mod render;
mod rotate;
mod state;
mod switch;
mod theme;
mod ui;
mod watch;
mod winconf;
#[cfg(feature = "xwayland")]
mod xwayland;
mod wm;

use std::path::PathBuf;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
use tracing::{error, info};

use crate::{config::Config, state::State};

const USAGE: &str = "\
usage: rtshell [options]
  --nested           run in a window on the host (needs the `winit` feature)
  --config PATH      config file (default ~/.config/rtshell/config.toml)
  --policy NAME      window policy (default: fullscreen)
  --journal          log to the systemd journal instead of stderr
  --no-autostart     do not start apps.autostart
  --locked           start with the session locked (rtshell-session passes it after a crash while locked)
  --greeter          the login screen under greetd (GREETD_SOCK): no apps, never unlocks; config from
                     /etc/rtshell/config.toml unless --config
  --demo             show the render-kit test scene over everything instead of Home (touch/mouse go to it)
  --demo-auto        the same, animating by itself (for fps measurements)
  -c, --command CMD  also start CMD (whitespace-separated) once running
  -h, --help
Environment: RUST_LOG (default info), RTSHELL_DRM_DEVICE, RTSHELL_NO_SCANOUT, RTSHELL_NO_OVERLAY,
  RTSHELL_INPUT_SCRIPT (scripted touch/keys for tests, see src/input/script.rs).";

struct Args {
    nested: bool,
    config: Option<PathBuf>,
    policy: String,
    journal: bool,
    autostart: bool,
    locked: bool,
    greeter: bool,
    commands: Vec<Vec<String>>,
    /// None, Some(false) = --demo, Some(true) = --demo-auto
    demo: Option<bool>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        nested: false,
        config: None,
        policy: "fullscreen".into(),
        journal: false,
        autostart: true,
        locked: false,
        greeter: false,
        commands: Vec::new(),
        demo: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--nested" => a.nested = true,
            // Accepted for run.sh compatibility with anvil's flag.
            "--tty-udev" => a.nested = false,
            "--config" => a.config = Some(it.next().ok_or("--config needs a path")?.into()),
            "--policy" => a.policy = it.next().ok_or("--policy needs a name")?,
            "--journal" => a.journal = true,
            "--no-autostart" => a.autostart = false,
            "--locked" => a.locked = true,
            "--greeter" => a.greeter = true,
            "--demo" => a.demo = Some(false),
            "--demo-auto" => a.demo = Some(true),
            "-c" | "--command" => {
                let cmd = it.next().ok_or("--command needs a command")?;
                a.commands.push(cmd.split_whitespace().map(String::from).collect());
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    Ok(a)
}

fn init_logging(journal: bool) {
    use tracing_subscriber::{EnvFilter, prelude::*};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if journal {
        match tracing_journald::layer() {
            Ok(layer) => {
                tracing_subscriber::registry()
                    .with(filter)
                    .with(layer.with_syslog_identifier("rtshell".into()))
                    .init();
                return;
            }
            Err(err) => eprintln!("rtshell: journald unavailable ({err}), logging to stderr"),
        }
    }
    use std::io::IsTerminal;
    // Plain text when logging to a file (run.sh writes ~/rtshell-<n>.log).
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

/// Block SIGTERM/SIGINT/SIGUSR1/SIGUSR2 before any thread exists (Mesa/EGL may start worker threads during init): every
/// later thread inherits the mask, so the signals can only arrive through calloop's signalfd in the loop.
fn block_exit_signals() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::sigaddset(&mut set, libc::SIGUSR2);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

fn main() {
    apps::add_local_bin_to_path();
    render::preload::mark_start();
    block_exit_signals();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    init_logging(args.journal);
    info!(version = env!("CARGO_PKG_VERSION"), "rtshell starting");

    if let Err(err) = run(args) {
        error!("{err}");
        std::process::exit(1);
    }
    if state::RESTART.load(std::sync::atomic::Ordering::Relaxed) {
        info!(status = state::RESTART_EXIT, "rtshell stopped for a restart");
        std::process::exit(state::RESTART_EXIT);
    }
    info!("rtshell stopped");
}

fn run(mut args: Args) -> Result<(), String> {
    if args.greeter {
        // The greeter runs as greetd's user: no home, no apps; the system-wide config or the defaults.
        args.config = args.config.or_else(|| Some(config::GREETER_PATH.into()));
        args.autostart = false;
        args.commands.clear();
        apps::disable();
        info!("greeter mode");
    }
    let mut config = Config::load(args.config.clone());
    if args.greeter {
        // The session's lock image, shared in /var/lib/rtshell (render::login_wall), wins over the config's.
        render::wallpaper::set_cache_dir(render::login_wall::cache_dir());
        if let Some(img) = render::login_wall::image() {
            info!(path = %img.display(), "login wallpaper");
            config.wallpaper = img.to_string_lossy().into_owned();
            config.lock_wallpaper.clear();
        }
    }
    config.keyboard = config.keyboard.resolved();
    let theme = config.theme();
    // Fonts and the icon theme load while DRM/EGL initialize (the first UI frame waited ~0.65 s for fonts).
    if config.style == "rt" || config.style == "ipad" || args.greeter || args.demo.is_some() {
        render::preload::start(&theme.font.ui, &theme.font.mono, &theme.icons.theme);
    }
    let split = wm::SplitMetrics {
        divider: theme.split.divider as i32,
        grab_margin: theme.split.grab_margin as i32,
        snaps: {
            let mut s: Vec<f64> = theme.split.snaps.iter().copied().filter(|r| *r > 0.0 && *r < 1.0).collect();
            s.sort_by(f64::total_cmp);
            if s.is_empty() { vec![0.5] } else { s }
        },
        dismiss: theme.split.dismiss,
        double_tap: std::time::Duration::from_millis(theme.split.double_tap_ms as u64),
    };
    let policy = wm::by_name(&args.policy, split).ok_or_else(|| format!("unknown policy {:?}", args.policy))?;
    info!(policy = policy.name(), theme = %theme.name, "setup");

    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let display: Display<State> = Display::new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();
    let signal = event_loop.get_signal();

    if args.nested {
        #[cfg(feature = "winit")]
        {
            let mut state = State::new(display, handle, signal, config, theme, policy, "winit");
            backend::winit::init(&mut state, &mut event_loop)?;
            return run_loop(event_loop, state, &args);
        }
        #[cfg(not(feature = "winit"))]
        return Err("built without the `winit` feature; --nested is unavailable".into());
    }

    #[cfg(feature = "udev")]
    {
        let (session, notifier) = backend::udev::new_session()?;
        use smithay::backend::session::Session;
        let seat = session.seat();
        let mut state = State::new(display, handle, signal, config, theme, policy, &seat);
        backend::udev::init(&mut state, &mut event_loop, session, notifier)?;
        run_loop(event_loop, state, &args)
    }
    #[cfg(not(feature = "udev"))]
    {
        let _ = (display, handle, signal, config, theme, policy);
        Err("built without the `udev` feature; use --nested".into())
    }
}

fn run_loop(mut event_loop: EventLoop<'static, State>, mut state: State, args: &Args) -> Result<(), String> {
    // SIGTERM (run.sh -t, pkill) and SIGINT end the loop normally, so DRM state is torn down cleanly.
    // Must come before any thread is spawned (app reapers): calloop blocks the signals only in threads
    // created afterwards, and a thread that still has them unblocked gets the default action (death).
    // SIGUSR1: debug screenshot of the output (render/screenshot.rs), e.g. `pkill -USR1 -x rtshell`.
    use calloop::signals::Signal;
    // SIGUSR2: burst dump of the next RTSHELL_BURST (default 12) dmabuf commits (state.rs start_burst).
    let signals = calloop::signals::Signals::new(&[Signal::SIGTERM, Signal::SIGINT, Signal::SIGUSR1, Signal::SIGUSR2])
        .map_err(|e| e.to_string())?;
    event_loop
        .handle()
        .insert_source(signals, |event, _, state| {
            if event.signal() == Signal::SIGUSR1 {
                state.take_screenshot();
                return;
            }
            if event.signal() == Signal::SIGUSR2 {
                let n = std::env::var("RTSHELL_BURST").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
                state.start_burst(n);
                return;
            }
            info!(signal = ?event.signal(), "stopping");
            state.loop_signal.stop();
        })
        .map_err(|e| e.to_string())?;
    let theme = state.theme.clone();
    let wallpaper = state.config.wallpaper.clone();
    let lock_wallpaper = state.config.lock_wallpaper.clone();
    let kb_layout = state.config.keyboard.layout.clone();
    state.greeter = args.greeter;
    // A style this build does not have (a theme made for a newer one) falls back to the default, so the
    // session never comes up without a shell UI.
    if !ui::STYLES.contains(&state.config.style.as_str()) {
        tracing::warn!(style = %state.config.style, known = ?ui::STYLES, "unknown style; using rt");
        state.config.style = "rt".into();
    }
    match (args.demo, state.config.style.as_str()) {
        (None, _) if args.greeter => {
            let osk = state.config.osk.enabled;
            let lock_config = state.config.lock.clone();
            let greeter_config = state.config.greeter.clone();
            let sock = std::env::var_os("GREETD_SOCK").map(PathBuf::from);
            state.set_overlay(move |ping| {
                let services = services::Services::spawn(ping.clone());
                let lock = ui::lock::LockScreen::greeter(&theme, &lock_config, &greeter_config, sock, Some(services.clone()), ping.clone());
                let mut ui = ui::shell::ShellUi::new(theme.clone(), &wallpaper, &lock_wallpaper, ping, ui::shell::Blank::new()).with_lock(lock).with_services(services);
                if osk {
                    ui = ui.with_osk(ui::osk::Keyboard::new(&theme).with_layout(&kb_layout));
                }
                Box::new(ui)
            })
        }
        (Some(auto), _) => state.set_overlay(move |ping| {
            let demo = ui::demo::Demo::new(&theme, auto);
            Box::new(ui::shell::ShellUi::new(theme, &wallpaper, "", ping, demo))
        }),
        (None, style @ ("rt" | "ipad")) => {
            let config_path = args.config.clone();
            let osk = state.config.osk.enabled;
            let lock_config = state.config.lock.clone();
            let ipad = style == "ipad";
            state.set_overlay(move |ping| {
                let services = services::Services::spawn(ping.clone());
                let cc = ui::control::ControlCenter::new(&theme, services.clone());
                let lock = ui::lock::LockScreen::new(&theme, &lock_config, Some(services.clone()), ping.clone());
                let osk = osk.then(|| ui::osk::Keyboard::new(&theme).with_layout(&kb_layout));
                // The style picks the Home Scene; the other layers take their looks from the theme.
                fn finish<S: ui::shell::Scene + 'static>(
                    ui: ui::shell::ShellUi<S>,
                    config_path: Option<PathBuf>,
                    cc: ui::control::ControlCenter,
                    lock: ui::lock::LockScreen,
                    services: services::Services,
                    osk: Option<ui::osk::Keyboard>,
                ) -> Box<dyn overlay::Overlay> {
                    // Only the real session shares its lock image with the login screen (not test runs with
                    // their own --config).
                    let ui = if config_path.is_none() { ui.with_login_sync() } else { ui };
                    let mut ui = ui.watch_config(config_path).with_settings().with_panel(cc).with_lock(lock).with_services(services);
                    if let Some(kb) = osk {
                        ui = ui.with_osk(kb);
                    }
                    Box::new(ui)
                }
                if ipad {
                    let home = ui::springboard::Springboard::new(&theme, ping.clone()).with_services(services.clone());
                    finish(ui::shell::ShellUi::new(theme.clone(), &wallpaper, &lock_wallpaper, ping, home), config_path, cc, lock, services, osk)
                } else {
                    let home = ui::home::Home::new(&theme, ping.clone()).with_services(services.clone());
                    finish(ui::shell::ShellUi::new(theme.clone(), &wallpaper, &lock_wallpaper, ping, home), config_path, cc, lock, services, osk)
                }
            })
        }
        (None, "none") => {}
        (None, other) => tracing::warn!(style = other, "unknown style; no shell UI"),
    }
    if args.locked || args.greeter {
        // Before the first frame: a shell restarted after a crash while locked never shows the desktop.
        info!("starting locked (--locked)");
        state.lock_session(overlay::LockReason::Sleep);
        if !state.locked() {
            tracing::error!("--locked/--greeter but the session could not be locked (no lock screen); quitting");
            return Err("cannot start locked".into());
        }
    }
    // RTSHELL_INPUT_SCRIPT: scripted touch/keys for tests (input/script.rs), after startup settles.
    input::script::start(&mut state, std::time::Duration::from_millis(1500));
    if args.autostart {
        for cmd in state.config.apps.autostart.clone() {
            state.spawn(&cmd);
        }
    }
    for cmd in &args.commands {
        state.spawn(cmd);
    }
    state.idle_arm();
    // X11 apps: Xwayland a little after the first frame (it must not slow down login), never in the greeter.
    #[cfg(feature = "xwayland")]
    if state.config.apps.xwayland && !args.greeter {
        let _ = state.loop_handle.insert_source(calloop::timer::Timer::from_duration(std::time::Duration::from_secs(2)), |_, _, state| {
            state.start_xwayland();
            calloop::timer::TimeoutAction::Drop
        });
    }
    info!(socket = %state.socket_name, "running; WAYLAND_DISPLAY for clients");
    // No timeout: the loop sleeps until an fd or timer fires (idle CPU 0 %).
    event_loop
        .run(None, &mut state, |state| {
            state.space.refresh();
            state.popups.cleanup();
            let _ = state.display_handle.flush_clients();
        })
        .map_err(|e| e.to_string())?;
    apps::terminate_all();
    Ok(())
}
