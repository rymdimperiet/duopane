//! One side of the window: a folder listing with a cursor, a selection,
//! sorting and back/forward history.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

/// How long a folder may take to read before the pane stops waiting for it and
/// shows "Loading…" (the window keeps responding while it finishes).
const LOAD_WAIT: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    Extension,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// A file that can be run (its permission says so).
    pub executable: bool,
}

impl Entry {
    /// The ".." row at the top of a folder (goes up one level).
    pub fn is_up(&self) -> bool {
        self.name == ".."
    }

    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }

    pub fn extension(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        Path::new(&self.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }
}

/// The archive a pane is browsing, when it is inside one.
pub struct ArchiveState {
    /// The archive file on disk.
    pub file: PathBuf,
    index: Arc<crate::archive::Index>,
    mtime: Option<SystemTime>,
}

pub struct Pane {
    pub path: PathBuf,
    all: Vec<Entry>,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    /// Names of the selected entries in the current folder.
    pub selected: HashSet<String>,
    pub sort: SortKey,
    pub ascending: bool,
    pub show_hidden: bool,
    /// Show a ".." row at the top of every folder but the root.
    pub show_dotdot: bool,
    pub back: Vec<PathBuf>,
    pub forward: Vec<PathBuf>,
    pub error: Option<String>,
    dir_mtime: Option<SystemTime>,
    /// Set while browsing inside an archive (the path then runs through the archive file).
    pub archive: Option<ArchiveState>,
    pub scroll_to_cursor: bool,
    pub scroll_offset: f32,
    pub view_height: f32,
    /// How far each folder was scrolled when we left it, so coming back up shows
    /// it as it was (the folder we came out of stays on the same row).
    scroll_memory: HashMap<PathBuf, f32>,
    /// `Some(text)` while the path bar is being edited.
    pub editing_path: Option<String>,
    /// A folder still being read on another thread.
    loading: Option<Loading>,
    /// Put the cursor on this name once the folder being read arrives.
    want_cursor: Option<String>,
    /// How long to wait for a read before going on in the background ([`LOAD_WAIT`]).
    load_wait: Duration,
    /// Set while showing search results instead of a folder ("feed to listbox").
    /// `path` is then the folder that was searched.
    pub results: Option<Arc<ResultSet>>,
}

/// Search results shown in a pane like a folder.
pub struct ResultSet {
    /// What was searched for, and where (shown in the path bar).
    pub query: String,
    pub root: PathBuf,
    pub paths: Vec<PathBuf>,
}

/// A folder (or a folder inside an archive) as read from disk, on any thread.
enum Fetched {
    Dir { all: Vec<Entry>, mtime: Option<SystemTime> },
    Results { all: Vec<Entry>, set: Arc<ResultSet> },
    Archive { all: Vec<Entry>, state: ArchiveState },
}

/// What to do once a folder has been read.
enum After {
    Navigate { old: PathBuf },
    Back { old: PathBuf },
    Forward { old: PathBuf },
    /// Re-reading the current folder; `gone` is set when it disappeared and a
    /// parent is being tried instead.
    Reload { gone: Option<PathBuf> },
}

struct Loading {
    path: PathBuf,
    rx: mpsc::Receiver<Result<Fetched, String>>,
    then: After,
    since: Instant,
}

impl Pane {
    pub fn new(path: &Path, show_hidden: bool, show_dotdot: bool, sort: SortKey, ascending: bool) -> Self {
        let mut p = Pane {
            path: PathBuf::new(),
            all: Vec::new(),
            entries: Vec::new(),
            cursor: 0,
            selected: HashSet::new(),
            sort,
            ascending,
            show_hidden,
            show_dotdot,
            back: Vec::new(),
            forward: Vec::new(),
            error: None,
            dir_mtime: None,
            archive: None,
            scroll_to_cursor: true,
            scroll_offset: 0.0,
            view_height: 400.0,
            scroll_memory: HashMap::new(),
            editing_path: None,
            loading: None,
            want_cursor: None,
            load_wait: LOAD_WAIT,
            results: None,
        };
        let fallbacks = [
            path.to_path_buf(),
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            PathBuf::from("/"),
        ];
        for candidate in fallbacks {
            if p.start_load(&candidate, After::Reload { gone: None }).is_ok() {
                break;
            }
        }
        p
    }

