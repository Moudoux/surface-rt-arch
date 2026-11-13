//! Unlocking the lock screen (DESKTOP-SHELL.md Phase 8 design): the account password checked by PAM, in
//! process, on a short-lived "auth" thread per attempt.
//!
//! libpam is opened with `dlopen("libpam.so.0")` at the first attempt, through the few functions declared
//! here, so the build needs no libpam (the cross sysroot has none). Without libpam nothing can unlock, which
//! is logged; the lock stays (fail-closed).
//!
//! Secrets: the typed text lives in a `SecretBuf` (fixed capacity, so it never reallocates and leaves
//! copies behind; zeroed on every pop, clear and drop; Debug hidden). It is moved into the auth thread; the
//! only copy is the malloc'd reply PAM asks for, which PAM itself overwrites and frees (Linux-PAM's
//! `_pam_drop_reply`). Messages from PAM modules (e.g. pam_faillock's "account locked") are handed to the UI
//! to show but are never logged; only the PAM return code is.
//!
//! The `test-auth` cargo feature replaces PAM with a fake that accepts only `TEST_PIN` (scripted tests on the
//! device without a real password). It is never part of a default build.

// The PAM half is unused in a `test-auth` build.
#![cfg_attr(feature = "test-auth", allow(dead_code))]

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    sync::{Arc, Mutex},
};

use smithay::reexports::calloop::ping::Ping;
use tracing::{info, warn};

/// The secret the `test-auth` build accepts (a throwaway, never a real password).
#[cfg(feature = "test-auth")]
pub const TEST_PIN: &str = "2468";

/// Longest secret accepted, in bytes (more is refused, not reallocated).
pub const MAX_SECRET: usize = 256;

/// Overwrite bytes so the compiler cannot drop the stores as dead.
pub fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: a valid, aligned, exclusive reference.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Typed secret text. Fixed capacity (`MAX_SECRET`), wiped on every change that drops bytes and on drop.
pub struct SecretBuf(Vec<u8>);

impl SecretBuf {
    pub fn new() -> SecretBuf {
        SecretBuf(Vec::with_capacity(MAX_SECRET))
    }

    /// Append text; false (nothing added) if it would not fit.
    pub fn push_str(&mut self, s: &str) -> bool {
        if self.0.len() + s.len() > MAX_SECRET {
            return false;
        }
        self.0.extend_from_slice(s.as_bytes());
        true
    }

    /// Remove the last character (its bytes are zeroed in the spare capacity).
    pub fn pop(&mut self) {
        let Ok(s) = std::str::from_utf8(&self.0) else {
            return self.clear();
        };
        let Some((i, _)) = s.char_indices().next_back() else { return };
        let len = self.0.len();
        wipe(&mut self.0[i..len]);
        self.0.truncate(i);
    }

    pub fn clear(&mut self) {
        let len = self.0.len();
        wipe(&mut self.0[..len]);
        self.0.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Characters typed (for drawing bullets).
    pub fn chars(&self) -> usize {
        std::str::from_utf8(&self.0).map_or(0, |s| s.chars().count())
    }

    /// Hand the contents over (for an attempt), leaving this one empty.
    pub fn take(&mut self) -> SecretBuf {
        std::mem::replace(self, SecretBuf::new())
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Default for SecretBuf {
    fn default() -> Self {
        SecretBuf::new()
    }
}

impl Drop for SecretBuf {
    fn drop(&mut self) {
        let cap = self.0.capacity();
        // SAFETY: the whole allocation is ours; bytes past `len` are plain u8 (any value is valid).
        unsafe {
            self.0.set_len(cap);
        }
        wipe(&mut self.0);
        self.0.clear();
    }
}

impl std::fmt::Debug for SecretBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBuf(..)")
    }
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    /// What the PAM modules said (shown on the lock screen, never logged), else a short reason.
    pub messages: Vec<String>,
}

/// The logged-in user (who the lock screen unlocks for).
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub name: String,
    /// The GECOS full name, else the login name.
    pub display: String,
}

