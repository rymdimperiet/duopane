//! "Open with" (Ctrl+Enter): the installed apps that can open a file, read the way
//! Linux desktops do it, from freedesktop.org's plain-text files:
//!
//! - the file's type comes from shared-mime-info's `mime/globs2` (by name only:
//!   sniffing contents would mean reading files, slow on network mounts),
//! - apps say which types they open in their `.desktop` files (`MimeType=`),
//! - the user's own choices (default app, added and removed links) are in
//!   `mimeapps.list`.
//!
//! Every installed app is offered as well, so any app can be chosen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An app from a `.desktop` file.
#[derive(Debug)]
pub struct DesktopApp {
    /// The desktop file ID, e.g. "org.kde.gwenview.desktop".
    pub id: String,
    pub name: String,
    exec: String,
    terminal: bool,
    mimes: Vec<String>,
    /// False for helpers marked NoDisplay: offered for their types, not in the full list.
    listed: bool,
    file: PathBuf,
}

/// Where the desktop's files live (XDG base directories).
pub struct Dirs {
    /// Data folders, most important first (~/.local/share, then /usr/local/share, /usr/share…).
    data: Vec<PathBuf>,
    /// Config folders, most important first (~/.config, then /etc/xdg…).
    config: Vec<PathBuf>,
    /// The current desktop's names, lowercase ("kde"), for `kde-mimeapps.list`.
    desktops: Vec<String>,
}

impl Dirs {
    pub fn system() -> Dirs {
        let home = dirs::home_dir().unwrap_or_default();
        let list = |var: &str, own: &str, own_default: PathBuf, rest_default: &str| -> Vec<PathBuf> {
            let own = std::env::var_os(own).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or(own_default);
            let rest = std::env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| rest_default.to_string());
            std::iter::once(own).chain(rest.split(':').filter(|s| !s.is_empty()).map(PathBuf::from)).collect()
        };
        Dirs {
            data: list("XDG_DATA_DIRS", "XDG_DATA_HOME", home.join(".local/share"), "/usr/local/share:/usr/share"),
            config: list("XDG_CONFIG_DIRS", "XDG_CONFIG_HOME", home.join(".config"), "/etc/xdg"),
            desktops: std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(str::to_lowercase)
                .collect(),
        }
    }
}

/// What the list offers for some files.
pub struct Choices {
    /// Apps for the files' type, the default (if one is set) first.
    pub suggested: Vec<Arc<DesktopApp>>,
    pub has_default: bool,
    /// Every other installed app, by name.
    pub others: Vec<Arc<DesktopApp>>,
}

/// The apps to offer for `files` (folders are `inode/directory`).
pub fn choices(dirs: &Dirs, files: &[(String, bool)]) -> Choices {
    let apps = installed_apps(dirs);
    let types = MimeDb::load(dirs);
    let lists = MimeAppsLists::load(dirs);
    let by_id: HashMap<&str, &Arc<DesktopApp>> = apps.iter().map(|a| (a.id.as_str(), a)).collect();

    // The apps for each file, in order of preference; offer the ones that suit them all.
    let mut common: Option<Vec<String>> = None;
    let mut has_default = false;
    for (i, (name, is_dir)) in files.iter().enumerate() {
        let Some(mime) = (if *is_dir { Some("inode/directory".to_string()) } else { types.type_of(name) }) else {
            common = Some(Vec::new());
            break;
        };
        let (ids, default) = apps_for_type(&types.with_parents(&mime), &apps, &lists, &by_id);
        if i == 0 {
            has_default = default;
        }
        common = Some(match common {
            None => ids,
            Some(prev) => prev.into_iter().filter(|id| ids.contains(id)).collect(),
        });
    }
    // Some apps have several desktop files (Okular has one per type); show each name once.
    let mut names = HashSet::new();
    let suggested: Vec<Arc<DesktopApp>> = common
        .unwrap_or_default()
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).map(|a| Arc::clone(a)))
        .filter(|a| names.insert(a.name.clone()))
        .collect();
    // The default is only first when it suits every file.
    let has_default = has_default && !suggested.is_empty();
    let taken: HashSet<&str> = suggested.iter().map(|a| a.id.as_str()).collect();
    let mut others: Vec<Arc<DesktopApp>> =
        apps.iter().filter(|a| a.listed && !taken.contains(a.id.as_str())).cloned().collect();
    others.sort_by_key(|a| a.name.to_lowercase());
    others.retain(|a| names.insert(a.name.clone()));
    Choices { suggested, has_default, others }
}