    /// Reads `path` on another thread. If that's quick, the pane shows it before
    /// returning; otherwise it keeps showing the current folder until
    /// [`Pane::poll_loading`] picks the result up. Errors that arrive straight away
    /// are returned; later ones come out of `poll_loading`.
    fn start_load(&mut self, path: &Path, then: After) -> Result<(), String> {
        self.start_load_as(path, then, None)
    }

    /// Like [`Pane::start_load`], showing search results when `results` is given.
    fn start_load_as(&mut self, path: &Path, then: After, results: Option<Arc<ResultSet>>) -> Result<(), String> {
        self.loading = None; // a newer request replaces an unfinished one
        let cache = self.archive.as_ref().map(|a| (a.file.clone(), a.mtime, a.index.clone()));
        let (tx, rx) = mpsc::channel();
        let p = path.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(match results {
                Some(set) => Ok(fetch_results(set)),
                None => fetch(&p, cache),
            });
        });
        match rx.recv_timeout(self.load_wait) {
            Ok(r) => self.complete(path, then, r).map(|_| ()),
            Err(_) => {
                self.loading = Some(Loading { path: path.to_path_buf(), rx, then, since: Instant::now() });
                Ok(())
            }
        }
    }

    /// Picks up a folder that finished reading in the background. Returns
    /// `Some(Ok(true))` when the pane moved to another folder, `Some(Ok(false))`
    /// when it re-read the same one, `Some(Err)` when reading failed.
    pub fn poll_loading(&mut self) -> Option<Result<bool, String>> {
        let r = match self.loading.as_ref()?.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("Reading the folder failed".to_string()),
        };
        let l = self.loading.take()?;
        let r = self.complete(&l.path, l.then, r);
        // A follow-up read (an older history entry, a parent folder) may now be under way.
        if self.loading.is_some() && r.is_ok() {
            return None;
        }
        Some(r)
    }

    /// The folder being read in the background, once it has taken long enough to mention.
    pub fn loading_path(&self) -> Option<&Path> {
        self.loading.as_ref().filter(|l| l.since.elapsed() >= self.load_wait).map(|l| l.path.as_path())
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Shows a finished read, or handles its failure. Returns whether the pane moved.
    fn complete(&mut self, path: &Path, then: After, result: Result<Fetched, String>) -> Result<bool, String> {
        let fetched = match result {
            Ok(f) => f,
            Err(e) => {
                return match then {
                    After::Navigate { .. } => Err(e),
                    // Skip history entries that can't be opened any more.
                    After::Back { .. } => self.go_back().map(|_| true),
                    After::Forward { .. } => self.go_forward().map(|_| true),
                    // The folder disappeared: move to the nearest one that still exists.
                    After::Reload { gone } => match path.parent() {
                        Some(parent) => {
                            let gone = gone.unwrap_or_else(|| path.to_path_buf());
                            self.start_load(parent, After::Reload { gone: Some(gone) }).map(|_| true)
                        }
                        None => {
                            self.error = Some(e.clone());
                            Err(e)
                        }
                    },
                };
            }
        };
        self.path = path.to_path_buf();
        match fetched {
            Fetched::Dir { all, mtime } => {
                self.all = all;
                self.archive = None;
                self.results = None;
                self.dir_mtime = mtime;
            }
            Fetched::Results { all, set } => {
                self.all = all;
                self.archive = None;
                self.results = Some(set);
                self.dir_mtime = None;
            }
            Fetched::Archive { all, state } => {
                self.all = all;
                self.dir_mtime = state.mtime;
                self.archive = Some(state);
                self.results = None;
            }
        }
        self.error = None;
        self.apply_view();
        let moved = match then {
            After::Navigate { old } => {
                self.after_dir_change(&old);
                if !old.as_os_str().is_empty() {
                    self.back.push(old);
                    if self.back.len() > 200 {
                        self.back.remove(0);
                    }
                }
                self.forward.clear();
                true
            }
            After::Back { old } => {
                self.after_dir_change(&old);
                self.forward.push(old);
                true
            }
            After::Forward { old } => {
                self.after_dir_change(&old);
                self.back.push(old);
                true
            }
            After::Reload { gone: Some(gone) } => {
                self.after_dir_change(&gone);
                true
            }
            After::Reload { gone: None } => false,
        };
        if let Some(name) = self.want_cursor.take() {
            self.set_cursor_to_name(&name);
        }
        Ok(moved)
    }

    /// The real folder on disk: the archive's folder while browsing inside an archive.
    pub fn real_dir(&self) -> PathBuf {
        match &self.archive {
            Some(a) => a.file.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")),
            None => self.path.clone(),
        }
    }

    /// For an entry in an archive: its path inside the archive ("docs/a.txt").
    pub fn inner_path(&self, p: &Path) -> Option<String> {
        let a = self.archive.as_ref()?;
        p.strip_prefix(&a.file).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
    }

    /// Total size of an entry inside the archive being browsed.
    pub fn archive_size(&self, p: &Path) -> u64 {
        match (&self.archive, self.inner_path(p)) {
            (Some(a), Some(inner)) => a.index.size_of(&inner),
            _ => 0,
        }
    }

    /// Re-filters and re-sorts, keeping the cursor on the same entry where possible.
    pub fn apply_view(&mut self) {
        let keep = self.entries.get(self.cursor).map(|e| e.name.clone());
        let show_hidden = self.show_hidden;
        let mut entries: Vec<Entry> = self
            .all
            .iter()
            .filter(|e| show_hidden || !e.is_hidden())
            .cloned()
            .collect();
        let (key, asc) = (self.sort, self.ascending);
        entries.sort_by(|a, b| {
            // Folders always come first.
            b.is_dir.cmp(&a.is_dir).then_with(|| {
                let ord = match key {
                    SortKey::Name => natural_cmp(&a.name, &b.name),
                    SortKey::Size => a.size.cmp(&b.size).then_with(|| natural_cmp(&a.name, &b.name)),
                    SortKey::Modified => a.modified.cmp(&b.modified).then_with(|| natural_cmp(&a.name, &b.name)),
                    SortKey::Extension => a
                        .extension()
                        .cmp(&b.extension())
                        .then_with(|| natural_cmp(&a.name, &b.name)),
                };
                if asc {
                    ord
                } else {
                    ord.reverse()
                }
            })
        });
        // In search results, ".." leads back to the folder that was searched.
        let up = if self.results.is_some() { Some(self.path.as_path()) } else { self.path.parent() };
        if self.show_dotdot {
            if let Some(parent) = up {
                entries.insert(
                    0,
                    Entry {
                        name: "..".into(),
                        path: parent.to_path_buf(),
                        is_dir: true,
                        is_symlink: false,
                        size: 0,
                        modified: None,
                        executable: false,
                    },
                );
            }
        }
        self.entries = entries;
        let names: HashSet<&str> = self.entries.iter().map(|e| e.name.as_str()).collect();
        self.selected.retain(|n| names.contains(n.as_str()));
        if let Some(name) = keep {
            if let Some(i) = self.entries.iter().position(|e| e.name == name) {
                self.cursor = i;
            }
        }
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.entries.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.entries.len() {
            self.cursor = self.entries.len() - 1;
        }
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort == key {
            self.ascending = !self.ascending;
        } else {
            self.sort = key;
            self.ascending = true;
        }
        self.apply_view();
        self.scroll_to_cursor = true;
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        self.show_hidden = show;
        self.apply_view();
        self.scroll_to_cursor = true;
    }

    /// Opens another folder, remembering where we came from.
    pub fn navigate(&mut self, path: &Path) -> Result<(), String> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if path == self.path && self.results.is_none() {
            self.loading = None; // e.g. clicked back to here while another folder was loading
            self.reload();
            return Ok(());
        }
        let old = self.path.clone();
        self.start_load(&path, After::Navigate { old })
    }

    fn after_dir_change(&mut self, old: &Path) {
        self.selected.clear();
        self.cursor = 0;
        // Plenty for a session's worth of folders; start over rather than grow forever.
        if self.scroll_memory.len() > 1000 {
            self.scroll_memory.clear();
        }
        self.scroll_memory.insert(old.to_path_buf(), self.scroll_offset);
        // Scrolled as it was when we left it. Only kept if the cursor is in view
        // there (the list scrolls just enough otherwise), so a folder entered
        // afresh, with the cursor at the top, still starts at the top.
        self.scroll_offset = self.scroll_memory.get(&self.path).copied().unwrap_or(0.0);
        self.editing_path = None;
        // Coming up from a child folder? Put the cursor on it.
        if old.parent() == Some(self.path.as_path()) {
            if let Some(n) = old.file_name() {
                let n = n.to_string_lossy();
                if let Some(i) = self.entries.iter().position(|e| e.name == n) {
                    self.cursor = i;
                }
            }
        }
        self.scroll_to_cursor = true;
    }

    /// Shows search results in the pane, like a folder (Backspace or ".." goes back
    /// to the folder that was searched).
    pub fn show_results(&mut self, set: ResultSet) -> Result<(), String> {
        let old = self.path.clone();
        let root = set.root.clone();
        self.start_load_as(&root, After::Navigate { old }, Some(Arc::new(set)))
    }

    /// Something was renamed: search results follow it (and anything inside it).
    pub fn renamed(&mut self, from: &Path, to: &Path) {
        let Some(set) = &self.results else { return };
        let paths = set
            .paths
            .iter()
            .map(|p| match p.strip_prefix(from) {
                Ok(rest) if rest.as_os_str().is_empty() => to.to_path_buf(),
                Ok(rest) => to.join(rest),
                Err(_) => p.clone(),
            })
            .collect();
        self.results = Some(Arc::new(ResultSet { query: set.query.clone(), root: set.root.clone(), paths }));
    }

    pub fn go_up(&mut self) -> Result<(), String> {
        if self.results.is_some() {
            let root = self.path.clone();
            return self.navigate(&root);
        }
        match self.path.parent() {
            Some(parent) => {
                let parent = parent.to_path_buf();
                self.navigate(&parent)
            }
            None => Ok(()),
        }
    }

    pub fn go_back(&mut self) -> Result<(), String> {
        let Some(prev) = self.back.pop() else { return Err("No earlier folder".into()) };
        let old = self.path.clone();
        self.start_load(&prev, After::Back { old })
    }

    pub fn go_forward(&mut self) -> Result<(), String> {
        let Some(next) = self.forward.pop() else { return Err("No later folder".into()) };
        let old = self.path.clone();
        self.start_load(&next, After::Forward { old })
    }

    /// Re-reads the folder. If it no longer exists, moves to the nearest folder that does.
    /// Does nothing while another folder is being read: that read brings fresh contents.
    pub fn reload(&mut self) {
        if self.loading.is_some() {
            return;
        }
        let path = self.path.clone();
        if let Err(e) = self.start_load_as(&path, After::Reload { gone: None }, self.results.clone()) {
            self.error = Some(e);
        }
    }

    /// True when the folder changed on disk since it was read.
    pub fn changed_on_disk(&self) -> bool {
        if self.results.is_some() {
            return false; // results are re-checked after file operations instead
        }
        let watched = self.archive.as_ref().map(|a| a.file.as_path()).unwrap_or(&self.path);
        let now = fs::metadata(watched).and_then(|m| m.modified()).ok();
        now != self.dir_mtime
    }

    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    /// The selected entries, or the entry under the cursor when nothing is selected.
    pub fn targets(&self) -> Vec<PathBuf> {
        if self.selected.is_empty() {
            self.cursor_entry().filter(|e| !e.is_up()).map(|e| vec![e.path.clone()]).unwrap_or_default()
        } else {
            self.entries
                .iter()
                .filter(|e| self.selected.contains(&e.name))
                .map(|e| e.path.clone())
                .collect()
        }
    }

    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i;
        self.clamp_cursor();
        self.scroll_to_cursor = true;
    }

    pub fn set_cursor_to_name(&mut self, name: &str) {
        if self.loading.is_some() {
            self.want_cursor = Some(name.to_string());
            return;
        }
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.set_cursor(i);
        }
    }

    /// Moves the cursor. With `extend`, the rows passed over get the opposite
    /// selection state of the row the move started on.
    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let old = self.cursor;
        let new = (old as isize + delta).clamp(0, n as isize - 1) as usize;
        if extend {
            let state = !self.is_selected(old);
            let range: Vec<usize> = if new > old {
                (old..new).collect()
            } else if new < old {
                (new + 1..=old).collect()
            } else {
                vec![old]
            };
            for i in range {
                self.set_selected(i, state);
            }
            if new == old {
                self.set_selected(new, state);
            }
        }
        self.cursor = new;
        self.scroll_to_cursor = true;
    }

    pub fn is_selected(&self, i: usize) -> bool {
        self.entries.get(i).is_some_and(|e| self.selected.contains(&e.name))
    }

    pub fn set_selected(&mut self, i: usize, on: bool) {
        if let Some(e) = self.entries.get(i).filter(|e| !e.is_up()) {
            if on {
                self.selected.insert(e.name.clone());
            } else {
                self.selected.remove(&e.name);
            }
        }
    }

    pub fn toggle_selected(&mut self, i: usize) {
        let on = !self.is_selected(i);
        self.set_selected(i, on);
    }

    pub fn select_range(&mut self, a: usize, b: usize) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        for i in lo..=hi.min(self.entries.len().saturating_sub(1)) {
            self.set_selected(i, true);
        }
    }

    pub fn select_all(&mut self) {
        self.selected = self.entries.iter().filter(|e| !e.is_up()).map(|e| e.name.clone()).collect();
    }

    pub fn invert_selection(&mut self) {
        self.selected = self
            .entries
            .iter()
            .filter(|e| !e.is_up() && !self.selected.contains(&e.name))
            .map(|e| e.name.clone())
            .collect();
    }

    /// Selects (or deselects) every entry matching `mask`. Returns how many matched.
    /// See [`Mask`] for the mask syntax.
    pub fn select_by_mask(&mut self, mask: &str, on: bool) -> usize {
        let mask = Mask::parse(mask);
        // Selecting takes folders only with a mask ending in "/"; deselecting takes
        // them whatever the mask, or "*" would leave selected folders behind.
        let hit = |e: &Entry| if on { mask.matches(&e.name, e.is_dir) } else { mask.matches_any(&e.name) };
        let names: Vec<String> = self.entries.iter().filter(|e| !e.is_up() && hit(e)).map(|e| e.name.clone()).collect();
        for n in &names {
            if on {
                self.selected.insert(n.clone());
            } else {
                self.selected.remove(n);
            }
        }
        names.len()
    }

    /// Jumps to the first entry starting with `prefix` (or containing it).
    pub fn quick_find(&mut self, prefix: &str) -> bool {
        let p = prefix.to_lowercase();
        let found = self
            .entries
            .iter()
            .position(|e| e.name.to_lowercase().starts_with(&p))
            .or_else(|| self.entries.iter().position(|e| e.name.to_lowercase().contains(&p)));
        if let Some(i) = found {
            self.set_cursor(i);
            true
        } else {
            false
        }
    }

    /// (folders, files, selected count, selected bytes)
    pub fn summary(&self) -> (usize, usize, usize, u64) {
        let dirs = self.entries.iter().filter(|e| e.is_dir && !e.is_up()).count();
        let files = self.entries.iter().filter(|e| !e.is_dir).count();
        let sel: Vec<&Entry> = self.entries.iter().filter(|e| self.selected.contains(&e.name)).collect();
        let bytes = sel.iter().map(|e| e.size).sum();
        (dirs, files, sel.len(), bytes)
    }
}