/// One `/etc/passwd` line as a person who can sign in: uid 1000..60000 and a login shell (not nologin /
/// false / empty).
fn passwd_user(line: &str) -> Option<(u32, User)> {
    let f: Vec<&str> = line.split(':').collect();
    if f.len() < 7 || f[0].is_empty() || f[0].starts_with('#') {
        return None;
    }
    let uid: u32 = f[2].parse().ok()?;
    let shell = f[6].trim();
    if !(1000..60000).contains(&uid) || shell.is_empty() || shell.ends_with("/nologin") || shell.ends_with("/false") {
        return None;
    }
    let full = f[4].split(',').next().unwrap_or("").trim();
    Some((uid, User { name: f[0].to_string(), display: if full.is_empty() { f[0].to_string() } else { full.to_string() } }))
}

/// The people who can sign in (the greeter's user picker), by uid, from passwd-format text.
pub fn users_from(text: &str) -> Vec<User> {
    let mut v: Vec<(u32, User)> = text.lines().filter_map(passwd_user).collect();
    v.sort_by_key(|(uid, _)| *uid);
    v.dedup_by(|a, b| a.1.name == b.1.name);
    v.into_iter().map(|(_, u)| u).collect()
}

impl User {
    /// The people who can sign in, from `/etc/passwd` (the greeter's user picker).
    /// `RTSHELL_PASSWD` (tests: more names in the picker) reads another passwd-format file; it only
    /// changes who is listed, greetd still checks every sign-in.
    pub fn list() -> Vec<User> {
        let path = std::env::var_os("RTSHELL_PASSWD").unwrap_or_else(|| "/etc/passwd".into());
        match std::fs::read_to_string(&path) {
            Ok(t) => users_from(&t),
            Err(err) => {
                warn!(%err, path = %std::path::Path::new(&path).display(), "auth: cannot read the user list");
                Vec::new()
            }
        }
    }

    pub fn current() -> Option<User> {
        // SAFETY: getpwuid_r with a buffer we own; the result points into it.
        unsafe {
            let mut pw: libc::passwd = std::mem::zeroed();
            let mut out: *mut libc::passwd = std::ptr::null_mut();
            let mut buf = vec![0 as c_char; 4096];
            let rc = libc::getpwuid_r(libc::getuid(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out);
            if rc != 0 || out.is_null() || pw.pw_name.is_null() {
                return None;
            }
            let name = CStr::from_ptr(pw.pw_name).to_string_lossy().into_owned();
            let gecos = if pw.pw_gecos.is_null() { String::new() } else { CStr::from_ptr(pw.pw_gecos).to_string_lossy().into_owned() };
            let full = gecos.split(',').next().unwrap_or("").trim().to_string();
            Some(User { display: if full.is_empty() { name.clone() } else { full }, name })
        }
    }
}

/// What checks the secret.
#[derive(Debug, Clone)]
pub enum Backend {
    /// The lock screen: PAM in this process, for the logged-in user.
    Pam { service: String },
    /// The greeter: greetd's IPC socket; success also starts `session` for the chosen user.
    Greetd { sock: std::path::PathBuf, session: super::greetd::SessionCmd, wrong_password: String },
}

/// Runs attempts on a worker thread and hands the outcome back through a slot + the UI's ping.
pub struct Authenticator {
    backend: Backend,
    user: Option<User>,
    slot: Arc<Mutex<Option<Outcome>>>,
    ping: Ping,
    busy: bool,
}

impl Authenticator {
    /// The lock screen's: PAM `service`, the current user.
    pub fn new(service: &str, ping: Ping) -> Authenticator {
        let user = User::current();
        if user.is_none() {
            warn!("auth: cannot find the current user; unlocking will fail");
        }
        #[cfg(feature = "test-auth")]
        warn!("auth: TEST BUILD: the lock screen accepts only the fixed test PIN, PAM is not used");
        Authenticator { backend: Backend::Pam { service: service.to_string() }, user, slot: Arc::new(Mutex::new(None)), ping, busy: false }
    }

