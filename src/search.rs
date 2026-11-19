//! Finding files by name (Ctrl+F / Alt+F7).
//!
//! Two sources run at once, and their results are merged:
//! - **plocate**, when installed: the system's file index, answered in a moment,
//!   but only as fresh as its last nightly update. Each result is checked on disk,
//!   so deleted files never show.
//! - **A live walk** of the folder being searched (the `ignore` crate's parallel
//!   walker, as in ripgrep and fd): slower, but it sees everything as it is now.
//!   When it finishes, plocate is stopped: the walk has seen it all.
//!
//! "Everywhere" uses plocate alone (walking the whole disk would take too long).
//!
//! A query is plain words: every word must appear in the name or the path below
//! the searched folder, in any order, with any case. A few filters, all starting
//! with a symbol so they never clash with names: `*.pdf` (mask), `>10mb` /
//! `<1kb` (size), `@today` `@yesterday` `@week` `@month` `@2025` `@2025-11` (date
//! modified), `@folders` / `@files`.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Stop collecting after this many results (the count still says how many matched).
pub const MAX_HITS: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Files,
    Folders,
}

/// A parsed search: words to find plus optional filters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    /// Lower-case words that must all appear in the path below the searched folder.
    pub words: Vec<String>,
    /// Lower-case masks (`*`, `?`) of which the name must match one.
    pub masks: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Modified within [from, to).
    pub from: Option<SystemTime>,
    pub to: Option<SystemTime>,
    pub kind: Option<Kind>,
    /// Words that look like filters but couldn't be read (shown to the user).
    pub unknown: Vec<String>,
}

impl Query {
    pub fn parse(text: &str) -> Query {
        let mut q = Query::default();
        for tok in text.split_whitespace() {
            let lower = tok.to_lowercase();
            if let Some(rest) = lower.strip_prefix('>') {
                match parse_size(rest.trim_start_matches('=')) {
                    Some(n) => q.min_size = Some(n),
                    None => q.unknown.push(tok.into()),
                }
            } else if let Some(rest) = lower.strip_prefix('<') {
                match parse_size(rest.trim_start_matches('=')) {
                    Some(n) => q.max_size = Some(n),
                    None => q.unknown.push(tok.into()),
                }
            } else if let Some(rest) = lower.strip_prefix('@') {
                match rest {
                    "folders" | "folder" | "dirs" => q.kind = Some(Kind::Folders),
                    "files" | "file" => q.kind = Some(Kind::Files),
                    _ => match parse_date(rest) {
                        Some((from, to)) => (q.from, q.to) = (Some(from), to),
                        None => q.unknown.push(tok.into()),
                    },
                }
            } else if lower.contains(['*', '?']) {
                q.masks.push(lower);
            } else {
                q.words.push(lower);
            }
        }
        q
    }

    /// True when there's nothing to look for at all.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
            && self.masks.is_empty()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.from.is_none()
            && self.kind.is_none()
    }

    /// Whether the name and path match (before looking at size or date).
    /// `rel` is the lower-case path below the searched folder, `name` its last part.
    fn text_matches(&self, rel: &str, name: &str) -> bool {
        self.words.iter().all(|w| rel.contains(w.as_str()))
            && (self.masks.is_empty() || self.masks.iter().any(|m| crate::pane::glob_str(m, name)))
    }

    /// Whether size, date and kind match.
    fn meta_matches(&self, is_dir: bool, size: u64, modified: Option<SystemTime>) -> bool {
        match self.kind {
            Some(Kind::Files) if is_dir => return false,
            Some(Kind::Folders) if !is_dir => return false,
            _ => {}
        }
        // Sizes only mean something for files.
        if (self.min_size.is_some() || self.max_size.is_some()) && is_dir {
            return false;
        }
        if self.min_size.is_some_and(|m| size < m) || self.max_size.is_some_and(|m| size > m) {
            return false;
        }
        if let Some(from) = self.from {
            match modified {
                Some(t) if t >= from && self.to.is_none_or(|to| t < to) => {}
                _ => return false,
            }
        }
        true
    }

    /// How good a match is: words in the name count most, near the start best;
    /// shallow paths beat deep ones.
    fn score(&self, rel: &str, name: &str) -> i32 {
        let mut s = 0;
        for w in &self.words {
            if let Some(i) = name.find(w.as_str()) {
                s += 10;
                if i == 0 {
                    s += 10;
                } else if !name.as_bytes()[i - 1].is_ascii_alphanumeric() {
                    s += 5; // starts a word: "holiday" in "2026 holiday.jpg"
                }
            } else if rel.contains(w.as_str()) {
                s += 2;
            }
        }
        if !self.words.is_empty() && name == self.words.join(" ") {
            s += 50;
        }
        s - rel.matches('/').count() as i32
    }
}

