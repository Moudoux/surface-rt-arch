//! Installed apps from freedesktop `.desktop` files (Desktop Entry Specification 1.5).
//!
//! Scans `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/*/applications` (earlier directories win for the
//! same desktop id), reads the `[Desktop Entry]` group with localized names, expands `Exec` (quoting and
//! field codes), drops `Hidden` entries and `TryExec` misses, and marks entries that should not be listed
//! (`NoDisplay`, `OnlyShowIn`/`NotShowIn` against our desktop name). `watch` rescans on changes (inotify).

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use tracing::info;

/// Our `XDG_CURRENT_DESKTOP` (also set for apps in apps/mod.rs).
pub const DESKTOP_NAME: &str = "rtshell";

#[derive(Debug, Clone, PartialEq)]
pub struct DesktopApp {
    /// Desktop file id without `.desktop` (e.g. `org.gnome.Nautilus`).
    pub id: String,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    /// Icon theme name or absolute path.
    pub icon: Option<String>,
    /// Command line with field codes expanded (no files: we launch apps, not documents).
    pub exec: Vec<String>,
    pub terminal: bool,
    /// Working directory (`Path=`).
    pub path: Option<PathBuf>,
    pub wm_class: Option<String>,
    /// Shown in launchers (not NoDisplay, allowed in our desktop).
    pub listed: bool,
    pub file: PathBuf,
}

impl DesktopApp {
    /// Names a running window of this app may carry as its xdg app_id, lowercased: StartupWMClass, the
    /// desktop id, its last dotted part, and the program's file name.
    pub fn window_ids(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(c) = &self.wm_class {
            v.push(c.to_lowercase());
        }
        v.push(self.id.to_lowercase());
        if let Some(last) = self.id.rsplit('.').next() {
            v.push(last.to_lowercase());
        }
        if let Some(prog) = self.exec.first().and_then(|p| Path::new(p).file_name()) {
            v.push(prog.to_string_lossy().to_lowercase());
        }
        let mut seen = HashSet::new();
        v.retain(|x| seen.insert(x.clone()));
        v
    }
}

/// `applications` directories in priority order.
pub fn app_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".local/share")));
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    data_home
        .into_iter()
        .chain(data_dirs.split(':').filter(|s| !s.is_empty()).map(PathBuf::from))
        .map(|d| d.join("applications"))
        .collect()
}

/// Directories to watch: every applications dir and its subdirectories, plus the parents (so a dir that
/// does not exist yet is noticed when it is created).
pub fn watch_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for d in app_dirs() {
        if let Some(p) = d.parent() {
            v.push(p.to_path_buf());
        }
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    v.push(e.path());
                }
            }
        }
        v.push(d);
    }
    v
}

/// The user's locale for `Key[locale]` lookups, most specific first (`de_AT@euro` -> de_AT@euro, de_AT,
/// de@euro, de).
fn locale_keys() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    let base = raw.split('.').next().unwrap_or("");
    let modifier = raw.split_once('@').map(|(_, m)| m.to_string());
    let base = base.split('@').next().unwrap_or("");
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Vec::new();
    }
    let lang = base.split('_').next().unwrap_or(base);
    let mut v = Vec::new();
    if let Some(m) = &modifier {
        v.push(format!("{base}@{m}"));
    }
    v.push(base.to_string());
    if let Some(m) = &modifier {
        v.push(format!("{lang}@{m}"));
    }
    v.push(lang.to_string());
    v.dedup();
    v
}

/// Undo the spec's string escapes (\s \n \t \r \\).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A `;`-separated list (`\;` is a literal semicolon).
fn list(s: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut cur = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some(';') => cur.push(';'),
                Some(o) => {
                    cur.push('\\');
                    cur.push(o);
                }
                None => {}
            },
            ';' => v.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    v.push(cur);
    v.into_iter().map(|x| unescape(x.trim())).filter(|x| !x.is_empty()).collect()
}

/// Split an (already unescaped) Exec value into arguments: double quotes group, and inside them
/// \" \` \$ \\ are escapes.
fn split_exec(s: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                have = true;
                loop {
                    match it.next()? {
                        '"' => break,
                        '\\' => match it.next()? {
                            e @ ('"' | '`' | '$' | '\\') => cur.push(e),
                            o => {
                                cur.push('\\');
                                cur.push(o);
                            }
                        },
                        o => cur.push(o),
                    }
                }
            }
            ' ' | '\t' => {
                if have {
                    args.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            o => {
                cur.push(o);
                have = true;
            }
        }
    }
    if have {
        args.push(cur);
    }
    Some(args)
}

/// Expand field codes. File/URL codes vanish (we start apps without documents).
fn expand_exec(args: Vec<String>, name: &str, icon: Option<&str>, file: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        match a.as_str() {
            "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => continue,
            "%i" => {
                if let Some(i) = icon {
                    out.push("--icon".into());
                    out.push(i.into());
                }
                continue;
            }
            _ => {}
        }
        let mut s = String::new();
        let mut it = a.chars();
        while let Some(c) = it.next() {
            if c != '%' {
                s.push(c);
                continue;
            }
            match it.next() {
                Some('%') => s.push('%'),
                Some('c') => s.push_str(name),
                Some('k') => s.push_str(&file.to_string_lossy()),
                _ => {}
            }
        }
        out.push(s);
    }
    out
}

fn in_path(prog: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let exe = |p: &Path| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if prog.contains('/') {
        return exe(Path::new(prog));
    }
    let home_bin = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).chain(home_bin).any(|d| exe(&d.join(prog)))
}

