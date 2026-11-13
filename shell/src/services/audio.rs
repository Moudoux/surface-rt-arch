//! Output volume of the default audio sink (PipeWire + WirePlumber on the Surface), through the `wpctl` and
//! `pactl` command-line tools (DESKTOP-SHELL.md Decisions, 2026-11-12).
//!
//! A blocking "audio" thread of its own (each tool call takes 40-230 ms on the Surface; the zbus thread and
//! the compositor never wait for one). It reads the volume at start, applies the UI's commands (several
//! volume sets that queued up while one ran are merged into the last), and re-reads after every change.
//! Changes made elsewhere (another mixer, a headset plugged in, the default sink switched) come from a
//! long-running `pactl subscribe` on a second thread; its sink/server events trigger a re-read.
//!
//! The volume is wpctl's value: 0-1 on a cubic scale (pactl shows the same number as a percentage), capped
//! at 1.0 here (no over-amplification).

use std::{
    io::{BufRead, BufReader},
    os::unix::process::CommandExt,
    process::{Command as Proc, Stdio},
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
    time::{Duration, Instant},
};

use smithay::reexports::calloop::ping::Ping;
use tracing::{debug, info, warn};

use super::{Shared, Volume};

const SINK: &str = "@DEFAULT_AUDIO_SINK@";

pub(super) enum Msg {
    /// 0-1.
    Set(f32),
    /// Some = mute / unmute, None = toggle.
    Mute(Option<bool>),
    /// Something changed outside (pactl subscribe): read again.
    Changed,
}

/// Start the audio threads; the sender takes the UI's volume commands.
pub(super) fn spawn(shared: Arc<Shared>, ping: Ping) -> Option<Sender<Msg>> {
    let (tx, rx) = channel();
    let sub_tx = tx.clone();
    let res = std::thread::Builder::new().name("audio".into()).spawn(move || run(rx, shared, ping));
    if let Err(err) = res {
        warn!(%err, "audio: cannot start thread");
        return None;
    }
    let res = std::thread::Builder::new().name("audio-watch".into()).spawn(move || subscribe(sub_tx));
    if let Err(err) = res {
        warn!(%err, "audio: cannot start the change watcher");
    }
    Some(tx)
}

fn run(rx: Receiver<Msg>, shared: Arc<Shared>, ping: Ping) {
    let t0 = Instant::now();
    let mut last = read();
    info!(ms = t0.elapsed().as_millis() as u64, volume = ?last, "audio: ready");
    publish(&shared, &ping, last);
    while let Ok(first) = rx.recv() {
        // Everything that queued up while the last call ran: only the last volume set counts.
        let mut set = None;
        let mut mutes = Vec::new();
        let mut changed = false;
        for m in std::iter::once(first).chain(rx.try_iter()) {
            match m {
                Msg::Set(v) => set = Some(v.clamp(0.0, 1.0)),
                Msg::Mute(m) => mutes.push(m),
                Msg::Changed => changed = true,
            }
        }
        let acted = set.is_some() || !mutes.is_empty();
        if let Some(v) = set {
            wpctl(&["set-volume", SINK, &format!("{v:.4}")]);
        }
        for m in mutes {
            let arg = match m {
                Some(true) => "1",
                Some(false) => "0",
                None => "toggle",
            };
            wpctl(&["set-mute", SINK, arg]);
        }
        if acted || changed {
            let now = read();
            if now != last {
                debug!(volume = ?now, "audio: changed");
                last = now;
                publish(&shared, &ping, last);
            }
        }
    }
}

/// Put the volume into the shared snapshot and wake the event loop.
fn publish(shared: &Shared, ping: &Ping, v: Option<Volume>) {
    let mut g = shared.sys.lock().unwrap();
    if g.1.volume != v {
        g.0 += 1;
        g.1.volume = v;
        drop(g);
        ping.ping();
    }
}

fn wpctl(args: &[&str]) -> Option<String> {
    let out = match Proc::new("wpctl").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output() {
        Ok(o) => o,
        Err(err) => {
            debug!(%err, "audio: cannot run wpctl");
            return None;
        }
    };
    if !out.status.success() {
        debug!(?args, status = ?out.status, "audio: wpctl failed");
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read() -> Option<Volume> {
    parse(&wpctl(&["get-volume", SINK])?)
}

/// `wpctl get-volume` output: "Volume: 0.10" or "Volume: 0.10 [MUTED]".
fn parse(s: &str) -> Option<Volume> {
    let rest = s.trim().strip_prefix("Volume:")?.trim();
    let num = rest.split_whitespace().next()?;
    let level: f32 = num.parse().ok()?;
    Some(Volume { level: level.clamp(0.0, 1.0), muted: rest.contains("[MUTED]") })
}

/// Does a `pactl subscribe` line mean the default sink's volume may have changed? Sink changes, new or
/// removed sinks, and server changes (the default sink switched). Not sink-inputs (app streams).
fn relevant(line: &str) -> bool {
    line.starts_with("Event ") && (line.contains(" on sink #") || line.contains(" on server"))
}

/// Watch for outside changes; restarts `pactl subscribe` if it ends (the audio server restarted).
fn subscribe(tx: Sender<Msg>) {
    let mut delay = Duration::from_secs(1);
    loop {
        let child = unsafe {
            Proc::new("pactl")
                .arg("subscribe")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                // Ends with rtshell (else it would live on until its next write fails).
                // SAFETY: prctl is async-signal-safe.
                .pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                })
                .spawn()
        };
        let mut child = match child {
            Ok(c) => c,
            Err(err) => {
                warn!(%err, "audio: cannot run pactl subscribe; outside volume changes will not show");
                return;
            }
        };
        let started = Instant::now();
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if relevant(&line) && tx.send(Msg::Changed).is_err() {
                    let _ = child.kill();
                    return;
                }
            }
        }
        let _ = child.wait();
        // Ran for a while: the server went away; else back off.
        delay = if started.elapsed() > Duration::from_secs(30) { Duration::from_secs(1) } else { (delay * 2).min(Duration::from_secs(30)) };
        debug!(secs = delay.as_secs(), "audio: pactl subscribe ended; restarting");
        std::thread::sleep(delay);
        let _ = tx.send(Msg::Changed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wpctl() {
        assert_eq!(parse("Volume: 0.10\n"), Some(Volume { level: 0.10, muted: false }));
        assert_eq!(parse("Volume: 0.45 [MUTED]\n"), Some(Volume { level: 0.45, muted: true }));
        assert_eq!(parse("Volume: 1.50\n"), Some(Volume { level: 1.0, muted: false }));
        assert_eq!(parse("Translate ID error: '@DEFAULT_AUDIO_SINK@' is not a valid ID\n"), None);
    }

    #[test]
    fn filters_events() {
        assert!(relevant("Event 'change' on sink #47"));
        assert!(relevant("Event 'change' on server #-1"));
        assert!(relevant("Event 'new' on sink #52"));
        assert!(!relevant("Event 'change' on sink-input #80"));
        assert!(!relevant("Event 'new' on client #71"));
        assert!(!relevant("Event 'change' on card #43"));
    }
}
