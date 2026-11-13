//! The springboard's layout (style "ipad", DESKTOP-SHELL.md Phase 10): pages of app icons, the dock, recently
//! launched apps and apps removed from Home, saved in `~/.config/rtshell/springboard.toml`.
//!
//! Entries are desktop ids. Until the user changes anything the layout is the first-run default built from the
//! installed apps and is not written to disk (like `pins.rs`). Apps installed later are added at the end of
//! the last page; ids that are not installed are kept in the file but not shown.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::apps::desktop::DesktopApp;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub apps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Board {
    #[serde(default)]
    pub dock: Vec<String>,
    /// Most recent first; never holds a docked app.
    #[serde(default)]
    pub recent: Vec<String>,
    /// Removed from Home (still found by search).
    #[serde(default)]
    pub hidden: Vec<String>,
    #[serde(default, rename = "page")]
    pub pages: Vec<Page>,
    /// The widget column (landscape, first page), top to bottom: "date", "battery" (small), "month",
    /// "network" (wide). Widgets that do not fit above the dock are left out.
    #[serde(default = "default_widgets")]
    pub widgets: Vec<String>,
}

pub fn default_widgets() -> Vec<String> {
    ["date", "battery", "month", "network"].iter().map(|s| s.to_string()).collect()
}

pub fn path() -> Option<PathBuf> {
    crate::config::default_path().map(|p| p.with_file_name("springboard.toml"))
}

fn first<'a>(apps: &'a [DesktopApp], ids: &[&str]) -> Option<&'a DesktopApp> {
    ids.iter().find_map(|id| apps.iter().find(|a| a.id.eq_ignore_ascii_case(id)))
}

impl Board {
    pub fn load() -> Option<Board> {
        let path = path()?;
        let text = std::fs::read_to_string(&path).ok()?;
        match toml::from_str::<Board>(&text) {
            Ok(b) => {
                info!(path = %path.display(), pages = b.pages.len(), "springboard layout loaded");
                Some(b)
            }
            Err(err) => {
                warn!(path = %path.display(), %err, "bad springboard layout, using defaults");
                None
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        let text = match toml::to_string(self) {
            Ok(t) => t,
            Err(err) => return warn!(%err, "cannot serialize springboard layout"),
        };
        let header = "# rtshell springboard layout (style \"ipad\"), written by the shell when you edit Home.\n\n";
        let tmp = path.with_extension("toml.tmp");
        let res = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&tmp, format!("{header}{text}")))
            .and_then(|_| std::fs::rename(&tmp, &path));
        match res {
            Ok(()) => info!(path = %path.display(), "springboard layout saved"),
            Err(err) => warn!(path = %path.display(), %err, "cannot save springboard layout"),
        }
    }

    /// First-run layout: browser, terminal, files and editor in the dock (what is installed), every other
    /// listed app A-Z on the pages (`per_page` icons each).
    pub fn defaults(apps: &[DesktopApp], per_page: usize) -> Board {
        let picks: [&[&str]; 4] = [
            &["chromium", "chromium-browser", "org.chromium.Chromium", "firefox", "org.mozilla.firefox"],
            &["foot", "org.codeberg.dnkl.foot", "org.kde.konsole", "org.gnome.Console", "org.gnome.Terminal"],
            &["org.kde.dolphin", "org.gnome.Nautilus", "thunar", "pcmanfm"],
            &["codium", "vscodium", "com.vscodium.codium", "code", "code-oss", "com.visualstudio.code"],
        ];
        let dock: Vec<String> = picks.iter().filter_map(|ids| first(apps, ids)).map(|a| a.id.clone()).collect();
        let mut b = Board { dock, widgets: default_widgets(), ..Board::default() };
        b.reconcile(apps, per_page);
        b
    }

    fn placed(&self, id: &str) -> bool {
        self.dock.iter().chain(&self.hidden).chain(self.pages.iter().flat_map(|p| &p.apps)).any(|x| x == id)
    }