    /// The greeter's: greetd at `backend`, no user until one is picked (`set_user`).
    pub fn with_backend(backend: Backend, ping: Ping) -> Authenticator {
        Authenticator { backend, user: None, slot: Arc::new(Mutex::new(None)), ping, busy: false }
    }

    pub fn user(&self) -> Option<&User> {
        self.user.as_ref()
    }

    /// Who the next attempt is for (the greeter's picker). Ignored while an attempt runs.
    pub fn set_user(&mut self, user: Option<User>) {
        if !self.busy {
            self.user = user;
        }
    }

    /// An attempt is running.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Check `secret` (moved in, wiped when the attempt ends). False if one is already running.
    pub fn start(&mut self, secret: SecretBuf) -> bool {
        if self.busy {
            return false;
        }
        let Some(user) = self.user.clone() else {
            *self.slot.lock().unwrap() = Some(Outcome { ok: false, messages: vec!["Unknown user".into()] });
            self.ping.ping();
            return true;
        };
        self.busy = true;
        let (backend, slot, ping) = (self.backend.clone(), self.slot.clone(), self.ping.clone());
        let res = std::thread::Builder::new().name("auth".into()).spawn(move || {
            let started = std::time::Instant::now();
            let outcome = match &backend {
                Backend::Pam { service } => check(service, &user.name, secret),
                Backend::Greetd { sock, session, wrong_password } => super::greetd::login(sock, &user.name, secret, session, wrong_password),
            };
            info!(ok = outcome.ok, ms = started.elapsed().as_millis() as u64, "auth: attempt finished");
            *slot.lock().unwrap() = Some(outcome);
            ping.ping();
        });
        if let Err(err) = res {
            warn!(%err, "auth: cannot start thread");
            self.busy = false;
            *self.slot.lock().unwrap() = Some(Outcome { ok: false, messages: vec!["Cannot check the password now".into()] });
            self.ping.ping();
        }
        true
    }

    /// The outcome of the running attempt, once it is there.
    pub fn poll(&mut self) -> Option<Outcome> {
        let o = self.slot.lock().unwrap().take()?;
        self.busy = false;
        Some(o)
    }
}

#[cfg(feature = "test-auth")]
fn check(_service: &str, _user: &str, secret: SecretBuf) -> Outcome {
    let ok = secret.bytes() == TEST_PIN.as_bytes();
    drop(secret);
    std::thread::sleep(std::time::Duration::from_millis(if ok { 200 } else { 1000 }));
    Outcome { ok, messages: if ok { Vec::new() } else { vec!["That's not the test PIN.".into()] } }
}

#[cfg(not(feature = "test-auth"))]
fn check(service: &str, user: &str, secret: SecretBuf) -> Outcome {
    pam_check(service, user, secret)
}

// ---- PAM through dlopen

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

struct Pam {
    start: unsafe extern "C" fn(*const c_char, *const c_char, *const PamConv, *mut *mut c_void) -> c_int,
    authenticate: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    acct_mgmt: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    end: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
}

fn pam() -> Option<&'static Pam> {
    static PAM: std::sync::OnceLock<Option<Pam>> = std::sync::OnceLock::new();
    PAM.get_or_init(|| {
        // SAFETY: dlopen/dlsym with NUL-terminated names; the symbols have the declared C signatures (pam(3)).
        unsafe {
            let h = libc::dlopen(c"libpam.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if h.is_null() {
                warn!("auth: libpam.so.0 not found; the lock screen cannot unlock");
                return None;
            }
            let sym = |name: &CStr| {
                let p = libc::dlsym(h, name.as_ptr());
                (!p.is_null()).then_some(p)
            };
            Some(Pam {
                start: std::mem::transmute::<*mut c_void, _>(sym(c"pam_start")?),
                authenticate: std::mem::transmute::<*mut c_void, _>(sym(c"pam_authenticate")?),
                acct_mgmt: std::mem::transmute::<*mut c_void, _>(sym(c"pam_acct_mgmt")?),
                end: std::mem::transmute::<*mut c_void, _>(sym(c"pam_end")?),
            })
        }
    })
    .as_ref()
}

