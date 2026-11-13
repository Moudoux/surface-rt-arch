//! The Start screen's pinned layout: named groups of tiles, saved in `~/.config/rtshell/home.toml`, and the
//! packing of tiles onto the cell grid.
//!
//! A tile shows an installed app (`app` = desktop id), a command (`exec`, for apps without a desktop file,
//! e.g. the unpacked Chromium) or a live widget (`live` = "clock" | "battery"). Until the user changes
//! anything, the layout is the first-run default built from the installed apps and is not written to disk.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::apps::desktop::DesktopApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TileSize {
    Small,
    Medium,
    Wide,
    Large,
}

impl TileSize {
    /// Width and height in cells.
    pub fn cells(self) -> (u32, u32) {
        match self {
            TileSize::Small => (1, 1),
            TileSize::Medium => (2, 2),
            TileSize::Wide => (4, 2),
            TileSize::Large => (4, 4),
        }
    }

    /// The size the resize button switches to.
    pub fn next(self) -> TileSize {
        match self {
            TileSize::Small => TileSize::Medium,
            TileSize::Medium => TileSize::Wide,
            TileSize::Wide => TileSize::Large,
            TileSize::Large => TileSize::Small,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exec: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<String>,
    /// Label and icon for `exec` tiles (app tiles take them from the desktop file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub size: TileSize,
}

impl TileSpec {
    pub fn app(id: &str, size: TileSize) -> TileSpec {
        TileSpec { app: Some(id.into()), exec: Vec::new(), live: None, name: None, icon: None, size }
    }
    pub fn live(kind: &str, size: TileSize) -> TileSpec {
        TileSpec { app: None, exec: Vec::new(), live: Some(kind.into()), name: None, icon: None, size }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tiles: Vec<TileSpec>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Pins {
    #[serde(default, rename = "group")]
    pub groups: Vec<Group>,
}

pub fn path() -> Option<PathBuf> {
    crate::config::default_path().map(|p| p.with_file_name("home.toml"))
}

/// First desktop id from `ids` that is installed.
fn first<'a>(apps: &'a [DesktopApp], ids: &[&str]) -> Option<&'a DesktopApp> {
    ids.iter().find_map(|id| apps.iter().find(|a| a.id.eq_ignore_ascii_case(id)))
}

fn in_path(prog: &str) -> bool {
    let home_bin = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).chain(home_bin).any(|d| d.join(prog).is_file())
}

impl Pins {
    /// Saved layout, or None on first run (or when the file is unreadable: then the defaults are used and
    /// the broken file is left alone until the user changes the layout).
    pub fn load() -> Option<Pins> {
        let path = path()?;
        let text = std::fs::read_to_string(&path).ok()?;
        match toml::from_str::<Pins>(&text) {
            Ok(p) => {
                info!(path = %path.display(), groups = p.groups.len(), "home layout loaded");
                Some(p)
            }
            Err(err) => {
                warn!(path = %path.display(), %err, "bad home layout, using defaults");
                None
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        let text = match toml::to_string(self) {
            Ok(t) => t,
            Err(err) => return warn!(%err, "cannot serialize home layout"),
        };
        let header = "# rtshell Start screen layout, written by the shell when you edit Home.\n\n";
        let tmp = path.with_extension("toml.tmp");
        let res = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&tmp, format!("{header}{text}")))
            .and_then(|_| std::fs::rename(&tmp, &path));
        match res {
            Ok(()) => info!(path = %path.display(), "home layout saved"),
            Err(err) => warn!(path = %path.display(), %err, "cannot save home layout"),
        }
    }

