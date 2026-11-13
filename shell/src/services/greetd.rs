//! Signing in through greetd (`rtshell --greeter`, DESKTOP-SHELL.md Phase 8 "Greeter mode").
//!
//! greetd's IPC (greetd-ipc(7)): a Unix socket at `$GREETD_SOCK`; each message is JSON preceded by its
//! length as a 4-byte native-endian u32. One attempt is one conversation on a fresh connection:
//!
//! ```text
//! -> create_session {username}
//! <- auth_message {auth_message_type: secret|visible|info|error, auth_message} | success | error
//! -> post_auth_message_response {response}      (the secret for the first secret prompt; null for info/error)
//! <- ... until success or error
//! -> start_session {cmd, env}
//! <- success                                    (the greeter then exits; greetd starts the session)
//! on any error: -> cancel_session
//! ```
//!
//! Secrets: the typed text arrives as a `SecretBuf` and is written, JSON-escaped, straight into a wiped
//! buffer (`Wiped`, sized up front so it never reallocates) that is sent and dropped. Nothing else holds
//! it. greetd's messages (PAM's prompts and errors) are shown on the screen, never logged; only the
//! message types are logged. The JSON here is the small flat subset greetd uses, so no JSON crate is needed.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use tracing::{info, warn};

use super::auth::{Outcome, SecretBuf, wipe};

/// Longest reply accepted from greetd (they are a few hundred bytes).
const MAX_REPLY: usize = 64 * 1024;
/// How long one reply may take (PAM may sleep a few seconds after a wrong password).
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// A byte buffer wiped on drop (holds an encoded message with the secret in it).
struct Wiped(Vec<u8>);

impl Drop for Wiped {
    fn drop(&mut self) {
        let cap = self.0.capacity();
        // SAFETY: the whole allocation is ours; bytes past `len` are plain u8.
        unsafe { self.0.set_len(cap) };
        wipe(&mut self.0);
        self.0.clear();
    }
}

/// Append `s` as a JSON string (with quotes) to `out`. `out` must have room (`escaped_len`) when it holds a
/// secret, so it never reallocates.
fn push_json_str(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &b in s {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0..=0x1f => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(b >> 4) as usize]);
                out.push(HEX[(b & 15) as usize]);
            }
            _ => out.push(b),
        }
    }
    out.push(b'"');
}

/// Upper bound of `push_json_str`'s output for `n` input bytes.
fn escaped_len(n: usize) -> usize {
    n * 6 + 2
}

fn json_str(s: &str) -> String {
    let mut v = Vec::new();
    push_json_str(&mut v, s.as_bytes());
    String::from_utf8(v).expect("escaping keeps UTF-8")
}

fn json_list(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| json_str(s)).collect::<Vec<_>>().join(","))
}

/// A JSON value in greetd's replies (flat objects; nested values are skipped).
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Str(String),
    Null,
    Other,
}

