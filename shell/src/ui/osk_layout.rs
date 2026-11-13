//! Key layouts for the on-screen keyboard (ui/osk.rs). Data only: which keys, in which rows, how wide.
//! US English and Swedish/Finnish letters (`rows` picks by xkb layout name); the symbol pages are shared. Widths are in units of a letter key;
//! every letter row adds up to `ROW_UNITS` (a `GROW` key takes what is left). The coding row is spread
//! evenly instead.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Letters,
    Symbols1,
    Symbols2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// A letter: lower case, upper case with Shift.
    Letter(char),
    /// A symbol, and what it types with Shift.
    Sym(&'static str, &'static str),
    Space,
    Back,
    Enter,
    Shift,
    Tab,
    Esc,
    Ctrl,
    Alt,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Page(Page),
    Hide,
}

impl Act {
    /// Function keys are drawn in the function colour and smaller label size.
    pub fn is_fn(self) -> bool {
        !matches!(self, Act::Letter(_) | Act::Sym(..) | Act::Space)
    }

    /// Held down, the key repeats.
    pub fn repeats(self) -> bool {
        matches!(self, Act::Back | Act::Left | Act::Right | Act::Up | Act::Down)
    }

    /// The name a script uses for it (`osktap NAME`): the character, or a word for function keys.
    pub fn name(self) -> String {
        match self {
            Act::Letter(c) => c.to_string(),
            Act::Sym(s, _) => s.to_string(),
            Act::Space => "space".into(),
            Act::Back => "backspace".into(),
            Act::Enter => "enter".into(),
            Act::Shift => "shift".into(),
            Act::Tab => "tab".into(),
            Act::Esc => "esc".into(),
            Act::Ctrl => "ctrl".into(),
            Act::Alt => "alt".into(),
            Act::Left => "left".into(),
            Act::Right => "right".into(),
            Act::Up => "up".into(),
            Act::Down => "down".into(),
            Act::Home => "home".into(),
            Act::End => "end".into(),
            Act::Page(Page::Letters) => "abc".into(),
            Act::Page(Page::Symbols1) => "123".into(),
            Act::Page(Page::Symbols2) => "more".into(),
            Act::Hide => "hide".into(),
        }
    }
}

/// Grow to fill the rest of the row.
pub const GROW: f32 = 0.0;
pub const ROW_UNITS: f32 = 12.0;

#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub act: Act,
    pub w: f32,
}

pub struct Row {
    pub keys: Vec<Key>,
    /// The thin coding row (evenly spread, its own height).
    pub coding: bool,
}

fn k(act: Act, w: f32) -> Key {
    Key { act, w }
}

fn letters(s: &str) -> impl Iterator<Item = Key> + '_ {
    s.chars().map(|c| k(Act::Letter(c), 1.0))
}

fn syms(list: &[(&'static str, &'static str)]) -> Vec<Key> {
    list.iter().map(|&(a, b)| k(Act::Sym(a, b), 1.0)).collect()
}

fn coding_row() -> Row {
    let mut keys = vec![k(Act::Esc, 1.0), k(Act::Tab, 1.0), k(Act::Ctrl, 1.0), k(Act::Alt, 1.0)];
    keys.extend(syms(&[("|", "|"), ("~", "~"), ("`", "`"), ("/", "/"), ("\\", "\\"), ("-", "_")]));
    keys.extend([k(Act::Home, 1.0), k(Act::End, 1.0), k(Act::Up, 1.0), k(Act::Down, 1.0)]);
    Row { keys, coding: true }
}

/// The bottom row (Windows 8: page key, Ctrl, space, cursor keys, hide).
fn bottom_row(page_key: Page) -> Row {
    Row {
        keys: vec![k(Act::Page(page_key), 1.5), k(Act::Ctrl, 1.25), k(Act::Space, GROW), k(Act::Left, 1.0), k(Act::Right, 1.0), k(Act::Hide, 1.5)],
        coding: false,
    }
}

/// Layouts with their own letter page (xkb names); others get US English.
pub const LAYOUTS: &[&str] = &["us", "se", "fi"];

