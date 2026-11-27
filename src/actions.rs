//! What each command does. `commands.rs` maps names and shortcuts to these.

use crate::app::{describe, plural, subdirs, tilde, App, CmdRef, InputPurpose, Modal, MountJob};
use crate::dialogs::{dir_item, PaletteItem, PaletteKind, PaletteState, PaletteTarget};
use crate::fs_ops::{self, ConflictPolicy, JobSpec};
use crate::pane::SortKey;
use crate::settings::Settings;
use crate::theme;
use eframe::egui;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::sync::mpsc;

impl App {
    /// The keyboard is in the stack's frame, so commands work on its items.
    pub(crate) fn on_stack(&self) -> bool {
        self.stack_view.focused && !self.settings.stack.is_empty()
    }

    /// The item under the cursor: the stack's while it has the keyboard,
    /// otherwise the active pane's.
    pub(crate) fn cursor_item(&self) -> Option<crate::pane::Entry> {
        if !self.on_stack() {
            return self.panes[self.active].cursor_entry().cloned();
        }
        let p = self.settings.stack.get(self.stack_view.cursor)?.clone();
        match self.stack_view.known(&p) {
            Some(found) => found,
            // Not looked at yet: ask now, but never wait long (a network folder).
            None => crate::pane::within(std::time::Duration::from_millis(300), move || crate::stack::entry_of(&p)).flatten(),
        }
    }

    /// The picked items (or the one under the cursor), from the stack or the pane.
    pub(crate) fn picked(&self) -> Vec<PathBuf> {
        if self.on_stack() {
            self.stack_view.targets(&self.settings.stack)
        } else {
            self.panes[self.active].targets()
        }
    }

    /// Where a command's result goes (a copy, an archive): the other pane, or,
    /// from the stack, the active pane (the one with the border).
    pub(crate) fn dest_pane(&self) -> usize {
        if self.on_stack() { self.active } else { self.other() }
    }

    /// Opening a folder from the stack shows it in the active pane, and the
    /// keyboard goes along.
    fn open_folder(&mut self, path: &Path) {
        self.stack_view.focused = false;
        self.navigate(self.active, path);
    }

    pub fn open_cursor(&mut self, _ctx: &egui::Context) {
        self.open_item();
    }

    /// Enter: a folder or archive opens in the pane, a file in its app.
    pub(crate) fn open_item(&mut self) {
        let Some(e) = self.cursor_item() else { return };
        let in_archive = self.in_archive();
        if e.is_dir {
            self.open_folder(&e.path);
        } else if !in_archive && crate::archive::is_archive(&e.path) {
            // Like Double Commander: step into the archive as if it were a folder.
            self.open_folder(&e.path);
            if self.panes[self.active].archive.is_some() {
            }
        } else {
            self.open_external_entry(&e);
        }
    }

    /// Open with the system's app, even archives (in the Shift+Enter menu).
    pub fn open_external(&mut self) {
        let Some(e) = self.cursor_item() else { return };
        if e.is_dir {
            return self.open_folder(&e.path);
        }
        self.open_external_entry(&e);
    }

    fn open_external_entry(&mut self, e: &crate::pane::Entry) {
        let Some(path) = self.materialize(&e.path) else { return };
        // The app the menus name as "Open (…)", so what they say is what happens.
        if let Some(app) = crate::open_with::default_app(&e.name, e.is_dir) {
            return self.launch_with(&app, &[path]);
        }
        match open::that_detached(&path) {
            Ok(()) => {},
            Err(err) => self.set_status(format!("Couldn't open {}: {err}", e.name), true),
        }
    }

    pub fn go_up(&mut self) {
        let r = self.panes[self.active].go_up();
        self.after_move(self.active, r);
    }

    /// The right-click menu, opened from the keyboard just below the cursor row.
    pub fn open_row_menu(&mut self) {
        let at = if self.on_stack() { self.stack_view.cursor_rect } else { self.panes[self.active].cursor_rect };
        let at = match (at, self.panes_rect) {
            (Some(r), _) => r.left_bottom() + egui::vec2(24.0, 2.0),
            (None, Some(r)) => r.center(),
            (None, None) => egui::pos2(100.0, 100.0),
        };
        self.open_row_menu_at(at);
    }

    /// The menu for the item under the cursor, at `at` (right-click, or below the row).
    pub(crate) fn open_row_menu_at(&mut self, at: egui::Pos2) {
        let Some(e) = self.cursor_item().filter(|e| !e.is_up()) else {
            if self.on_stack() {
                self.set_status("It isn't there any more; Delete takes it off the stack", true);
            }
            return;
        };
        let mut menu = crate::ui::row_menu(&e.name, e.is_dir, self.in_archive());
        if self.on_stack() {
            // From the stack, copies go to the pane with the border, and nothing is pasted here.
            menu.retain(|(id, _)| *id != "clip_paste");
            for (id, title) in &mut menu {
                match *id {
                    "copy" => *title = "Copy to the pane…".into(),
                    "move" => *title = "Move to the pane…".into(),
                    "copy_in_place" => *title = "Copy next to it as…".into(),
                    _ => {}
                }
            }
            menu.insert(1, ("stack_reveal", "Show it in the pane".into()));
            menu.insert(2, ("stack", "Take off the stack".into()));
        }
        let items = menu
            .into_iter()
            .map(|(id, title)| {
                let keys = self.commands.iter().position(|c| c.id == id).map(|i| self.keys_for(CmdRef::Builtin(i))).unwrap_or_default();
                crate::dialogs::MenuItem { id, title, keys }
            })
            .collect();
        self.modal = Some(Modal::RowMenu { at, selected: 0, items });
    }

    /// "Open with": the apps for the selected files (or the one under the cursor),
    /// the default first, then every other app.
    pub fn open_with(&mut self) {
        let pane = &self.panes[self.active];
        let on_stack = self.on_stack();
        let picked: Vec<(PathBuf, String, bool)> = self
            .picked()
            .into_iter()
            .map(|p| {
                let known = if on_stack { self.stack_view.known(&p).flatten() } else { pane.entries.iter().find(|e| e.path == p).cloned() };
                let is_dir = known.map_or_else(|| p.is_dir(), |e| e.is_dir);
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                (p, name, is_dir)
            })
            .collect();
        if picked.is_empty() {
            return;
        }
        let mut files = Vec::new();
        for (p, _, _) in &picked {
            let Some(real) = self.materialize(p) else { return };
            files.push(real);
        }
        let types: Vec<(String, bool)> = picked.iter().map(|(_, n, d)| (n.clone(), *d)).collect();
        let c = crate::open_with::choices(&crate::open_with::Dirs::system(), &types);
        if c.suggested.is_empty() && c.others.is_empty() {
            return self.set_status("Couldn't find any installed apps", true);
        }
        let files = std::sync::Arc::new(files);
        let mut base = Vec::new();
        for (i, app) in c.suggested.iter().enumerate() {
            let detail = if i == 0 && c.has_default { "Default app" } else { "Suggested" };
            base.push(PaletteItem { title: app.name.clone(), detail: detail.into(), target: PaletteTarget::App(app.clone(), files.clone()), boost: 1000 - i as i32 });
        }
        for app in &c.others {
            base.push(PaletteItem { title: app.name.clone(), detail: String::new(), target: PaletteTarget::App(app.clone(), files.clone()), boost: 0 });
        }
        let what = if picked.len() == 1 { format!("\"{}\"", picked[0].1) } else { plural(picked.len(), "item") };
        let mut st = PaletteState::new(PaletteKind::OpenWith, base);
        st.hint = Some(format!("Open {what} with… (type to find any app)"));
        self.modal = Some(Modal::Palette(st));
    }

    pub(crate) fn launch_with(&mut self, app: &crate::open_with::DesktopApp, files: &[PathBuf]) {
        let dir = files.first().and_then(|f| f.parent()).map(Path::to_path_buf).unwrap_or_else(|| self.panes[self.active].real_dir());
        for argv in crate::open_with::command_lines(app, files) {
            let argv = if crate::open_with::needs_terminal(app) {
                match self.in_terminal(argv) {
                    Some(a) => a,
                    None => return self.set_status("No terminal found; set \"terminal\" in the settings file", true),
                }
            } else {
                argv
            };
            let mut cmd = Process::new(&argv[0]);
            cmd.args(&argv[1..]).current_dir(&dir);
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
            if let Err(e) = cmd.spawn_reaped() {
                return self.set_status(format!("Couldn't start {}: {e}", app.name), true);
            }
        }
    }