/// A selection mask in the style of Double Commander / Total Commander:
///
/// - `*` matches any run of characters, `?` exactly one; case doesn't matter
/// - several masks are separated by `;`, e.g. `*.jpg;*.png`
/// - masks after `|` exclude, e.g. `*.txt | readme*`
/// - a mask ending in `/` matches folders, e.g. `src*/`; others match files only
///   (`*` alone, or `*.*`, therefore selects all files but no folders). Deselecting
///   ignores that and takes folders too, so `*` clears a whole selection.
pub struct Mask {
    include: Vec<(Vec<char>, bool)>,
    exclude: Vec<(Vec<char>, bool)>,
}

impl Mask {
    pub fn parse(text: &str) -> Mask {
        let (inc, exc) = match text.split_once('|') {
            Some((a, b)) => (a, b),
            None => (text, ""),
        };
        let list = |s: &str| -> Vec<(Vec<char>, bool)> {
            s.split(';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| {
                    let dirs = p.ends_with('/') || p.ends_with('\\');
                    let p = p.trim_end_matches(['/', '\\']);
                    // "*.*" also matches names without a dot, like other commanders do.
                    let p = if p == "*.*" { "*" } else { p };
                    (p.to_lowercase().chars().collect(), dirs)
                })
                .collect()
        };
        let mut include = list(inc);
        if include.is_empty() && !exc.trim().is_empty() {
            include.push((vec!['*'], false));
        }
        Mask { include, exclude: list(exc) }
    }

    pub fn matches(&self, name: &str, is_dir: bool) -> bool {
        self.matches_as(name, Some(is_dir))
    }

    /// Like [`Mask::matches`], but files and folders alike (for deselecting).
    pub fn matches_any(&self, name: &str) -> bool {
        self.matches_as(name, None)
    }

    fn matches_as(&self, name: &str, is_dir: Option<bool>) -> bool {
        let n: Vec<char> = name.to_lowercase().chars().collect();
        let hit = |list: &[(Vec<char>, bool)]| list.iter().any(|(p, dirs)| is_dir.is_none_or(|d| *dirs == d) && glob(p, &n));
        hit(&self.include) && !hit(&self.exclude)
    }
}

