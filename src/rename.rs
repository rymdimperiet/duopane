//! Batch rename (Ctrl+M), in the style of Double Commander's multi-rename tool.
//!
//! Name and extension patterns may contain:
//! `[N]` the old name without extension, `[N2-5]` characters 2 to 5 of it,
//! `[N3]` character 3, `[E]` the old extension, `[C]` a counter,
//! `[P]` the parent folder's name, `[Y] [M] [D] [h] [m] [s]` the file's
//! modification date and time. Then find & replace and a case change are applied.

use chrono::{Datelike, Timelike};
use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    Unchanged,
    Lower,
    Upper,
    FirstCapital,
    Title,
}

impl Case {
    pub const ALL: [Case; 5] = [Case::Unchanged, Case::Lower, Case::Upper, Case::FirstCapital, Case::Title];

    pub fn label(self) -> &'static str {
        match self {
            Case::Unchanged => "Unchanged",
            Case::Lower => "lower case",
            Case::Upper => "UPPER CASE",
            Case::FirstCapital => "First letter capital",
            Case::Title => "Every Word Capital",
        }
    }
}

#[derive(Clone)]
pub struct Item {
    pub name: String,
    pub is_dir: bool,
    pub modified: Option<SystemTime>,
}

#[derive(Clone)]
pub struct Rules {
    pub name_mask: String,
    pub ext_mask: String,
    pub find: String,
    pub replace: String,
    pub regex: bool,
    pub case: Case,
    pub counter_start: i64,
    pub counter_step: i64,
    pub counter_width: usize,
    /// Name of the folder the files are in (for `[P]`).
    pub parent: String,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            name_mask: "[N]".into(),
            ext_mask: "[E]".into(),
            find: String::new(),
            replace: String::new(),
            regex: false,
            case: Case::Unchanged,
            counter_start: 1,
            counter_step: 1,
            counter_width: 1,
            parent: String::new(),
        }
    }
}

