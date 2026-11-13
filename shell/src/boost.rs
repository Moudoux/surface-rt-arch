//! Input boost: raise the CPU and memory-controller clock floors while the user interacts, so the first
//! frames of an animation do not run on idle clocks (DESKTOP-SHELL.md Phase 4, notes/17).
//!
//! On the Surface the CPU runs `ondemand` (51 MHz-1.4 GHz) and the memory controller follows
//! `tegra_actmon` devfreq (12.75-750 MHz). Both drop while the screen is still, and the first frame after a
//! pause (a switch, the Overview leaving) took ~20 ms instead of ~7. The shell writes the maximum into the
//! `min_freq` files on input and restores the old floors after `HOLD` without input or animation.
//!
//! The files are root-only by default; `build/shell/surface-rt-boost.conf` (tmpfiles.d) lets the `video`
//! group write them. Without it the boost logs once and does nothing. `RTSHELL_BOOST=0` disables it.
//! Writes happen on a worker thread: a clock/voltage change can take milliseconds.

use std::{
    path::PathBuf,
    sync::mpsc::{Sender, channel},
    time::{Duration, Instant},
};

use tracing::{info, warn};

/// How long the floors stay raised after the last input (animations extend it while they run).
pub const HOLD: Duration = Duration::from_millis(600);

const CPU: &str = "/sys/devices/system/cpu/cpufreq/policy0";
const EMC: &str = "/sys/class/devfreq/6000c800.actmon";

struct Knob {
    path: PathBuf,
    low: String,
    high: String,
}

fn read(dir: &str, file: &str) -> Option<String> {
    Some(std::fs::read_to_string(PathBuf::from(dir).join(file)).ok()?.trim().to_string())
}

impl Knob {
    /// The floor `min_file` is set to `max_file`'s value while boosted, and back to the hardware minimum
    /// after (not to the value found at start: after a crash that could be the boosted one).
    fn new(dir: &str, min_file: &str, max_file: &str, low: Option<String>) -> Option<Knob> {
        Some(Knob { path: PathBuf::from(dir).join(min_file), low: low?, high: read(dir, max_file)? })
    }

    fn writable(&self) -> bool {
        std::fs::OpenOptions::new().write(true).open(&self.path).is_ok()
    }

    fn set(&self, high: bool) {
        let v = if high { &self.high } else { &self.low };
        if let Err(err) = std::fs::write(&self.path, v) {
            warn!(path = %self.path.display(), %err, "boost write failed");
        }
    }
}

pub struct Boost {
    tx: Option<Sender<bool>>,
    knobs: std::sync::Arc<Vec<Knob>>,
    on: bool,
    /// Input keeps the boost until this time.
    pub until: Option<Instant>,
}

impl Boost {
    pub fn new() -> Boost {
        let off = Boost { tx: None, knobs: Default::default(), on: false, until: None };
        if std::env::var_os("RTSHELL_BOOST").is_some_and(|v| v == "0") {
            info!("input boost off (RTSHELL_BOOST=0)");
            return off;
        }
        let emc_low = read(EMC, "available_frequencies")
            .and_then(|v| v.split_whitespace().filter_map(|f| f.parse::<u64>().ok()).min())
            .map(|f| f.to_string());
        let knobs: Vec<Knob> = [
            Knob::new(CPU, "scaling_min_freq", "cpuinfo_max_freq", read(CPU, "cpuinfo_min_freq")),
            Knob::new(EMC, "min_freq", "max_freq", emc_low),
        ]
        .into_iter()
        .flatten()
        .collect();
        if knobs.is_empty() || !knobs.iter().all(Knob::writable) {
            info!("input boost unavailable: clock floors not writable (install build/shell/surface-rt-boost.conf)");
            return off;
        }
        for k in &knobs {
            info!(path = %k.path.display(), low = %k.low, high = %k.high, "input boost");
        }
        let knobs = std::sync::Arc::new(knobs);
        let worker = knobs.clone();
        let (tx, rx) = channel::<bool>();
        let spawned = std::thread::Builder::new().name("boost".into()).spawn(move || {
            while let Ok(mut high) = rx.recv() {
                // Only the latest request matters.
                while let Ok(next) = rx.try_recv() {
                    high = next;
                }
                for k in worker.iter() {
                    k.set(high);
                }
            }
        });
        match spawned {
            Ok(_) => Boost { tx: Some(tx), knobs, on: false, until: None },
            Err(err) => {
                warn!(%err, "boost thread");
                off
            }
        }
    }

    pub fn available(&self) -> bool {
        self.tx.is_some()
    }

    /// Input happened: raise the floors (if not already) and keep them for `HOLD`.
    pub fn kick(&mut self) -> bool {
        let Some(tx) = self.tx.as_ref() else { return false };
        self.until = Some(Instant::now() + HOLD);
        if !self.on {
            self.on = true;
            let _ = tx.send(true);
            return true;
        }
        false
    }

    /// Restore the old floors.
    pub fn release(&mut self) {
        if self.on && let Some(tx) = self.tx.as_ref() {
            self.on = false;
            self.until = None;
            let _ = tx.send(false);
        }
    }
}

impl Drop for Boost {
    fn drop(&mut self) {
        // The shell is exiting: restore the floors here (the worker may not get to run again).
        if self.on {
            for k in self.knobs.iter() {
                k.set(false);
            }
        }
    }
}
