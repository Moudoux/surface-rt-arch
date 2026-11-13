//! System services over D-Bus (DESKTOP-SHELL.md 3.5 `services/`, Phase 5): battery (UPower), Wi-Fi
//! (NetworkManager), Bluetooth (BlueZ), brightness and power actions (logind). Output volume comes from
//! `audio.rs` (wpctl/pactl on a thread of its own) into the same snapshot.
//!
//! Everything runs on one thread ("services") inside `zbus::block_on`: the compositor thread never waits for
//! the bus. The thread keeps a `Sys` snapshot of what the shell shows; whenever it changes, the new
//! snapshot goes into a shared slot and the event loop is woken (the `Ping` the shell UI already polls on).
//! The UI sends `Command`s back through a small queue that wakes the thread.
//!
//! Change tracking: one match rule per service (PropertiesChanged from that service's bus name, plus BlueZ's
//! InterfacesAdded/Removed). A signal marks the service dirty; signals that arrived together are drained
//! first, then each dirty service is read again in full (a handful of property gets, all small). That is
//! simpler than tracking individual properties and keeps working when objects come and go (a new access
//! point, a device pairing). NetworkManager signals from objects we do not show (other access points during
//! a scan) are ignored by path.
//!
//! Secrets: nothing here reads Wi-Fi passwords or connection secrets, and commands are never logged with
//! their arguments where those could hold one.

use std::{
    collections::{HashMap, VecDeque},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};

use futures_util::{
    FutureExt, StreamExt,
    stream::{BoxStream, SelectAll},
    task::AtomicWaker,
};
use smithay::reexports::calloop::ping::Ping;
use tracing::{debug, info, warn};
use zbus::{Connection, MatchRule, MessageStream, OwnedObjectPath, OwnedVariant, Proxy, message::Type as MsgType, proxy::CacheProperties};

mod audio;
pub mod auth;
pub mod greetd;
mod notify;
mod read;

pub use notify::{Note, NoteEvent};

