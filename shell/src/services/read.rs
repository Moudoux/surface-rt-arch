//! Reading each service's state in full (see mod.rs for when). Every function returns None when the service
//! or the object is missing, so the UI can hide or grey out what it cannot show.

use std::{collections::HashMap, path::PathBuf};

use tracing::debug;
use zbus::{Connection, OwnedObjectPath, Variant};

use super::{BLUEZ, SENSORS, Battery, Bluetooth, BtDevice, Charge, LOGIN, Link, MIN_BRIGHTNESS, NM, NM_SETTINGS, Network, PROPS, Props, Security, UPOWER, Wifi, proxy};

/// Our logind session: `XDG_SESSION_ID` if set (rtshell may be started from another session, e.g. over
/// ssh for tests, and acts for the seat session), else the session of our process.
pub async fn session_path(conn: &Connection) -> Option<OwnedObjectPath> {
    let m = proxy(conn, LOGIN, "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await.ok()?;
    if let Ok(id) = std::env::var("XDG_SESSION_ID")
        && let Ok(p) = m.call::<_, _, OwnedObjectPath>("GetSession", &(id.as_str(),)).await
    {
        return Some(p);
    }
    m.call::<_, _, OwnedObjectPath>("GetSessionByPID", &(std::process::id(),)).await.ok()
}

/// The first backlight in sysfs. Read directly (world-readable); written through logind.
pub struct Backlight {
    pub name: String,
    dir: PathBuf,
    max: u32,
}

impl Backlight {
    pub fn find() -> Option<Backlight> {
        let mut entries: Vec<_> = std::fs::read_dir("/sys/class/backlight").ok()?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        let e = entries.into_iter().next()?;
        let dir = e.path();
        let max = std::fs::read_to_string(dir.join("max_brightness")).ok()?.trim().parse().ok().filter(|m| *m > 0)?;
        Some(Backlight { name: e.file_name().to_string_lossy().into_owned(), dir, max })
    }

    pub fn get(&self) -> Option<f32> {
        let v: u32 = std::fs::read_to_string(self.dir.join("brightness")).ok()?.trim().parse().ok()?;
        Some(v as f32 / self.max as f32)
    }

    /// The current raw value (0..=max).
    pub fn get_raw(&self) -> Option<u32> {
        std::fs::read_to_string(self.dir.join("brightness")).ok()?.trim().parse().ok()
    }

    pub fn raw(&self, f: f32) -> u32 {
        ((f.clamp(MIN_BRIGHTNESS, 1.0) * self.max as f32).round() as u32).max(1)
    }
}

pub async fn battery(conn: &Connection) -> Option<Battery> {
    let p = proxy(conn, UPOWER, "/org/freedesktop/UPower/devices/DisplayDevice", "org.freedesktop.UPower.Device").await.ok()?;
    if !p.get_property::<bool>("IsPresent").await.ok()? {
        return None;
    }
    let charge = match p.get_property::<u32>("State").await.unwrap_or(0) {
        1 => Charge::Charging,
        2 => Charge::Discharging,
        3 => Charge::Empty,
        4 => Charge::Full,
        5 | 6 => Charge::Pending,
        _ => Charge::Unknown,
    };
    Some(Battery {
        percent: p.get_property::<f64>("Percentage").await.ok()? as f32,
        charge,
        time_to_empty: p.get_property::<i64>("TimeToEmpty").await.unwrap_or(0),
        time_to_full: p.get_property::<i64>("TimeToFull").await.unwrap_or(0),
    })
}

/// A saved NetworkManager Wi-Fi connection (name and path only; its settings are read with GetSettings,
/// which never includes secrets).
pub struct Saved {
    pub ssid: String,
    pub path: OwnedObjectPath,
}

/// What the services loop keeps from a NetworkManager read (for commands and change filtering).
#[derive(Default)]
pub struct NmState {
    /// Object paths whose changes matter: manager, settings, the Wi-Fi device, its active access point.
    pub paths: Vec<String>,
    /// The Wi-Fi device.
    pub device: Option<OwnedObjectPath>,
    /// Best access point per SSID.
    pub aps: HashMap<String, OwnedObjectPath>,
    pub security: HashMap<String, Security>,
    pub saved: Vec<Saved>,
    /// The device's last state change reason (NM_DEVICE_STATE_REASON_*).
    pub reason: u32,
}

// NM_802_11_AP_FLAGS_PRIVACY and NM_802_11_AP_SEC_KEY_MGMT_* bits of WpaFlags / RsnFlags.
const AP_PRIVACY: u32 = 0x1;
const KEY_MGMT_PSK: u32 = 0x100;
const KEY_MGMT_802_1X: u32 = 0x200;

/// Open, WPA/WPA2 personal, or something this shell does not offer (enterprise, WEP, WPA3-only).
pub fn security(flags: u32, wpa: u32, rsn: u32) -> Security {
    if wpa == 0 && rsn == 0 {
        return if flags & AP_PRIVACY == 0 { Security::Open } else { Security::Unsupported }; // WEP
    }
    if (wpa | rsn) & KEY_MGMT_PSK != 0 && (wpa | rsn) & KEY_MGMT_802_1X == 0 { Security::Psk } else { Security::Unsupported }
}

/// Settings for a new Wi-Fi connection (NetworkManager fills in the rest from the access point). The
/// password goes only into this message.
pub fn new_wifi_settings<'a>(ssid: &'a str, security: Option<Security>, psk: Option<&'a str>) -> HashMap<&'static str, HashMap<&'static str, Variant<'a>>> {
    let mut s: HashMap<&'static str, HashMap<&'static str, Variant<'a>>> = HashMap::new();
    s.insert("connection", HashMap::from([("type", Variant::from("802-11-wireless")), ("id", Variant::from(ssid))]));
    s.insert(
        "802-11-wireless",
        HashMap::from([("ssid", Variant::from(ssid.as_bytes().to_vec())), ("mode", Variant::from("infrastructure"))]),
    );
    if let (Some(Security::Psk), Some(psk)) = (security, psk) {
        s.insert("802-11-wireless-security", HashMap::from([("key-mgmt", Variant::from("wpa-psk")), ("psk", Variant::from(psk))]));
    }
    s
}

fn ssid_text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Saved Wi-Fi connections (GetSettings: no secrets).
async fn saved_wifi(conn: &Connection) -> Vec<Saved> {
    let Ok(settings) = proxy(conn, NM, NM_SETTINGS, "org.freedesktop.NetworkManager.Settings").await else { return Vec::new() };
    let list: Vec<OwnedObjectPath> = settings.call("ListConnections", &()).await.unwrap_or_default();
    let mut out = Vec::new();
    for path in list {
        let Ok(c) = proxy(conn, NM, path.as_str(), "org.freedesktop.NetworkManager.Settings.Connection").await else { continue };
        let Ok(s) = c.call::<_, _, HashMap<String, Props>>("GetSettings", &()).await else { continue };
        let Some(w) = s.get("802-11-wireless") else { continue };
        let Some(ssid) = w.get("ssid").and_then(|v| v.try_clone().ok()).and_then(|v| Vec::<u8>::try_from(v).ok()) else { continue };
        out.push(Saved { ssid: ssid_text(&ssid), path });
    }
    out
}

/// NetworkManager's Wi-Fi state, and what the loop keeps for commands.
/// `full = false`: only the device and its active access point (~5 calls instead of ~30); `networks` is then
/// empty and `NmState::aps/security/saved` are not filled (the caller keeps the last full read's).
pub async fn wifi(conn: &Connection, full: bool) -> (Option<Wifi>, NmState) {
    let mut st = NmState { paths: vec!["/org/freedesktop/NetworkManager".to_string(), NM_SETTINGS.to_string()], ..NmState::default() };
    let Ok(nm) = proxy(conn, NM, "/org/freedesktop/NetworkManager", NM).await else { return (None, st) };
    let Ok(enabled) = nm.get_property::<bool>("WirelessEnabled").await else { return (None, st) };
    let hw_enabled = nm.get_property::<bool>("WirelessHardwareEnabled").await.unwrap_or(true);
    let devices: Vec<OwnedObjectPath> = nm.call("GetDevices", &()).await.unwrap_or_default();
    for d in devices {
        let Ok(dev) = proxy(conn, NM, d.as_str(), "org.freedesktop.NetworkManager.Device").await else { continue };
        // NM_DEVICE_TYPE_WIFI
        if dev.get_property::<u32>("DeviceType").await.ok() != Some(2) {
            continue;
        }
        st.paths.push(d.as_str().to_string());
        let state = dev.get_property::<u32>("State").await.unwrap_or(0);
        st.reason = dev.get_property::<(u32, u32)>("StateReason").await.map(|r| r.1).unwrap_or(0);
        // NM_DEVICE_STATE_*: 40-90 = prepare .. secondaries, 100 = activated.
        let link = match state {
            100 => Link::Connected,
            40..=90 => Link::Connecting,
            _ => Link::Disconnected,
        };
        let mut w = Wifi { enabled, hw_enabled, link, ..Wifi::default() };
        let mut active_ap = None;
        if let Ok(wl) = proxy(conn, NM, d.as_str(), "org.freedesktop.NetworkManager.Device.Wireless").await {
            if let Ok(ap) = wl.get_property::<OwnedObjectPath>("ActiveAccessPoint").await
                && ap.as_str() != "/"
            {
                st.paths.push(ap.as_str().to_string());
                active_ap = Some(ap);
            }
            // Every access point in range (one GetAll each).
            let aps: Vec<OwnedObjectPath> =
                if full { wl.call("GetAllAccessPoints", &()).await.unwrap_or_default() } else { active_ap.iter().cloned().collect() };
            let mut by_ssid: HashMap<String, (Network, OwnedObjectPath)> = HashMap::new();
            for ap in aps {
                let Ok(pp) = proxy(conn, NM, ap.as_str(), PROPS).await else { continue };
                let Ok(props) = pp.call::<_, _, Props>("GetAll", &("org.freedesktop.NetworkManager.AccessPoint",)).await else { continue };
                let u = |k: &str| props.get(k).and_then(|v| v.downcast_ref::<u32>().ok()).unwrap_or(0);
                let ssid = props.get("Ssid").and_then(|v| v.try_clone().ok()).and_then(|v| Vec::<u8>::try_from(v).ok()).unwrap_or_default();
                let strength = props.get("Strength").and_then(|v| v.downcast_ref::<u8>().ok()).unwrap_or(0);
                let is_active = active_ap.as_ref() == Some(&ap);
                if is_active {
                    w.ssid = Some(ssid_text(&ssid));
                    w.strength = strength;
                }
                if ssid.is_empty() || ssid.iter().all(|&b| b == 0) {
                    continue; // hidden network
                }
                let net = Network {
                    ssid: ssid_text(&ssid),
                    strength,
                    security: security(u("Flags"), u("WpaFlags"), u("RsnFlags")),
                    saved: false,
                    active: is_active,
                };
                match by_ssid.get_mut(&net.ssid) {
                    Some((best, path)) => {
                        let active = best.active || net.active;
                        if net.active || (!best.active && net.strength > best.strength) {
                            *best = net;
                            *path = ap;
                        }
                        best.active = active;
                    }
                    None => {
                        by_ssid.insert(net.ssid.clone(), (net, ap));
                    }
                }
            }
            if full {
                st.saved = saved_wifi(conn).await;
            }
            let mut nets: Vec<Network> = Vec::new();
            for (ssid, (mut net, path)) in by_ssid {
                net.saved = st.saved.iter().any(|s| s.ssid == ssid);
                st.aps.insert(ssid.clone(), path);
                st.security.insert(ssid, net.security);
                nets.push(net);
            }
            nets.sort_by(|a, b| {
                b.active.cmp(&a.active).then(b.saved.cmp(&a.saved)).then(b.strength.cmp(&a.strength)).then_with(|| a.ssid.cmp(&b.ssid))
            });
            if std::env::var_os("RTSHELL_FAKE_NETWORKS").is_some() {
                // UI tests only: entries that are not really there (never tap Connect on them).
                for (ssid, strength, security) in [("Test WPA2", 45, Security::Psk), ("Test Open", 30, Security::Open), ("Test Enterprise", 15, Security::Unsupported)] {
                    nets.push(Network { ssid: ssid.into(), strength, security, saved: false, active: false });
                }
            }
            w.networks = nets;
        }
        st.device = Some(d);
        return (Some(w), st);
    }
    debug!("services: no Wi-Fi device");
    (None, st)
}

/// The accelerometer orientation (None without iio-sensor-proxy or an accelerometer).
pub async fn orientation(conn: &Connection) -> Option<String> {
    let p = proxy(conn, SENSORS, "/net/hadess/SensorProxy", SENSORS).await.ok()?;
    if !p.get_property::<bool>("HasAccelerometer").await.ok()? {
        return None;
    }
    p.get_property::<String>("AccelerometerOrientation").await.ok()
}

pub async fn bluetooth(conn: &Connection) -> Option<Bluetooth> {
    let om = proxy(conn, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager").await.ok()?;
    let objects: HashMap<OwnedObjectPath, HashMap<String, Props>> = om.call("GetManagedObjects", &()).await.ok()?;
    let flag = |p: &Props, k: &str| p.get(k).and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false);
    let text = |p: &Props, k: &str| p.get(k).and_then(|v| v.downcast_ref::<String>().ok()).unwrap_or_default();
    // The first adapter (the Surface has one).
    let mut adapters: Vec<(&OwnedObjectPath, &Props)> =
        objects.iter().filter_map(|(path, ifs)| ifs.get("org.bluez.Adapter1").map(|p| (path, p))).collect();
    adapters.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    let (adapter_path, adapter) = adapters.first()?;
    let mut devices: Vec<BtDevice> = objects
        .iter()
        .filter_map(|(path, ifs)| ifs.get("org.bluez.Device1").map(|p| (path, p)))
        .filter(|(_, p)| flag(p, "Paired"))
        .map(|(path, p)| BtDevice { path: path.as_str().to_string(), name: text(p, "Alias"), icon: text(p, "Icon"), connected: flag(p, "Connected") })
        .collect();
    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Some(Bluetooth { adapter: adapter_path.as_str().to_string(), powered: flag(adapter, "Powered"), devices, failed: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_from_flags() {
        assert_eq!(security(0, 0, 0), Security::Open);
        assert_eq!(security(AP_PRIVACY, 0, 0), Security::Unsupported); // WEP
        assert_eq!(security(AP_PRIVACY, 0, 0x188), Security::Psk); // WPA2-PSK, CCMP
        assert_eq!(security(AP_PRIVACY, 0x188, 0x188), Security::Psk);
        assert_eq!(security(AP_PRIVACY, 0, 0x288), Security::Unsupported); // WPA2-Enterprise
        assert_eq!(security(AP_PRIVACY, 0, 0x400), Security::Unsupported); // WPA3-SAE only
        assert_eq!(security(AP_PRIVACY, 0, 0x500), Security::Psk); // WPA2/WPA3 transition
    }

    #[test]
    fn new_settings_carry_the_password_only_for_psk() {
        let s = new_wifi_settings("Home", Some(Security::Psk), Some("hunter22"));
        assert!(s["802-11-wireless-security"].contains_key("psk"));
        let s = new_wifi_settings("Cafe", Some(Security::Open), None);
        assert!(!s.contains_key("802-11-wireless-security"));
    }
}
