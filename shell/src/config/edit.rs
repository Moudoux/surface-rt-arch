//! In-place edits of the user's config.toml for the Settings app (DESKTOP-SHELL.md "Settings app design"):
//! comments, ordering and keys Settings does not know are kept (`toml_edit`); a missing file is created with
//! only the changed keys (the built-in defaults fill in the rest at load). Written to a temp file and
//! renamed, so a reader (the config watcher, the next start) never sees half a file.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value};

/// One setting: a key path below the document root (`["idle", "battery", "sleep"]`) and its new value.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub path: Vec<&'static str>,
    pub value: Val,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    StrList(Vec<String>),
}

impl Val {
    fn to_value(&self) -> Value {
        match self {
            Val::Str(s) => s.as_str().into(),
            Val::Int(i) => (*i).into(),
            Val::Float(f) => (*f).into(),
            Val::Bool(b) => (*b).into(),
            Val::StrList(v) => Value::Array(v.iter().map(|s| s.as_str()).collect()),
        }
    }
}

pub fn edit(path: &str, value: Val) -> Edit {
    let path: Vec<&'static str> = path.split('.').map(|p| &*Box::leak(p.to_string().into_boxed_str())).collect();
    Edit { path, value }
}

/// Apply `edits` to the TOML text (empty = a new file); the new text.
pub fn apply_text(text: &str, edits: &[Edit]) -> Result<String, String> {
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    for e in edits {
        let (last, parents) = e.path.split_last().ok_or("empty key path")?;
        let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
        for key in parents {
            if table.get(key).is_none_or(|i| !i.is_table_like()) {
                // A missing parent: a [table] at the top level, an inline table below one that is inline.
                let mut t = Table::new();
                t.set_implicit(false);
                table.insert(key, Item::Table(t));
            }
            table = table.get_mut(key).and_then(|i| i.as_table_like_mut()).ok_or_else(|| format!("{key} is not a table"))?;
        }
        let mut v = e.value.to_value();
        match table.get_mut(last) {
            // Replaced in place: the key (and the comment lines above it) stay; the value keeps its own
            // spacing and trailing comment (`dim = 60   # seconds`).
            Some(item) => {
                if let Some(old) = item.as_value() {
                    *v.decor_mut() = old.decor().clone();
                }
                *item = Item::Value(v);
            }
            None => {
                table.insert(last, Item::Value(v));
            }
        }
    }
    Ok(doc.to_string())
}

/// Apply `edits` to the file at `path` (created with its folder if missing).
pub fn apply(path: &Path, edits: &[Edit]) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let new = apply_text(&text, edits)?;
    // The result must still load as a config (never write something the next start rejects).
    super::Config::parse_over_defaults(&new).map_err(|e| format!("edited config would not load: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, new).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_comments_and_unknown_keys() {
        let text = "# mine\ntheme = \"dark\"   # look\nfoo = 1\n\n[idle]\nbattery = { dim = 60, blank = 90, sleep = 300 }\n";
        let out = apply_text(
            text,
            &[edit("theme", Val::Str("light".into())), edit("idle.battery.sleep", Val::Int(0)), edit("keyboard.layout", Val::Str("us".into()))],
        )
        .unwrap();
        assert!(out.starts_with("# mine\ntheme = \"light\"   # look\nfoo = 1\n"), "{out}");
        assert!(out.contains("battery = { dim = 60, blank = 90, sleep = 0 }"), "{out}");
        assert!(out.contains("[keyboard]\nlayout = \"us\""), "{out}");
        let c = crate::config::Config::parse_over_defaults(&out).unwrap();
        assert_eq!(c.theme, "light");
        assert_eq!(c.idle.battery.sleep, 0.0);
        assert_eq!(c.idle.battery.dim, 60.0);
        assert_eq!(c.keyboard.layout, "us");
    }

    #[test]
    fn new_file() {
        let out = apply_text("", &[edit("idle.ac.sleep", Val::Float(1800.0)), edit("apps.terminal", Val::StrList(vec!["foot".into()]))]).unwrap();
        let c = crate::config::Config::parse_over_defaults(&out).unwrap();
        assert_eq!(c.idle.ac.sleep, 1800.0);
        assert_eq!(c.idle.ac.dim, 270.0); // the rest from the defaults
        assert_eq!(c.apps.terminal, vec!["foot".to_string()]);
    }
}