/// Parse a flat JSON object into its keys and values. Nested objects/arrays are accepted and skipped.
fn parse_object(text: &str) -> Result<Vec<(String, Val)>, String> {
    let b = text.as_bytes();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
    };
    fn string(b: &[u8], i: &mut usize) -> Result<String, String> {
        if b.get(*i) != Some(&b'"') {
            return Err("expected a string".into());
        }
        *i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *b.get(*i).ok_or("unterminated string")?;
            *i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *b.get(*i).ok_or("bad escape")?;
                    *i += 1;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex = |i: &mut usize| -> Result<u32, String> {
                                let h = b.get(*i..*i + 4).ok_or("short \\u")?;
                                *i += 4;
                                u32::from_str_radix(std::str::from_utf8(h).map_err(|_| "bad \\u")?, 16).map_err(|_| "bad \\u".to_string())
                            };
                            let mut cp = hex(i)?;
                            if (0xd800..0xdc00).contains(&cp) && b.get(*i..*i + 2) == Some(b"\\u") {
                                *i += 2;
                                let lo = hex(i)?;
                                cp = 0x10000 + ((cp - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                            let mut tmp = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "string is not UTF-8".to_string())
    }
    /// Skip any value (for nested ones).
    fn skip(b: &[u8], i: &mut usize) -> Result<(), String> {
        let mut depth = 0i32;
        loop {
            let c = *b.get(*i).ok_or("unterminated value")?;
            match c {
                b'"' => {
                    string(b, i)?;
                    if depth == 0 {
                        return Ok(());
                    }
                    continue;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    if depth == 0 {
                        return Ok(());
                    }
                    depth -= 1;
                    if depth == 0 {
                        *i += 1;
                        return Ok(());
                    }
                }
                b',' if depth == 0 => return Ok(()),
                _ => {}
            }
            *i += 1;
        }
    }
    ws(&mut i);
    if b.get(i) != Some(&b'{') {
        return Err("expected an object".into());
    }
    i += 1;
    let mut out = Vec::new();
    loop {
        ws(&mut i);
        match b.get(i) {
            Some(b'}') => break,
            Some(b',') => {
                i += 1;
                continue;
            }
            Some(b'"') => {}
            _ => return Err("expected a key".into()),
        }
        let key = string(b, &mut i)?;
        ws(&mut i);
        if b.get(i) != Some(&b':') {
            return Err("expected ':'".into());
        }
        i += 1;
        ws(&mut i);
        let val = match b.get(i) {
            Some(b'"') => Val::Str(string(b, &mut i)?),
            Some(b'n') if b[i..].starts_with(b"null") => {
                i += 4;
                Val::Null
            }
            Some(_) => {
                skip(b, &mut i)?;
                Val::Other
            }
            None => return Err("expected a value".into()),
        };
        out.push((key, val));
    }
    Ok(out)
}

/// A reply from greetd.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Success,
    /// `error_type` is "auth_error" (wrong secret) or "error"; `description` is shown, not logged.
    Error { auth: bool, description: String },
    AuthMessage { kind: MsgKind, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Visible,
    Secret,
    Info,
    Error,
}

fn parse_reply(text: &str) -> Result<Reply, String> {
    let obj = parse_object(text)?;
    let get = |k: &str| {
        obj.iter().find(|(key, _)| key == k).and_then(|(_, v)| match v {
            Val::Str(s) => Some(s.clone()),
            _ => None,
        })
    };
    match get("type").as_deref() {
        Some("success") => Ok(Reply::Success),
        Some("error") => Ok(Reply::Error { auth: get("error_type").as_deref() == Some("auth_error"), description: get("description").unwrap_or_default() }),
        Some("auth_message") => {
            let kind = match get("auth_message_type").as_deref() {
                Some("visible") => MsgKind::Visible,
                Some("secret") => MsgKind::Secret,
                Some("info") => MsgKind::Info,
                Some("error") => MsgKind::Error,
                other => return Err(format!("unknown auth_message_type {other:?}")),
            };
            Ok(Reply::AuthMessage { kind, text: get("auth_message").unwrap_or_default() })
        }
        other => Err(format!("unknown reply type {other:?}")),
    }
}

struct Conn(UnixStream);

impl Conn {
    fn open(sock: &Path) -> std::io::Result<Conn> {
        let s = UnixStream::connect(sock)?;
        s.set_read_timeout(Some(REPLY_TIMEOUT))?;
        s.set_write_timeout(Some(REPLY_TIMEOUT))?;
        Ok(Conn(s))
    }

    fn send(&mut self, msg: &[u8]) -> std::io::Result<()> {
        let len = u32::try_from(msg.len()).map_err(|_| std::io::Error::other("message too long"))?;
        self.0.write_all(&len.to_ne_bytes())?;
        self.0.write_all(msg)
    }

    fn recv(&mut self) -> Result<Reply, String> {
        let mut len = [0u8; 4];
        self.0.read_exact(&mut len).map_err(|e| e.to_string())?;
        let n = u32::from_ne_bytes(len) as usize;
        if n > MAX_REPLY {
            return Err(format!("reply too long ({n} bytes)"));
        }
        let mut buf = vec![0u8; n];
        self.0.read_exact(&mut buf).map_err(|e| e.to_string())?;
        parse_reply(std::str::from_utf8(&buf).map_err(|_| "reply is not UTF-8")?)
    }

    fn call(&mut self, msg: &[u8]) -> Result<Reply, String> {
        self.send(msg).map_err(|e| e.to_string())?;
        self.recv()
    }

    /// Answer an auth message with the secret (JSON-encoded into a wiped buffer).
    fn answer_secret(&mut self, secret: &SecretBuf) -> Result<Reply, String> {
        const HEAD: &[u8] = b"{\"type\":\"post_auth_message_response\",\"response\":";
        let mut buf = Wiped(Vec::with_capacity(HEAD.len() + escaped_len(secret.bytes().len()) + 1));
        buf.0.extend_from_slice(HEAD);
        push_json_str(&mut buf.0, secret.bytes());
        buf.0.push(b'}');
        self.call(&buf.0)
    }

    fn cancel(&mut self) {
        match self.call(br#"{"type":"cancel_session"}"#) {
            Ok(Reply::Success) => {}
            Ok(r) => warn!(reply = reply_kind(&r), "greetd: cancel_session not accepted"),
            Err(err) => warn!(%err, "greetd: cancel_session failed"),
        }
    }
}

/// The reply's type for the log (never its text).
fn reply_kind(r: &Reply) -> &'static str {
    match r {
        Reply::Success => "success",
        Reply::Error { auth: true, .. } => "auth_error",
        Reply::Error { auth: false, .. } => "error",
        Reply::AuthMessage { kind: MsgKind::Secret, .. } => "auth_message/secret",
        Reply::AuthMessage { kind: MsgKind::Visible, .. } => "auth_message/visible",
        Reply::AuthMessage { kind: MsgKind::Info, .. } => "auth_message/info",
        Reply::AuthMessage { kind: MsgKind::Error, .. } => "auth_message/error",
    }
}