/// Wildcard match of a whole name against a mask with `*` and `?` (exact case).
pub(crate) fn glob_str(mask: &str, name: &str) -> bool {
    glob(&mask.chars().collect::<Vec<_>>(), &name.chars().collect::<Vec<_>>())
}

/// Wildcard match with `*` and `?`.
fn glob(p: &[char], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// An archive's index kept from the last read: (file, its modification time, index).
type IndexCache = Option<(PathBuf, Option<SystemTime>, Arc<crate::archive::Index>)>;

/// Reads a folder, or a folder inside an archive. Runs on a background thread.
fn fetch(path: &Path, cache: IndexCache) -> Result<Fetched, String> {
    if !path.is_dir() {
        if let Some(file) = archive_file_in(path) {
            return fetch_archive(path, &file, cache);
        }
    }
    let all = read_entries(path).map_err(|e| format!("Can't open {}: {}", path.display(), e))?;
    let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
    Ok(Fetched::Dir { all, mtime })
}

/// Search results as entries: named by their path below the searched folder, so
/// files with the same name in different folders stay apart. Ones that no longer
/// exist are left out.
fn fetch_results(set: Arc<ResultSet>) -> Fetched {
    let all = set
        .paths
        .iter()
        .filter_map(|p| {
            let lmeta = fs::symlink_metadata(p).ok()?;
            let is_symlink = lmeta.file_type().is_symlink();
            let meta = if is_symlink { fs::metadata(p).unwrap_or(lmeta) } else { lmeta };
            let name = p.strip_prefix(&set.root).unwrap_or(p).to_string_lossy().into_owned();
            Some(Entry {
                name,
                path: p.clone(),
                is_dir: meta.is_dir(),
                is_symlink,
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta.modified().ok(),
                executable: is_executable(&meta),
            })
        })
        .collect();
    Fetched::Results { all, set }
}

/// Lists a folder inside an archive, like a normal folder.
fn fetch_archive(path: &Path, file: &Path, cache: IndexCache) -> Result<Fetched, String> {
    let inner = path.strip_prefix(file).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
    let inner = inner.trim_matches('/').to_string();
    let mtime = fs::metadata(file).and_then(|m| m.modified()).ok();
    let index = match cache {
        Some((f, m, index)) if f == file && m == mtime => index,
        _ => Arc::new(crate::archive::build_index(file).map_err(|e| format!("Can't open {}: {e}", file.display()))?),
    };
    if !index.is_dir(&inner) {
        return Err(format!("No folder “{inner}” in {}", file.display()));
    }
    let all = index
        .children(&inner)
        .map(|e| Entry {
            name: e.path.rsplit('/').next().unwrap_or(&e.path).to_string(),
            path: file.join(&e.path),
            is_dir: e.is_dir,
            is_symlink: false,
            size: e.size,
            modified: e.modified,
            executable: false,
        })
        .collect();
    Ok(Fetched::Archive { all, state: ArchiveState { file: file.to_path_buf(), index, mtime } })
}

/// If `path` is an archive file, or a folder inside one, returns the archive file.
fn archive_file_in(path: &Path) -> Option<PathBuf> {
    let mut p = Some(path);
    while let Some(cur) = p {
        if let Ok(m) = fs::metadata(cur) {
            return (m.is_file() && crate::archive::is_archive(cur)).then(|| cur.to_path_buf());
        }
        p = cur.parent();
    }
    None
}

fn read_entries(path: &Path) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for de in fs::read_dir(path)? {
        let Ok(de) = de else { continue };
        let p = de.path();
        let name = de.file_name().to_string_lossy().into_owned();
        let lmeta = de.metadata().ok();
        let is_symlink = lmeta.as_ref().is_some_and(|m| m.file_type().is_symlink());
        let meta = if is_symlink { fs::metadata(&p).ok().or(lmeta) } else { lmeta };
        let (is_dir, size, modified) = match &meta {
            Some(m) => (m.is_dir(), if m.is_dir() { 0 } else { m.len() }, m.modified().ok()),
            None => (false, 0, None),
        };
        let executable = meta.as_ref().is_some_and(is_executable);
        out.push(Entry { name, path: p, is_dir, is_symlink, size, modified, executable });
    }
    Ok(out)
}