/// The app a file opens with (the user's default for its type), if one is set.
/// Asked for every frame while a menu shows it, so the last answer is kept a moment.
pub fn default_app(name: &str, is_dir: bool) -> Option<Arc<DesktopApp>> {
    type Last = Option<((String, bool), std::time::Instant, Option<Arc<DesktopApp>>)>;
    static LAST: std::sync::Mutex<Last> = std::sync::Mutex::new(None);
    let key = (name.to_string(), is_dir);
    if let Ok(last) = LAST.lock() {
        if let Some((k, at, app)) = last.as_ref() {
            if *k == key && at.elapsed() < std::time::Duration::from_secs(3) {
                return app.clone();
            }
        }
    }
    let c = choices(&Dirs::system(), std::slice::from_ref(&key));
    let app = if c.has_default { c.suggested.into_iter().next() } else { None };
    if let Ok(mut last) = LAST.lock() {
        *last = Some((key, std::time::Instant::now(), app.clone()));
    }
    app
}

/// The apps for a type (most specific type first in `chain`) and whether the first is the default.
fn apps_for_type(
    chain: &[String],
    apps: &[Arc<DesktopApp>],
    lists: &MimeAppsLists,
    by_id: &HashMap<&str, &Arc<DesktopApp>>,
) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::new();
    let push = |id: &str, out: &mut Vec<String>| {
        if by_id.contains_key(id) && !out.iter().any(|o| o == id) {
            out.push(id.to_string());
        }
    };
    let default = chain.iter().find_map(|m| lists.defaults.get(m)?.iter().find(|id| by_id.contains_key(id.as_str())));
    if let Some(d) = default {
        push(d, &mut out);
    }
    for mime in chain {
        let removed = lists.removed.get(mime);
        for id in lists.added.get(mime).into_iter().flatten() {
            push(id, &mut out);
        }
        let mut own: Vec<&Arc<DesktopApp>> = apps
            .iter()
            .filter(|a| a.mimes.iter().any(|m| m == mime))
            .filter(|a| !removed.is_some_and(|r| r.contains(&a.id)))
            .collect();
        own.sort_by_key(|a| a.name.to_lowercase());
        for a in own {
            push(&a.id, &mut out);
        }
    }
    (out, default.is_some())
}

/// Every app with a `.desktop` file; for an ID found in several folders, the first wins.
fn installed_apps(dirs: &Dirs) -> Vec<Arc<DesktopApp>> {
    let lang = language();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for data in &dirs.data {
        let root = data.join("applications");
        let mut files = Vec::new();
        desktop_files(&root, &root, &mut files, 0);
        for (id, path) in files {
            if !seen.insert(id.clone()) {
                continue; // hidden by a more important folder (that may also hide it on purpose)
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if let Some(app) = parse_desktop(&id, &path, &text, &lang) {
                out.push(Arc::new(app));
            }
        }
    }
    out
}

/// `.desktop` files under `root`; a file in a subfolder gets the ID "sub-name.desktop".
fn desktop_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>, depth: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        if path.is_dir() && depth < 4 {
            desktop_files(root, &path, out, depth + 1);
        } else if path.extension().is_some_and(|x| x == "desktop") {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push((rel.to_string_lossy().replace('/', "-"), path));
            }
        }
    }
}

/// The user's language for app names, e.g. ["sv_SE", "sv"].
fn language() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let base = raw.split(['.', '@']).next().unwrap_or("").to_string();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return Vec::new();
    }
    let short = base.split('_').next().unwrap_or("").to_string();
    if short == base { vec![base] } else { vec![base, short] }
}

/// Reads the `[Desktop Entry]` group; None for anything that isn't a usable app.
fn parse_desktop(id: &str, file: &Path, text: &str, lang: &[String]) -> Option<DesktopApp> {
    let mut keys: HashMap<&str, String> = HashMap::new();
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            keys.entry(k.trim()).or_insert_with(|| unescape(v.trim()));
        }
    }
    let yes = |k: &str| keys.get(k).is_some_and(|v| v == "true");
    if keys.get("Type").map(String::as_str) != Some("Application") || yes("Hidden") {
        return None;
    }
    let exec = keys.get("Exec")?.clone();
    if let Some(t) = keys.get("TryExec") {
        if !program_exists(t) {
            return None;
        }
    }
    let name = lang
        .iter()
        .find_map(|l| keys.get(format!("Name[{l}]").as_str()))
        .or_else(|| keys.get("Name"))?
        .clone();
    let mimes = keys.get("MimeType").map(|m| m.split(';').filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default();
    Some(DesktopApp { id: id.to_string(), name, exec, terminal: yes("Terminal"), mimes, listed: !yes("NoDisplay"), file: file.to_path_buf() })
}