/// "10mb" -> 10 485 760. Units: b, k/kb, m/mb, g/gb, t/tb (1024-based); a bare number is bytes.
fn parse_size(s: &str) -> Option<u64> {
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mul: u64 = match unit {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => return None,
    };
    Some((n * mul as f64) as u64)
}

/// "today", "week", "2025", "2025-11"… -> modified from (and before, when bounded).
fn parse_date(s: &str) -> Option<(SystemTime, Option<SystemTime>)> {
    use chrono::{Datelike, Local, NaiveDate, TimeZone};
    let local = |d: NaiveDate| -> Option<SystemTime> {
        Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest().map(SystemTime::from)
    };
    let today = Local::now().date_naive();
    let day = Duration::from_secs(86_400);
    Some(match s {
        "today" => (local(today)?, None),
        "yesterday" => (local(today.pred_opt()?)?, Some(local(today)?)),
        "week" => (SystemTime::now() - 7 * day, None),
        "month" => (SystemTime::now() - 30 * day, None),
        "year" => (SystemTime::now() - 365 * day, None),
        _ => {
            let mut parts = s.splitn(2, '-');
            let y: i32 = parts.next()?.parse().ok().filter(|y| (1970..=2200).contains(y))?;
            match parts.next() {
                None => (local(NaiveDate::from_ymd_opt(y, 1, 1)?)?, Some(local(NaiveDate::from_ymd_opt(y + 1, 1, 1)?)?)),
                Some(m) => {
                    let m: u32 = m.parse().ok().filter(|m| (1..=12).contains(m))?;
                    let start = NaiveDate::from_ymd_opt(y, m, 1)?;
                    let next = if m == 12 { NaiveDate::from_ymd_opt(y + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(y, m + 1, 1)? };
                    debug_assert!(next.year() >= start.year());
                    (local(start)?, Some(local(next)?))
                }
            }
        }
    })
}

/// One file or folder found.
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub score: i32,
}

enum Msg {
    Hit(Hit),
    WalkDone,
    LocateDone,
}

/// A search under way. Dropping it stops it.
pub struct Search {
    pub root: PathBuf,
    rx: mpsc::Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    locate: Arc<Mutex<Option<Child>>>,
    seen: HashSet<PathBuf>,
    /// The results so far, best first.
    pub hits: Vec<Hit>,
    /// How many matched in all (may exceed `hits` when capped).
    pub found: usize,
    /// Files and folders the walk has looked at.
    pub looked_at: Arc<AtomicU64>,
    pub walking: bool,
    pub locating: bool,
}

impl Search {
    /// Starts searching `root` for `q`. `everywhere` searches the whole index
    /// instead (plocate only). Hidden files are skipped unless `hidden`.
    pub fn start(q: Query, root: PathBuf, everywhere: bool, hidden: bool) -> Search {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let looked_at = Arc::new(AtomicU64::new(0));
        let locate = Arc::new(Mutex::new(None));
        let q = Arc::new(q);
        let walking = !everywhere;
        // plocate needs something to look for in the path itself.
        let locating = has_plocate() && (!q.words.is_empty() || !q.masks.is_empty());
        if walking {
            let (tx, cancel, looked_at, q, root) = (tx.clone(), cancel.clone(), looked_at.clone(), q.clone(), root.clone());
            std::thread::spawn(move || {
                walk(&q, &root, hidden, &tx, &cancel, &looked_at);
                let _ = tx.send(Msg::WalkDone);
            });
        }
        if locating {
            let (tx, cancel, locate, q, root) = (tx.clone(), cancel.clone(), locate.clone(), q.clone(), root.clone());
            std::thread::spawn(move || {
                run_plocate(&q, &root, hidden, &tx, &cancel, &locate);
                let _ = tx.send(Msg::LocateDone);
            });
        }
        Search { root, rx, cancel, locate, seen: HashSet::new(), hits: Vec::new(), found: 0, looked_at, walking, locating }
    }