/// Lowest brightness the slider can set, as a fraction of the maximum (0 would switch the panel's backlight
/// off, which looks like a dead screen).
pub const MIN_BRIGHTNESS: f32 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Charge {
    #[default]
    Unknown,
    Charging,
    Discharging,
    Empty,
    Full,
    /// Plugged in but not charging (UPower "pending charge"/"pending discharge").
    Pending,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Battery {
    /// 0-100.
    pub percent: f32,
    pub charge: Charge,
    /// Seconds; 0 = unknown.
    pub time_to_empty: i64,
    pub time_to_full: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Link {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Wifi {
    /// The software switch (NetworkManager WirelessEnabled).
    pub enabled: bool,
    /// False if a hardware switch / rfkill blocks it.
    pub hw_enabled: bool,
    pub link: Link,
    /// The network in use (or being joined).
    pub ssid: Option<String>,
    /// 0-100.
    pub strength: u8,
    /// Networks in range (one entry per SSID, the strongest access point), the one in use first, then
    /// saved ones, then by strength. Hidden networks (no SSID) are left out.
    pub networks: Vec<Network>,
    /// A connection the shell started and its outcome (until the next one).
    pub join: Option<Join>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Security {
    #[default]
    Open,
    /// WPA/WPA2 personal (a password).
    Psk,
    /// Enterprise (802.1X), WEP or WPA3-only (SAE): not offered (the Surface's 88W8797 has no SAE).
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Network {
    pub ssid: String,
    /// 0-100.
    pub strength: u8,
    pub security: Security,
    /// NetworkManager has a saved connection for it (connects without asking for the password).
    pub saved: bool,
    /// In use (or being joined).
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinState {
    Connecting,
    Connected,
    /// The password was probably wrong (NM: no secrets / supplicant disconnect). A connection the shell had
    /// just added is deleted again, so the next try asks for the password again.
    WrongPassword,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub ssid: String,
    pub state: JoinState,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BtDevice {
    /// BlueZ object path (commands go there).
    pub path: String,
    pub name: String,
    pub icon: String,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bluetooth {
    /// The adapter's object path (commands go there).
    pub adapter: String,
    pub powered: bool,
    /// Paired devices, connected ones first.
    pub devices: Vec<BtDevice>,
    /// The device whose last Connect/Disconnect failed (until the next command).
    pub failed: Option<String>,
}

/// Output volume of the default audio sink.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Volume {
    /// 0-1 (wpctl's cubic scale).
    pub level: f32,
    pub muted: bool,
}

/// What the shell shows of the system. `None` = that service is not there (or not answering).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sys {
    pub battery: Option<Battery>,
    /// 0-1 of the backlight's maximum.
    pub brightness: Option<f32>,
    pub wifi: Option<Wifi>,
    pub bluetooth: Option<Bluetooth>,
    /// iio-sensor-proxy's accelerometer orientation ("normal", "left-up", "right-up", "bottom-up",
    /// "undefined"); None = no accelerometer. Only updated while claimed (`Command::Accelerometer(true)`).
    pub orientation: Option<String>,
    /// None = no audio server or no output (from the audio thread, not the bus).
    pub volume: Option<Volume>,
}

/// A Wi-Fi password on its way to NetworkManager. `Debug` never shows it (commands are logged), and the
/// text is overwritten when it is dropped. Nothing reads a password back from NetworkManager.
pub struct Secret(String);

impl Secret {
    pub fn new(s: String) -> Secret {
        Secret(s)
    }
    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Clone for Secret {
    fn clone(&self) -> Secret {
        Secret(self.0.clone())
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Secret) -> bool {
        self.0 == other.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Zero bytes are valid UTF-8, so the String stays well-formed.
        // SAFETY: only overwrites the bytes in place with 0.
        unsafe { self.0.as_bytes_mut() }.fill(0);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// 0-1 (clamped to `MIN_BRIGHTNESS`). Several in a row are merged: only the last is applied.
    SetBrightness(f32),
    SetWifi(bool),
    /// Ask NetworkManager for a fresh scan (it refuses while one ran a moment ago; that is fine).
    WifiScan,
    /// Join a network: its saved connection if there is one (and no password is given), else a new one
    /// (open, or WPA2 personal with `psk`).
    WifiConnect { ssid: String, psk: Option<Secret> },
    WifiDisconnect,
    /// Delete the saved connection(s) for this SSID.
    WifiForget(String),
    SetBluetooth(bool),
    /// Connect (true) or disconnect a paired device (its object path).
    BtConnect(String, bool),
    /// Claim (true) or release iio-sensor-proxy's accelerometer (it only reads the sensor while claimed).
    Accelerometer(bool),
    /// Output volume 0-1 (several in a row are merged: only the last is applied).
    SetVolume(f32),
    /// Mute (Some(true)), unmute, or toggle (None).
    SetMute(Option<bool>),
    /// A notification banner went away: dismissed by the user (true) or timed out.
    NoteClosed(u32, bool),
    Lock,
    Suspend,
    /// The power button was tapped: Suspend, unless the system resumed moments ago (the press that woke it
    /// can arrive after resume).
    PowerKey,
    Reboot,
    PowerOff,
    /// The lock screen is on the panel: let a pending sleep go ahead (drop the delay inhibitor). `Some(n)`
    /// = the fallback timer of sleep number n (ignored if that sleep is over).
    SleepReady(Option<u64>),
    /// Internal: a Network re-read that was held back by the rate limit is due.
    NetRefresh,
    /// logind's LockedHint for our session.
    SetLockedHint(bool),
    /// Idle dimming: `Some(level)` = keep the brightness and set `level` x it (not below
    /// `MIN_BRIGHTNESS`); `None` = bring the kept brightness back.
    Dim(Option<f32>),
    /// logind's IdleHint for our session (true while the screen is blank).
    SetIdleHint(bool),
    /// Sleep after idle: like `Suspend`, but a refusal is only logged (nobody is looking at the screen).
    IdleSuspend,
}

/// logind events for the lock screen (taken with `Services::take_session_events`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// `loginctl lock-session(s)`, or our own Session.Lock call.
    Lock,
    Unlock,
    /// The system is about to sleep (true; we hold a delay inhibitor until the lock is drawn) or woke up.
    PrepareForSleep(bool),
}

/// Shared between the services thread and the UI.
struct Shared {
    sys: Mutex<(u64, Sys)>,
    queue: Mutex<VecDeque<Command>>,
    waker: AtomicWaker,
    /// Notifications for the UI, oldest first (taken with `Services::take_notes`).
    notes: Mutex<Vec<NoteEvent>>,
    /// logind events for the lock screen, oldest first.
    session: Mutex<Vec<SessionEvent>>,
}

/// The UI's handle: read the latest snapshot, send commands. Cheap to clone.
#[derive(Clone)]
pub struct Services {
    shared: Arc<Shared>,
    /// The audio thread's queue (volume commands go there, not to the bus thread).
    audio: Option<std::sync::mpsc::Sender<audio::Msg>>,
}

impl Services {
    /// Start the services thread. `ping` wakes the event loop whenever the snapshot changes.
    pub fn spawn(ping: Ping) -> Services {
        let shared = Arc::new(Shared {
            sys: Mutex::new((0, Sys::default())),
            queue: Mutex::new(VecDeque::new()),
            waker: AtomicWaker::new(),
            notes: Mutex::new(Vec::new()),
            session: Mutex::new(Vec::new()),
        });
        let audio = audio::spawn(shared.clone(), ping.clone());
        let s = shared.clone();
        let res = std::thread::Builder::new().name("services".into()).spawn(move || {
            if let Err(err) = zbus::block_on(run(s, ping)) {
                warn!(%err, "services: system bus unavailable; no battery, network or power controls");
            }
        });
        if let Err(err) = res {
            warn!(%err, "services: cannot start thread");
        }
        Services { shared, audio }
    }

    /// The latest snapshot and its generation (bumped on every change).
    pub fn snapshot(&self) -> (u64, Sys) {
        self.shared.sys.lock().unwrap().clone()
    }

    /// Notifications that arrived (or were closed by their app) since the last call.
    pub fn take_notes(&self) -> Vec<NoteEvent> {
        std::mem::take(&mut *self.shared.notes.lock().unwrap())
    }

    /// logind Lock / Unlock / PrepareForSleep since the last call.
    pub fn take_session_events(&self) -> Vec<SessionEvent> {
        std::mem::take(&mut *self.shared.session.lock().unwrap())
    }

    pub fn generation(&self) -> u64 {
        self.shared.sys.lock().unwrap().0
    }

    pub fn send(&self, cmd: Command) {
        match cmd {
            Command::SetVolume(v) => return self.audio_send(audio::Msg::Set(v)),
            Command::SetMute(m) => return self.audio_send(audio::Msg::Mute(m)),
            _ => {}
        }
        self.shared.queue.lock().unwrap().push_back(cmd);
        self.shared.waker.wake();
    }
}

impl Services {
    fn audio_send(&self, m: audio::Msg) {
        if let Some(tx) = self.audio.as_ref()
            && tx.send(m).is_err()
        {
            debug!("services: audio thread gone");
        }
    }
}

/// The command queue as a stream for the services loop.
struct Commands(Arc<Shared>);

impl futures_util::Stream for Commands {
    type Item = Command;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Command>> {
        self.0.waker.register(cx.waker());
        match self.0.queue.lock().unwrap().pop_front() {
            Some(c) => Poll::Ready(Some(c)),
            None => Poll::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Svc {
    Battery,
    Network,
    Bluetooth,
    Sensor,
}

enum Ev {
    /// A signal from a service (the object path it came from).
    Signal(Svc, Option<String>),
    Cmd(Command),
    /// A method call to the notification server.
    Call(zbus::Message),
    /// A slow call that ran beside the loop finished (Bluetooth connect can take seconds).
    Done(Svc, Done),
    /// A logind signal for the lock screen.
    Session(SessionEvent),
}

/// Outcome of a call that ran beside the loop.
enum Done {
    BtConnect { path: String, ok: bool },
    /// NetworkManager activation started: the active connection's path, and the connection we added for it.
    Activated { ssid: String, active: OwnedObjectPath, added: Option<OwnedObjectPath> },
    ActivateFailed { ssid: String },
}

const UPOWER: &str = "org.freedesktop.UPower";
const NM: &str = "org.freedesktop.NetworkManager";
const NM_SETTINGS: &str = "/org/freedesktop/NetworkManager/Settings";
const BLUEZ: &str = "org.bluez";
const LOGIN: &str = "org.freedesktop.login1";
const SENSORS: &str = "net.hadess.SensorProxy";
const PROPS: &str = "org.freedesktop.DBus.Properties";
/// Low-battery notification: warn at LOW %, again (critical, stays) at CRITICAL %; re-armed once charging or
/// back above LOW + LOW_REARM.
const LOW: f32 = 10.0;
const CRITICAL: f32 = 5.0;
const LOW_REARM: f32 = 2.0;
/// The id of the shell's own low-battery notification (apps' ids count up from 1).
const LOW_BATTERY_ID: u32 = u32::MAX;

/// 0 = fine, 1 = low, 2 = critical. `prev` gives the hysteresis between LOW and LOW + LOW_REARM.
fn low_level(b: Option<&Battery>, prev: u8) -> u8 {
    let Some(b) = b else { return 0 };
    if !matches!(b.charge, Charge::Discharging | Charge::Empty) {
        return 0;
    }
    match b.percent {
        p if p <= CRITICAL => 2,
        p if p <= LOW => 1.max(prev),
        p if p <= LOW + LOW_REARM => prev.min(1),
        _ => 0,
    }
}

/// The notification for a low-battery level.
fn low_note(b: &Battery, level: u8) -> Note {
    let left = if b.time_to_empty > 0 {
        let m = (b.time_to_empty + 30) / 60;
        if m >= 60 { format!(" (about {} h {} min)", m / 60, m % 60) } else { format!(" (about {m} min)") }
    } else {
        String::new()
    };
    let critical = level >= 2;
    Note {
        id: LOW_BATTERY_ID,
        app: "Battery".into(),
        icon: if critical { "battery-caution".into() } else { "battery-low".into() },
        summary: if critical { "Battery critically low".into() } else { "Battery low".into() },
        body: format!("{:.0} % left{left}. Plug in the charger.", b.percent),
        urgency: if critical { 2 } else { 1 },
        timeout_ms: if critical { Some(0) } else { None },
        transient: false,
    }
}

/// The id of the shell's own "could not sleep / restart / power off" notification.
const POWER_FAILED_ID: u32 = u32::MAX - 1;

/// Shown when logind refuses a power action (e.g. rtshell runs outside the active seat session, where polkit
/// wants a password and there is no agent to ask for it).
fn power_failed_note(action: &str, denied: bool) -> Note {
    Note {
        id: POWER_FAILED_ID,
        app: "System".into(),
        icon: "dialog-warning".into(),
        summary: format!("Couldn't {action}"),
        body: if denied {
            "The system did not allow it from this session (it is not the active session on the screen).".into()
        } else {
            "The system did not answer.".into()
        },
        urgency: 1,
        timeout_ms: None,
        transient: false,
    }
}

/// A power-button tap this soon after resume is the press that woke the system: ignored.
const POWER_AFTER_RESUME: std::time::Duration = std::time::Duration::from_secs(2);
/// Longest a pending sleep (PrepareForSleep(true) without (false)) makes the power button ignored.
const POWER_SLEEP_PENDING: std::time::Duration = std::time::Duration::from_secs(30);

/// At most one Network re-read per this while nobody watches the list (`Ctx::net_live`).
const NET_MIN_GAP: std::time::Duration = std::time::Duration::from_secs(10);
/// How long after a scan request (Networks page opened) every NM signal re-reads at once.
const NET_LIVE: std::time::Duration = std::time::Duration::from_secs(30);

/// Give up on a join that neither connects nor fails within this.
const JOIN_TIMEOUT_S: u64 = 60;

async fn signals(conn: &Connection, svc: Svc, sender: &'static str, interface: &'static str) -> zbus::Result<BoxStream<'static, Ev>> {
    let rule = MatchRule::builder().msg_type(MsgType::Signal).sender(sender).interface(interface).build()?;
    let stream = MessageStream::for_match_rule(rule, conn, None).await?;
    Ok(stream
        .map(move |m| {
            let path = m.ok().and_then(|m| m.header().path().map(|p| p.to_string()));
            Ev::Signal(svc, path)
        })
        .boxed())
}

/// A join the shell started and is watching.
struct Joining {
    ssid: String,
    /// The active connection (None until NetworkManager answered).
    active: Option<OwnedObjectPath>,
    /// A connection we added for this join (deleted again if the join fails).
    added: Option<OwnedObjectPath>,
    since: Instant,
}

/// State the loop keeps between refreshes.
struct Ctx {
    conn: Connection,
    /// logind session object (for SetBrightness and Lock).
    session: Option<OwnedObjectPath>,
    backlight: Option<read::Backlight>,
    /// NetworkManager objects whose changes matter: the manager, settings, the Wi-Fi device, its access
    /// point, a connection being joined.
    nm_paths: Vec<String>,
    nm: read::NmState,
    joining: Option<Joining>,
    join: Option<Join>,
    bt_failed: Option<String>,
    notify: Option<notify::Server>,
    /// Low-battery level last notified (see `low_level`).
    low: u8,
    /// Notifications made here (low battery) for the UI.
    notes_out: Vec<NoteEvent>,
    sys: Sys,
    /// logind delay inhibitor for sleep (closing it lets a pending sleep go ahead).
    inhibit: Option<zbus::OwnedFd>,
    /// logind's handle-power-key block lock: while held, logind leaves the power button to us (its default
    /// is to power off). Released with the process, so a crashed shell falls back to logind's handling.
    power_key: Option<zbus::OwnedFd>,
    /// When the system last resumed (PrepareForSleep(false)).
    resumed_at: Option<Instant>,
    /// Sleeps announced so far, and whether one is pending now (PrepareForSleep(true) without (false)).
    sleeps: u64,
    sleeping: bool,
    /// When the pending sleep was announced (PrepareForSleep(true)); monotonic, so suspended time is not counted.
    sleep_at: Option<Instant>,
    /// The raw brightness before idle dimming (restored by `Dim(None)`).
    undim: Option<u32>,
    /// logind refused SetIdleHint (it allows it only for graphical sessions, not a tty one): stop asking.
    no_idle_hint: bool,
    /// Network re-read rate limit (each re-read is ~30 D-Bus round trips; NM signals access-point strength
    /// changes several times a minute): last re-read, a deferred one queued, and "live" until (Wi-Fi page).
    net_last: Option<Instant>,
    net_deferred: bool,
    net_live_until: Option<Instant>,
}

async fn run(shared: Arc<Shared>, ping: Ping) -> zbus::Result<()> {
    let t0 = Instant::now();
    let conn = Connection::system().await?;
    let mut streams: SelectAll<BoxStream<'static, Ev>> = SelectAll::new();
    for (svc, sender, iface) in [
        (Svc::Battery, UPOWER, PROPS),
        (Svc::Network, NM, PROPS),
        (Svc::Bluetooth, BLUEZ, PROPS),
        (Svc::Bluetooth, BLUEZ, "org.freedesktop.DBus.ObjectManager"),
        (Svc::Sensor, SENSORS, PROPS),
    ] {
        match signals(&conn, svc, sender, iface).await {
            Ok(s) => streams.push(s),
            Err(err) => warn!(%err, sender, "services: cannot watch"),
        }
    }
    streams.push(Commands(shared.clone()).map(Ev::Cmd).boxed());
    let notify = notify::Server::start().await;
    if let Some(n) = notify.as_ref() {
        streams.push(
            MessageStream::from(&n.conn)
                .filter_map(|m| futures_util::future::ready(m.ok().filter(notify::Server::is_ours).map(Ev::Call)))
                .boxed(),
        );
    }

    let session = read::session_path(&conn).await;
    info!(session = session.as_ref().map(|p| p.as_str()).unwrap_or("-"), "services: logind session");
    let mut ctx = Ctx {
        conn,
        session,
        backlight: read::Backlight::find(),
        nm_paths: Vec::new(),
        nm: read::NmState::default(),
        joining: None,
        join: None,
        bt_failed: None,
        notify,
        low: 0,
        notes_out: Vec::new(),
        sys: Sys::default(),
        inhibit: None,
        power_key: None,
        resumed_at: None,
        sleep_at: None,
        sleeps: 0,
        sleeping: false,
        undim: None,
        no_idle_hint: false,
        net_last: None,
        net_deferred: false,
        net_live_until: None,
    };
    match session_signals(&ctx.conn, ctx.session.as_ref()).await {
        Ok(s) => streams.push(s),
        Err(err) => warn!(%err, "services: cannot watch logind session signals"),
    }
    ctx.take_inhibitor().await;
    ctx.take_power_key().await;
    // The last run ended while dimmed (crash, kill): its brightness comes back.
    if let Some(raw) = undim_file_read() {
        info!(raw, "services: brightness from before an idle dim of the last run");
        ctx.undim = Some(raw);
        ctx.execute(Command::Dim(None), &mut streams).await;
    }
    let mut dirty = vec![Svc::Battery, Svc::Network, Svc::Bluetooth, Svc::Sensor];
    let mut first = true;
    let mut published = false;
    loop {
        // Commands first (the user is waiting for them), merged: only the last brightness counts.
        let mut cmds: Vec<Command> = Vec::new();
        let mut events = Vec::new();
        if !first {
            match streams.next().await {
                Some(ev) => events.push(ev),
                None => return Ok(()),
            }
        }
        while let Some(Some(ev)) = streams.next().now_or_never() {
            events.push(ev);
        }
        for ev in events {
            match ev {
                Ev::Signal(Svc::Network, Some(path)) if !ctx.nm_paths.is_empty() && !ctx.nm_paths.contains(&path) => {}
                Ev::Signal(Svc::Network, _) if !ctx.net_live() && !dirty.contains(&Svc::Network) => {
                    let since = ctx.net_last.map_or(NET_MIN_GAP, |t| t.elapsed());
                    if since >= NET_MIN_GAP {
                        dirty.push(Svc::Network);
                    } else if !ctx.net_deferred {
                        ctx.net_deferred = true;
                        let sh = shared.clone();
                        let _ = std::thread::Builder::new().name("net-defer".into()).spawn(move || {
                            std::thread::sleep(NET_MIN_GAP - since);
                            sh.queue.lock().unwrap().push_back(Command::NetRefresh);
                            sh.waker.wake();
                        });
                    }
                }
                Ev::Signal(svc, _) => {
                    if !dirty.contains(&svc) {
                        dirty.push(svc);
                    }
                }
                Ev::Cmd(c) => {
                    if let (Command::SetBrightness(_), Some(Command::SetBrightness(_))) = (&c, cmds.last()) {
                        cmds.pop();
                    }
                    cmds.push(c);
                }
                Ev::Call(msg) => {
                    if let Some(n) = ctx.notify.as_mut()
                        && let Some(ev) = n.handle(&msg).await
                    {
                        shared.notes.lock().unwrap().push(ev);
                        ping.ping();
                    }
                }
                Ev::Session(ev) => {
                    info!(?ev, "services: logind");
                    match ev {
                        SessionEvent::PrepareForSleep(true) => {
                            ctx.sleeping = true;
                            ctx.sleep_at = Some(Instant::now());
                            ctx.sleeps += 1;
                            // If the lock screen never reports in, let the sleep go ahead anyway after SLEEP_GRACE.
                            let (n, sh) = (ctx.sleeps, shared.clone());
                            let _ = std::thread::Builder::new().name("sleep-grace".into()).spawn(move || {
                                std::thread::sleep(SLEEP_GRACE);
                                sh.queue.lock().unwrap().push_back(Command::SleepReady(Some(n)));
                                sh.waker.wake();
                            });
                        }
                        SessionEvent::PrepareForSleep(false) => {
                            ctx.resumed_at = Some(Instant::now());
                            ctx.sleeping = false;
                            ctx.take_inhibitor().await;
                        }
                        _ => {}
                    }
                    shared.session.lock().unwrap().push(ev);
                    ping.ping();
                }
                Ev::Done(svc, done) => {
                    ctx.done(done);
                    if !dirty.contains(&svc) {
                        dirty.push(svc);
                    }
                }
            }
        }
        for c in cmds {
            if let Some(svc) = ctx.execute(c, &mut streams).await
                && !dirty.contains(&svc)
            {
                dirty.push(svc);
            }
        }
        let before = ctx.sys.clone();
        for svc in dirty.drain(..) {
            ctx.refresh(svc).await;
        }
        if !ctx.notes_out.is_empty() {
            shared.notes.lock().unwrap().append(&mut ctx.notes_out);
            ping.ping();
        }
        if first {
            info!(ms = t0.elapsed().as_millis() as u64, sys = ?ctx.sys, "services: ready");
            first = false;
        }
        if ctx.sys != before || !published {
            published = true;
            let mut g = shared.sys.lock().unwrap();
            g.0 += 1;
            // The volume is the audio thread's part of the snapshot.
            let volume = g.1.volume;
            g.1 = ctx.sys.clone();
            g.1.volume = volume;
            drop(g);
            ping.ping();
        }
    }
}

/// `$XDG_RUNTIME_DIR/rtshell-undim`: the raw brightness before an idle dim, while dimmed (so a restarted
/// shell can bring it back).
fn undim_file() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("rtshell-undim"))
}

fn undim_file_write(raw: Option<u32>) {
    let Some(path) = undim_file() else { return };
    let res = match raw {
        Some(raw) => std::fs::write(&path, raw.to_string()),
        None => std::fs::remove_file(&path).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }),
    };
    if let Err(err) = res {
        warn!(path = %path.display(), %err, "services: undim file");
    }
}

fn undim_file_read() -> Option<u32> {
    std::fs::read_to_string(undim_file()?).ok()?.trim().parse().ok()
}

/// How long a sleep waits for the lock screen at most (logind's own limit, InhibitDelayMaxSec, is 5 s).
const SLEEP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// logind's Lock/Unlock for our session and the manager's PrepareForSleep, as events.
async fn session_signals(conn: &Connection, session: Option<&OwnedObjectPath>) -> zbus::Result<BoxStream<'static, Ev>> {
    let mut all: SelectAll<BoxStream<'static, Ev>> = SelectAll::new();
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender(LOGIN)
        .interface("org.freedesktop.login1.Manager")
        .member("PrepareForSleep")
        .build()?;
    let s = MessageStream::for_match_rule(rule, conn, None).await?;
    all.push(
        s.filter_map(|m| {
            let ev = m.ok().and_then(|m| m.body().deserialize::<bool>().ok()).map(|b| Ev::Session(SessionEvent::PrepareForSleep(b)));
            futures_util::future::ready(ev)
        })
        .boxed(),
    );
    if let Some(path) = session {
        let rule = MatchRule::builder()
            .msg_type(MsgType::Signal)
            .sender(LOGIN)
            .interface("org.freedesktop.login1.Session")
            .path(path.as_str().to_string())
            .build()?;
        let s = MessageStream::for_match_rule(rule, conn, None).await?;
        all.push(
            s.filter_map(|m| {
                let ev = m.ok().and_then(|m| match m.header().member().map(|n| n.as_str()) {
                    Some("Lock") => Some(Ev::Session(SessionEvent::Lock)),
                    Some("Unlock") => Some(Ev::Session(SessionEvent::Unlock)),
                    _ => None,
                });
                futures_util::future::ready(ev)
            })
            .boxed(),
        );
    }
    Ok(all.boxed())
}

impl Ctx {
    /// Network changes matter right now: a join is running, or the Networks page asked for a scan lately.
    fn net_live(&self) -> bool {
        self.joining.is_some() || self.net_live_until.is_some_and(|t| Instant::now() < t)
    }

    /// Take logind's delay inhibitor for sleep (so the lock is drawn before the system sleeps).
    async fn take_power_key(&mut self) {
        let res = async {
            let m = proxy(&self.conn, LOGIN, "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await?;
            m.call::<_, _, zbus::OwnedFd>("Inhibit", &("handle-power-key", "rtshell", "rtshell handles the power button (tap = sleep)", "block"))
                .await
        }
        .await;
        match res {
            Ok(fd) => {
                info!("services: power button taken from logind");
                self.power_key = Some(fd);
            }
            Err(err) => warn!(%err, "services: logind keeps the power button (its HandlePowerKey applies)"),
        }
    }

    async fn take_inhibitor(&mut self) {
        if self.inhibit.is_some() {
            return;
        }
        let res = async {
            let m = proxy(&self.conn, LOGIN, "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await?;
            m.call::<_, _, zbus::OwnedFd>("Inhibit", &("sleep", "rtshell", "Lock the screen before sleep", "delay")).await
        }
        .await;
        match res {
            Ok(fd) => {
                debug!("services: sleep delay inhibitor taken");
                self.inhibit = Some(fd);
            }
            Err(err) => warn!(%err, "services: no sleep inhibitor; the lock may be drawn only after resume"),
        }
    }

    async fn refresh(&mut self, svc: Svc) {
        match svc {
            Svc::Battery => {
                self.sys.battery = read::battery(&self.conn).await;
                self.check_low_battery();
                // No signal for the backlight (only we change it): re-read it with the battery.
                self.sys.brightness = self.backlight.as_ref().and_then(|b| b.get());
            }
            Svc::Network => {
                self.net_last = Some(Instant::now());
                // A light read (device + active access point) unless someone looks at the list, a join runs,
                // or the link / network changed; then everything (all access points, saved networks).
                let prev = self.sys.wifi.as_ref().map(|w| (w.link, w.ssid.clone(), w.enabled));
                let mut full = self.net_live() || prev.is_none();
                let (mut wifi, mut nm) = read::wifi(&self.conn, full).await;
                if !full && wifi.as_ref().map(|w| (w.link, w.ssid.clone(), w.enabled)) != prev {
                    full = true;
                    (wifi, nm) = read::wifi(&self.conn, true).await;
                }
                if !full {
                    // Keep the last full list; only the active network's strength is new.
                    nm.aps = std::mem::take(&mut self.nm.aps);
                    nm.security = std::mem::take(&mut self.nm.security);
                    nm.saved = std::mem::take(&mut self.nm.saved);
                    if let (Some(w), Some(old)) = (wifi.as_mut(), self.sys.wifi.as_ref()) {
                        w.networks = old.networks.clone();
                        for n in &mut w.networks {
                            if n.active {
                                n.strength = w.strength;
                            }
                        }
                    }
                }
                self.nm = nm;
                self.watch_join().await;
                self.nm_paths = self.nm.paths.clone();
                if let Some(a) = self.joining.as_ref().and_then(|j| j.active.as_ref()) {
                    self.nm_paths.push(a.as_str().to_string());
                }
                self.sys.wifi = wifi.map(|mut w| {
                    w.join = self.join.clone();
                    w
                });
            }
            Svc::Sensor => self.sys.orientation = read::orientation(&self.conn).await,
            Svc::Bluetooth => {
                self.sys.bluetooth = read::bluetooth(&self.conn).await.map(|mut b| {
                    b.failed = self.bt_failed.clone();
                    b
                })
            }
        }
    }

    /// Notify when the battery runs low (and take the notification back once charging).
    fn check_low_battery(&mut self) {
        let mut b = self.sys.battery.clone();
        // RTSHELL_TEST_LOW_BATTERY=PERCENT: pretend to be on battery at PERCENT (tests on a full battery).
        if let Some(p) = std::env::var("RTSHELL_TEST_LOW_BATTERY").ok().and_then(|v| v.parse::<f32>().ok()) {
            b = Some(Battery { percent: p, charge: Charge::Discharging, time_to_empty: 50 * 60, time_to_full: 0 });
        }
        let level = low_level(b.as_ref(), self.low);
        if level > self.low
            && let Some(b) = b.as_ref()
        {
            info!(percent = b.percent, level, "services: low battery");
            self.notes_out.push(NoteEvent::Show(low_note(b, level)));
        } else if level == 0 && self.low > 0 {
            self.notes_out.push(NoteEvent::Close(LOW_BATTERY_ID));
        }
        self.low = level;
    }

    /// Follow a join the shell started: done when its active connection is activated, failed when it goes
    /// away (or deactivates) first.
    async fn watch_join(&mut self) {
        let Some(j) = self.joining.as_ref() else { return };
        let Some(active) = j.active.clone() else { return };
        // NM_ACTIVE_CONNECTION_STATE_*: 1 activating, 2 activated, 3 deactivating, 4 deactivated.
        let state = match proxy(&self.conn, NM, active.as_str(), "org.freedesktop.NetworkManager.Connection.Active").await {
            Ok(p) => p.get_property::<u32>("State").await.unwrap_or(4),
            Err(_) => 4,
        };
        let timed_out = j.since.elapsed().as_secs() > JOIN_TIMEOUT_S;
        let outcome = match state {
            2 => JoinState::Connected,
            3 | 4 => {
                // NM_DEVICE_STATE_REASON_NO_SECRETS (7) / SUPPLICANT_DISCONNECT (8): the usual results of a
                // wrong WPA2 password when no secret agent can ask again.
                if matches!(self.nm.reason, 7 | 8) { JoinState::WrongPassword } else { JoinState::Failed }
            }
            _ if timed_out => JoinState::Failed,
            _ => return,
        };
        let j = self.joining.take().unwrap();
        info!(ssid = %j.ssid, ?outcome, reason = self.nm.reason, secs = j.since.elapsed().as_secs(), "services: Wi-Fi join finished");
        if outcome != JoinState::Connected
            && let Some(added) = j.added.as_ref()
        {
            // A connection we just made with a password that did not work: remove it again.
            self.delete_connection(added).await;
        }
        self.join = Some(Join { ssid: j.ssid, state: outcome });
    }

    async fn delete_connection(&self, path: &OwnedObjectPath) {
        let res = async {
            let p = proxy(&self.conn, NM, path.as_str(), "org.freedesktop.NetworkManager.Settings.Connection").await?;
            p.call::<_, _, ()>("Delete", &()).await
        }
        .await;
        match res {
            Ok(()) => info!(path = path.as_str(), "services: deleted Wi-Fi connection"),
            Err(err) => warn!(%err, path = path.as_str(), "services: cannot delete Wi-Fi connection"),
        }
    }

    fn done(&mut self, done: Done) {
        match done {
            Done::BtConnect { path, ok } => {
                if !ok {
                    self.bt_failed = Some(path);
                }
            }
            Done::Activated { ssid, active, added } => {
                if let Some(j) = self.joining.as_mut().filter(|j| j.ssid == ssid) {
                    j.active = Some(active);
                    j.added = added;
                }
            }
            Done::ActivateFailed { ssid } => {
                if self.joining.as_ref().is_some_and(|j| j.ssid == ssid) {
                    self.joining = None;
                    self.join = Some(Join { ssid, state: JoinState::Failed });
                }
            }
        }
    }

    /// Carry out a command; returns the service to read again afterwards. Slow calls are pushed to
    /// `streams` and report back as `Ev::Done`.
    async fn execute(&mut self, cmd: Command, streams: &mut SelectAll<BoxStream<'static, Ev>>) -> Option<Svc> {
        let started = Instant::now();
        // Secret's Debug hides the password.
        let what = format!("{cmd:?}");
        let res: zbus::Result<Option<Svc>> = async {
            match cmd {
                Command::SetBrightness(f) => {
                    let (Some(b), Some(session)) = (self.backlight.as_ref(), self.session.as_ref()) else { return Ok(None) };
                    let raw = b.raw(f);
                    // A brightness set by the user while dimmed (should not happen: input undims first) wins.
                    if self.undim.take().is_some() {
                        undim_file_write(None);
                    }
                    let p = proxy(&self.conn, LOGIN, session.as_str(), "org.freedesktop.login1.Session").await?;
                    p.call::<_, _, ()>("SetBrightness", &("backlight", b.name.as_str(), raw)).await?;
                    self.sys.brightness = b.get();
                    Ok(None)
                }
                Command::SetWifi(on) => {
                    let p = proxy(&self.conn, NM, "/org/freedesktop/NetworkManager", NM).await?;
                    p.set_property("WirelessEnabled", on).await.map_err(zbus::Error::from)?;
                    Ok(Some(Svc::Network))
                }
                Command::NetRefresh => {
                    self.net_deferred = false;
                    Ok(Some(Svc::Network))
                }
                Command::WifiScan => {
                    // The Networks page is open: follow scan results as they come for a while.
                    self.net_live_until = Some(Instant::now() + NET_LIVE);
                    let Some(dev) = self.nm.device.as_ref() else { return Ok(None) };
                    let p = proxy(&self.conn, NM, dev.as_str(), "org.freedesktop.NetworkManager.Device.Wireless").await?;
                    let opts: HashMap<&str, zbus::Variant<'_>> = HashMap::new();
                    if let Err(err) = p.call::<_, _, ()>("RequestScan", &(opts,)).await {
                        // "Scanning not allowed immediately following previous scan" and the like.
                        debug!(%err, "services: scan request refused");
                    }
                    // A full read now (the list may be from minutes ago); scan results follow by signals.
                    Ok(Some(Svc::Network))
                }
                Command::WifiConnect { ssid, psk } => {
                    let Some(dev) = self.nm.device.clone() else { return Ok(None) };
                    let ap = self.nm.aps.get(&ssid).cloned();
                    let saved = self.nm.saved.iter().find(|s| s.ssid == ssid).map(|s| s.path.clone());
                    self.join = Some(Join { ssid: ssid.clone(), state: JoinState::Connecting });
                    self.joining = Some(Joining { ssid: ssid.clone(), active: None, added: None, since: Instant::now() });
                    let conn = self.conn.clone();
                    let root = OwnedObjectPath::try_from("/").unwrap();
                    let specific = ap.unwrap_or_else(|| root.clone());
                    let security = match psk {
                        Some(_) => Some(Security::Psk),
                        None => self.nm.security.get(&ssid).copied(),
                    };
                    let fut = async move {
                        let res: zbus::Result<(OwnedObjectPath, Option<OwnedObjectPath>)> = async {
                            let nm = proxy(&conn, NM, "/org/freedesktop/NetworkManager", NM).await?;
                            match (saved, psk) {
                                (Some(path), None) => {
                                    let active: OwnedObjectPath = nm.call("ActivateConnection", &(&path, &dev, &specific)).await?;
                                    Ok((active, None))
                                }
                                (_, psk) => {
                                    let settings = read::new_wifi_settings(&ssid, security, psk.as_ref().map(|p| p.expose()));
                                    let (path, active): (OwnedObjectPath, OwnedObjectPath) =
                                        nm.call("AddAndActivateConnection", &(settings, &dev, &specific)).await?;
                                    Ok((active, Some(path)))
                                }
                            }
                        }
                        .await;
                        match res {
                            Ok((active, added)) => {
                                info!(ssid = %ssid, active = active.as_str(), new = added.is_some(), "services: Wi-Fi activation started");
                                Ev::Done(Svc::Network, Done::Activated { ssid, active, added })
                            }
                            Err(err) => {
                                warn!(%err, ssid = %ssid, "services: Wi-Fi activation failed");
                                Ev::Done(Svc::Network, Done::ActivateFailed { ssid })
                            }
                        }
                    };
                    streams.push(futures_util::stream::once(fut).boxed());
                    Ok(Some(Svc::Network))
                }
                Command::WifiDisconnect => {
                    let Some(dev) = self.nm.device.as_ref() else { return Ok(None) };
                    let p = proxy(&self.conn, NM, dev.as_str(), "org.freedesktop.NetworkManager.Device").await?;
                    p.call::<_, _, ()>("Disconnect", &()).await?;
                    Ok(Some(Svc::Network))
                }
                Command::WifiForget(ssid) => {
                    let paths: Vec<OwnedObjectPath> = self.nm.saved.iter().filter(|s| s.ssid == ssid).map(|s| s.path.clone()).collect();
                    for p in &paths {
                        self.delete_connection(p).await;
                    }
                    if self.join.as_ref().is_some_and(|j| j.ssid == ssid) {
                        self.join = None;
                    }
                    Ok(Some(Svc::Network))
                }
                Command::SetBluetooth(on) => {
                    let Some(adapter) = self.sys.bluetooth.as_ref().map(|b| b.adapter.clone()) else { return Ok(None) };
                    let p = proxy(&self.conn, BLUEZ, &adapter, "org.bluez.Adapter1").await?;
                    p.set_property("Powered", on).await.map_err(zbus::Error::from)?;
                    Ok(Some(Svc::Bluetooth))
                }
                Command::BtConnect(path, on) => {
                    self.bt_failed = None;
                    let conn = self.conn.clone();
                    let fut = async move {
                        let res: zbus::Result<()> = async {
                            let p = proxy(&conn, BLUEZ, &path, "org.bluez.Device1").await?;
                            p.call::<_, _, ()>(if on { "Connect" } else { "Disconnect" }, &()).await
                        }
                        .await;
                        if let Err(err) = &res {
                            warn!(%err, path = %path, on, "services: Bluetooth device connect/disconnect failed");
                        }
                        Ev::Done(Svc::Bluetooth, Done::BtConnect { path, ok: res.is_ok() })
                    };
                    streams.push(futures_util::stream::once(fut).boxed());
                    Ok(Some(Svc::Bluetooth))
                }
                Command::Accelerometer(on) => {
                    let p = proxy(&self.conn, SENSORS, "/net/hadess/SensorProxy", SENSORS).await?;
                    p.call::<_, _, ()>(if on { "ClaimAccelerometer" } else { "ReleaseAccelerometer" }, &()).await?;
                    Ok(Some(Svc::Sensor))
                }
                // Handled by the audio thread (`Services::send`).
                Command::SetVolume(_) | Command::SetMute(_) => Ok(None),
                Command::NoteClosed(id, dismissed) => {
                    if id != LOW_BATTERY_ID
                        && let Some(n) = self.notify.as_ref()
                    {
                        n.closed(id, if dismissed { notify::Closed::Dismissed } else { notify::Closed::Expired }).await;
                    }
                    Ok(None)
                }
                Command::SleepReady(n) => {
                    if self.sleeping && n.is_none_or(|n| n == self.sleeps) && self.inhibit.take().is_some() {
                        info!(fallback = n.is_some(), ms_since_signal = started.elapsed().as_millis() as u64, "services: sleep may go ahead (inhibitor released)");
                    }
                    Ok(None)
                }
                Command::Dim(level) => {
                    let (Some(b), Some(session)) = (self.backlight.as_ref(), self.session.as_ref()) else { return Ok(None) };
                    let target = match level {
                        Some(level) => {
                            if self.undim.is_some() {
                                return Ok(None);
                            }
                            let Some(now) = b.get_raw() else { return Ok(None) };
                            let dim = ((now as f32 * level.clamp(0.0, 1.0)).round() as u32).max(b.raw(MIN_BRIGHTNESS));
                            if dim >= now {
                                return Ok(None);
                            }
                            self.undim = Some(now);
                            undim_file_write(Some(now));
                            info!(from = now, to = dim, "services: dim");
                            dim
                        }
                        None => {
                            let Some(raw) = self.undim.take() else { return Ok(None) };
                            undim_file_write(None);
                            info!(to = raw, "services: undim");
                            raw
                        }
                    };
                    let p = proxy(&self.conn, LOGIN, session.as_str(), "org.freedesktop.login1.Session").await?;
                    p.call::<_, _, ()>("SetBrightness", &("backlight", b.name.as_str(), target)).await?;
                    self.sys.brightness = b.get();
                    Ok(None)
                }
                Command::SetIdleHint(on) => {
                    let Some(session) = self.session.as_ref().filter(|_| !self.no_idle_hint) else { return Ok(None) };
                    let p = proxy(&self.conn, LOGIN, session.as_str(), "org.freedesktop.login1.Session").await?;
                    if let Err(err) = p.call::<_, _, ()>("SetIdleHint", &(on,)).await {
                        if !err.to_string().contains("NotSupported") {
                            return Err(err);
                        }
                        info!(%err, "services: logind takes no idle hint from this session; not sent again");
                        self.no_idle_hint = true;
                    }
                    Ok(None)
                }
                Command::SetLockedHint(on) => {
                    let Some(session) = self.session.as_ref() else { return Ok(None) };
                    let p = proxy(&self.conn, LOGIN, session.as_str(), "org.freedesktop.login1.Session").await?;
                    p.call::<_, _, ()>("SetLockedHint", &(on,)).await?;
                    Ok(None)
                }
                Command::Lock => {
                    let Some(session) = self.session.as_ref() else { return Ok(None) };
                    let p = proxy(&self.conn, LOGIN, session.as_str(), "org.freedesktop.login1.Session").await?;
                    p.call::<_, _, ()>("Lock", &()).await?;
                    Ok(None)
                }
                // The press that wakes the system can reach us before logind's PrepareForSleep(false) (seen
                // 105 ms earlier on the Surface): ignore it while a sleep is pending, too. The bound keeps a lost
                // resume signal from disabling the button (monotonic time does not run while suspended).
                Command::PowerKey
                    if self.resumed_at.is_some_and(|t| t.elapsed() < POWER_AFTER_RESUME)
                        || (self.sleeping && self.sleep_at.is_some_and(|t| t.elapsed() < POWER_SLEEP_PENDING)) =>
                {
                    info!(sleeping = self.sleeping, "services: power button during or right after sleep ignored");
                    Ok(None)
                }
                Command::Suspend | Command::IdleSuspend | Command::PowerKey | Command::Reboot | Command::PowerOff => {
                    let method = match cmd {
                        Command::Suspend | Command::IdleSuspend | Command::PowerKey => "Suspend",
                        Command::Reboot => "Reboot",
                        _ => "PowerOff",
                    };
                    let p = proxy(&self.conn, LOGIN, "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await?;
                    // Not interactive: there is no polkit agent; an active local session is allowed anyway.
                    p.call::<_, _, ()>(method, &(false,)).await?;
                    Ok(None)
                }
            }
        }
        .await;
        let ms = started.elapsed().as_millis() as u64;
        match res {
            Ok(svc) => {
                debug!(cmd = %what, ms, "services: done");
                svc
            }
            Err(err) => {
                warn!(cmd = %what, ms, %err, "services: command failed");
                // A refused power action would otherwise look like nothing happened.
                let action = match what.as_str() {
                    "Suspend" | "PowerKey" => Some("sleep"),
                    "Reboot" => Some("restart"),
                    "PowerOff" => Some("power off"),
                    _ => None,
                };
                if let Some(action) = action {
                    let e = err.to_string();
                    let denied = e.contains("Authorization") || e.contains("AccessDenied") || e.contains("Access denied");
                    self.notes_out.push(NoteEvent::Show(power_failed_note(action, denied)));
                }
                None
            }
        }
    }
}

/// A proxy without property caching (each refresh reads what it needs once).
async fn proxy<'a>(conn: &Connection, dest: &'a str, path: &'a str, iface: &'a str) -> zbus::Result<Proxy<'a>> {
    zbus::proxy::Builder::<Proxy<'a>>::new(conn)
        .destination(dest)
        .path(path)
        .interface(iface)
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

/// `a{sv}` property maps as BlueZ's GetManagedObjects returns them.
type Props = HashMap<String, OwnedVariant>;

#[cfg(test)]
mod tests {
    use super::*;

    fn bat(percent: f32, charge: Charge) -> Battery {
        Battery { percent, charge, time_to_empty: 0, time_to_full: 0 }
    }

    #[test]
    fn low_battery_levels() {
        let d = Charge::Discharging;
        assert_eq!(low_level(Some(&bat(50.0, d)), 0), 0);
        assert_eq!(low_level(Some(&bat(10.0, d)), 0), 1);
        assert_eq!(low_level(Some(&bat(11.0, d)), 1), 1); // hysteresis: no re-arm yet
        assert_eq!(low_level(Some(&bat(11.0, d)), 0), 0);
        assert_eq!(low_level(Some(&bat(13.0, d)), 1), 0);
        assert_eq!(low_level(Some(&bat(5.0, d)), 1), 2);
        assert_eq!(low_level(Some(&bat(8.0, d)), 2), 2); // stays critical until re-armed
        assert_eq!(low_level(Some(&bat(4.0, Charge::Charging)), 2), 0);
        assert_eq!(low_level(None, 2), 0);
        assert!(low_note(&bat(4.0, d), 2).timeout_ms == Some(0));
    }
}