/// What the conversation function works with.
struct ConvData {
    secret: SecretBuf,
    /// The secret was given once (a second password prompt gets an error, not the same secret).
    answered: bool,
    messages: Vec<String>,
}

unsafe extern "C" fn conversation(n: c_int, msgs: *mut *const PamMessage, resp: *mut *mut PamResponse, data: *mut c_void) -> c_int {
    // SAFETY: PAM passes `n` message pointers (Linux-PAM layout: an array of pointers) and our ConvData.
    unsafe {
        if n <= 0 || msgs.is_null() || resp.is_null() || data.is_null() {
            return PAM_CONV_ERR;
        }
        let data = &mut *(data as *mut ConvData);
        let replies = libc::calloc(n as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
        if replies.is_null() {
            return PAM_BUF_ERR;
        }
        for i in 0..n as usize {
            let m = *msgs.add(i);
            if m.is_null() {
                continue;
            }
            let text = || if (*m).msg.is_null() { String::new() } else { CStr::from_ptr((*m).msg).to_string_lossy().into_owned() };
            match (*m).msg_style {
                PAM_PROMPT_ECHO_OFF if !data.answered => {
                    let b = data.secret.bytes();
                    let p = libc::malloc(b.len() + 1) as *mut u8;
                    if p.is_null() {
                        free_replies(replies, i);
                        return PAM_BUF_ERR;
                    }
                    std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len());
                    *p.add(b.len()) = 0;
                    (*replies.add(i)).resp = p as *mut c_char;
                    data.answered = true;
                }
                PAM_ERROR_MSG | PAM_TEXT_INFO => {
                    let t = text();
                    if !t.trim().is_empty() {
                        data.messages.push(t.trim().to_string());
                    }
                }
                // A user name prompt (we always pass the user) or a second secret prompt (2FA): not ours.
                _ => {
                    free_replies(replies, i);
                    return PAM_CONV_ERR;
                }
            }
        }
        *resp = replies;
        PAM_SUCCESS
    }
}

/// Free (and wipe) the first `n` replies and the array, after a conversation failed half-way.
unsafe fn free_replies(replies: *mut PamResponse, n: usize) {
    // SAFETY: `replies` came from calloc with at least `n` entries; their `resp` are ours (malloc) or null.
    unsafe {
        for i in 0..n {
            let r = (*replies.add(i)).resp;
            if !r.is_null() {
                let len = libc::strlen(r);
                wipe(std::slice::from_raw_parts_mut(r as *mut u8, len));
                libc::free(r as *mut c_void);
            }
        }
        libc::free(replies as *mut c_void);
    }
}

