//! App search for the All-apps sheet: fuzzy matching over names, generic names, keywords and the program.
//!
//! Each field scores by the best kind of match (whole-field prefix > word prefix > substring > letters in
//! order), weighted by field (name highest). Case-insensitive.

use crate::apps::desktop::DesktopApp;

/// Score of `q` (lowercase, non-empty) in `field`; 0 = no match.
fn field_score(q: &str, field: &str) -> f32 {
    let f = field.to_lowercase();
    if f.starts_with(q) {
        return 100.0 - (f.len() - q.len()).min(40) as f32 * 0.1;
    }
    if let Some(pos) = f.find(q) {
        let word_start = pos == 0 || !f[..pos].chars().next_back().is_some_and(char::is_alphanumeric);
        return if word_start { 80.0 } else { 60.0 } - (pos as f32 * 0.5).min(20.0);
    }
    // Letters in order (e.g. "vsc" in "VSCodium", "lbo" in "LibreOffice"): fewer gaps score higher.
    let mut gaps = 0;
    let mut chars = f.chars();
    for qc in q.chars() {
        let mut skipped = 0;
        loop {
            match chars.next() {
                Some(c) if c == qc => break,
                Some(_) => skipped += 1,
                None => return 0.0,
            }
        }
        gaps += skipped;
    }
    // Scattered letters are noise ("fir" in "Qt Widgets Designer"): allow only a few skipped characters.
    if gaps > 2 + q.chars().count() {
        return 0.0;
    }
    40.0 - gaps as f32 * 2.0
}

pub fn score(q: &str, app: &DesktopApp) -> f32 {
    let mut best = field_score(q, &app.name) * 1.0;
    if let Some(g) = &app.generic_name {
        best = best.max(field_score(q, g) * 0.8);
    }
    for k in &app.keywords {
        best = best.max(field_score(q, k) * 0.7);
    }
    if let Some(prog) = app.exec.first().and_then(|p| std::path::Path::new(p).file_name()) {
        best = best.max(field_score(q, &prog.to_string_lossy()) * 0.6);
    }
    best
}

/// Indices into `apps` of listed apps matching `query`, best first.
pub fn search(apps: &[DesktopApp], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(f32, usize)> =
        apps.iter().enumerate().filter(|(_, a)| a.listed).map(|(i, a)| (score(&q, a), i)).filter(|(s, _)| *s > 0.0).collect();
    hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, generic: Option<&str>, keywords: &[&str], exec: &str) -> DesktopApp {
        DesktopApp {
            id: name.to_lowercase(),
            name: name.into(),
            generic_name: generic.map(String::from),
            comment: None,
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            categories: Vec::new(),
            icon: None,
            exec: vec![exec.into()],
            terminal: false,
            path: None,
            wm_class: None,
            listed: true,
            file: "/x".into(),
        }
    }

    #[test]
    fn ranking() {
        let apps = vec![
            app("Firefox", Some("Web Browser"), &["internet"], "firefox"),
            app("VSCodium", Some("Text Editor"), &["vscode", "code"], "codium"),
            app("Dolphin", Some("File Manager"), &[], "dolphin"),
            app("System Settings", None, &[], "systemsettings"),
        ];
        assert_eq!(search(&apps, "fi"), vec![0, 2]); // Firefox (name prefix), Dolphin ("File Manager")
        assert_eq!(search(&apps, "code")[0], 1);
        assert_eq!(search(&apps, "settings"), vec![3]);
        assert_eq!(search(&apps, "brow"), vec![0]);
        assert_eq!(search(&apps, "vsc"), vec![1]);
        assert!(search(&apps, "zzz").is_empty());
        assert!(search(&apps, "fsg").is_empty()); // f..s..g scattered over "Firefox"/"System Settings"
    }
}