/// The rows of `page` for the xkb layout `xkb` (first group only, e.g. "se").
pub fn rows(xkb: &str, page: Page, coding: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    if coding {
        rows.push(coding_row());
    }
    match page {
        Page::Letters => {
            let [r1, r2, r3] = match xkb {
                "se" | "fi" => nordic_letters(),
                _ => us_letters(),
            };
            rows.extend([r1, r2, r3].map(|keys| Row { keys, coding: false }));
            rows.push(bottom_row(Page::Symbols1));
        }
        Page::Symbols1 => {
            let mut r1 = syms(&[("1", "1"), ("2", "2"), ("3", "3"), ("4", "4"), ("5", "5"), ("6", "6"), ("7", "7"), ("8", "8"), ("9", "9"), ("0", "0")]);
            r1.push(k(Act::Back, GROW));
            let mut r2 = syms(&[("@", "@"), ("#", "#"), ("$", "$"), ("%", "%"), ("&", "&"), ("*", "*"), ("-", "-"), ("+", "+"), ("(", "("), (")", ")")]);
            r2.push(k(Act::Enter, GROW));
            let mut r3 = vec![k(Act::Page(Page::Symbols2), 1.5)];
            r3.extend(syms(&[("!", "!"), ("?", "?"), ("\"", "\""), ("'", "'"), (":", ":"), (";", ";"), ("/", "/"), (",", ","), (".", ".")]));
            r3.push(k(Act::Sym("=", "="), GROW));
            rows.extend([r1, r2, r3].map(|keys| Row { keys, coding: false }));
            rows.push(bottom_row(Page::Letters));
        }
        Page::Symbols2 => {
            let mut r1 = syms(&[("~", "~"), ("`", "`"), ("|", "|"), ("\\", "\\"), ("^", "^"), ("{", "{"), ("}", "}"), ("[", "["), ("]", "]"), ("_", "_")]);
            r1.push(k(Act::Back, GROW));
            let mut r2 = syms(&[("<", "<"), (">", ">"), ("=", "="), ("+", "+"), ("€", "€"), ("£", "£"), ("¥", "¥"), ("°", "°"), ("•", "•"), ("÷", "÷")]);
            r2.push(k(Act::Enter, GROW));
            let mut r3 = vec![k(Act::Page(Page::Symbols1), 1.5)];
            r3.extend(syms(&[("¡", "¡"), ("¿", "¿"), ("«", "«"), ("»", "»"), ("…", "…"), ("–", "–"), ("—", "—"), ("§", "§"), ("±", "±")]));
            r3.push(k(Act::Sym("×", "×"), GROW));
            rows.extend([r1, r2, r3].map(|keys| Row { keys, coding: false }));
            rows.push(bottom_row(Page::Letters));
        }
    }
    rows
}

/// US English.
fn us_letters() -> [Vec<Key>; 3] {
    let mut r1: Vec<Key> = letters("qwertyuiop").collect();
    r1.push(k(Act::Back, GROW));
    let mut r2: Vec<Key> = letters("asdfghjkl").collect();
    r2.push(k(Act::Sym("'", "\""), 1.0));
    r2.push(k(Act::Enter, GROW));
    let mut r3 = vec![k(Act::Shift, 1.0)];
    r3.extend(letters("zxcvbnm"));
    r3.extend(syms(&[(",", ";"), (".", ":"), ("?", "!")]));
    r3.push(k(Act::Shift, GROW));
    [r1, r2, r3]
}

/// Swedish / Finnish: å after p, ö ä after l, and the Nordic `- _` key on the bottom letter row.
fn nordic_letters() -> [Vec<Key>; 3] {
    let mut r1: Vec<Key> = letters("qwertyuiopå").collect();
    r1.push(k(Act::Back, GROW));
    let mut r2: Vec<Key> = letters("asdfghjklöä").collect();
    r2.push(k(Act::Enter, GROW));
    let mut r3 = vec![k(Act::Shift, 1.0)];
    r3.extend(letters("zxcvbnm"));
    r3.extend(syms(&[(",", ";"), (".", ":"), ("-", "_")]));
    r3.push(k(Act::Shift, GROW));
    [r1, r2, r3]
}

/// What a long press on a key offers (lower case; upper-cased with Shift). Empty: no long press.
pub fn variants(base: &str) -> &'static [&'static str] {
    match base {
        "a" => &["à", "á", "â", "ä", "ã", "å", "æ", "ā"],
        "c" => &["ç", "ć", "č"],
        "d" => &["ð", "ď"],
        "e" => &["è", "é", "ê", "ë", "ē", "ę", "ė", "ě"],
        "g" => &["ğ"],
        "i" => &["ì", "í", "î", "ï", "ī", "ı"],
        "l" => &["ł"],
        "n" => &["ñ", "ń", "ň"],
        "o" => &["ò", "ó", "ô", "ö", "õ", "ø", "œ", "ō"],
        "r" => &["ř"],
        "s" => &["ß", "ś", "š", "ş"],
        "t" => &["þ", "ť"],
        "u" => &["ù", "ú", "û", "ü", "ū", "ů"],
        "y" => &["ý", "ÿ"],
        "z" => &["ž", "ź", "ż"],
        "'" => &["‘", "’", "`"],
        "\"" => &["“", "”", "«", "»"],
        "-" => &["–", "—", "_"],
        "." => &["…"],
        "?" => &["¿"],
        "!" => &["¡"],
        "$" => &["€", "£", "¥", "¢"],
        "0" => &["°"],
        "%" => &["‰"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_rows_add_up() {
        for (layout, page) in LAYOUTS.iter().flat_map(|l| [Page::Letters, Page::Symbols1, Page::Symbols2].map(|p| (*l, p))) {
            for row in rows(layout, page, true).iter().filter(|r| !r.coding) {
                let fixed: f32 = row.keys.iter().map(|k| k.w).sum();
                let grow = row.keys.iter().filter(|k| k.w == GROW).count();
                assert_eq!(grow, 1, "{layout} {page:?}: one growing key per row");
                assert!(fixed < ROW_UNITS, "{layout} {page:?}: {fixed} units");
            }
        }
    }

    #[test]
    fn names_are_unique_per_page() {
        for (layout, page) in LAYOUTS.iter().flat_map(|l| [Page::Letters, Page::Symbols1, Page::Symbols2].map(|p| (*l, p))) {
            let rows = rows(layout, page, false);
            let mut names: Vec<String> = rows.iter().flat_map(|r| r.keys.iter().map(|k| k.act.name())).collect();
            names.retain(|n| n != "shift");
            let n = names.len();
            names.sort();
            names.dedup();
            assert_eq!(n, names.len(), "{layout} {page:?}");
        }
    }
}