/// What the session is started with.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionCmd {
    pub cmd: Vec<String>,
    pub env: Vec<String>,
}

/// Sign `user` in with `secret` and start the session. `ok` = greetd accepted `start_session` (the greeter
/// should now exit). The secret is wiped when this returns.
pub fn login(sock: &Path, user: &str, secret: SecretBuf, session: &SessionCmd, wrong_password: &str) -> Outcome {
    let fail = |m: String| Outcome { ok: false, messages: vec![m] };
    let mut conn = match Conn::open(sock) {
        Ok(c) => c,
        Err(err) => {
            warn!(%err, sock = %sock.display(), "greetd: cannot connect");
            return fail("Cannot reach the login service (greetd).".into());
        }
    };
    let create = format!("{{\"type\":\"create_session\",\"username\":{}}}", json_str(user));
    let mut reply = conn.call(create.as_bytes());
    let mut messages = Vec::new();
    let mut answered = false;
    loop {
        let r = match reply {
            Ok(r) => r,
            Err(err) => {
                warn!(%err, "greetd: conversation failed");
                conn.cancel();
                return fail("The login service did not answer.".into());
            }
        };
        info!(reply = reply_kind(&r), "greetd: reply");
        match r {
            Reply::Success => break,
            Reply::Error { auth, description } => {
                conn.cancel();
                // PAM's own words (e.g. pam_faillock) first; greetd's description ("pam_authenticate:
                // AUTH_ERR") is only a fallback.
                if messages.is_empty() {
                    messages.push(if auth { wrong_password.to_string() } else { description });
                }
                return Outcome { ok: false, messages };
            }
            Reply::AuthMessage { kind: MsgKind::Secret, .. } if !answered && !secret.is_empty() => {
                answered = true;
                reply = conn.answer_secret(&secret);
            }
            Reply::AuthMessage { kind: MsgKind::Info | MsgKind::Error, text } => {
                if !text.trim().is_empty() {
                    messages.push(text.trim().to_string());
                }
                reply = conn.call(br#"{"type":"post_auth_message_response","response":null}"#);
            }
            // A visible prompt or a second secret (2FA, a new password): not something this screen asks.
            Reply::AuthMessage { .. } => {
                conn.cancel();
                messages.push("This account needs a sign-in step this screen does not support.".into());
                return Outcome { ok: false, messages };
            }
        }
    }
    drop(secret);
    let start = format!("{{\"type\":\"start_session\",\"cmd\":{},\"env\":{}}}", json_list(&session.cmd), json_list(&session.env));
    match conn.call(start.as_bytes()) {
        Ok(Reply::Success) => {
            info!("greetd: session start accepted");
            Outcome { ok: true, messages }
        }
        Ok(r) => {
            warn!(reply = reply_kind(&r), "greetd: start_session refused");
            conn.cancel();
            let msg = match r {
                Reply::Error { description, .. } if !description.is_empty() => description,
                _ => "The session could not be started.".into(),
            };
            fail(msg)
        }
        Err(err) => {
            warn!(%err, "greetd: start_session failed");
            fail("The session could not be started.".into())
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn json_escaping_and_parsing() {
        assert_eq!(json_str("a\"b\\c\n\u{1}é"), "\"a\\\"b\\\\c\\n\\u0001é\"");
        let v = parse_object(r#" {"type":"auth_message","auth_message_type":"secret","auth_message":"Password: \"x\"","n":3,"o":{"a":[1,{"b":"}"}]},"z":null} "#).unwrap();
        assert_eq!(v[0], ("type".into(), Val::Str("auth_message".into())));
        assert_eq!(v[2], ("auth_message".into(), Val::Str("Password: \"x\"".into())));
        assert_eq!(v[3].1, Val::Other);
        assert_eq!(v[4].1, Val::Other);
        assert_eq!(v[5], ("z".into(), Val::Null));
        assert_eq!(parse_reply(r#"{"type":"error","error_type":"auth_error","description":"pam_authenticate: AUTH_ERR"}"#).unwrap(), Reply::Error {
            auth: true,
            description: "pam_authenticate: AUTH_ERR".into()
        });
        assert!(parse_object("{\"a\":\"\\ud83d\\ude00\"}").unwrap()[0].1 == Val::Str("😀".into()));
        assert!(parse_object("[1]").is_err());
        assert!(parse_object("{\"a\":\"x").is_err());
    }

    /// What the mock greetd saw.
    #[derive(Debug, Default, Clone)]
    pub struct Seen {
        pub types: Vec<String>,
        pub started: Option<String>,
    }

    /// A tiny greetd for tests: accepts `user` with `secret`, says `info` first if given; one connection.
    pub fn mock(path: &Path, user: &'static str, secret: &'static str, info: Option<&'static str>, second_secret: bool) -> std::thread::JoinHandle<Seen> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut seen = Seen::default();
            let send = |s: &mut UnixStream, m: &str| {
                s.write_all(&(m.len() as u32).to_ne_bytes()).unwrap();
                s.write_all(m.as_bytes()).unwrap();
            };
            let mut stage = 0;
            let mut ok_user = false;
            loop {
                let mut len = [0u8; 4];
                if s.read_exact(&mut len).is_err() {
                    break;
                }
                let mut buf = vec![0u8; u32::from_ne_bytes(len) as usize];
                s.read_exact(&mut buf).unwrap();
                let text = String::from_utf8(buf).unwrap();
                let obj = parse_object(&text).unwrap();
                let get = |k: &str| obj.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
                let Some(Val::Str(t)) = get("type") else { panic!("no type") };
                seen.types.push(t.clone());
                match t.as_str() {
                    "create_session" => {
                        ok_user = get("username") == Some(Val::Str(user.into()));
                        stage = 0;
                        if let Some(i) = info {
                            send(&mut s, &format!(r#"{{"type":"auth_message","auth_message_type":"info","auth_message":{}}}"#, json_str(i)));
                        } else {
                            send(&mut s, r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#);
                            stage = 1;
                        }
                    }
                    "post_auth_message_response" if stage == 0 => {
                        assert_eq!(get("response"), Some(Val::Null));
                        send(&mut s, r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#);
                        stage = 1;
                    }
                    "post_auth_message_response" if stage == 1 => {
                        if ok_user && get("response") == Some(Val::Str(secret.into())) {
                            if second_secret {
                                send(&mut s, r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Code: "}"#);
                                stage = 2;
                            } else {
                                send(&mut s, r#"{"type":"success"}"#);
                            }
                        } else {
                            send(&mut s, r#"{"type":"error","error_type":"auth_error","description":"pam_authenticate: AUTH_ERR"}"#);
                        }
                    }
                    "start_session" => {
                        seen.started = Some(text.clone());
                        send(&mut s, r#"{"type":"success"}"#);
                    }
                    "cancel_session" => send(&mut s, r#"{"type":"success"}"#),
                    _ => send(&mut s, r#"{"type":"error","error_type":"error","description":"unexpected"}"#),
                }
            }
            seen
        })
    }

    fn sock(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rtshell-greetd-{}-{name}.sock", std::process::id()))
    }

    fn secret(s: &str) -> SecretBuf {
        let mut b = SecretBuf::new();
        b.push_str(s);
        b
    }

    fn session() -> SessionCmd {
        SessionCmd { cmd: vec!["rtshell-session".into()], env: vec!["XDG_SESSION_TYPE=wayland".into()] }
    }

    #[test]
    fn login_success_starts_session() {
        let p = sock("ok");
        let h = mock(&p, "tester", "2468\"x", Some("Welcome"), false);
        let out = login(&p, "tester", secret("2468\"x"), &session(), "wrong");
        assert!(out.ok);
        assert_eq!(out.messages, vec!["Welcome".to_string()]);
        let seen = h.join().unwrap();
        assert_eq!(seen.types, ["create_session", "post_auth_message_response", "post_auth_message_response", "start_session"]);
        assert_eq!(seen.started.unwrap(), r#"{"type":"start_session","cmd":["rtshell-session"],"env":["XDG_SESSION_TYPE=wayland"]}"#);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn wrong_secret_cancels() {
        let p = sock("bad");
        let h = mock(&p, "tester", "2468", None, false);
        let out = login(&p, "tester", secret("1357"), &session(), "wrong");
        assert!(!out.ok);
        assert_eq!(out.messages, vec!["wrong".to_string()]);
        let seen = h.join().unwrap();
        assert_eq!(seen.types, ["create_session", "post_auth_message_response", "cancel_session"]);
        assert!(seen.started.is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn second_prompt_is_refused() {
        let p = sock("2fa");
        let h = mock(&p, "tester", "2468", None, true);
        let out = login(&p, "tester", secret("2468"), &session(), "wrong");
        assert!(!out.ok);
        let seen = h.join().unwrap();
        assert_eq!(seen.types.last().unwrap(), "cancel_session");
        assert!(seen.started.is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn no_greetd() {
        let out = login(&sock("none"), "tester", secret("2468"), &session(), "wrong");
        assert!(!out.ok);
    }
}