/// Splits "photo.final.jpg" into ("photo.final", "jpg"); folders and dot-files keep it all.
pub fn split(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

fn expand(mask: &str, stem: &str, ext: &str, counter: i64, rules: &Rules, modified: Option<SystemTime>) -> String {
    let chars: Vec<char> = stem.chars().collect();
    let dt: Option<chrono::DateTime<chrono::Local>> = modified.map(Into::into);
    let mut out = String::new();
    let mut rest = mask;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find(']') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let token = &rest[open + 1..open + close];
        let piece = match token {
            "N" => Some(stem.to_string()),
            "E" => Some(ext.to_string()),
            "P" => Some(rules.parent.clone()),
            "C" => {
                let w = rules.counter_width.clamp(1, 12);
                Some(if counter < 0 { format!("-{:0w$}", -counter) } else { format!("{counter:0w$}") })
            }
            "Y" => dt.map(|d| format!("{:04}", d.year())),
            "M" => dt.map(|d| format!("{:02}", d.month())),
            "D" => dt.map(|d| format!("{:02}", d.day())),
            "h" => dt.map(|d| format!("{:02}", d.hour())),
            "m" => dt.map(|d| format!("{:02}", d.minute())),
            "s" => dt.map(|d| format!("{:02}", d.second())),
            t if t.starts_with('N') => {
                // [N3] or [N2-5]: characters of the old name, counting from 1.
                let spec = &t[1..];
                let (a, b) = match spec.split_once('-') {
                    Some((a, b)) => (a.parse::<usize>().ok(), if b.is_empty() { Some(chars.len()) } else { b.parse().ok() }),
                    None => (spec.parse().ok(), spec.parse().ok()),
                };
                match (a, b) {
                    (Some(a), Some(b)) if a >= 1 && b >= a => Some(chars.iter().skip(a - 1).take(b - a + 1).collect()),
                    _ => None,
                }
            }
            _ => None,
        };
        match piece {
            Some(p) => out.push_str(&p),
            None => out.push_str(&rest[open..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

fn change_case(s: &str, case: Case) -> String {
    match case {
        Case::Unchanged => s.to_string(),
        Case::Lower => s.to_lowercase(),
        Case::Upper => s.to_uppercase(),
        Case::FirstCapital => {
            let lower = s.to_lowercase();
            let mut c = lower.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
        Case::Title => {
            let mut out = String::new();
            let mut start = true;
            for ch in s.chars() {
                if start && ch.is_alphanumeric() {
                    out.extend(ch.to_uppercase());
                    start = false;
                } else {
                    out.extend(ch.to_lowercase());
                }
                if ch.is_whitespace() || matches!(ch, '-' | '_' | '.' | '(' | '[') {
                    start = true;
                }
            }
            out
        }
    }
}

/// The new names, in the same order as `items`.
pub fn new_names(items: &[Item], rules: &Rules) -> Result<Vec<String>, String> {
    let re = if rules.regex && !rules.find.is_empty() {
        Some(regex::Regex::new(&rules.find).map_err(|e| format!("Search pattern: {e}"))?)
    } else {
        None
    };
    let mut out = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        let (stem, ext) = split(&it.name, it.is_dir);
        let counter = rules.counter_start + rules.counter_step * i as i64;
        let name = expand(&rules.name_mask, stem, ext, counter, rules, it.modified);
        let ext_new = if it.is_dir { String::new() } else { expand(&rules.ext_mask, stem, ext, counter, rules, it.modified) };
        let mut full = if ext_new.is_empty() { name } else { format!("{name}.{ext_new}") };
        if !rules.find.is_empty() {
            full = match &re {
                Some(re) => re.replace_all(&full, rules.replace.as_str()).into_owned(),
                None => full.replace(&rules.find, &rules.replace),
            };
        }
        out.push(change_case(&full, rules.case));
    }
    Ok(out)
}

/// Why a new name can't be used (None when it's fine). `others` are names in the
/// folder that aren't being renamed.
pub fn problems(old: &[String], new: &[String], others: &HashSet<String>) -> Vec<Option<&'static str>> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for n in new {
        *count.entry(n.as_str()).or_default() += 1;
    }
    old.iter()
        .zip(new)
        .map(|(o, n)| {
            if n.trim().is_empty() {
                Some("empty name")
            } else if n.contains('/') || n == "." || n == ".." {
                Some("not allowed")
            } else if count[n.as_str()] > 1 {
                Some("same as another new name")
            } else if n != o && others.contains(n) {
                Some("already exists")
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[&str]) -> Vec<Item> {
        names.iter().map(|n| Item { name: n.to_string(), is_dir: false, modified: None }).collect()
    }

    #[test]
    fn masks_and_counter() {
        let r = Rules { name_mask: "Holiday [C] - [N]".into(), counter_width: 3, ..Default::default() };
        let n = new_names(&items(&["a.jpg", "b.JPG"]), &r).unwrap();
        assert_eq!(n, vec!["Holiday 001 - a.jpg", "Holiday 002 - b.JPG"]);
        let r = Rules { name_mask: "[N2-4]".into(), ext_mask: "txt".into(), ..Default::default() };
        assert_eq!(new_names(&items(&["abcdef.md"]), &r).unwrap(), vec!["bcd.txt"]);
        let r = Rules { name_mask: "[N]".into(), ext_mask: "".into(), ..Default::default() };
        assert_eq!(new_names(&items(&["x.tmp"]), &r).unwrap(), vec!["x"]);
    }

    #[test]
    fn replace_and_case() {
        let r = Rules { find: "_".into(), replace: " ".into(), case: Case::Title, ..Default::default() };
        assert_eq!(new_names(&items(&["my_summer_trip.JPG"]), &r).unwrap(), vec!["My Summer Trip.Jpg"]);
        let r = Rules { find: r"(\d+)".into(), replace: "#$1".into(), regex: true, case: Case::Lower, ..Default::default() };
        assert_eq!(new_names(&items(&["IMG42.JPG"]), &r).unwrap(), vec!["img#42.jpg"]);
        let r = Rules { find: "(".into(), regex: true, ..Default::default() };
        assert!(new_names(&items(&["a"]), &r).is_err());
    }

    #[test]
    fn spots_clashes() {
        let old = vec!["a.txt".to_string(), "b.txt".to_string()];
        let others: HashSet<String> = ["c.txt".to_string()].into();
        let p = problems(&old, &["x.txt".into(), "x.txt".into()], &others);
        assert!(p.iter().all(|p| p.is_some()));
        let p = problems(&old, &["c.txt".into(), "a.txt".into()], &others);
        assert_eq!(p[0], Some("already exists"));
        assert_eq!(p[1], None, "swapping names within the batch is fine");
    }
}