/// The escapes allowed in a desktop file's values.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
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

fn program_exists(p: &str) -> bool {
    if p.contains('/') {
        return Path::new(p).is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(p).is_file()))
}

/// shared-mime-info: file name patterns, aliases and parent types.
#[derive(Default)]
struct MimeDb {
    /// (weight, type, pattern lowercased unless case-sensitive, case-sensitive)
    globs: Vec<(u32, String, String, bool)>,
    aliases: HashMap<String, String>,
    parents: HashMap<String, Vec<String>>,
}

impl MimeDb {
    fn load(dirs: &Dirs) -> MimeDb {
        let mut db = MimeDb::default();
        // Less important folders first, so the user's own files can add to them.
        for data in dirs.data.iter().rev() {
            let mime = data.join("mime");
            if let Ok(t) = std::fs::read_to_string(mime.join("globs2")) {
                db.add_globs(&t);
            }
            if let Ok(t) = std::fs::read_to_string(mime.join("aliases")) {
                for (a, b) in t.lines().filter_map(|l| l.split_once(' ')) {
                    db.aliases.insert(a.to_string(), b.trim().to_string());
                }
            }
            if let Ok(t) = std::fs::read_to_string(mime.join("subclasses")) {
                for (a, b) in t.lines().filter_map(|l| l.split_once(' ')) {
                    let v = db.parents.entry(a.to_string()).or_default();
                    if !v.iter().any(|p| p == b.trim()) {
                        v.push(b.trim().to_string());
                    }
                }
            }
        }
        db
    }

    fn add_globs(&mut self, text: &str) {
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(4, ':');
            let (Some(w), Some(t), Some(p)) = (parts.next(), parts.next(), parts.next()) else { continue };
            let cs = parts.next().is_some_and(|f| f.split(',').any(|f| f == "cs"));
            let Ok(w) = w.parse() else { continue };
            let p = if cs { p.to_string() } else { p.to_lowercase() };
            self.globs.push((w, t.to_string(), p, cs));
        }
    }

    /// The type a file name suggests: the heaviest match, then the longest pattern.
    fn type_of(&self, name: &str) -> Option<String> {
        let lower = name.to_lowercase();
        self.globs
            .iter()
            .filter(|(_, _, p, cs)| glob_match(p, if *cs { name } else { &lower }))
            .max_by_key(|(w, _, p, _)| (*w, p.len()))
            .map(|(_, t, _, _)| self.aliases.get(t).cloned().unwrap_or_else(|| t.clone()))
    }

    /// The type and the types it's a kind of (text/x-python → text/plain), closest first.
    fn with_parents(&self, mime: &str) -> Vec<String> {
        let mut out = vec![mime.to_string()];
        let mut i = 0;
        while i < out.len() && out.len() < 16 {
            let m = out[i].clone();
            let mut parents = self.parents.get(&m).cloned().unwrap_or_default();
            if m.starts_with("text/") && m != "text/plain" {
                parents.push("text/plain".into());
            }
            for p in parents {
                let p = self.aliases.get(&p).cloned().unwrap_or(p);
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        out
    }
}

/// Glob matching with `*`, `?` and `[...]`, enough for shared-mime-info's patterns.
fn glob_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| go(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some('[') => {
                let Some(end) = p.iter().skip(1).position(|&c| c == ']').map(|e| e + 1) else {
                    return n.first() == Some(&'[') && go(&p[1..], &n[1..]);
                };
                let Some(&c) = n.first() else { return false };
                let set = &p[1..end];
                let (neg, set) = if set.first() == Some(&'!') { (true, &set[1..]) } else { (false, set) };
                let mut hit = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == '-' {
                        hit |= set[i] <= c && c <= set[i + 2];
                        i += 3;
                    } else {
                        hit |= set[i] == c;
                        i += 1;
                    }
                }
                hit != neg && go(&p[end + 1..], &n[1..])
            }
            Some(&c) => n.first() == Some(&c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    // Most patterns are "*.ext": skip the general matcher for those.
    if p.first() == Some(&'*') && !p[1..].iter().any(|c| matches!(c, '*' | '?' | '[')) {
        return name.ends_with(&pattern[1..]);
    }
    go(&p, &n)
}

/// The user's and the system's `mimeapps.list` choices, merged (more important files first).
#[derive(Default)]
struct MimeAppsLists {
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, HashSet<String>>,
}

impl MimeAppsLists {
    fn load(dirs: &Dirs) -> MimeAppsLists {
        let mut out = MimeAppsLists::default();
        let folders = dirs.config.iter().cloned().chain(dirs.data.iter().map(|d| d.join("applications")));
        for folder in folders {
            let names = dirs.desktops.iter().map(|d| format!("{d}-mimeapps.list")).chain(["mimeapps.list".to_string()]);
            for name in names {
                if let Ok(t) = std::fs::read_to_string(folder.join(name)) {
                    out.add(&t);
                }
            }
        }
        out
    }

    fn add(&mut self, text: &str) {
        let mut group = "";
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                group = line;
                continue;
            }
            let Some((mime, ids)) = line.split_once('=') else { continue };
            let ids = ids.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from);
            let (mime, ids) = (mime.trim().to_string(), ids.collect::<Vec<_>>());
            match group {
                "[Default Applications]" => self.defaults.entry(mime).or_default().extend(ids),
                "[Added Associations]" => {
                    let v = self.added.entry(mime).or_default();
                    v.extend(ids.into_iter().filter(|i| !v.contains(i)).collect::<Vec<_>>())
                }
                "[Removed Associations]" => self.removed.entry(mime).or_default().extend(ids),
                _ => {}
            }
        }
    }
}