    /// A command line wrapped to run in a terminal window (for apps marked Terminal=true).
    fn in_terminal(&self, argv: Vec<String>) -> Option<Vec<String>> {
        let configured = if !self.settings.terminal.trim().is_empty() {
            Some(self.settings.terminal.clone())
        } else {
            std::env::var("TERMINAL").ok().filter(|t| !t.is_empty())
        };
        if let Some(t) = configured {
            let script = format!("{t} -e \"$@\"");
            return Some(["sh", "-c", &script, "sh"].iter().map(|s| s.to_string()).chain(argv).collect());
        }
        let (term, flag): (&str, &[&str]) = [
            ("xdg-terminal-exec", &[][..]),
            ("konsole", &["-e"][..]),
            ("kgx", &["--"][..]),
            ("gnome-terminal", &["--"][..]),
            ("xfce4-terminal", &["-x"][..]),
            ("kitty", &[][..]),
            ("alacritty", &["-e"][..]),
            ("foot", &[][..]),
            ("wezterm", &["start", "--"][..]),
            ("tilix", &["-e"][..]),
            ("xterm", &["-e"][..]),
        ]
        .into_iter()
        .find(|(t, _)| find_in_path(t))?;
        Some(std::iter::once(term).chain(flag.iter().copied()).map(String::from).chain(argv).collect())
    }

    /// A real file for an entry: entries inside an archive are unpacked to a temporary folder.
    pub(crate) fn materialize(&mut self, path: &Path) -> Option<PathBuf> {
        let pane = &self.panes[self.active];
        let Some(a) = &pane.archive else { return Some(path.to_path_buf()) };
        let inner = pane.inner_path(path)?;
        let file = a.file.clone();
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        let out = match temp_area() {
            Ok(t) => t.join(n.to_string()).join(name),
            Err(e) => {
                self.set_status(format!("Couldn't unpack it: {e}"), true);
                return None;
            }
        };
        match crate::archive::extract_one(&file, &inner, &out) {
            Ok(()) => Some(out),
            Err(e) => {
                self.set_status(format!("Couldn't unpack it: {e}"), true);
                None
            }
        }
    }

    /// Commands that would change an archive being browsed say so instead.
    /// The files the commands work on are inside an archive (never so on the stack).
    pub(crate) fn in_archive(&self) -> bool {
        !self.on_stack() && self.panes[self.active].archive.is_some()
    }

    fn refuse_in_archive(&mut self, what: &str) -> bool {
        if self.in_archive() {
            self.set_status(format!("{what} isn't possible inside an archive (F5 copies files out)"), true);
            return true;
        }
        false
    }

    /// Commands that need one real folder say so while search results are shown.
    fn refuse_in_results(&mut self, what: &str) -> bool {
        if self.panes[self.active].results.is_some() {
            self.set_status(format!("{what} isn't possible in search results; go to a folder first (Backspace)"), true);
            return true;
        }
        false
    }

    /// Ctrl+F: find files below the folder in the active pane.
    pub fn open_search(&mut self) {
        let folder = self.panes[self.active].real_dir();
        let d = crate::search_ui::SearchDialog::new(folder, &self.last_search, self.settings.show_hidden);
        self.modal = Some(Modal::Search(d));
    }

    /// Alt+F1 / Alt+F2: the drive menu for one pane.
    pub fn open_places(&mut self, ctx: &egui::Context, pane: usize) {
        let places = crate::places::gather(&self.settings.bookmarks, &self.settings.sftp_recent, &self.settings.remounts);
        let here = [self.panes[0].real_dir(), self.panes[1].real_dir()];
        self.modal = Some(Modal::Places(crate::places::PlacesMenu::new(pane, places, here, ctx)));
    }

    pub(crate) fn places_outcome(&mut self, o: crate::places::PlacesOutcome) {
        use crate::places::PlacesOutcome;
        match o {
            PlacesOutcome::Open(pane, dir) => {
                self.active = pane;
                self.navigate(pane, &dir);
            }
            PlacesOutcome::Mount(pane, place) => {
                if self.mount_job.is_some() {
                    return self.set_status("Still busy with the last drive", true);
                }
                self.set_status(format!("Mounting {}…", place.name), false);
                self.mount_job = Some(MountJob { pane, name: place.name.clone(), sftp: None, rx: crate::places::mount(&place) });
            }
            PlacesOutcome::Sftp(pane, target) => self.connect_sftp(pane, target, None),
            PlacesOutcome::AskSftp(pane) => {
                let last = self.settings.sftp_recent.first().cloned().unwrap_or_default();
                let n = last.chars().count();
                self.ask("Connect to SFTP".into(), "user@host:/folder, a Host from ~/.ssh/config, or sftp://host:port/folder", last, InputPurpose::Sftp { pane }, Some(n));
            }
            PlacesOutcome::ForgetMount(point) => {
                self.settings.remounts.retain(|r| r.point != point);
                self.dirty = true;
            }
            PlacesOutcome::Forget(target) => {
                self.settings.sftp_recent.retain(|t| *t != target);
                self.dirty = true;
            }
            PlacesOutcome::Unmount(place) => {
                if self.mount_job.is_some() {
                    return self.set_status("Still busy with the last drive", true);
                }
                // A pane showing the drive would keep it busy.
                if let Some(point) = &place.path {
                    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
                    for i in 0..2 {
                        if self.panes[i].real_dir().starts_with(point) {
                            self.navigate(i, &home);
                        }
                    }
                }
                self.set_status(format!("Unmounting {}…", place.name), false);
                self.mount_job = Some(MountJob { pane: self.active, name: place.name.clone(), sftp: None, rx: crate::places::unmount(&place) });
            }
            PlacesOutcome::Close => {}
        }
    }

    /// Mounts an SFTP server with sshfs; asks for a password if its keys aren't enough.
    pub(crate) fn connect_sftp(&mut self, pane: usize, target: String, password: Option<String>) {
        if self.mount_job.is_some() {
            return self.set_status("Still busy with the last drive", true);
        }
        let name = match crate::places::Target::parse(&target) {
            Ok(t) => t.label,
            Err(e) => return self.set_status(e, true),
        };
        self.set_status(format!("Connecting to {name}…"), false);
        let rx = crate::places::sftp_mount(&target, password);
        self.mount_job = Some(MountJob { pane, name, sftp: Some(target), rx });
    }

    pub(crate) fn mount_done(&mut self, job: MountJob, r: Result<crate::places::Done, crate::places::MountError>) {
        use crate::places::{Done, MountError};
        match r {
            Ok(Done::Mounted(dir)) => {
                self.settings.remounts.retain(|r| r.point != dir);
                if let Some(t) = job.sftp {
                    // Most recent first; a handful is plenty.
                    self.settings.sftp_recent.retain(|x| *x != t);
                    self.settings.sftp_recent.insert(0, t);
                    self.settings.sftp_recent.truncate(10);
                    self.dirty = true;
                }
                self.active = job.pane;
                self.navigate(job.pane, &dir);
            }
            Ok(Done::Unmounted(again)) => {
                if let Some(r) = again {
                    self.settings.remounts.retain(|x| x.point != r.point);
                    self.settings.remounts.insert(0, r);
                    self.settings.remounts.truncate(10);
                    self.dirty = true;
                }
                self.set_status(format!("Unmounted {}", job.name), false);
            }
            Err(MountError::NeedsPassword) => {
                let Some(target) = job.sftp else { return };
                self.status = None;
                self.ask(format!("Password for {}", job.name), "No ssh key let Duopane in. The password isn't saved.", String::new(), InputPurpose::SftpPassword { pane: job.pane, target }, None);
            }
            Err(MountError::Failed(e)) => self.set_status(format!("Couldn't open {}: {e}", job.name), true),
        }
    }