/// A regular file anyone may run (Unix permission bits; never on other systems).
fn is_executable(m: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        false
    }
}

/// Case-insensitive comparison where "file2" sorts before "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(&c) = ai.peek() {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(&c) = bi.peek() {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    nb.push(c);
                    bi.next();
                }
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sorting() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("B", "a"), Ordering::Greater);
        assert_eq!(natural_cmp("x", "x"), Ordering::Equal);
    }

    #[test]
    fn masks() {
        let m = Mask::parse("*.jpg;*.PNG");
        assert!(m.matches("Cat.JPG", false));
        assert!(m.matches("a.png", false));
        assert!(!m.matches("a.jpg", true)); // folders need a trailing "/"
        assert!(!m.matches("a.gif", false));
        let m = Mask::parse("*.txt | readme*");
        assert!(m.matches("notes.txt", false));
        assert!(!m.matches("README.txt", false));
        let m = Mask::parse("src*/");
        assert!(m.matches("src-old", true));
        assert!(!m.matches("src-old", false));
        let m = Mask::parse("*.*");
        assert!(m.matches("Makefile", false));
        let m = Mask::parse("report-??.pdf");
        assert!(m.matches("report-07.pdf", false));
        assert!(!m.matches("report-7.pdf", false));
        let m = Mask::parse("| *.tmp");
        assert!(m.matches("a.txt", false));
        assert!(!m.matches("a.tmp", false));
    }

    #[test]
    fn deselecting_by_mask_takes_folders_too() {
        let dir = std::env::temp_dir().join(format!("duopane-demask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let mut p = Pane::new(&dir, false, false, SortKey::Name, true);
        p.select_by_mask("*", true);
        assert_eq!(p.selected.len(), 1, "selecting * takes the files only");
        p.select_all();
        assert_eq!(p.selected.len(), 3);
        p.select_by_mask("p*", false);
        assert!(!p.selected.contains("photos"), "a folder is deselected by a plain mask");
        p.select_by_mask("* | src", false);
        assert_eq!(p.selected.iter().collect::<Vec<_>>(), ["src"], "exclusions still count");
        p.select_by_mask("*", false);
        assert!(p.selected.is_empty(), "* clears the whole selection");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dotdot_row() {
        let dir = std::env::temp_dir().join(format!("duopane-dotdot-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let mut p = Pane::new(&dir, false, true, SortKey::Name, true);
        assert_eq!(p.entries[0].name, "..");
        assert_eq!(p.entries[0].path, dir.parent().unwrap());
        p.set_cursor(0);
        assert!(p.targets().is_empty(), ".. must never be copied or deleted");
        p.select_all();
        assert!(!p.selected.contains(".."));
        assert_eq!(p.summary().0, 1); // one real folder
        p.sort = SortKey::Size;
        p.ascending = false;
        p.apply_view();
        assert_eq!(p.entries[0].name, "..", ".. stays on top whatever the sort");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn browse_into_archive() {
        use crate::fs_ops::{start, JobSpec};
        let dir = std::env::temp_dir().join(format!("duopane-inarc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pics/2024")).unwrap();
        std::fs::write(dir.join("pics/2024/cat.jpg"), b"meow").unwrap();
        let arc = dir.join("pics.zip");
        let mut h = start(JobSpec::Pack { sources: vec![dir.join("pics")], archive: arc.clone(), format: crate::archive::Format::Zip, level: 6, delete_after: false });
        while !h.poll() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut p = Pane::new(&dir, false, true, SortKey::Name, true);
        p.navigate(&arc).unwrap();
        assert!(p.archive.is_some());
        assert_eq!(p.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["..", "pics"]);
        p.navigate(&arc.join("pics/2024")).unwrap();
        assert_eq!(p.entries[1].name, "cat.jpg");
        assert_eq!(p.entries[1].size, 4);
        assert_eq!(p.real_dir(), dir.canonicalize().unwrap_or(dir.clone()));
        p.go_up().unwrap();
        p.go_up().unwrap();
        assert_eq!(p.path, arc);
        p.go_up().unwrap(); // out of the archive, cursor on it
        assert!(p.archive.is_none());
        assert_eq!(p.cursor_entry().unwrap().name, "pics.zip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Waits for a background read, returning what `poll_loading` reported.
    fn settle(p: &mut Pane) -> Result<bool, String> {
        let t = std::time::Instant::now();
        loop {
            if let Some(r) = p.poll_loading() {
                return r;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "folder never finished loading");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn slow_folders_load_in_the_background() {
        let dir = std::env::temp_dir().join(format!("duopane-bg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("a/inner")).unwrap();
        fs::write(dir.join("a/x.txt"), b"x").unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut p = Pane::new(&dir, false, false, SortKey::Name, true);
        p.load_wait = Duration::ZERO; // every read now finishes in the background

        p.navigate(&dir.join("a")).unwrap();
        assert!(p.is_loading());
        assert_eq!(p.path, dir, "keeps showing the old folder meanwhile");
        p.reload(); // ignored: mustn't cancel the navigation
        p.set_cursor_to_name("x.txt"); // remembered until the folder arrives
        assert_eq!(settle(&mut p), Ok(true));
        assert_eq!(p.path, dir.join("a"));
        assert_eq!(p.cursor_entry().unwrap().name, "x.txt");
        assert_eq!(p.back, vec![dir.clone()]);

        p.go_back().unwrap();
        assert_eq!(settle(&mut p), Ok(true));
        assert_eq!(p.path, dir);
        assert_eq!(p.cursor_entry().unwrap().name, "a", "cursor on the folder we came from");
        assert_eq!(p.forward, vec![dir.join("a")]);

        // The folder being shown disappears: the pane moves to its parent.
        p.navigate(&dir.join("a/inner")).unwrap();
        settle(&mut p).unwrap();
        fs::remove_dir_all(dir.join("a/inner")).unwrap();
        p.reload();
        assert_eq!(settle(&mut p), Ok(true));
        assert_eq!(p.path, dir.join("a"));

        assert!(p.navigate(&dir.join("missing")).is_ok());
        assert!(settle(&mut p).is_err());
        assert_eq!(p.path, dir.join("a"), "a failed read leaves the pane where it was");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_results_as_a_folder() {
        let dir = std::env::temp_dir().join(format!("duopane-results-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("b")).unwrap();
        fs::write(dir.join("a/x.txt"), b"1").unwrap();
        fs::write(dir.join("b/x.txt"), b"22").unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut p = Pane::new(&dir, false, true, SortKey::Name, true);
        let set = ResultSet { query: "x".into(), root: dir.clone(), paths: vec![dir.join("a/x.txt"), dir.join("b/x.txt"), dir.join("gone.txt")] };
        p.show_results(set).unwrap();
        assert!(p.results.is_some());
        let names: Vec<&str> = p.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["..", "a/x.txt", "b/x.txt"], "same names kept apart; vanished files left out");
        assert_eq!(p.entries[0].path, dir, "\"..\" leads back to the searched folder");
        assert_eq!(p.entries[2].size, 2);
        p.select_all();
        assert_eq!(p.targets(), vec![dir.join("a/x.txt"), dir.join("b/x.txt")]);
        assert!(!p.changed_on_disk());

        // After a file operation, results are re-checked.
        fs::remove_file(dir.join("a/x.txt")).unwrap();
        p.reload();
        assert_eq!(p.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["..", "b/x.txt"]);

        p.go_up().unwrap();
        assert!(p.results.is_none());
        assert_eq!(p.path, dir);
        assert_eq!(p.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["..", "a", "b"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(10), "10 B");
        assert_eq!(human_size(1536), "1.5 KB");
    }
}