    /// Takes in new results. Returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            match msg {
                Msg::Hit(h) => {
                    if self.seen.insert(h.path.clone()) {
                        self.found += 1;
                        if self.hits.len() < MAX_HITS {
                            self.hits.push(h);
                        }
                    }
                }
                Msg::WalkDone => {
                    self.walking = false;
                    // The walk saw everything under the folder: the index can't add more.
                    self.stop_locate();
                }
                Msg::LocateDone => self.locating = false,
            }
        }
        if changed {
            // Equal scores: the shorter (closer to what was typed) first, then by name.
            self.hits.sort_by(|a, b| {
                b.score.cmp(&a.score).then_with(|| a.path.as_os_str().len().cmp(&b.path.as_os_str().len())).then_with(|| a.path.cmp(&b.path))
            });
        }
        changed
    }

    pub fn running(&self) -> bool {
        self.walking || self.locating
    }

    fn stop_locate(&mut self) {
        if let Ok(mut c) = self.locate.lock() {
            if let Some(mut child) = c.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.locating = false;
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.stop_locate();
    }
}

/// Checks one path and, if it matches, sends it. `rel` is relative to the root.
fn consider(q: &Query, path: &Path, rel: &Path, meta: Option<std::fs::Metadata>, tx: &mpsc::Sender<Msg>) {
    let rel_s = rel.to_string_lossy().to_lowercase();
    let name = rel.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if !q.text_matches(&rel_s, &name) {
        return;
    }
    let Some(m) = meta.or_else(|| std::fs::symlink_metadata(path).ok()) else { return }; // gone since indexed
    let (is_dir, size, modified) = (m.is_dir(), if m.is_dir() { 0 } else { m.len() }, m.modified().ok());
    if !q.meta_matches(is_dir, size, modified) {
        return;
    }
    let score = q.score(&rel_s, &name);
    let _ = tx.send(Msg::Hit(Hit { path: path.to_path_buf(), is_dir, size, modified, score }));
}

/// Folders a whole-disk walk should never go into.
fn is_system_dir(p: &Path) -> bool {
    ["/proc", "/sys", "/dev", "/run"].iter().any(|s| p == Path::new(s))
}

fn walk(q: &Query, root: &Path, hidden: bool, tx: &mpsc::Sender<Msg>, cancel: &AtomicBool, looked_at: &AtomicU64) {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(!hidden)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .filter_entry(|e| !is_system_dir(e.path()))
        .build_parallel();
    walker.run(|| {
        Box::new(|entry| {
            if cancel.load(Ordering::Relaxed) {
                return ignore::WalkState::Quit;
            }
            let Ok(e) = entry else { return ignore::WalkState::Continue };
            if e.depth() == 0 {
                return ignore::WalkState::Continue;
            }
            looked_at.fetch_add(1, Ordering::Relaxed);
            let rel = e.path().strip_prefix(root).unwrap_or(e.path());
            consider(q, e.path(), rel, None, tx);
            ignore::WalkState::Continue
        })
    });
}

fn has_plocate() -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| crate::lister_load::have("plocate"))
}

/// When plocate's index was last updated, if it can be told.
pub fn index_age() -> Option<Duration> {
    if !has_plocate() {
        return None;
    }
    let m = std::fs::metadata("/var/lib/plocate/plocate.db").ok()?;
    m.modified().ok()?.elapsed().ok()
}