    /// First-run layout (DESKTOP-SHELL.md Phase 3): clock, browser, VS Code, terminal, files, settings,
    /// battery, from whatever is installed.
    pub fn defaults(apps: &[DesktopApp]) -> Pins {
        let mut main = Vec::new();
        main.push(TileSpec::live("clock", TileSize::Wide));
        // Chromium is the default browser (DESKTOP-SHELL.md Phase 1.5); on the Surface it is unpacked
        // without a desktop file, so it may only exist as the `chromium` wrapper in PATH.
        let chromium = first(apps, &["chromium", "chromium-browser", "org.chromium.Chromium"]);
        match chromium {
            Some(a) => main.push(TileSpec::app(&a.id, TileSize::Wide)),
            None if in_path("chromium") => main.push(TileSpec {
                app: None,
                exec: vec!["chromium".into()],
                live: None,
                name: Some("Chromium".into()),
                icon: Some("internet-web-browser".into()),
                size: TileSize::Wide,
            }),
            None => {
                if let Some(a) = first(apps, &["firefox", "org.mozilla.firefox"]) {
                    main.push(TileSpec::app(&a.id, TileSize::Wide));
                }
            }
        }
        let pick = [
            &["codium", "vscodium", "com.vscodium.codium", "code", "code-oss", "com.visualstudio.code"][..],
            &["foot", "org.codeberg.dnkl.foot", "org.kde.konsole", "org.gnome.Console", "org.gnome.Terminal"],
            &["org.kde.dolphin", "org.gnome.Nautilus", "thunar", "pcmanfm"],
            &["systemsettings", "org.gnome.Settings"],
        ];
        for ids in pick {
            if let Some(a) = first(apps, ids) {
                main.push(TileSpec::app(&a.id, TileSize::Medium));
            }
        }
        main.push(TileSpec::live("battery", TileSize::Medium));
        // A second group with a few more listed apps, so the screen shows what groups look like.
        let used: Vec<&DesktopApp> = main.iter().filter_map(|t| t.app.as_ref()).filter_map(|id| apps.iter().find(|a| &a.id == id)).collect();
        // Skip variants of pinned apps (same icon, e.g. "Foot Client" next to "Foot").
        let mut icons: Vec<Option<String>> = used.iter().map(|a| a.icon.clone()).collect();
        let extra: Vec<TileSpec> = apps
            .iter()
            .filter(|a| a.listed && !used.iter().any(|u| u.id == a.id))
            .filter(|a| {
                let fresh = a.icon.is_none() || !icons.contains(&a.icon);
                icons.push(a.icon.clone());
                fresh
            })
            .filter(|a| a.categories.iter().any(|c| matches!(c.as_str(), "Utility" | "System" | "Network" | "Development")))
            .take(4)
            .map(|a| TileSpec::app(&a.id, TileSize::Medium))
            .collect();
        let mut groups = vec![Group { name: String::new(), tiles: main }];
        if !extra.is_empty() {
            groups.push(Group { name: "Tools".into(), tiles: extra });
        }
        Pins { groups }
    }

    pub fn is_pinned(&self, app_id: &str) -> bool {
        self.groups.iter().any(|g| g.tiles.iter().any(|t| t.app.as_deref() == Some(app_id)))
    }

    /// Pin an app at the end of the last group, or unpin it. Returns true if it is pinned now.
    pub fn toggle(&mut self, app_id: &str) -> bool {
        if self.is_pinned(app_id) {
            for g in &mut self.groups {
                g.tiles.retain(|t| t.app.as_deref() != Some(app_id));
            }
            self.groups.retain(|g| !g.tiles.is_empty());
            false
        } else {
            if self.groups.is_empty() {
                self.groups.push(Group { name: String::new(), tiles: Vec::new() });
            }
            self.groups.last_mut().unwrap().tiles.push(TileSpec::app(app_id, TileSize::Medium));
            true
        }
    }
}

/// Place a group's tiles on a grid `rows` cells high, in blocks 4 cells wide filled row by row (so two
/// medium tiles sit side by side, like a wide one). Returns each tile's (x, y) in cells and the group's width
/// in cells.
pub fn pack(tiles: &[TileSize], rows: u32) -> (Vec<(u32, u32)>, u32) {
    const BLOCK: u32 = 4;
    let rows = rows.max(1);
    let mut used: Vec<Vec<bool>> = Vec::new(); // [x][y]
    let free = |used: &Vec<Vec<bool>>, x: u32, y: u32| used.get(x as usize).is_none_or(|c| !c[y as usize]);
    let mut out = Vec::with_capacity(tiles.len());
    let mut width = 0;
    for &t in tiles {
        let (w, h) = t.cells();
        let h = h.min(rows);
        let mut block = 0;
        let pos = 'search: loop {
            for y in 0..=rows - h {
                for x in 0..=BLOCK - w {
                    let x = block * BLOCK + x;
                    if (x..x + w).all(|cx| (y..y + h).all(|cy| free(&used, cx, cy))) {
                        break 'search (x, y);
                    }
                }
            }
            block += 1;
        };
        while used.len() < (pos.0 + w) as usize {
            used.push(vec![false; rows as usize]);
        }
        for cx in pos.0..pos.0 + w {
            for cy in pos.1..pos.1 + h {
                used[cx as usize][cy as usize] = true;
            }
        }
        width = width.max(pos.0 + w);
        out.push(pos);
    }
    (out, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use TileSize::*;

    #[test]
    fn packing_fills_blocks_row_by_row() {
        let (pos, w) = pack(&[Wide, Medium, Medium, Small, Small, Medium], 6);
        assert_eq!(pos, vec![(0, 0), (0, 2), (2, 2), (0, 4), (1, 4), (2, 4)]);
        assert_eq!(w, 4);
        // A seventh tile that does not fit opens the next block.
        let (pos, w) = pack(&[Large, Wide, Medium], 6);
        assert_eq!(pos, vec![(0, 0), (0, 4), (4, 0)]);
        assert_eq!(w, 6);
    }

    #[test]
    fn toml_round_trip_and_toggle() {
        let mut p = Pins {
            groups: vec![Group { name: "A".into(), tiles: vec![TileSpec::app("foot", Medium), TileSpec::live("clock", Wide)] }],
        };
        let text = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<Pins>(&text).unwrap(), p);
        assert!(p.toggle("firefox"));
        assert!(p.is_pinned("firefox"));
        assert!(!p.toggle("foot"));
        assert_eq!(p.groups[0].tiles.len(), 2);
    }
}