    /// Shows `path` in its folder, with the cursor on it.
    pub(crate) fn jump_to(&mut self, path: &Path) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return };
        let name = name.to_string_lossy().into_owned();
        self.navigate(self.active, dir);
        self.panes[self.active].set_cursor_to_name(&name);
    }

    fn refuse_other_in_archive(&mut self) -> bool {
        if self.panes[self.dest_pane()].archive.is_some() {
            let which = if self.on_stack() { "That pane" } else { "The other pane" };
            self.set_status(format!("{which} is inside an archive; files can't be put into an archive that way yet"), true);
            return true;
        }
        false
    }

    /// F3: opens the lister on the file under the cursor.
    pub fn view_file(&mut self) {
        let Some(e) = self.cursor_item() else { return };
        // A folder shows as a tree of everything below it.
        if e.is_dir && self.in_archive() {
            return self.set_status("That's a folder inside the archive; Enter opens it", true);
        }
        crate::lister_load::set_tree_hidden(self.settings.show_hidden);
        let Some(path) = self.materialize(&e.path) else { return };
        self.lister = Some(crate::lister::Lister::open(&path, crate::lister_load::Mode::Auto, self.settings.lister_wrap, self.settings.lister_plugins.clone()));
    }

    /// Lister N / P: view the next or previous file in the folder.
    pub(crate) fn lister_step(&mut self, dir: isize) {
        let mode = self.lister.as_ref().map(|l| l.mode).unwrap_or(crate::lister_load::Mode::Auto);
        let mode = if mode == crate::lister_load::Mode::Hex { mode } else { crate::lister_load::Mode::Auto };
        if self.on_stack() {
            // The next (or previous) file on the stack, skipping folders.
            let stack = &self.settings.stack;
            let mut i = self.stack_view.cursor as isize;
            let path = loop {
                i += dir;
                if i < 0 || i >= stack.len() as isize {
                    return;
                }
                let p = &stack[i as usize];
                if self.stack_view.known(p).flatten().is_some_and(|e| !e.is_dir) {
                    break p.clone();
                }
            };
            self.stack_view.cursor = i as usize;
            self.lister = Some(crate::lister::Lister::open(&path, mode, self.settings.lister_wrap, self.settings.lister_plugins.clone()));
            return;
        }
        let pane = &mut self.panes[self.active];
        let n = pane.entries.len() as isize;
        let mut i = pane.cursor as isize;
        loop {
            i += dir;
            if i < 0 || i >= n {
                return;
            }
            if !pane.entries[i as usize].is_dir {
                break;
            }
        }
        pane.set_cursor(i as usize);
        let virt = pane.entries[i as usize].path.clone();
        let Some(path) = self.materialize(&virt) else { return };
        self.lister = Some(crate::lister::Lister::open(&path, mode, self.settings.lister_wrap, self.settings.lister_plugins.clone()));
    }

    /// Alt+F5: pack the selection into an archive, Double Commander style.
    pub fn ask_pack(&mut self) {
        if self.refuse_in_archive("Packing") || self.refuse_other_in_archive() {
            return;
        }
        let sources = self.picked();
        if sources.is_empty() {
            return self.set_status("Nothing to pack here", true);
        }
        let name = if sources.len() == 1 {
            let s = &sources[0];
            if s.is_dir() {
                s.file_name().map(|n| n.to_string_lossy().into_owned())
            } else {
                s.file_stem().map(|n| n.to_string_lossy().into_owned())
            }
        } else if self.on_stack() {
            Some("stack".to_string())
        } else {
            self.panes[self.active].path.file_name().map(|n| n.to_string_lossy().into_owned())
        }
        .unwrap_or_else(|| "archive".into());
        let dir = self.panes[self.dest_pane()].path.clone();
        let format = crate::archive::Format::Zip;
        let path = tilde(&dir.join(format!("{name}.{}", format.ext())));
        self.modal = Some(Modal::Pack { path, format, level: 6, separate: false, delete_after: false, sources, first: true });
    }

    /// Turns the Pack dialog's choices into background jobs.
    pub(crate) fn submit_pack(&mut self, path: &str, format: crate::archive::Format, level: u32, separate: bool, delete_after: bool, sources: Vec<PathBuf>) {
        let archive = self.resolve_input_path(path);
        let Some(dir) = archive.parent().map(Path::to_path_buf) else { return };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.set_status(format!("Couldn't create {}: {e}", dir.display()), true);
        }
        if separate && sources.len() > 1 {
            for s in sources {
                let name = s.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let archive = dir.join(format!("{name}.{}", format.ext()));
                self.start_job(JobSpec::Pack { sources: vec![s], archive, format, level, delete_after });
            }
        } else {
            self.start_job(JobSpec::Pack { sources, archive, format, level, delete_after });
        }
    }

    /// Alt+F9: unpack the selected archives.
    pub fn ask_unpack(&mut self) {
        if self.refuse_in_archive("Unpacking a nested archive") || self.refuse_other_in_archive() {
            return;
        }
        let archives: Vec<PathBuf> = self.picked().into_iter().filter(|p| crate::archive::is_archive(p)).collect();
        if archives.is_empty() {
            return self.set_status("Select a .zip, .tar, .tar.gz or .tgz file to unpack", true);
        }
        let dest = tilde(&self.panes[self.dest_pane()].path);
        self.modal = Some(Modal::Unpack { archives, dest, into_folder: true, first: true });
    }

    pub(crate) fn submit_unpack(&mut self, archives: Vec<PathBuf>, dest: &str, into_folder: bool) {
        let dest = self.resolve_input_path(dest);
        if let Err(e) = std::fs::create_dir_all(&dest) {
            return self.set_status(format!("Couldn't create {}: {e}", dest.display()), true);
        }
        self.start_job(JobSpec::Unpack { archives, dest, into_folder, policy: ConflictPolicy::Ask });
    }

    /// Ctrl+M: rename many files at once with patterns.
    pub fn ask_batch_rename(&mut self) {
        if self.refuse_in_archive("Renaming") || self.refuse_in_results("Batch rename") {
            return;
        }
        let pane = &self.panes[self.active];
        let chosen: Vec<&crate::pane::Entry> = if pane.selected.is_empty() {
            pane.entries.iter().filter(|e| !e.is_up()).collect()
        } else {
            pane.entries.iter().filter(|e| pane.selected.contains(&e.name)).collect()
        };
        if chosen.is_empty() {
            return self.set_status("Nothing to rename here", true);
        }
        let items: Vec<crate::rename::Item> =
            chosen.iter().map(|e| crate::rename::Item { name: e.name.clone(), is_dir: e.is_dir, modified: e.modified }).collect();
        let dir = pane.path.clone();
        let taken: std::collections::HashSet<&str> = items.iter().map(|i| i.name.as_str()).collect();
        // Every other name in the folder, hidden ones too, so clashes are caught.
        let others = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !taken.contains(n.as_str())).collect())
            .unwrap_or_default();
        let parent = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let rules = crate::rename::Rules { parent, ..Default::default() };
        self.modal = Some(Modal::BatchRename { dir, items, others, rules, first: true });
    }

    /// Renames in two steps (to temporary names, then to the new ones) so names can be swapped.
    /// Returns the (new, old) pairs that succeeded, and any errors.
    fn rename_pairs(dir: &Path, pairs: &[(String, String)]) -> (Vec<(String, String)>, Vec<String>) {
        let mut errors = Vec::new();
        let mut temps = Vec::new();
        for (i, (old, _)) in pairs.iter().enumerate() {
            let tmp = format!(".duopane-rename-{}-{i}", std::process::id());
            match std::fs::rename(dir.join(old), dir.join(&tmp)) {
                Ok(()) => temps.push(Some(tmp)),
                Err(e) => {
                    errors.push(format!("{old}: {e}"));
                    temps.push(None);
                }
            }
        }
        let mut done = Vec::new();
        for ((old, new), tmp) in pairs.iter().zip(temps) {
            let Some(tmp) = tmp else { continue };
            match std::fs::rename(dir.join(&tmp), dir.join(new)) {
                Ok(()) => done.push((new.clone(), old.clone())),
                Err(e) => {
                    errors.push(format!("{old} → {new}: {e}"));
                    let _ = std::fs::rename(dir.join(&tmp), dir.join(old)); // put it back
                }
            }
        }
        (done, errors)
    }

    pub(crate) fn apply_batch_rename(&mut self, dir: PathBuf, olds: Vec<String>, news: Vec<String>) {
        let pairs: Vec<(String, String)> = olds.into_iter().zip(news).filter(|(o, n)| o != n).collect();
        if pairs.is_empty() {
            return self.set_status("Nothing changes with these settings", true);
        }
        let (done, errors) = Self::rename_pairs(&dir, &pairs);
        let n = done.len();
        self.last_batch = Some((dir, done));
        self.reload_all();
        if errors.is_empty() {
        } else {
            self.set_status(format!("Renamed {n}, {} failed", errors.len()), true);
            self.modal = Some(Modal::Message { title: "Some items couldn't be renamed".into(), text: errors.join("\n") });
        }
    }

    pub fn undo_batch_rename(&mut self) {
        let Some((dir, done)) = self.last_batch.take() else {
            return self.set_status("No batch rename to undo", true);
        };
        let (_, errors) = Self::rename_pairs(&dir, &done);
        self.reload_all();
        if !errors.is_empty() {
            self.modal = Some(Modal::Message { title: "Some names couldn't be put back".into(), text: errors.join("\n") });
        }
    }

    /// F12: colour scheme and fonts.
    pub fn open_appearance(&mut self) {
        self.modal = Some(Modal::Appearance { ui_filter: String::new(), mono_filter: String::new() });
    }

    /// Goes down into the folder under the cursor (files are left alone).
    pub fn enter_folder(&mut self) {
        let Some(e) = self.panes[self.active].cursor_entry().cloned() else { return };
        let into_archive = !e.is_dir && self.panes[self.active].archive.is_none() && crate::archive::is_archive(&e.path);
        if e.is_dir || into_archive {
            self.navigate(self.active, &e.path);
        }
    }

    /// Double Commander style "select/deselect by mask" (numpad + / -).
    pub fn ask_mask(&mut self, select: bool) {
        let last = self.settings.mask_history.first().cloned().unwrap_or_else(|| "*".into());
        let n = last.chars().count();
        let title = if select { "Select by mask" } else { "Deselect by mask" };
        let mut hint = "e.g. *.jpg;*.png   ·   *.txt | readme*   ·   end with / for folders".to_string();
        if self.settings.mask_history.len() > 1 {
            hint = format!("Recent: {}", self.settings.mask_history.iter().take(5).cloned().collect::<Vec<_>>().join("   "));
        }
        self.modal = Some(Modal::Input {
            title: title.into(),
            hint,
            text: last,
            purpose: InputPurpose::Mask { select },
            select_chars: Some(n),
            first: true,
        });
    }

    fn apply_mask(&mut self, select: bool, mask: &str) {
        self.settings.mask_history.retain(|m| m != mask);
        self.settings.mask_history.insert(0, mask.to_string());
        self.settings.mask_history.truncate(20);
        self.dirty = true;
        let n = self.panes[self.active].select_by_mask(mask, select);
        if n == 0 {
            self.set_status(format!("Nothing matches “{mask}” (end a mask with / to match folders)"), true);
        }
    }

    pub fn back(&mut self) {
        let r = self.panes[self.active].go_back();
        self.after_move(self.active, r);
    }

    pub fn forward(&mut self) {
        let r = self.panes[self.active].go_forward();
        self.after_move(self.active, r);
    }

    pub fn switch_pane(&mut self) {
        self.active = self.other();
        self.quick.clear();
    }

    pub fn go_home(&mut self) {
        if let Some(h) = dirs::home_dir() {
            self.navigate(self.active, &h);
        }
    }

    pub fn open_in(&mut self, side: usize) {
        let pane = &self.panes[self.active];
        let target = match pane.cursor_entry() {
            Some(e) if e.is_dir => e.path.clone(),
            _ => pane.path.clone(),
        };
        self.navigate(side, &target);
    }

    pub fn mirror(&mut self) {
        let p = self.panes[self.active].path.clone();
        let other = self.other();
        self.navigate(other, &p);
    }

    pub fn swap_panes(&mut self) {
        self.panes.swap(0, 1);
        self.active = self.other();
        self.dirty = true;
    }

    pub fn edit_path(&mut self) {
        let pane = &mut self.panes[self.active];
        pane.editing_path = Some(tilde(&pane.path));
    }

    pub fn toggle_hidden(&mut self) {
        self.settings.show_hidden = !self.settings.show_hidden;
        let show = self.settings.show_hidden;
        for p in &mut self.panes {
            p.set_show_hidden(show);
        }
        self.dirty = true;
    }

    pub fn reload_all(&mut self) {
        for p in &mut self.panes {
            p.reload();
        }
    }

    pub fn sort_by(&mut self, key: SortKey) {
        self.panes[self.active].set_sort(key);
        self.dirty = true;
    }

    /// Ctrl+Shift+3–6 / F3–F6: the second sort, for entries the first one calls equal.
    pub fn then_sort_by(&mut self, key: SortKey) {
        let pane = &mut self.panes[self.active];
        if pane.sort == key {
            return;
        }
        pane.set_then_sort(key);
        self.dirty = true;
    }

    pub fn copy_paths(&mut self, ctx: &egui::Context) {
        let targets = self.picked();
        if targets.is_empty() {
            return;
        }
        let text = targets.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        ctx.copy_text(text);
    }

    /// Ctrl+C / Ctrl+X: puts the selected files on the clipboard; Ctrl+V copies
    /// them (or, after Ctrl+X, moves them) into the folder in the active pane.
    /// On Wayland they go on the system clipboard as files, so they can be pasted
    /// into other apps too; elsewhere their paths go on it as text.
    pub fn clip_files(&mut self, ctx: &egui::Context, cut: bool) {
        if self.in_archive() {
            return self.set_status("Files inside an archive can't go on the clipboard yet; copy them out first (F5)", true);
        }
        let paths = self.picked();
        if paths.is_empty() {
            return;
        }
        #[cfg(target_os = "linux")]
        let as_files = self.native_drag.as_ref().is_some_and(|nd| nd.set_clipboard(&paths, cut));
        #[cfg(not(target_os = "linux"))]
        let as_files = false;
        if !as_files {
            ctx.copy_text(paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n"));
        }
        self.clip = Some(crate::app::Clip { paths, cut });
    }

    /// Ctrl+V: pastes files from the clipboard into the folder in the active pane.
    pub fn paste_files(&mut self) {
        // The same Ctrl+V can be reported twice (by egui and by the Wayland
        // keyboard); one paste is enough.
        if self.last_paste.elapsed() < std::time::Duration::from_millis(400) {
            return;
        }
        self.last_paste = std::time::Instant::now();
        if self.refuse_in_archive("Pasting") || self.refuse_in_results("Pasting") {
            return;
        }
        let dest = self.panes[self.active].real_dir();
        // On Wayland, read the system clipboard (files copied in other apps too);
        // the answer is picked up in `poll_background`.
        #[cfg(target_os = "linux")]
        if let Some(nd) = &self.native_drag {
            self.paste_rx = Some((dest, nd.read_clipboard()));
            return;
        }
        match self.clip.clone() {
            Some(c) => self.paste_into(c, &dest),
            None => self.set_status("Nothing to paste: copy files with Ctrl+C first", true),
        }
    }

    /// Copies (or moves, when cut) clipboard files into `dest`, through the usual
    /// copy / move job (with its questions when something's already there).
    pub(crate) fn paste_into(&mut self, clip: crate::app::Clip, dest: &Path) {
        let paths: Vec<PathBuf> = clip.paths.into_iter().filter(|p| p.symlink_metadata().is_ok()).collect();
        if paths.is_empty() {
            return self.set_status("The files on the clipboard are gone", true);
        }
        if clip.cut {
            // Moved once; the clipboard no longer offers them.
            self.clip = None;
            #[cfg(target_os = "linux")]
            if let Some(nd) = &self.native_drag {
                nd.forget_cut();
            }
        }
        self.prepare_transfer(clip.cut, paths, &dest.to_string_lossy(), false);
    }

    pub fn add_bookmark(&mut self) {
        let p = self.panes[self.active].path.clone();
        if !self.settings.bookmarks.contains(&p) {
            self.settings.bookmarks.push(p.clone());
            self.dirty = true;
        }
    }

    pub fn remove_bookmark(&mut self) {
        let p = self.panes[self.active].path.clone();
        let before = self.settings.bookmarks.len();
        self.settings.bookmarks.retain(|b| b != &p);
        if self.settings.bookmarks.len() != before {
            self.dirty = true;
        } else {
            self.set_status("This folder isn't bookmarked", true);
        }
    }

    fn ask(&mut self, title: String, hint: &str, text: String, purpose: InputPurpose, select_chars: Option<usize>) {
        self.modal = Some(Modal::Input { title, hint: hint.into(), text, purpose, select_chars, first: true });
    }

    pub fn ask_transfer(&mut self, mv: bool) {
        if self.on_stack() {
            // From the stack: to the pane with the border.
            let paths = self.picked();
            return self.stack_action(crate::stack::StackAction::Send { to: self.active, mv, paths });
        }
        let sources = self.panes[self.active].targets();
        if sources.is_empty() {
            return self.set_status("Nothing to copy here", true);
        }
        if self.refuse_other_in_archive() {
            return;
        }
        let dest = self.panes[self.other()].path.clone();
        self.ask_transfer_to(mv, sources, &dest, Some(self.active), false);
    }

    /// The Copy / Move dialog for `sources`, with `dest` filled in. `from` is the
    /// pane they come from, if any (files dropped in from another app have none).
    fn ask_transfer_to(&mut self, mv: bool, sources: Vec<PathBuf>, dest: &Path, from: Option<usize>, from_stack: bool) {
        let dest = tilde(dest);
        if let Some((pane, a)) = from.map(|i| &self.panes[i]).and_then(|p| p.archive.as_ref().map(|a| (p, a))) {
            if mv {
                return self.set_status("Moving out of an archive isn't possible; F5 copies files out", true);
            }
            let items: Vec<String> = sources.iter().filter_map(|p| pane.inner_path(p)).collect();
            let base = pane.inner_path(&pane.path).unwrap_or_default();
            let purpose = InputPurpose::Extract { archive: a.file.clone(), items, base };
            let title = format!("Copy {} out of the archive to:", describe(&sources));
            return self.ask(title, "Folder", dest, purpose, None);
        }
        let title = format!("{} {} to:", if mv { "Move" } else { "Copy" }, describe(&sources));
        self.ask(title, "Folder (or a new name)", dest, InputPurpose::Transfer { mv, sources, from_stack }, None);
    }

    /// Files dragged with the mouse and dropped on a pane or a folder in one.
    /// Opens the Copy (or, with Shift, Move) dialog with the destination filled in,
    /// so a stray drop never moves anything by itself.
    pub(crate) fn drop_files(&mut self, from: Option<usize>, to: usize, sources: Vec<PathBuf>, dest: PathBuf, mv: bool) {
        if self.panes[to].archive.is_some() {
            return self.set_status("Files can't be put into an archive that way yet", true);
        }
        // Dropped on itself, or into the folder it's already in: nothing to do.
        let sources: Vec<PathBuf> = sources.into_iter().filter(|s| *s != dest && s.parent() != Some(dest.as_path())).collect();
        if sources.is_empty() {
            return;
        }
        if let Some(i) = from {
            self.active = i;
        }
        self.ask_transfer_to(mv, sources, &dest, from, false);
    }

    /// Hands the selected files to a small drag-source window (ripdrag or
    /// dragon-drop), from which they can be dragged into other apps.
    pub fn drag_out(&mut self) {
        let targets = self.picked();
        if targets.is_empty() {
            return self.set_status("Nothing to drag here", true);
        }
        // Not plain "dragon": on KDE that's the Dragon Player video player.
        let Some(helper) = ["ripdrag", "dragon-drop", "xdragon"].into_iter().find(|c| find_in_path(c)) else {
            return self.set_status("Dragging to other apps needs ripdrag or dragon-drop installed", true);
        };
        let mut paths = Vec::new();
        for t in &targets {
            match self.materialize(t) {
                Some(p) => paths.push(p),
                None => return,
            }
        }
        // Both helpers take these: close after one drag, and (for several files)
        // drag them all at once instead of one by one.
        let mut cmd = Process::new(helper);
        cmd.arg("--and-exit");
        if paths.len() > 1 {
            cmd.arg("--all-compact");
        }
        match cmd.args(&paths).spawn_reaped() {
            Ok(()) => {},
            Err(e) => self.set_status(format!("Couldn't start {helper}: {e}"), true),
        }
    }

    pub fn ask_rename(&mut self) {
        if self.refuse_in_archive("Renaming") {
            return;
        }
        // With several files selected, rename them all at once (not in search
        // results: batch rename works on one folder).
        let pane = &self.panes[self.active];
        if !self.on_stack() && pane.selected.len() > 1 && pane.results.is_none() {
            return self.ask_batch_rename();
        }
        let Some(e) = self.cursor_item().filter(|e| !e.is_up()) else { return };
        // The file's own name (in search results, entries are named by their path).
        let name = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(e.name);
        // Pre-select the name without its extension.
        let stem_len = match name.rfind('.') {
            Some(i) if i > 0 && !e.is_dir => name[..i].chars().count(),
            _ => name.chars().count(),
        };
        let title = format!("Rename “{name}” to:");
        self.ask(title, "New name", name, InputPurpose::Rename(e.path), Some(stem_len));
    }

    pub fn ask_new_folder(&mut self) {
        if self.refuse_in_archive("Making a folder") || self.refuse_in_results("Making a folder") {
            return;
        }
        self.ask("New folder".into(), "Folder name (a/b/c makes nested folders)", String::new(), InputPurpose::NewFolder, None);
    }

    pub fn ask_new_file(&mut self) {
        if self.refuse_in_archive("Making a file") || self.refuse_in_results("Making a file") {
            return;
        }
        self.ask("New file".into(), "File name", String::new(), InputPurpose::NewFile, None);
    }

    pub fn edit_cursor(&mut self) {
        let Some(e) = self.cursor_item() else { return };
        if e.is_dir {
            return self.set_status("That's a folder; press Enter to open it", true);
        }
        let in_archive = self.in_archive();
        let Some(path) = self.materialize(&e.path) else { return };
        self.edit_in_editor(&path);
        if in_archive {
            self.set_status("Editing a temporary copy: changes won't go back into the archive", false);
        }
    }

    pub(crate) fn edit_in_editor(&mut self, path: &Path) {
        let res = if self.settings.editor.trim().is_empty() {
            open::that_detached(path).map_err(|e| e.to_string())
        } else {
            Process::new("sh")
                .arg("-c")
                .arg(format!("{} \"$1\"", self.settings.editor))
                .arg("sh")
                .arg(path)
                .spawn_reaped()
                .map_err(|e| e.to_string())
        };
        if let Err(e) = res {
            self.set_status(format!("Couldn't open an editor: {e}"), true);
        }
    }

    pub fn ask_delete(&mut self, permanent: bool) {
        if self.refuse_in_archive("Deleting") {
            return;
        }
        let paths = self.picked();
        if paths.is_empty() {
            return;
        }
        let what = describe(&paths);
        let spec = JobSpec::Delete { paths, permanent };
        if !permanent && !self.settings.confirm_delete {
            return self.start_job(spec);
        }
        let (title, message, button) = if permanent {
            ("Delete permanently", format!("Permanently delete {what}? This can't be undone."), "Delete")
        } else {
            ("Move to trash", format!("Move {what} to the trash?"), "Move to trash")
        };
        self.modal = Some(Modal::Confirm {
            title: title.into(),
            message,
            buttons: vec![(button.into(), Some(crate::app::Confirmed::Job(spec))), ("Cancel".into(), None)],
            focus: 0,
        });
    }

    pub fn open_terminal(&mut self) {
        let dir = self.panes[self.active].real_dir();
        let configured = if !self.settings.terminal.trim().is_empty() {
            Some(self.settings.terminal.clone())
        } else {
            std::env::var("TERMINAL").ok().filter(|t| !t.is_empty())
        };
        let res = match configured {
            Some(t) => Process::new("sh").arg("-c").arg(t).current_dir(&dir).spawn_reaped(),
            None => {
                let candidates = [
                    "x-terminal-emulator", "kgx", "gnome-terminal", "konsole", "xfce4-terminal", "kitty",
                    "alacritty", "foot", "wezterm", "tilix", "xterm",
                ];
                match candidates.iter().find(|c| find_in_path(c)) {
                    Some(t) => Process::new(t).current_dir(&dir).spawn_reaped(),
                    None => return self.set_status("No terminal found; set \"terminal\" in the settings file", true),
                }
            }
        };
        if let Err(e) = res {
            self.set_status(format!("Couldn't open a terminal: {e}"), true);
        }
    }

    pub fn calculate_size(&mut self) {
        let targets = self.picked();
        if targets.is_empty() {
            return;
        }
        if self.in_archive() {
            let pane = &self.panes[self.active];
            let bytes: u64 = targets.iter().map(|p| pane.archive_size(p)).sum();
            return self.set_status(format!("{}: {} (unpacked)", plural(targets.len(), "item"), crate::pane::human_size(bytes)), false);
        }
        let (tx, rx) = mpsc::channel();
        let n = targets.len();
        std::thread::spawn(move || {
            let total = targets.iter().map(|p| fs_ops::tree_size(p)).sum();
            let _ = tx.send((n, total));
        });
        self.size_rx = Some(rx);
        self.set_status("Calculating size…", false);
    }

    pub fn cancel_job(&mut self) {
        self.queue.clear();
        if let Some(j) = &self.job {
            j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn edit_settings(&mut self) {
        self.store_pane_state();
        if let Err(e) = self.settings.save() {
            return self.set_status(format!("Couldn't write settings: {e}"), true);
        }
        self.edit_in_editor(&Settings::file());
    }

    pub fn reload_settings(&mut self, ctx: &egui::Context) {
        let (mut s, err) = Settings::load();
        // Keep the folders currently open.
        s.left_path = Some(self.panes[0].path.clone());
        s.right_path = Some(self.panes[1].path.clone());
        self.settings = s;
        theme::apply(ctx, &self.settings);
        crate::pane::set_collation(self.settings.sort_language);
        let show = self.settings.show_hidden;
        for p in &mut self.panes {
            p.show_dotdot = self.settings.show_dotdot;
            p.set_show_hidden(show);
        }
        self.rebuild_bindings();
        if let Some(e) = err {
            self.set_status(e, true);
        }
    }

    pub fn change_font(&mut self, ctx: &egui::Context, delta: f32) {
        self.settings.font_size = (self.settings.font_size + delta).clamp(9.0, 32.0);
        theme::apply(ctx, &self.settings);
        self.dirty = true;
    }

    pub fn show_help(&mut self) {
        let mut lines = vec![
            "Up / Down / PgUp / PgDn / Home / End — move (hold Shift to select)".to_string(),
            "Space or Insert — select / deselect the item under the cursor".to_string(),
            "Numpad + / - / * — select by mask, deselect by mask, invert selection".to_string(),
            "Mouse back / forward buttons — go up / down the folder tree".to_string(),
            "Type letters — jump to a matching name".to_string(),
            "Escape — clear the selection".to_string(),
            "Ctrl+click / Shift+click — select with the mouse; right-click for a menu".to_string(),
            String::new(),
        ];
        for (i, c) in self.commands.iter().enumerate() {
            let keys = self.keys_for(CmdRef::Builtin(i));
            if !keys.is_empty() {
                lines.push(format!("{keys} — {}", c.title));
            }
        }
        lines.push(String::new());
        lines.push(format!("Settings file: {}", Settings::file().display()));
        self.modal = Some(Modal::Message { title: "Keyboard shortcuts".into(), text: lines.join("\n") });
    }

    pub fn open_palette(&mut self) {
        let mut base = Vec::new();
        for (i, c) in self.commands.iter().enumerate() {
            let r = CmdRef::Builtin(i);
            base.push(PaletteItem { title: c.title.to_string(), detail: self.keys_for(r), target: PaletteTarget::Cmd(r), boost: 0 });
        }
        for (i, c) in self.settings.custom_commands.iter().enumerate() {
            let r = CmdRef::Custom(i);
            base.push(PaletteItem { title: c.title.clone(), detail: self.keys_for(r), target: PaletteTarget::Cmd(r), boost: 0 });
        }
        self.modal = Some(Modal::Palette(PaletteState::new(PaletteKind::Commands, base)));
    }

    /// "Go to": bookmarks and often-visited folders rank first.
    pub fn open_goto(&mut self, _ctx: &egui::Context) {
        fn add(found: &mut BTreeMap<PathBuf, i32>, p: PathBuf, boost: i32) {
            let e = found.entry(p).or_insert(i32::MIN);
            *e = (*e).max(boost);
        }
        let mut found: BTreeMap<PathBuf, i32> = BTreeMap::new();
        for b in &self.settings.bookmarks {
            add(&mut found, b.clone(), 400);
        }
        for (k, v) in &self.settings.visited {
            add(&mut found, PathBuf::from(k), 40 + ((*v as f32).ln() * 60.0) as i32);
        }
        let home = dirs::home_dir();
        if let Some(h) = &home {
            add(&mut found, h.clone(), 60);
        }
        for pane in &self.panes {
            for a in pane.path.ancestors().skip(1) {
                add(&mut found, a.to_path_buf(), 25);
            }
        }
        add(&mut found, PathBuf::from("/"), 0);
        // Subfolders, and leaving out folders that are gone, need the disk, which
        // can hang on a network folder whose server has gone: then the list is
        // shown without them.
        let panes: Vec<(PathBuf, bool)> = self.panes.iter().map(|p| (p.path.clone(), p.show_hidden)).collect();
        let quick = found.clone();
        let looked = crate::pane::within(std::time::Duration::from_millis(400), move || {
            if let Some(h) = &home {
                for d in subdirs(h, false) {
                    add(&mut found, d, 20);
                }
            }
            for (path, hidden) in &panes {
                for d in subdirs(path, *hidden) {
                    add(&mut found, d, 30);
                }
            }
            for root in ["/media", "/mnt", "/run/media"] {
                for d in subdirs(Path::new(root), false) {
                    add(&mut found, d.clone(), 15);
                    for dd in subdirs(&d, false) {
                        add(&mut found, dd, 15);
                    }
                }
            }
            found.into_iter().filter(|(p, _)| p.is_dir()).collect::<Vec<_>>()
        });
        let found = looked.unwrap_or_else(|| quick.into_iter().collect());
        let base: Vec<PaletteItem> = found.into_iter().map(|(p, boost)| dir_item(p, boost)).collect();
        self.modal = Some(Modal::Palette(PaletteState::new(PaletteKind::GoTo, base)));
    }

    /// Runs one of the user's own commands from the settings file.
    pub(crate) fn run_custom(&mut self, i: usize) {
        let Some(c) = self.settings.custom_commands.get(i).cloned() else { return };
        let pane = &self.panes[self.active];
        let path = self.cursor_item().map(|e| e.path.display().to_string()).unwrap_or_default();
        let dir = pane.path.display().to_string();
        let other = self.panes[self.other()].path.display().to_string();
        let selected: Vec<String> = self.picked().iter().map(|p| p.display().to_string()).collect();
        let all = selected.iter().map(|s| shell_quote(s)).collect::<Vec<_>>().join(" ");
        let cmd = fill_placeholders(&c.run, &[("{path}", shell_quote(&path)), ("{dir}", shell_quote(&dir)), ("{other}", shell_quote(&other)), ("{selected}", all)]);
        let res = Process::new("sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&pane.path)
            .env("DUOPANE_PATH", &path)
            .env("DUOPANE_DIR", &dir)
            .env("DUOPANE_OTHER", &other)
            .env("DUOPANE_SELECTED", selected.join("\n"))
            .spawn_reaped();
        match res {
            Ok(_) => {},
            Err(e) => self.set_status(format!("“{}” failed: {e}", c.title), true),
        }
    }

    /// Called when a text dialog is confirmed.
    pub(crate) fn submit_input(&mut self, purpose: InputPurpose, text: String) {
        // A password is taken as typed, spaces and all.
        if let InputPurpose::SftpPassword { pane, target } = purpose {
            return self.connect_sftp(pane, target, Some(text));
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        match purpose {
            InputPurpose::Sftp { pane } => self.connect_sftp(pane, text, None),
            InputPurpose::SftpPassword { .. } => {}
            InputPurpose::Rename(from) => self.rename(&from, &text),
            InputPurpose::NewFolder => {
                let path = self.resolve_input_path(&text);
                match std::fs::create_dir_all(&path) {
                    Ok(()) => {
                        let first = text.split('/').next().unwrap_or(&text).to_string();
                        let p = &mut self.panes[self.active];
                        p.reload();
                        p.set_cursor_to_name(&first);
                    }
                    Err(e) => self.set_status(format!("Couldn't create the folder: {e}"), true),
                }
            }
            InputPurpose::NewFile => {
                let path = self.resolve_input_path(&text);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::File::create_new(&path) {
                    Ok(_) => {
                        let p = &mut self.panes[self.active];
                        p.reload();
                        if let Some(n) = path.file_name() {
                            p.set_cursor_to_name(&n.to_string_lossy());
                        }
                        self.edit_in_editor(&path);
                    }
                    Err(e) => self.set_status(format!("Couldn't create the file: {e}"), true),
                }
            }
            InputPurpose::Transfer { mv, sources, from_stack } => self.prepare_transfer(mv, sources, &text, from_stack),
            InputPurpose::Extract { archive, items, base } => {
                let dest = self.resolve_input_path(&text);
                if let Err(e) = std::fs::create_dir_all(&dest) {
                    return self.set_status(format!("Couldn't create {text}: {e}"), true);
                }
                self.start_job(JobSpec::Extract { archive, items, base, dest, policy: ConflictPolicy::Ask });
            }
            InputPurpose::Mask { select } => self.apply_mask(select, &text),
            InputPurpose::CopyInPlace(from) => self.copy_in_place(from, &text),
        }
    }

    fn rename(&mut self, from: &Path, text: &str) {
        if text.contains('/') {
            return self.set_status("A name can't contain “/”", true);
        }
        let to = from.parent().unwrap_or(Path::new("/")).join(text);
        if to == from {
            return;
        }
        // On a drive that ignores case, "Readme" -> "README" finds the file itself at
        // the new name: that's fine. On Linux it is usually a different file, which
        // the rename would silently replace.
        let only_case_changed = from.file_name().map(|n| n.to_string_lossy().to_lowercase()) == Some(text.to_lowercase())
            && crate::fs_ops::same_entry(from, &to);
        if to.symlink_metadata().is_ok() && !only_case_changed {
            return self.set_status(format!("“{text}” already exists"), true);
        }
        match std::fs::rename(from, &to) {
            Ok(()) => {
                for p in &mut self.panes {
                    p.renamed(from, &to); // keeps it in search results under its new name
                }
                if crate::stack::renamed(&mut self.settings.stack, from, &to) {
                    self.dirty = true;
                }
                let p = &mut self.panes[self.active];
                p.reload();
                let shown = p.results.as_ref().and_then(|r| to.strip_prefix(&r.root).ok()).map(|r| r.to_string_lossy().into_owned());
                p.set_cursor_to_name(shown.as_deref().unwrap_or(text));
                let other = self.other();
                self.panes[other].reload();
            }
            Err(e) => self.set_status(format!("Couldn't rename: {e}"), true),
        }
    }

    fn prepare_transfer(&mut self, mv: bool, sources: Vec<PathBuf>, text: &str, from_stack: bool) {
        let dest = self.resolve_input_path(text);
        let into = |dest: &Path| -> Vec<(PathBuf, PathBuf)> {
            sources.iter().filter_map(|s| s.file_name().map(|n| (s.clone(), dest.join(n)))).collect()
        };
        let items = if dest.is_dir() {
            into(&dest)
        } else if sources.len() == 1 {
            match dest.parent() {
                Some(parent) if parent.is_dir() => vec![(sources[0].clone(), dest.clone())],
                _ => return self.set_status(format!("Folder doesn't exist: {text}"), true),
            }
        } else {
            if let Err(e) = std::fs::create_dir_all(&dest) {
                return self.set_status(format!("Couldn't create {text}: {e}"), true);
            }
            into(&dest)
        };
        // Copying something onto itself makes "name copy" instead.
        let items: Vec<(PathBuf, PathBuf)> = items
            .into_iter()
            .filter_map(|(s, d)| {
                if s != d {
                    Some((s, d))
                } else if mv {
                    None
                } else {
                    let u = fs_ops::unique_copy_name(&d);
                    Some((s, u))
                }
            })
            .collect();
        if items.is_empty() {
            return self.set_status("Nothing to do: it's already there", true);
        }
        if from_stack && !mv {
            self.stack_copying.extend(items.iter().map(|(s, _)| s.clone()));
        }
        // Anything already there is asked about file by file while the job runs.
        self.start_job(JobSpec::Transfer { mv, items, policy: ConflictPolicy::Ask });
    }
}

/// Starts a program that runs on its own (terminal, editor, custom command).
pub(crate) trait SpawnReaped {
    /// Like `spawn`, but waits for the program on a small thread, so it doesn't
    /// linger as a zombie process after it exits.
    fn spawn_reaped(&mut self) -> std::io::Result<()>;
}

impl SpawnReaped for Process {
    fn spawn_reaped(&mut self) -> std::io::Result<()> {
        let mut child = self.spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn find_in_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
}

/// Where files unpacked from archives (for viewing or opening) go; removed on exit.
/// A new folder only we can enter, with a name nobody can guess: in a shared
/// /tmp, another user could otherwise make it first and plant links in it.
pub fn temp_area() -> Result<PathBuf, String> {
    TEMP_AREA.get_or_init(make_temp_area).clone()
}

static TEMP_AREA: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();

/// The temporary folder, if one was made (for removing it on exit).
pub fn temp_area_made() -> Option<PathBuf> {
    TEMP_AREA.get().and_then(|r| r.as_ref().ok().cloned())
}

fn make_temp_area() -> Result<PathBuf, String> {
    use std::hash::{BuildHasher, Hasher};
    // The user's own runtime folder when there is one; it's private already.
    let root = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
    let mut err = String::new();
    for _ in 0..10 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        let dir = root.join(format!("duopane-{}-{:016x}", std::process::id(), h.finish()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        // `create`, not `create_dir_all`: a folder that's already there isn't ours.
        match b.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) => err = e.to_string(),
        }
    }
    Err(format!("couldn't make a temporary folder: {err}"))
}

impl App {
    /// Ctrl+G: the selected files (or the one under the cursor) go on the stack,
    /// or come off it when they're all on it already.
    pub fn stack_toggle(&mut self) {
        if self.on_stack() {
            let paths = self.picked();
            self.settings.stack.retain(|p| !paths.contains(p));
            self.dirty = true;
            return;
        }
        if self.refuse_in_archive("Putting files on the stack") {
            return;
        }
        let paths = self.panes[self.active].targets();
        let (added, removed) = crate::stack::toggle(&mut self.settings.stack, paths);
        if added + removed > 0 {
            self.dirty = true;
        }
    }

    /// Ctrl+Tab: the keyboard goes to the stack's frame, or back to the panes.
    pub fn stack_focus(&mut self) {
        if self.stack_view.focused {
            self.stack_view.focused = false;
        } else if self.settings.stack.is_empty() {
            self.set_status("The stack is empty: Ctrl+G puts the selected files on it", true);
        } else {
            self.stack_view.focused = true;
            self.quick.clear();
        }
    }

    /// The stack's frame under the panes.
    pub(crate) fn stack_ui(&mut self, ctx: &egui::Context) {
        // Things from the stack go to the active pane, when it's a plain folder.
        let pane = &self.panes[self.active];
        let side = match (self.settings.stacked_panes, self.active) {
            (false, 0) => "left pane",
            (false, _) => "right pane",
            (true, 0) => "top pane",
            (true, _) => "bottom pane",
        };
        let dest = (pane.archive.is_none() && pane.results.is_none()).then(|| crate::stack::Dest { path: pane.path.clone(), side });
        let (changed, action) = self.stack_view.ui(ctx, &mut self.settings.stack, self.settings.font_size, dest);
        if changed {
            self.dirty = true;
        }
        if let Some(a) = action {
            self.stack_action(a);
        }
    }

    /// What a key or click in the stack's frame asked for.
    pub(crate) fn stack_action(&mut self, action: crate::stack::StackAction) {
        use crate::stack::StackAction;
        match action {
            StackAction::Used => {}
            StackAction::Changed => self.dirty = true,
            StackAction::Leave => self.stack_view.focused = false,
            StackAction::Clear => self.ask_clear_stack(),
            StackAction::Focus => self.stack_view.focused = true,
            StackAction::Open => {
                self.stack_view.focused = true;
                self.open_item();
            }
            StackAction::Menu(at) => {
                self.stack_view.focused = true;
                self.open_row_menu_at(at);
            }
            StackAction::Reveal(p) => {
                if let Some((dir, name)) = crate::stack::reveal_target(&p) {
                    self.stack_view.focused = false;
                    let i = self.active;
                    self.panes[i].want_cursor = Some(name);
                    self.navigate(i, &dir);
                }
            }
            StackAction::Send { to, mv, paths } => {
                if paths.is_empty() {
                    return;
                }
                if self.panes[to].archive.is_some() {
                    return self.set_status("That pane is inside an archive; files can't be put into an archive that way yet", true);
                }
                if self.panes[to].results.is_some() {
                    return self.set_status("That pane shows search results; open a folder there first", true);
                }
                let dest = self.panes[to].path.clone();
                // Already in that folder: nothing to copy or move.
                let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.parent() != Some(dest.as_path())).collect();
                if paths.is_empty() {
                    return self.set_status("It's already in that folder", true);
                }
                self.ask_transfer_to(mv, paths, &dest, None, true);
            }
        }
    }

    /// Ctrl+Shift+G: empties the stack, after a quick Enter (Esc keeps it).
    pub fn ask_clear_stack(&mut self) {
        let n = self.settings.stack.len();
        if n == 0 {
            return self.set_status("The stack is already empty", false);
        }
        self.modal = Some(Modal::Confirm {
            title: "Clear the stack".into(),
            message: format!("Take {} off the stack? The files stay where they are.", plural(n, "item")),
            buttons: vec![("Clear".into(), Some(crate::app::Confirmed::ClearStack)), ("Cancel".into(), None)],
            focus: 0,
        });
    }

    /// "Show it in the pane" (the stack's menu): its folder in the active pane,
    /// with the cursor on it.
    pub fn stack_reveal(&mut self) {
        if let Some(p) = self.settings.stack.get(self.stack_view.cursor).cloned().filter(|_| self.on_stack()) {
            self.stack_action(crate::stack::StackAction::Reveal(p));
        }
    }

    /// A finished copy: what came from the stack and was copied whole has been
    /// put down, so it comes off. Anything skipped or failed stays on.
    pub(crate) fn stack_put_down(&mut self, sources: &[PathBuf], copied: &[PathBuf]) {
        let before = self.settings.stack.len();
        let copying = &self.stack_copying;
        self.settings.stack.retain(|p| !(copied.contains(p) && copying.contains(p)));
        for s in sources {
            self.stack_copying.remove(s);
        }
        if self.settings.stack.len() != before {
            self.dirty = true;
        }
    }

    /// A finished move or delete: what's gone from where it was comes off the stack too.
    pub(crate) fn stack_forget_moved(&mut self, moved: Vec<PathBuf>) {
        let on: Vec<PathBuf> = moved.into_iter().filter(|p| self.settings.stack.contains(p)).collect();
        if on.is_empty() {
            return;
        }
        // Asked off the UI thread: the old place may be a slow network folder.
        let gone = crate::pane::within(std::time::Duration::from_millis(500), move || {
            on.into_iter().filter(|p| std::fs::symlink_metadata(p).is_err()).collect::<Vec<_>>()
        });
        if let Some(gone) = gone.filter(|g| !g.is_empty()) {
            self.settings.stack.retain(|p| !gone.contains(p));
            self.dirty = true;
        }
    }

    /// Ctrl+S: typing narrows the list to matching names. Pressed again, typing
    /// goes back to jumping (the filter stays until Esc).
    pub fn toggle_filter(&mut self) {
        let pane = &mut self.panes[self.active];
        pane.filtering = !pane.filtering;
        self.quick.clear();
    }

    /// Shift+F2: selects what differs between the two panes (see `pane::compare`).
    pub fn compare_panes(&mut self) {
        let (l, r) = crate::pane::compare(&self.panes[0], &self.panes[1]);
        if l.is_empty() && r.is_empty() {
            self.set_status("Both folders hold the same files", false);
        }
        self.panes[0].selected = l;
        self.panes[1].selected = r;
    }

    /// Alt+Plus / Alt+Minus.
    pub fn select_same_type(&mut self, on: bool) {
        self.panes[self.active].select_same_type(on);
    }

    /// Shift+F5: a copy next to the original, under a name you choose.
    pub fn ask_copy_in_place(&mut self) {
        if self.refuse_in_archive("Copying in place") {
            return;
        }
        let Some(e) = self.cursor_item().filter(|e| !e.is_up()) else { return };
        let suggested = fs_ops::unique_copy_name(&e.path);
        let name = suggested.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let original = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem_len = match name.rfind('.') {
            Some(i) if i > 0 && !e.is_dir => name[..i].chars().count(),
            _ => name.chars().count(),
        };
        self.ask(format!("Copy “{original}” as:"), "New name, in the same folder", name, InputPurpose::CopyInPlace(e.path), Some(stem_len));
    }

    pub(crate) fn copy_in_place(&mut self, from: PathBuf, text: &str) {
        if text.contains('/') {
            return self.set_status("A name can't contain “/”", true);
        }
        let to = from.parent().unwrap_or(Path::new("/")).join(text);
        if to == from {
            return self.set_status("Nothing to do: it's already there", true);
        }
        let p = &mut self.panes[self.active];
        p.want_cursor = Some(text.to_string());
        self.start_job(JobSpec::Transfer { mv: false, items: vec![(from, to)], policy: ConflictPolicy::Ask });
    }

    /// Ctrl+Z: puts back what the last "Move to trash" took. Nothing is
    /// overwritten: a file whose place is taken again stays in the trash.
    pub fn restore_trashed(&mut self) {
        if self.last_trashed.is_empty() {
            return self.set_status("Nothing to put back: nothing was moved to the trash since Duopane started", true);
        }
        match restore_from_trash(&self.last_trashed) {
            Ok(left) => {
                self.last_trashed.clear();
                self.reload_all();
                if !left.is_empty() {
                    self.modal = Some(Modal::Message { title: "Some items stay in the trash".into(), text: left.join("\n") });
                }
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// "Checksum": SHA-256 of the file under the cursor, or a check of a SHA256SUMS list.
    pub fn checksum(&mut self, ctx: &egui::Context) {
        let Some(e) = self.cursor_item().filter(|e| !e.is_up()) else { return };
        if e.is_dir {
            return self.set_status("Checksums are for files, not folders", true);
        }
        if self.refuse_in_archive("Taking a checksum") {
            return;
        }
        if let Some((_, cancel)) = self.checksum.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let (tx, rx) = mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::checksum::check(&e.path, &stop));
            ctx.request_repaint();
        });
        self.checksum = Some((rx, cancel));
        self.set_status("Calculating checksum…", false);
    }

    /// Alt+Enter.
    pub fn properties(&mut self) {
        if self.refuse_in_archive("Properties") {
            return;
        }
        let Some(e) = self.cursor_item() else { return };
        let path = if e.is_up() { self.panes[self.active].path.clone() } else { e.path.clone() };
        self.modal = Some(Modal::Properties(crate::properties::Properties::new(path)));
    }
}

/// Restores each path from the trash (its most recent copy there). Returns the
/// ones that couldn't be, with the reason.
#[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
fn restore_from_trash(paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let items = trash::os_limited::list().map_err(|e| format!("Couldn't read the trash: {e}"))?;
    let mut left = Vec::new();
    for (path, item) in pick_restores(items, paths) {
        let Some(item) = item else {
            left.push(format!("{}: not in the trash any more", tilde(&path)));
            continue;
        };
        if path.symlink_metadata().is_ok() {
            left.push(format!("{}: something else has that name now", tilde(&path)));
            continue;
        }
        if let Err(e) = trash::os_limited::restore_all([item]) {
            left.push(format!("{}: {e}", tilde(&path)));
        }
    }
    Ok(left)
}

#[cfg(not(any(target_os = "windows", all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
fn restore_from_trash(_: &[PathBuf]) -> Result<Vec<String>, String> {
    Err("Putting files back from the trash isn't possible on this system yet".into())
}

/// For each path, its most recently trashed copy (if the trash still has one).
#[cfg_attr(not(any(test, target_os = "windows", all(unix, not(target_os = "macos")))), allow(dead_code))]
fn pick_restores(items: Vec<trash::TrashItem>, paths: &[PathBuf]) -> Vec<(PathBuf, Option<trash::TrashItem>)> {
    // The trash keeps the real folder, so compare against that.
    let real = |p: &Path| match (p.parent().and_then(|d| std::fs::canonicalize(d).ok()), p.file_name()) {
        (Some(d), Some(n)) => d.join(n),
        _ => p.to_path_buf(),
    };
    let mut best: std::collections::HashMap<PathBuf, trash::TrashItem> = std::collections::HashMap::new();
    for item in items {
        let at = item.original_path();
        if best.get(&at).is_none_or(|b| b.time_deleted < item.time_deleted) {
            best.insert(at, item);
        }
    }
    paths.iter().map(|p| (p.clone(), best.remove(&real(p)).or_else(|| best.remove(p)))).collect()
}

/// Puts the values in for the placeholders, in one pass from left to right.
/// Replacing one placeholder after another would look for the later ones inside
/// the values already put in: a file named `x{selected}` would then have its
/// quoting broken, and the rest of its name read by the shell as commands.
fn fill_placeholders(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    'outer: while !rest.is_empty() {
        for (key, value) in values {
            if let Some(after) = rest.strip_prefix(key) {
                out.push_str(value);
                rest = after;
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_temporary_folder_is_new_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let a = make_temp_area().unwrap();
        let b = make_temp_area().unwrap();
        assert_ne!(a, b, "names can't be guessed from the process id alone");
        assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::remove_dir(&a).unwrap();
        std::fs::remove_dir(&b).unwrap();
    }

    /// A file name holding a placeholder and shell code must reach the command as
    /// one quoted word, whatever placeholders the command uses.
    #[cfg(unix)]
    #[test]
    fn file_names_cant_break_out_of_custom_commands() {
        let dir = std::env::temp_dir().join(format!("duopane-custom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["x{selected}$(touch PWNED)", "x{path}'$(touch PWNED)'", "{dir}{other}`touch PWNED`"] {
            let path = dir.join(name).display().to_string();
            let values = [("{path}", shell_quote(&path)), ("{dir}", shell_quote(&path)), ("{other}", shell_quote("o")), ("{selected}", shell_quote(&path))];
            for template in ["printf %s {path}", "printf %s {selected}", "printf %s {dir}"] {
                let cmd = fill_placeholders(template, &values);
                let out = std::process::Command::new("sh").arg("-c").arg(&cmd).current_dir(&dir).output().unwrap();
                assert_eq!(String::from_utf8_lossy(&out.stdout), path, "{cmd}");
                assert!(!dir.join("PWNED").exists(), "{cmd} ran code from the file name");
            }
        }
        assert_eq!(fill_placeholders("a {path} b {selected}{dir}", &[("{path}", "P".into()), ("{dir}", "D".into()), ("{selected}", "S".into())]), "a P b SD");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_newest_trashed_copy_comes_back() {
        let item = |name: &str, parent: &str, t: i64| trash::TrashItem {
            id: format!("{name}{t}").into(),
            name: name.into(),
            original_parent: parent.into(),
            time_deleted: t,
        };
        let items = vec![item("a.txt", "/x", 10), item("a.txt", "/x", 30), item("a.txt", "/y", 50), item("b.txt", "/x", 5)];
        let picked = pick_restores(items, &[PathBuf::from("/x/a.txt"), PathBuf::from("/x/gone.txt")]);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].1.as_ref().map(|i| i.time_deleted), Some(30));
        assert!(picked[1].1.is_none());
    }
}