/// Parse one desktop file. None if it is not a launchable application (or is `Hidden`).
pub fn parse(id: &str, file: &Path, text: &str, locales: &[String]) -> Option<DesktopApp> {
    let mut in_entry = false;
    let mut kv: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if in_entry && let Some((k, v)) = line.split_once('=') {
            kv.entry(k.trim()).or_insert(v.trim());
        }
    }
    let get = |k: &str| kv.get(k).copied();
    let local = |k: &str| {
        locales
            .iter()
            .find_map(|l| kv.get(format!("{k}[{l}]").as_str()).copied())
            .or_else(|| get(k))
            .map(unescape)
    };
    let yes = |k: &str| get(k).is_some_and(|v| v == "true");
    if get("Type") != Some("Application") || yes("Hidden") {
        return None;
    }
    let name = local("Name")?;
    if let Some(t) = get("TryExec").map(unescape)
        && !in_path(&t)
    {
        return None;
    }
    let icon = local("Icon").filter(|s| !s.is_empty());
    let exec = split_exec(&unescape(get("Exec")?))?;
    let exec = expand_exec(exec, &name, icon.as_deref(), file);
    if exec.is_empty() {
        return None;
    }
    // We are the desktop: the compositor's own XDG_CURRENT_DESKTOP (often unset on a VT) does not matter.
    let ours = |l: Vec<String>| l.iter().any(|d| d.eq_ignore_ascii_case(DESKTOP_NAME));
    let mut listed = !yes("NoDisplay");
    if let Some(only) = get("OnlyShowIn") {
        listed &= ours(list(only));
    }
    if let Some(not) = get("NotShowIn") {
        listed &= !ours(list(not));
    }
    Some(DesktopApp {
        id: id.to_string(),
        name,
        generic_name: local("GenericName"),
        comment: local("Comment"),
        keywords: local("Keywords").map(|k| list(&k)).unwrap_or_default(),
        categories: get("Categories").map(list).unwrap_or_default(),
        icon,
        exec,
        terminal: yes("Terminal"),
        path: get("Path").map(unescape).filter(|p| !p.is_empty()).map(PathBuf::from),
        wm_class: get("StartupWMClass").map(unescape),
        listed,
        file: file.to_path_buf(),
    })
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        let fname = e.file_name().to_string_lossy().into_owned();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            if depth < 3 {
                collect(&path, &format!("{prefix}{fname}-"), out, depth + 1);
            }
        } else if let Some(stem) = fname.strip_suffix(".desktop") {
            out.push((format!("{prefix}{stem}"), path));
        }
    }
}

/// Scan every applications dir. All parsed apps (listed or not), sorted by name.
pub fn scan() -> Vec<DesktopApp> {
    let started = std::time::Instant::now();
    let locales = locale_keys();
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in app_dirs() {
        let mut files = Vec::new();
        collect(&dir, "", &mut files, 0);
        for (id, path) in files {
            // The first directory that has an id owns it, even when that file hides the app.
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if let Some(app) = parse(&id, &path, &text, &locales) {
                apps.push(app);
            }
        }
    }
    apps.sort_by_cached_key(|a| (a.name.to_lowercase(), a.id.clone()));
    info!(apps = apps.len(), listed = apps.iter().filter(|a| a.listed).count(), ms = started.elapsed().as_millis() as u64, "desktop files scanned");
    apps
}

/// Scan on a worker thread now and again whenever an applications dir changes; `on_scan` runs on that
/// thread with each new list (only when it differs from the last one).
pub fn watch(mut on_scan: impl FnMut(Vec<DesktopApp>) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("apps-scan".into()).spawn(move || {
        let mut last = scan();
        on_scan(last.clone());
        crate::watch::spawn("apps", watch_dirs, move || {
            let apps = scan();
            if apps != last {
                last = apps.clone();
                on_scan(apps);
            }
        });
    });
    if let Err(err) = spawned {
        tracing::warn!(%err, "could not start the app scanner");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quoting_and_codes() {
        let args = split_exec(r#"foo "a b" c\ d "x\"y" %U --name=%c %%"#).unwrap();
        assert_eq!(args, vec!["foo", "a b", r"c\", "d", "x\"y", "%U", "--name=%c", "%%"]);
        let out = expand_exec(args, "Foo", Some("ico"), Path::new("/f.desktop"));
        assert_eq!(out, vec!["foo", "a b", r"c\", "d", "x\"y", "--name=Foo", "%"]);
        assert_eq!(split_exec(r#"bad "open"#), None);
    }

    #[test]
    fn parse_entry() {
        let text = "\
[Desktop Entry]
Type=Application
Name=Files
Name[de]=Dateien
GenericName=File Manager
Keywords=folder;explorer\\;x;
Exec=nautilus --new-window %U
Icon=org.gnome.Nautilus
Categories=GTK;Utility;
StartupWMClass=Nautilus

[Desktop Action new]
Name=Other
Exec=nope
";
        let a = parse("org.gnome.Nautilus", Path::new("/x.desktop"), text, &["de_DE".into(), "de".into()]).unwrap();
        assert_eq!(a.name, "Dateien");
        assert_eq!(a.exec, vec!["nautilus", "--new-window"]);
        assert_eq!(a.keywords, vec!["folder", "explorer;x"]);
        assert!(a.listed);
        assert_eq!(a.window_ids(), vec!["nautilus".to_string(), "org.gnome.nautilus".to_string()]);
        let hidden = text.replace("Type=Application", "Type=Application\nNoDisplay=true");
        assert!(!parse("x", Path::new("/x"), &hidden, &[]).unwrap().listed);
        let gone = text.replace("Type=Application", "Type=Application\nHidden=true");
        assert!(parse("x", Path::new("/x"), &gone, &[]).is_none());
        let kde = text.replace("Type=Application", "Type=Application\nOnlyShowIn=KDE;");
        assert!(!parse("x", Path::new("/x"), &kde, &[]).unwrap().listed);
    }
}