/// The command lines that open `files` with `app`: one when it takes a list
/// (%F, %U), else one per file (%f, %u). Apps without a field code get the files appended.
pub fn command_lines(app: &DesktopApp, files: &[PathBuf]) -> Vec<Vec<String>> {
    let words = split_exec(&app.exec);
    let codes: String = words.iter().flat_map(|w| field_codes(w)).collect();
    let takes_list = codes.contains(['F', 'U']);
    let takes_one = codes.contains(['f', 'u']);
    let groups: Vec<&[PathBuf]> =
        if takes_one && !takes_list { files.chunks(1).collect() } else { vec![files] };
    let mut out = Vec::new();
    for group in groups {
        let mut argv = Vec::new();
        for w in &words {
            match w.as_str() {
                "%F" => argv.extend(group.iter().map(|p| p.to_string_lossy().into_owned())),
                "%U" => argv.extend(group.iter().map(|p| file_url(p))),
                "%i" => {} // an icon we don't track
                _ => {
                    let first = group.first();
                    let mut s = String::new();
                    let mut chars = w.chars();
                    while let Some(c) = chars.next() {
                        if c != '%' {
                            s.push(c);
                            continue;
                        }
                        match chars.next() {
                            Some('%') => s.push('%'),
                            Some('f') => s.push_str(&first.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()),
                            Some('u') => s.push_str(&first.map(|p| file_url(p)).unwrap_or_default()),
                            Some('c') => s.push_str(&app.name),
                            Some('k') => s.push_str(&app.file.to_string_lossy()),
                            _ => {} // deprecated or unknown codes are dropped
                        }
                    }
                    // A word that was only a code with nothing to put in it disappears.
                    if !s.is_empty() || field_codes(w).is_empty() {
                        argv.push(s);
                    }
                }
            }
        }
        if codes.is_empty() {
            argv.extend(group.iter().map(|p| p.to_string_lossy().into_owned()));
        }
        if !argv.is_empty() {
            out.push(argv);
        }
    }
    out
}

pub fn needs_terminal(app: &DesktopApp) -> bool {
    app.terminal
}

/// The field codes (letters after %) in one word of Exec.
fn field_codes(w: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut chars = w.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('%') | None => {}
                Some(code) => out.push(code),
            }
        }
    }
    out
}