fn run_plocate(q: &Query, root: &Path, hidden: bool, tx: &mpsc::Sender<Msg>, cancel: &AtomicBool, slot: &Mutex<Option<Child>>) {
    // plocate wants every pattern to match the full path; a mask goes in as a glob.
    let mut cmd = Command::new("plocate");
    cmd.args(["-i", "-0"]).args(&q.words).args(&q.masks).stdout(Stdio::piped()).stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else { return };
    let Some(out) = child.stdout.take() else { return };
    if let Ok(mut s) = slot.lock() {
        *s = Some(child);
    }
    for item in BufReader::new(out).split(0) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(bytes) = item else { break };
        #[cfg(unix)]
        let path = PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes));
        #[cfg(not(unix))]
        let path = PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
        let Ok(rel) = path.strip_prefix(root) else { continue };
        if rel.as_os_str().is_empty() || (!hidden && rel.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'))) {
            continue;
        }
        consider(q, &path, rel, None, tx);
    }
    if let Ok(mut s) = slot.lock() {
        if let Some(mut child) = s.take() {
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let q = Query::parse("Holiday JPG *.Png >1.5mb <2g @folders @bogus");
        assert_eq!(q.words, ["holiday", "jpg"]);
        assert_eq!(q.masks, ["*.png"]);
        assert_eq!(q.min_size, Some(1_572_864));
        assert_eq!(q.max_size, Some(2 << 30));
        assert_eq!(q.kind, Some(Kind::Folders));
        assert_eq!(q.unknown, ["@bogus"]);
        assert!(Query::parse("  ").is_empty());
        assert_eq!(Query::parse("2025 taxes").words, ["2025", "taxes"], "plain numbers are words");
        let y = Query::parse("@2025");
        assert!(y.from.is_some() && y.to.is_some() && y.words.is_empty());
        assert!(Query::parse("@2025-13").unknown.len() == 1);
    }

    #[test]
    fn matching_and_ranking() {
        let q = Query::parse("holiday jpg");
        assert!(q.text_matches("pictures/holiday 2026/img_4411.jpg", "img_4411.jpg"));
        assert!(!q.text_matches("pictures/img_4411.jpg", "img_4411.jpg"));
        let in_name = q.score("pics/holiday.jpg", "holiday.jpg");
        let in_path = q.score("holiday/img.jpg", "img.jpg");
        assert!(in_name > in_path);
        let m = Query::parse("*.pdf invoice");
        assert!(m.text_matches("downloads/invoice inv-0042.pdf", "invoice inv-0042.pdf"));
        assert!(!m.text_matches("downloads/invoice.txt", "invoice.txt"));
        let big = Query::parse(">1kb");
        assert!(big.meta_matches(false, 5000, None) && !big.meta_matches(false, 10, None) && !big.meta_matches(true, 0, None));
    }

    #[test]
    fn walks_a_folder() {
        let base = std::env::temp_dir().join(format!("duopane-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("Pictures/Holiday 2026")).unwrap();
        std::fs::create_dir_all(base.join(".hidden")).unwrap();
        std::fs::write(base.join("Pictures/Holiday 2026/IMG_1.jpg"), vec![0u8; 3000]).unwrap();
        std::fs::write(base.join("Pictures/holiday.jpg"), b"x").unwrap();
        std::fs::write(base.join("Pictures/notes.txt"), b"x").unwrap();
        std::fs::write(base.join(".hidden/holiday.jpg"), b"x").unwrap();
        let run = |text: &str, hidden: bool| {
            let mut s = Search::start(Query::parse(text), base.clone(), false, hidden);
            let t = std::time::Instant::now();
            while s.walking {
                s.poll();
                assert!(t.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(2));
            }
            s.poll();
            s.hits.iter().map(|h| h.path.strip_prefix(&base).unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>()
        };
        // Best first: the word in the name beats the word only in the folder.
        assert_eq!(run("holiday jpg", false), ["Pictures/holiday.jpg", "Pictures/Holiday 2026/IMG_1.jpg"]);
        assert_eq!(run("holiday jpg", true).len(), 3, "hidden files when asked for");
        assert_eq!(run("holiday @folders", false), ["Pictures/Holiday 2026"]);
        assert_eq!(run(">1kb", false), ["Pictures/Holiday 2026/IMG_1.jpg"]);
        assert_eq!(run("*.txt", false), ["Pictures/notes.txt"]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