    /// Add listed apps the layout does not know yet at the end of the last page (new pages when full).
    /// Returns true if something was added.
    pub fn reconcile(&mut self, apps: &[DesktopApp], per_page: usize) -> bool {
        let per_page = per_page.max(1);
        let mut new: Vec<&DesktopApp> = apps.iter().filter(|a| a.listed && !self.placed(&a.id)).collect();
        if new.is_empty() {
            return false;
        }
        new.sort_by_key(|a| a.name.to_lowercase());
        for a in new {
            if self.pages.last().is_none_or(|p| p.apps.len() >= per_page) {
                self.pages.push(Page::default());
            }
            self.pages.last_mut().unwrap().apps.push(a.id.clone());
        }
        true
    }

    /// The pages as shown: installed apps only, each stored page split into `per_page` chunks, empty pages
    /// dropped (at least one page).
    pub fn shown_pages(&self, installed: impl Fn(&str) -> bool, per_page: usize) -> Vec<Vec<String>> {
        let per_page = per_page.max(1);
        let mut out = Vec::new();
        for p in &self.pages {
            let ids: Vec<String> = p.apps.iter().filter(|id| installed(id)).cloned().collect();
            for chunk in ids.chunks(per_page) {
                out.push(chunk.to_vec());
            }
        }
        if out.is_empty() {
            out.push(Vec::new());
        }
        out
    }

    /// Replace the pages with what is shown (after an edit), keeping ids that are not installed where they
    /// were (at the end of their old page).
    pub fn set_pages(&mut self, shown: Vec<Vec<String>>, installed: impl Fn(&str) -> bool) {
        let missing: Vec<String> = self.pages.iter().flat_map(|p| &p.apps).filter(|id| !installed(id)).cloned().collect();
        self.pages = shown.into_iter().filter(|p| !p.is_empty()).map(|apps| Page { apps }).collect();
        if !missing.is_empty() {
            if self.pages.is_empty() {
                self.pages.push(Page::default());
            }
            self.pages.last_mut().unwrap().apps.extend(missing);
        }
    }

    /// Remember a launch: first in `recent` (unless docked), at most `keep` entries.
    pub fn launched(&mut self, id: &str, keep: usize) -> bool {
        if self.dock.iter().any(|d| d == id) {
            return false;
        }
        if self.recent.first().is_some_and(|r| r == id) {
            return false;
        }
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(keep.max(1));
        true
    }

    /// Remove an app from Home (pages and dock); search still finds it.
    pub fn hide(&mut self, id: &str) {
        for p in &mut self.pages {
            p.apps.retain(|a| a != id);
        }
        self.dock.retain(|a| a != id);
        self.recent.retain(|a| a != id);
        if !self.hidden.iter().any(|h| h == id) {
            self.hidden.push(id.to_string());
        }
    }

    /// Put a hidden app back at the end of the last page.
    pub fn unhide(&mut self, id: &str, per_page: usize) -> bool {
        if !self.hidden.iter().any(|h| h == id) {
            return false;
        }
        self.hidden.retain(|h| h != id);
        if self.pages.last().is_none_or(|p| p.apps.len() >= per_page.max(1)) {
            self.pages.push(Page::default());
        }
        self.pages.last_mut().unwrap().apps.push(id.to_string());
        true
    }

    #[cfg(test)]
    pub fn on_home(&self, id: &str) -> bool {
        self.dock.iter().chain(self.pages.iter().flat_map(|p| &p.apps)).any(|x| x == id)
    }
}

/// Where an icon sits in the shown layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Page(usize, usize),
    Dock(usize),
}

