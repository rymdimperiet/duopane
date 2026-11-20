//! What each command does. `commands.rs` maps names and shortcuts to these.

use crate::app::{describe, plural, subdirs, tilde, App, CmdRef, InputPurpose, Modal};
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
    pub fn open_cursor(&mut self, _ctx: &egui::Context) {
        let Some(e) = self.panes[self.active].cursor_entry().cloned() else { return };
        let in_archive = self.panes[self.active].archive.is_some();
        if e.is_dir {
            self.navigate(self.active, &e.path);
        } else if !in_archive && crate::archive::is_archive(&e.path) {
            // Like Double Commander: step into the archive as if it were a folder.
            self.navigate(self.active, &e.path);
            if self.panes[self.active].archive.is_some() {
                self.set_status("Inside the archive · F5 copies files out · “..” leaves", false);
            }
        } else {
            self.open_external_entry(&e);
        }
    }

    /// Shift+Enter: open with the system's app, even archives.
    pub fn open_external(&mut self) {
        let Some(e) = self.panes[self.active].cursor_entry().cloned() else { return };
        if e.is_dir {
            return self.navigate(self.active, &e.path);
        }
        self.open_external_entry(&e);
    }

    fn open_external_entry(&mut self, e: &crate::pane::Entry) {
        let Some(path) = self.materialize(&e.path) else { return };
        {
            match open::that_detached(&path) {
                Ok(()) => self.set_status(format!("Opened {}", e.name), false),
                Err(err) => self.set_status(format!("Couldn't open {}: {err}", e.name), true),
            }
        }
    }

    pub fn go_up(&mut self) {
        let r = self.panes[self.active].go_up();
        self.after_move(self.active, r);
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
        let out = temp_area().join(n.to_string()).join(name);
        match crate::archive::extract_one(&file, &inner, &out) {
            Ok(()) => Some(out),
            Err(e) => {
                self.set_status(format!("Couldn't unpack it: {e}"), true);
                None
            }
        }
    }

    /// Commands that would change an archive being browsed say so instead.
    fn refuse_in_archive(&mut self, what: &str) -> bool {
        if self.panes[self.active].archive.is_some() {
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

    /// Shows `path` in its folder, with the cursor on it.
    pub(crate) fn jump_to(&mut self, path: &Path) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return };
        let name = name.to_string_lossy().into_owned();
        self.navigate(self.active, dir);
        self.panes[self.active].set_cursor_to_name(&name);
    }

    fn refuse_other_in_archive(&mut self) -> bool {
        if self.panes[self.other()].archive.is_some() {
            self.set_status("The other pane is inside an archive; files can't be put into an archive that way yet", true);
            return true;
        }
        false
    }

    /// F3: opens the lister on the file under the cursor.
    pub fn view_file(&mut self) {
        let Some(e) = self.panes[self.active].cursor_entry().cloned() else { return };
        // A folder shows as a tree of everything below it.
        if e.is_dir && self.panes[self.active].archive.is_some() {
            return self.set_status("That's a folder inside the archive; Enter opens it", true);
        }
        crate::lister_load::set_tree_hidden(self.settings.show_hidden);
        let Some(path) = self.materialize(&e.path) else { return };
        self.lister = Some(crate::lister::Lister::open(&path, crate::lister_load::Mode::Auto, self.settings.lister_wrap, self.settings.lister_plugins.clone()));
    }

    /// Lister N / P: view the next or previous file in the folder.
    pub(crate) fn lister_step(&mut self, dir: isize) {
        let mode = self.lister.as_ref().map(|l| l.mode).unwrap_or(crate::lister_load::Mode::Auto);
        let pane = &mut self.panes[self.active];
        let n = pane.entries.len() as isize;
        let mut i = pane.cursor as isize;
        loop {
            i += dir;
            if i < 0 || i >= n {
                self.set_status(if dir > 0 { "That was the last file" } else { "That was the first file" }, false);
                return;
            }
            if !pane.entries[i as usize].is_dir {
                break;
            }
        }
        pane.set_cursor(i as usize);
        let virt = pane.entries[i as usize].path.clone();
        let Some(path) = self.materialize(&virt) else { return };
        let mode = if mode == crate::lister_load::Mode::Hex { mode } else { crate::lister_load::Mode::Auto };
        self.lister = Some(crate::lister::Lister::open(&path, mode, self.settings.lister_wrap, self.settings.lister_plugins.clone()));
    }

    /// Alt+F5: pack the selection into an archive, Double Commander style.
    pub fn ask_pack(&mut self) {
        if self.refuse_in_archive("Packing") || self.refuse_other_in_archive() {
            return;
        }
        let sources = self.panes[self.active].targets();
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
        } else {
            self.panes[self.active].path.file_name().map(|n| n.to_string_lossy().into_owned())
        }
        .unwrap_or_else(|| "archive".into());
        let dir = self.panes[self.other()].path.clone();
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
        let archives: Vec<PathBuf> = self.panes[self.active].targets().into_iter().filter(|p| crate::archive::is_archive(p)).collect();
        if archives.is_empty() {
            return self.set_status("Select a .zip, .tar, .tar.gz or .tgz file to unpack", true);
        }
        let dest = tilde(&self.panes[self.other()].path);
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
            return self.set_status("Nothing changes with these settings", false);
        }
        let (done, errors) = Self::rename_pairs(&dir, &pairs);
        let n = done.len();
        self.last_batch = Some((dir, done));
        self.reload_all();
        if errors.is_empty() {
            self.set_status(format!("Renamed {} (undo from the command palette)", plural(n, "item")), false);
        } else {
            self.set_status(format!("Renamed {n}, {} failed", errors.len()), true);
            self.modal = Some(Modal::Message { title: "Some items couldn't be renamed".into(), text: errors.join("\n") });
        }
    }

    pub fn undo_batch_rename(&mut self) {
        let Some((dir, done)) = self.last_batch.take() else {
            return self.set_status("No batch rename to undo", true);
        };
        let (undone, errors) = Self::rename_pairs(&dir, &done);
        self.reload_all();
        if errors.is_empty() {
            self.set_status(format!("Undid the rename of {}", plural(undone.len(), "item")), false);
        } else {
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
        let verb = if select { "Selected" } else { "Deselected" };
        if n == 0 {
            self.set_status(format!("Nothing matches “{mask}” (end a mask with / to match folders)"), true);
        } else {
            self.set_status(format!("{verb} {}", plural(n, "item")), false);
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
        self.set_status(if show { "Showing hidden files" } else { "Hiding hidden files" }, false);
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

    pub fn copy_paths(&mut self, ctx: &egui::Context) {
        let targets = self.panes[self.active].targets();
        if targets.is_empty() {
            return;
        }
        let text = targets.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        ctx.copy_text(text);
        self.set_status(format!("Copied {} to the clipboard", plural(targets.len(), "path")), false);
    }

    /// Ctrl+C / Ctrl+X: puts the selected files on the clipboard; Ctrl+V copies
    /// them (or, after Ctrl+X, moves them) into the folder in the active pane.
    /// On Wayland they go on the system clipboard as files, so they can be pasted
    /// into other apps too; elsewhere their paths go on it as text.
    pub fn clip_files(&mut self, ctx: &egui::Context, cut: bool) {
        if self.panes[self.active].archive.is_some() {
            return self.set_status("Files inside an archive can't go on the clipboard yet; copy them out first (F5)", true);
        }
        let paths = self.panes[self.active].targets();
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
        let what = describe(&paths);
        let them = if paths.len() == 1 { "it" } else { "them" };
        self.clip = Some(crate::app::Clip { paths, cut });
        let msg = if cut { format!("Cut {what}: Ctrl+V moves {them} here") } else { format!("Copied {what}: Ctrl+V pastes {them}") };
        self.set_status(msg, false);
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
        self.prepare_transfer(clip.cut, paths, &dest.to_string_lossy());
    }

    pub fn add_bookmark(&mut self) {
        let p = self.panes[self.active].path.clone();
        if !self.settings.bookmarks.contains(&p) {
            self.settings.bookmarks.push(p.clone());
            self.dirty = true;
        }
        self.set_status(format!("Bookmarked {} (find it with Go to)", tilde(&p)), false);
    }

    pub fn remove_bookmark(&mut self) {
        let p = self.panes[self.active].path.clone();
        let before = self.settings.bookmarks.len();
        self.settings.bookmarks.retain(|b| b != &p);
        if self.settings.bookmarks.len() != before {
            self.dirty = true;
            self.set_status("Bookmark removed", false);
        } else {
            self.set_status("This folder isn't bookmarked", true);
        }
    }

    fn ask(&mut self, title: String, hint: &str, text: String, purpose: InputPurpose, select_chars: Option<usize>) {
        self.modal = Some(Modal::Input { title, hint: hint.into(), text, purpose, select_chars, first: true });
    }

    pub fn ask_transfer(&mut self, mv: bool) {
        let sources = self.panes[self.active].targets();
        if sources.is_empty() {
            return self.set_status("Nothing to copy here", true);
        }
        if self.refuse_other_in_archive() {
            return;
        }
        let dest = self.panes[self.other()].path.clone();
        self.ask_transfer_to(mv, sources, &dest, Some(self.active));
    }

    /// The Copy / Move dialog for `sources`, with `dest` filled in. `from` is the
    /// pane they come from, if any (files dropped in from another app have none).
    fn ask_transfer_to(&mut self, mv: bool, sources: Vec<PathBuf>, dest: &Path, from: Option<usize>) {
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
        self.ask(title, "Folder (or a new name)", dest, InputPurpose::Transfer { mv, sources }, None);
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
        self.ask_transfer_to(mv, sources, &dest, from);
    }

    /// Hands the selected files to a small drag-source window (ripdrag or
    /// dragon-drop), from which they can be dragged into other apps.
    pub fn drag_out(&mut self) {
        let targets = self.panes[self.active].targets();
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
            Ok(()) => self.set_status(format!("Drag {} from the small window that opened", describe(&targets)), false),
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
        if pane.selected.len() > 1 && pane.results.is_none() {
            return self.ask_batch_rename();
        }
        let Some(e) = self.panes[self.active].cursor_entry().cloned().filter(|e| !e.is_up()) else { return };
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
        let Some(e) = self.panes[self.active].cursor_entry().cloned() else { return };
        if e.is_dir {
            return self.set_status("That's a folder; press Enter to open it", true);
        }
        let in_archive = self.panes[self.active].archive.is_some();
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
        let paths = self.panes[self.active].targets();
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
            buttons: vec![(button.into(), Some(spec)), ("Cancel".into(), None)],
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
        let targets = self.panes[self.active].targets();
        if targets.is_empty() {
            return;
        }
        if self.panes[self.active].archive.is_some() {
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
            self.set_status("Cancelling…", false);
        } else {
            self.set_status("Nothing is running", false);
        }
    }

    pub fn edit_settings(&mut self) {
        self.store_pane_state();
        if let Err(e) = self.settings.save() {
            return self.set_status(format!("Couldn't write settings: {e}"), true);
        }
        self.edit_in_editor(&Settings::file());
        self.set_status("After saving your changes, run “Reload settings file”", false);
    }

    pub fn reload_settings(&mut self, ctx: &egui::Context) {
        let (mut s, err) = Settings::load();
        // Keep the folders currently open.
        s.left_path = Some(self.panes[0].path.clone());
        s.right_path = Some(self.panes[1].path.clone());
        self.settings = s;
        theme::apply(ctx, &self.settings);
        let show = self.settings.show_hidden;
        for p in &mut self.panes {
            p.show_dotdot = self.settings.show_dotdot;
            p.set_show_hidden(show);
        }
        self.rebuild_bindings();
        match err {
            Some(e) => self.set_status(e, true),
            None => self.set_status("Settings reloaded", false),
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
        let mut found: BTreeMap<PathBuf, i32> = BTreeMap::new();
        let mut add = |p: PathBuf, boost: i32| {
            let e = found.entry(p).or_insert(i32::MIN);
            *e = (*e).max(boost);
        };
        for b in &self.settings.bookmarks {
            add(b.clone(), 400);
        }
        for (k, v) in &self.settings.visited {
            add(PathBuf::from(k), 40 + ((*v as f32).ln() * 60.0) as i32);
        }
        if let Some(h) = dirs::home_dir() {
            add(h.clone(), 60);
            for d in subdirs(&h, false) {
                add(d, 20);
            }
        }
        for pane in &self.panes {
            for d in subdirs(&pane.path, pane.show_hidden) {
                add(d, 30);
            }
            for a in pane.path.ancestors().skip(1) {
                add(a.to_path_buf(), 25);
            }
        }
        add(PathBuf::from("/"), 0);
        for root in ["/media", "/mnt", "/run/media"] {
            for d in subdirs(Path::new(root), false) {
                add(d.clone(), 15);
                for dd in subdirs(&d, false) {
                    add(dd, 15);
                }
            }
        }
        let base: Vec<PaletteItem> =
            found.into_iter().filter(|(p, _)| p.is_dir()).map(|(p, boost)| dir_item(p, boost)).collect();
        self.modal = Some(Modal::Palette(PaletteState::new(PaletteKind::GoTo, base)));
    }

    /// Runs one of the user's own commands from the settings file.
    pub(crate) fn run_custom(&mut self, i: usize) {
        let Some(c) = self.settings.custom_commands.get(i).cloned() else { return };
        let pane = &self.panes[self.active];
        let path = pane.cursor_entry().map(|e| e.path.display().to_string()).unwrap_or_default();
        let dir = pane.path.display().to_string();
        let other = self.panes[self.other()].path.display().to_string();
        let selected: Vec<String> = pane.targets().iter().map(|p| p.display().to_string()).collect();
        let cmd = c
            .run
            .replace("{path}", &shell_quote(&path))
            .replace("{dir}", &shell_quote(&dir))
            .replace("{other}", &shell_quote(&other))
            .replace("{selected}", &selected.iter().map(|s| shell_quote(s)).collect::<Vec<_>>().join(" "));
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
            Ok(_) => self.set_status(format!("Ran “{}”", c.title), false),
            Err(e) => self.set_status(format!("“{}” failed: {e}", c.title), true),
        }
    }

    /// Called when a text dialog is confirmed.
    pub(crate) fn submit_input(&mut self, purpose: InputPurpose, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        match purpose {
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
            InputPurpose::Transfer { mv, sources } => self.prepare_transfer(mv, sources, &text),
            InputPurpose::Extract { archive, items, base } => {
                let dest = self.resolve_input_path(&text);
                if let Err(e) = std::fs::create_dir_all(&dest) {
                    return self.set_status(format!("Couldn't create {text}: {e}"), true);
                }
                self.start_job(JobSpec::Extract { archive, items, base, dest, policy: ConflictPolicy::Ask });
            }
            InputPurpose::Mask { select } => self.apply_mask(select, &text),
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

    fn prepare_transfer(&mut self, mv: bool, sources: Vec<PathBuf>, text: &str) {
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
            return self.set_status("Nothing to do: it's already there", false);
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
pub fn temp_area() -> PathBuf {
    std::env::temp_dir().join(format!("duopane-{}", std::process::id()))
}