/// Splits Exec into words: spaces separate them, double quotes group them, and
/// inside quotes a backslash escapes the next character.
fn split_exec(exec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' if !quoted => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            _ => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// A `file://` URL with everything but unreserved characters and `/` percent-encoded.
fn file_url(p: &Path) -> String {
    let mut s = String::from("file://");
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(p.as_os_str()).to_vec();
    #[cfg(not(unix))]
    let bytes = p.to_string_lossy().into_owned().into_bytes();
    for b in bytes {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(exec: &str) -> DesktopApp {
        DesktopApp {
            id: "x.desktop".into(),
            name: "X".into(),
            exec: exec.into(),
            terminal: false,
            mimes: vec![],
            listed: true,
            file: PathBuf::from("/a/x.desktop"),
        }
    }

    #[test]
    fn exec_lines() {
        let files = [PathBuf::from("/p/a b.jpg"), PathBuf::from("/p/c.jpg")];
        assert_eq!(command_lines(&app("gimp %U"), &files), vec![vec!["gimp", "file:///p/a%20b.jpg", "file:///p/c.jpg"]]);
        assert_eq!(command_lines(&app("feh %f"), &files), vec![vec!["feh", "/p/a b.jpg"], vec!["feh", "/p/c.jpg"]]);
        assert_eq!(command_lines(&app("plain"), &files[..1]), vec![vec!["plain", "/p/a b.jpg"]]);
        assert_eq!(command_lines(&app("vlc --started-from-file %U"), &files[..1])[0][1], "--started-from-file");
        assert_eq!(
            command_lines(&app(r#"sh -c "echo \"hi\" 100%%" %F --name=%c %i"#), &files[..1]),
            vec![vec!["sh", "-c", "echo \"hi\" 100%", "/p/a b.jpg", "--name=X"]]
        );
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.jpg", "photo.jpg"));
        assert!(!glob_match("*.jpg", "photo.jpeg"));
        assert!(glob_match("makefile", "makefile"));
        assert!(glob_match("*.[ch]", "x.h"));
        assert!(glob_match("readme*", "readme.md"));
        assert!(glob_match("*.tar.?z", "a.tar.gz"));
        let mut db = MimeDb::default();
        db.add_globs("50:text/plain:*.txt\n50:application/x-compressed-tar:*.tar.gz\n10:application/gzip:*.gz\n50:text/x-csrc:*.c:cs\n50:text/x-c++src:*.C:cs\n");
        assert_eq!(db.type_of("A.TXT").as_deref(), Some("text/plain"));
        assert_eq!(db.type_of("x.tar.gz").as_deref(), Some("application/x-compressed-tar"));
        assert_eq!(db.type_of("x.C").as_deref(), Some("text/x-c++src"));
        assert_eq!(db.type_of("README"), None);
    }

    #[test]
    fn apps_for_a_file() {
        let root = std::env::temp_dir().join(format!("duopane-openwith-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (user, sys, conf) = (root.join("user"), root.join("sys"), root.join("conf"));
        let w = |p: PathBuf, t: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        let entry = |name: &str, extra: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec={name} %f\n{extra}\n[Desktop Action x]\nName=Not this\n");
        w(sys.join("mime/globs2"), "50:image/jpeg:*.jpg\n50:text/x-python:*.py\n");
        w(sys.join("mime/subclasses"), "text/x-python text/plain\n");
        w(sys.join("applications/viewer.desktop"), &entry("Viewer", "MimeType=image/jpeg;"));
        w(sys.join("applications/painter.desktop"), &entry("Painter", "MimeType=image/jpeg;image/png;"));
        w(sys.join("applications/editor.desktop"), &entry("Editor", "MimeType=text/plain;"));
        w(sys.join("applications/editor-py.desktop"), &entry("Editor", "MimeType=text/x-python;"));
        w(sys.join("applications/helper.desktop"), &entry("Helper", "MimeType=image/jpeg;\nNoDisplay=true"));
        w(sys.join("applications/gone.desktop"), &entry("Gone", ""));
        w(sys.join("applications/kde/term.desktop"), &entry("Term", "Name[sv]=Terminal"));
        w(user.join("applications/gone.desktop"), "[Desktop Entry]\nType=Application\nName=Gone\nExec=gone\nHidden=true\n");
        w(conf.join("mimeapps.list"), "[Default Applications]\nimage/jpeg=missing.desktop;painter.desktop;\n[Removed Associations]\nimage/jpeg=helper.desktop;\n");
        let dirs = Dirs { data: vec![user, sys], config: vec![conf], desktops: vec!["kde".into()] };
        let names = |v: &[Arc<DesktopApp>]| v.iter().map(|a| a.name.clone()).collect::<Vec<_>>();

        let c = choices(&dirs, &[("a.jpg".into(), false)]);
        assert_eq!(names(&c.suggested), ["Painter", "Viewer"]);
        assert!(c.has_default);
        // Hidden in the user's folder, and NoDisplay helpers stay out of the full list.
        assert_eq!(names(&c.others), ["Editor", "Term"]);
        assert!(c.others.iter().any(|a| a.id == "kde-term.desktop"));

        let c = choices(&dirs, &[("x.py".into(), false)]);
        assert_eq!(names(&c.suggested), ["Editor"]);
        assert!(!c.has_default);

        // Nothing suits both a picture and a script, and an unknown type gets no suggestions.
        assert!(choices(&dirs, &[("a.jpg".into(), false), ("x.py".into(), false)]).suggested.is_empty());
        let c = choices(&dirs, &[("README".into(), false)]);
        assert!(c.suggested.is_empty() && c.others.len() == 4);
        let _ = std::fs::remove_dir_all(&root);
    }
}