fn pam_check(service: &str, user: &str, secret: SecretBuf) -> Outcome {
    let fail = |m: &str| Outcome { ok: false, messages: vec![m.to_string()] };
    let Some(pam) = pam() else { return fail("PAM is not available") };
    let (Ok(c_service), Ok(c_user)) = (CString::new(service), CString::new(user)) else { return fail("Bad service or user name") };
    if secret.bytes().contains(&0) {
        return fail("Bad input");
    }
    let mut data = Box::new(ConvData { secret, answered: false, messages: Vec::new() });
    let conv = PamConv { conv: conversation, appdata_ptr: &mut *data as *mut ConvData as *mut c_void };
    let mut handle: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid C strings, a conversation that outlives the handle (both live until pam_end below).
    let rc = unsafe {
        let mut rc = (pam.start)(c_service.as_ptr(), c_user.as_ptr(), &conv, &mut handle);
        if rc == PAM_SUCCESS {
            rc = (pam.authenticate)(handle, 0);
            if rc == PAM_SUCCESS {
                rc = (pam.acct_mgmt)(handle, 0);
            }
            (pam.end)(handle, rc);
        }
        rc
    };
    info!(service, rc, "auth: pam result");
    let ConvData { secret, messages, .. } = *data;
    drop(secret);
    if rc == PAM_SUCCESS {
        return Outcome { ok: true, messages };
    }
    let mut messages = messages;
    if messages.is_empty() {
        messages.push(match rc {
            7 => "The password is incorrect. Try again.".into(),  // PAM_AUTH_ERR
            9 => "Cannot check the password right now.".into(),   // PAM_AUTHINFO_UNAVAIL
            10 => "Unknown user.".into(),                            // PAM_USER_UNKNOWN
            11 => "Too many attempts. Try again later.".into(),     // PAM_MAXTRIES
            12 => "The password has expired.".into(),               // PAM_NEW_AUTHTOK_REQD
            13 => "The account has expired.".into(),                // PAM_ACCT_EXPIRED
            _ => "Sign-in failed.".into(),
        });
    }
    Outcome { ok: false, messages }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_buf_edits() {
        let mut s = SecretBuf::new();
        assert!(s.push_str("ab"));
        assert!(s.push_str("é"));
        assert_eq!(s.chars(), 3);
        s.pop();
        assert_eq!(s.bytes(), b"ab");
        s.pop();
        s.pop();
        s.pop();
        assert!(s.is_empty());
        assert!(!s.push_str(&"x".repeat(MAX_SECRET + 1)));
        assert!(s.push_str(&"x".repeat(MAX_SECRET)));
        assert_eq!(s.0.capacity(), MAX_SECRET);
        let t = s.take();
        assert!(s.is_empty());
        assert_eq!(t.chars(), MAX_SECRET);
        assert_eq!(format!("{t:?}"), "SecretBuf(..)");
    }

    #[test]
    fn passwd_users() {
        let t = "root:x:0:0::/root:/bin/bash\n\
                 greeter:x:962:962:greetd greeter:/:/bin/bash\n\
                 bob:x:1001:1001:Bob B,,,:/home/bob:/usr/bin/zsh\n\
                 alarm:x:1000:1000::/home/alarm:/bin/bash\n\
                 svc:x:1002:1002::/:/usr/bin/nologin\n\
                 off:x:1003:1003::/:/bin/false\n\
                 nobody:x:65534:65534:Kernel Overflow User:/:/usr/bin/nologin\n\
                 broken:x:abc\n";
        let u = users_from(t);
        assert_eq!(u, vec![User { name: "alarm".into(), display: "alarm".into() }, User { name: "bob".into(), display: "Bob B".into() }]);
    }

    #[test]
    fn pop_wipes_bytes() {
        let mut s = SecretBuf::new();
        s.push_str("xyz");
        s.pop();
        // SAFETY: reading our own spare capacity (initialised by the earlier push).
        let spare = unsafe { *s.0.as_ptr().add(2) };
        assert_eq!(spare, 0);
    }

    /// The PAM plumbing (dlopen, conversation, return codes) against a service that does not exist, which
    /// Linux-PAM answers with /etc/pam.d/other (deny). No real account is tried, so pam_faillock is not
    /// involved.
    #[cfg(not(feature = "test-auth"))]
    #[test]
    fn pam_unknown_service_denies() {
        if pam().is_none() {
            return; // no libpam on this machine
        }
        let mut s = SecretBuf::new();
        s.push_str("not-a-password");
        let out = pam_check("rtshell-test-no-such-service", "nobody", s);
        assert!(!out.ok);
        assert!(!out.messages.is_empty());
    }
}
