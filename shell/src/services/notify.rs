//! Notification server (`org.freedesktop.Notifications` 1.2 on the session bus), DESKTOP-SHELL.md 3.2.
//!
//! Runs inside the services thread on its own session-bus connection, without zbus's object server: method
//! calls arrive on a plain message stream and are answered with `Connection::reply`. Capabilities: "body"
//! only (no markup, actions, images or sounds), so clients send plain text. New and closed notifications go
//! to the UI as `NoteEvent`s; the UI reports dismissals back (`Command::NoteClosed`) and this emits
//! `NotificationClosed`.

use std::collections::HashMap;

use tracing::{debug, info, warn};
use zbus::{Connection, Message, OwnedVariant, message::Type as MsgType};

pub const PATH: &str = "/org/freedesktop/Notifications";
pub const IFACE: &str = "org.freedesktop.Notifications";

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: u32,
    pub app: String,
    /// Icon name (freedesktop icon theme); empty = the UI's default.
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    /// How long the banner stays: None = the theme's default, Some(0) = until dismissed.
    pub timeout_ms: Option<u32>,
    /// Banner only, not kept in the Notifications list (hint "transient").
    pub transient: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NoteEvent {
    /// New, or replacing the one with the same id.
    Show(Note),
    /// Closed by the app (CloseNotification).
    Close(u32),
}

/// Why a notification closed (NotificationClosed reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    Expired = 1,
    Dismissed = 2,
    ByCall = 3,
}

pub struct Server {
    pub conn: Connection,
    next_id: u32,
}

/// Plain text from a body that may still carry markup from clients that ignore the capabilities.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

type NotifyArgs = (String, u32, String, String, String, Vec<String>, HashMap<String, OwnedVariant>, i32);

impl Server {
    /// Connect to the session bus and take the name. None if there is no session bus or another server
    /// owns the name.
    pub async fn start() -> Option<Server> {
        let conn = match Connection::session().await {
            Ok(c) => c,
            Err(err) => {
                warn!(%err, "notifications: no session bus");
                return None;
            }
        };
        if let Err(err) = conn.request_name(IFACE).await {
            warn!(%err, "notifications: cannot own org.freedesktop.Notifications (another server running?)");
            return None;
        }
        info!("notifications: serving org.freedesktop.Notifications");
        Some(Server { conn, next_id: 1 })
    }

    /// Is this an incoming call for us?
    pub fn is_ours(msg: &Message) -> bool {
        let h = msg.header();
        h.message_type() == MsgType::MethodCall && h.path().is_some_and(|p| p.as_str() == PATH)
    }

    /// Answer one method call; returns what the UI must hear about.
    pub async fn handle(&mut self, msg: &Message) -> Option<NoteEvent> {
        let h = msg.header();
        let member = h.member().map(|m| m.as_str().to_string()).unwrap_or_default();
        let iface = h.interface().map(|i| i.as_str().to_string()).unwrap_or_default();
        let res: zbus::Result<Option<NoteEvent>> = async {
            match (iface.as_str(), member.as_str()) {
                (IFACE, "GetCapabilities") => {
                    self.conn.reply(&h, &(vec!["body"],)).await?;
                    Ok(None)
                }
                (IFACE, "GetServerInformation") => {
                    self.conn.reply(&h, &("rtshell", "rtshell", env!("CARGO_PKG_VERSION"), "1.2")).await?;
                    Ok(None)
                }
                (IFACE, "Notify") => {
                    let (app, replaces, icon, summary, body, _actions, hints, timeout): NotifyArgs = msg.body().deserialize()?;
                    let id = if replaces != 0 {
                        replaces
                    } else {
                        let id = self.next_id;
                        self.next_id = self.next_id.wrapping_add(1).max(1);
                        id
                    };
                    self.conn.reply(&h, &(id,)).await?;
                    let urgency = hints.get("urgency").and_then(|v| v.downcast_ref::<u8>().ok()).unwrap_or(1);
                    // Icon names only (paths and image data are not drawn); the desktop entry's name is
                    // often the app's icon name too.
                    let icon = if !icon.is_empty() && !icon.contains('/') {
                        icon
                    } else {
                        hints.get("desktop-entry").and_then(|v| v.downcast_ref::<String>().ok()).unwrap_or_default()
                    };
                    let timeout_ms = if urgency >= 2 { Some(0) } else { u32::try_from(timeout).ok().filter(|_| timeout >= 0) };
                    let timeout_ms = if timeout < 0 { None } else { timeout_ms };
                    let transient = hints.get("transient").and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false);
                    // Summary and app name are logged, the body is not (it may be private).
                    info!(id, app = %app, summary = %summary, urgency, "notification");
                    Ok(Some(NoteEvent::Show(Note { id, app, icon, summary: plain(&summary), body: plain(&body), urgency, timeout_ms, transient })))
                }
                (IFACE, "CloseNotification") => {
                    let (id,): (u32,) = msg.body().deserialize()?;
                    self.conn.reply(&h, &()).await?;
                    self.closed(id, Closed::ByCall).await;
                    Ok(Some(NoteEvent::Close(id)))
                }
                ("org.freedesktop.DBus.Peer", "Ping") => {
                    self.conn.reply(&h, &()).await?;
                    Ok(None)
                }
                _ => {
                    self.conn.reply_dbus_error(&h, zbus::fdo::Error::UnknownMethod(format!("{iface}.{member}"))).await?;
                    Ok(None)
                }
            }
        }
        .await;
        res.unwrap_or_else(|err| {
            debug!(%err, member, "notifications: call failed");
            None
        })
    }

    /// Tell the app its notification went away.
    pub async fn closed(&self, id: u32, why: Closed) {
        if let Err(err) = self.conn.emit_signal(None::<&str>, PATH, IFACE, "NotificationClosed", &(id, why as u32)).await {
            debug!(%err, id, "notifications: cannot emit NotificationClosed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_stripped() {
        assert_eq!(plain("<b>Hi</b> &amp; bye"), "Hi & bye");
        assert_eq!(plain("a < b"), "a "); // an unclosed '<' hides the rest (clients were told: no markup)
    }
}