/// Move the app at `from` to `to` in the shown pages and dock. A page that overflows pushes its last icon to
/// the start of the next page (a new one at the end); the dock refuses more than `dock_max`. Returns false if
/// nothing moved.
pub fn move_icon(pages: &mut Vec<Vec<String>>, dock: &mut Vec<String>, from: Slot, to: Slot, per_page: usize, dock_max: usize) -> bool {
    if from == to {
        return false;
    }
    if matches!(to, Slot::Dock(_)) && !matches!(from, Slot::Dock(_)) && dock.len() >= dock_max {
        return false;
    }
    let id = match from {
        Slot::Page(p, i) if p < pages.len() && i < pages[p].len() => pages[p].remove(i),
        Slot::Dock(i) if i < dock.len() => dock.remove(i),
        _ => return false,
    };
    match to {
        Slot::Dock(i) => dock.insert(i.min(dock.len()), id),
        Slot::Page(p, i) => {
            while pages.len() <= p {
                pages.push(Vec::new());
            }
            let i = i.min(pages[p].len());
            pages[p].insert(i, id);
            // Overflow ripples forward.
            let mut q = p;
            while pages[q].len() > per_page.max(1) {
                let last = pages[q].pop().unwrap();
                if q + 1 >= pages.len() {
                    pages.push(Vec::new());
                }
                pages[q + 1].insert(0, last);
                q += 1;
            }
        }
    }
    // Empty pages close up (keep one).
    pages.retain(|p| !p.is_empty());
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str) -> DesktopApp {
        let text = format!("[Desktop Entry]\nType=Application\nName={name}\nExec={id}\n");
        crate::apps::desktop::parse(id, std::path::Path::new("/x.desktop"), &text, &[]).unwrap()
    }

    #[test]
    fn defaults_dock_and_pages() {
        let apps = vec![app("foot", "Foot"), app("htop", "Htop"), app("chromium", "Chromium"), app("a", "Alpha"), app("b", "Beta"), app("c", "Gamma")];
        let b = Board::defaults(&apps, 2);
        assert_eq!(b.dock, ["chromium", "foot"]);
        assert_eq!(b.pages.len(), 2);
        assert_eq!(b.pages[0].apps, ["a", "b"]);
        assert_eq!(b.pages[1].apps, ["c", "htop"]);
        let text = toml::to_string(&b).unwrap();
        assert_eq!(toml::from_str::<Board>(&text).unwrap(), b);
    }

    #[test]
    fn hide_unhide_recent() {
        let apps = vec![app("a", "A"), app("b", "B")];
        let mut b = Board::defaults(&apps, 4);
        b.hide("a");
        assert!(!b.on_home("a"));
        assert!(!b.reconcile(&apps, 4), "hidden apps do not come back");
        assert!(b.unhide("a", 4));
        assert_eq!(b.pages[0].apps, ["b", "a"]);
        assert!(b.launched("b", 2));
        assert!(b.launched("a", 2));
        assert_eq!(b.recent, ["a", "b"]);
    }

    #[test]
    fn moving_icons() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let mut pages = vec![s(&["a", "b", "c"]), s(&["d"])];
        let mut dock = s(&["x"]);
        // Into a full page: c ripples to the next page.
        assert!(move_icon(&mut pages, &mut dock, Slot::Dock(0), Slot::Page(0, 1), 3, 4));
        assert_eq!(pages, vec![s(&["a", "x", "b"]), s(&["c", "d"])]);
        assert!(dock.is_empty());
        // Into the dock, emptying nothing.
        assert!(move_icon(&mut pages, &mut dock, Slot::Page(1, 1), Slot::Dock(0), 3, 1));
        assert_eq!(dock, s(&["d"]));
        // A full dock refuses.
        assert!(!move_icon(&mut pages, &mut dock, Slot::Page(0, 0), Slot::Dock(0), 3, 1));
        // The last icon of a page leaves: the page closes.
        assert!(move_icon(&mut pages, &mut dock, Slot::Page(1, 0), Slot::Page(0, 0), 4, 1));
        assert_eq!(pages, vec![s(&["c", "a", "x", "b"])]);
    }
}
