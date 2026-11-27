//! The app state and the per-frame loop. Commands live in `actions.rs`,
//! drawing in `ui.rs` and dialogs in `dialogs.rs`.

use crate::commands::{self, Command, Shortcut};
use crate::fs_ops::{self, JobHandle, JobSpec};
use crate::pane::{human_size, Pane};
use crate::settings::Settings;
use crate::theme;
use eframe::egui::{self, Event, Key};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdRef {
    Builtin(usize),
    Custom(usize),
}

pub enum Action {
    Activate(usize),
    Run(CmdRef),
    RunId(&'static str),
    Navigate(usize, PathBuf),
    /// Files dropped with the mouse: (from pane, onto pane, files, destination folder, move).
    Drop { from: Option<usize>, to: usize, paths: Vec<PathBuf>, dest: PathBuf, mv: bool },
}

pub enum InputPurpose {
    Rename(PathBuf),
    NewFolder,
    NewFile,
    Transfer { mv: bool, sources: Vec<PathBuf> },
    /// Shift+F5: a copy of this, next to it, under the name typed.
    CopyInPlace(PathBuf),
    /// Select (or deselect) by mask.
    Mask { select: bool },
    /// Copy entries out of the archive being browsed.
    Extract { archive: PathBuf, items: Vec<String>, base: String },
    /// "Connect to SFTP…" from the drive menu, for this pane.
    Sftp { pane: usize },
    /// The password for an SFTP server (shown as dots, never saved).
    SftpPassword { pane: usize, target: String },
}

pub enum Modal {
    Input { title: String, hint: String, text: String, purpose: InputPurpose, select_chars: Option<usize>, first: bool },
    /// A question with buttons; `focus` is the one Enter presses (arrows / Tab move it).
    Confirm { title: String, message: String, buttons: Vec<(String, Option<JobSpec>)>, focus: usize },
    Palette(crate::dialogs::PaletteState),
    Message { title: String, text: String },
    /// Colour scheme and fonts.
    Appearance { ui_filter: String, mono_filter: String },
    /// Pack files into an archive (Alt+F5).
    Pack {
        path: String,
        format: crate::archive::Format,
        level: u32,
        separate: bool,
        delete_after: bool,
        sources: Vec<PathBuf>,
        first: bool,
    },
    /// Batch rename (Ctrl+M).
    BatchRename {
        dir: PathBuf,
        items: Vec<crate::rename::Item>,
        others: std::collections::HashSet<String>,
        rules: crate::rename::Rules,
        first: bool,
    },
    /// Unpack archives (Alt+F9).
    Unpack { archives: Vec<PathBuf>, dest: String, into_folder: bool, first: bool },
    /// A running job found a file already where it's going.
    Conflict(crate::dialogs::ConflictDialog),
    /// Find files (Ctrl+F).
    Search(crate::search_ui::SearchDialog),
    /// About Duopane (Shift+F1).
    About,
    /// The drive menu (Alt+F1 / Alt+F2).
    Places(crate::places::PlacesMenu),
    /// The right-click menu opened from the keyboard (Shift+Enter), with each item's keys.
    RowMenu { at: egui::Pos2, selected: usize, items: Vec<crate::dialogs::MenuItem> },
    /// Alt+Enter.
    Properties(crate::properties::Properties),
}

/// Mounting or unmounting in the background (see `places.rs`).
pub(crate) struct MountJob {
    /// The pane to open it in.
    pub pane: usize,
    pub name: String,
    /// The SFTP server, as typed (to remember it, or to ask for a password).
    pub sftp: Option<String>,
    pub rx: crate::places::MountAnswer,
}

/// Files on Duopane's clipboard, and whether they were cut (to be moved).
#[derive(Clone, Debug)]
pub struct Clip {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

pub struct Status {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct App {
    pub panes: [Pane; 2],
    pub active: usize,
    pub settings: Settings,
    pub(crate) commands: Vec<Command>,
    pub(crate) bindings: HashMap<Shortcut, CmdRef>,
    pub(crate) modal: Option<Modal>,
    pub(crate) job: Option<JobHandle>,
    pub(crate) queue: VecDeque<JobSpec>,
    pub(crate) status: Option<Status>,
    pub(crate) quick: String,
    pub(crate) quick_at: Instant,
    last_check: Instant,
    pub(crate) dirty: bool,
    last_save: Instant,
    title: String,
    pub(crate) actions: Vec<Action>,
    pub(crate) size_rx: Option<mpsc::Receiver<(usize, u64)>>,
    /// A drive being mounted or unmounted, or an SFTP server being connected.
    pub(crate) mount_job: Option<MountJob>,
    /// A newer release, once known (shown in the key bar and the About box).
    pub(crate) update: Option<crate::update::Release>,
    update_rx: Option<std::sync::mpsc::Receiver<Option<crate::update::Release>>>,
    /// What the last "Move to trash" took, for Ctrl+Z.
    pub(crate) last_trashed: Vec<PathBuf>,
    /// A checksum being worked out, and how to stop it.
    pub(crate) checksum: Option<(mpsc::Receiver<crate::checksum::Report>, std::sync::Arc<std::sync::atomic::AtomicBool>)>,
    /// The F3 viewer, when open (it takes over the window).
    pub(crate) lister: Option<crate::lister::Lister>,
    /// The last batch rename: folder and (new name, old name) pairs, for undo.
    pub(crate) last_batch: Option<(PathBuf, Vec<(String, String)>)>,
    /// Where the two panes were drawn last frame (the status note floats over it).
    pub(crate) panes_rect: Option<egui::Rect>,
    /// Drags out of the window into other apps (Wayland only; see `native_drag.rs`).
    #[cfg(target_os = "linux")]
    pub(crate) native_drag: Option<crate::native_drag::NativeDrag>,
    /// Files copied or cut with Ctrl+C / Ctrl+X (also put on the system clipboard
    /// where possible; this copy serves pasting where it isn't).
    pub(crate) clip: Option<Clip>,
    /// A paste waiting for the clipboard's contents: (destination folder, answer).
    #[cfg(target_os = "linux")]
    pub(crate) paste_rx: Option<(PathBuf, mpsc::Receiver<Option<crate::native_drag::Clip>>)>,
    /// When the last paste started (Ctrl+V can be reported twice; see `paste_files`).
    pub(crate) last_paste: Instant,
    /// Quick view (Ctrl+Q): shown in place of one pane, following the other's cursor.
    pub(crate) quick_view: Option<crate::quickview::QuickView>,
    /// Tab presses kept from egui's own focus handling (see `raw_input_hook`).
    held_keys: Vec<Event>,
    /// What was last searched for (Ctrl+F starts with it).
    pub(crate) last_search: String,
    /// A drag of ours is in the desktop's hands (see `hand_drag_to_desktop`).
    native_dragging: bool,
    /// Files from another app are being dragged over the window.
    foreign_hover: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Vec<PathBuf>) -> Self {
        let (settings, settings_error) = Settings::load();
        crate::pane::set_collation(settings.sort_language);
        theme::apply(&cc.egui_ctx, &settings);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        // Lets the lister's Markdown view show pictures from files.
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let abs = |p: &PathBuf| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        let left = args
            .first()
            .map(abs)
            .or_else(|| settings.left_path.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| home.clone());
        let right = args.get(1).map(abs).or_else(|| settings.right_path.clone()).unwrap_or_else(|| home.clone());

        let panes = [
            Pane::new(&left, settings.show_hidden, settings.show_dotdot, settings.left_sort, settings.left_ascending),
            Pane::new(&right, settings.show_hidden, settings.show_dotdot, settings.right_sort, settings.right_ascending),
        ];
        let mut panes = panes;
        for (p, then) in panes.iter_mut().zip([settings.left_then, settings.right_then]) {
            if then.is_some_and(|(k, _)| k != p.sort) {
                p.then = then;
                p.apply_view();
            }
        }
        let mut app = App {
            panes,
            active: 0,
            settings,
            commands: commands::builtin(),
            bindings: HashMap::new(),
            modal: None,
            job: None,
            queue: VecDeque::new(),
            status: None,
            quick: String::new(),
            quick_at: Instant::now(),
            last_check: Instant::now(),
            dirty: false,
            last_save: Instant::now(),
            title: String::new(),
            actions: Vec::new(),
            size_rx: None,
            mount_job: None,
            update: None,
            update_rx: None,
            last_trashed: Vec::new(),
            checksum: None,
            lister: None,
            last_batch: None,
            panes_rect: None,
            #[cfg(target_os = "linux")]
            native_drag: native_drag(cc),
            held_keys: Vec::new(),
            last_search: String::new(),
            quick_view: None,
            clip: None,
            #[cfg(target_os = "linux")]
            paste_rx: None,
            last_paste: Instant::now() - Duration::from_secs(10),
            native_dragging: false,
            foreign_hover: false,
        };
        app.rebuild_bindings();
        if let Some(e) = settings_error {
            app.set_status(e, true);
        }
        app.start_update_check(&cc.egui_ctx);
        app
    }

    /// Shows what the last check found, and asks again when a day has passed.
    fn start_update_check(&mut self, ctx: &egui::Context) {
        use crate::update;
        if !self.settings.check_for_updates {
            return;
        }
        if let Some(v) = self.settings.newest_release.clone().filter(|v| update::is_newer(v, env!("CARGO_PKG_VERSION"))) {
            self.update = Some(update::Release { version: v, url: update::RELEASES.into() });
        }
        let now = chrono::Utc::now().timestamp();
        let due = self.settings.last_update_check.is_none_or(|t| t > now || now - t >= update::EVERY.as_secs() as i64);
        if due {
            self.update_rx = Some(update::check(ctx.clone()));
            self.settings.last_update_check = Some(now);
            self.dirty = true;
        }
    }

    pub fn active_pane(&mut self) -> &mut Pane {
        &mut self.panes[self.active]
    }

    pub(crate) fn other(&self) -> usize {
        1 - self.active
    }

    pub(crate) fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some(Status { text: text.into(), error, at: Instant::now() });
    }

    pub(crate) fn rebuild_bindings(&mut self) {
        self.bindings.clear();
        let mut bad = Vec::new();
        for (i, c) in self.commands.iter().enumerate() {
            let keys: Vec<String> = self
                .settings
                .key_bindings
                .get(c.id)
                .cloned()
                .unwrap_or_else(|| c.keys.iter().map(|s| s.to_string()).collect());
            for k in keys {
                match Shortcut::parse(&k) {
                    Some(s) => {
                        self.bindings.insert(s, CmdRef::Builtin(i));
                    }
                    None => bad.push(k),
                }
            }
        }
        for (i, c) in self.settings.custom_commands.iter().enumerate() {
            for k in &c.keys {
                match Shortcut::parse(k) {
                    Some(s) => {
                        self.bindings.insert(s, CmdRef::Custom(i));
                    }
                    None => bad.push(k.clone()),
                }
            }
        }
        if !bad.is_empty() {
            self.set_status(format!("Unknown keys in settings: {}", bad.join(", ")), true);
        }
    }

    pub(crate) fn keys_for(&self, c: CmdRef) -> String {
        let mut labels: Vec<String> =
            self.bindings.iter().filter(|(_, v)| **v == c).map(|(k, _)| k.label()).collect();
        labels.sort_by_key(|l| (l.len(), l.clone()));
        labels.join("  ")
    }

    pub(crate) fn navigate(&mut self, idx: usize, path: &Path) {
        let r = self.panes[idx].navigate(path);
        self.after_move(idx, r);
    }

    /// A pane's folder is on a network mount that has stopped answering: go home
    /// rather than sit in it, and say how to let go of the connection.
    fn leave_dead_connection(&mut self, idx: usize, dir: &Path) {
        let Some((_, what)) = crate::places::network_mount_of(dir) else { return };
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        self.navigate(idx, &home);
        self.set_status(format!("{what} stopped answering, so the pane went home. To disconnect, press Del on it in the drive menu (Alt+F1)."), true);
    }

    pub(crate) fn after_move(&mut self, idx: usize, res: Result<(), String>) {
        match res {
            // Still reading in the background: this runs again when it arrives.
            Ok(()) if self.panes[idx].is_loading() => {}
            Ok(()) => {
                if self.panes[idx].archive.is_none() {
                    let p = self.panes[idx].path.clone();
                    self.settings.record_visit(&p);
                }
                self.dirty = true;
            }
            Err(e) => self.set_status(e, true),
        }
    }

    pub(crate) fn run(&mut self, ctx: &egui::Context, c: CmdRef) {
        match c {
            CmdRef::Builtin(i) => {
                let f = self.commands[i].run;
                f(self, ctx);
            }
            CmdRef::Custom(i) => self.run_custom(i),
        }
    }

    pub(crate) fn run_id(&mut self, ctx: &egui::Context, id: &str) {
        if let Some(i) = self.commands.iter().position(|c| c.id == id) {
            self.run(ctx, CmdRef::Builtin(i));
        }
    }

    pub(crate) fn start_job(&mut self, spec: JobSpec) {
        if let JobSpec::Delete { paths, permanent: false } = &spec {
            self.last_trashed = paths.clone();
        }
        if self.job.is_some() {
            self.queue.push_back(spec);
        } else {
            self.job = Some(fs_ops::start(spec));
        }
    }

    pub(crate) fn resolve_input_path(&self, text: &str) -> PathBuf {
        let p = expand_tilde(text.trim());
        if p.is_absolute() {
            p
        } else {
            self.panes[self.active].real_dir().join(p)
        }
    }

    pub(crate) fn store_pane_state(&mut self) {
        self.settings.left_path = Some(self.panes[0].real_dir());
        self.settings.right_path = Some(self.panes[1].real_dir());
        self.settings.left_sort = self.panes[0].sort;
        self.settings.left_ascending = self.panes[0].ascending;
        self.settings.right_sort = self.panes[1].sort;
        self.settings.right_ascending = self.panes[1].ascending;
        self.settings.left_then = self.panes[0].then;
        self.settings.right_then = self.panes[1].then;
    }

    fn save_if_dirty(&mut self) {
        if self.dirty && self.last_save.elapsed() > Duration::from_secs(2) {
            self.store_pane_state();
            if let Err(e) = self.settings.save() {
                self.set_status(format!("Couldn't save settings: {e}"), true);
            }
            self.dirty = false;
            self.last_save = Instant::now();
        }
    }

    /// Finished copies, size calculations and folders changed on disk.
    fn poll_background(&mut self) {
        if let Some(job) = &mut self.job {
            if job.poll() {
                let (errors, cancelled) = job.finished.take().unwrap_or_default();
                let label = past_tense(&job.label);
                self.job = None;
                if cancelled {
                    self.set_status("Cancelled", true);
                } else if errors.is_empty() {
                    self.set_status(label, false);
                } else {
                    self.set_status(format!("{label}, with {}", plural(errors.len(), "problem")), true);
                    if self.modal.is_none() {
                        self.modal = Some(Modal::Message {
                            title: "Some items couldn't be processed".into(),
                            text: errors.join("\n"),
                        });
                    }
                }
                if matches!(self.modal, Some(Modal::Conflict(_))) {
                    self.modal = None; // the job ended (e.g. cancelled) while asking
                }
                self.reload_all();
                if let Some(next) = self.queue.pop_front() {
                    self.job = Some(fs_ops::start(next));
                }
            }
        }
        // A job asks about a file that's already there. (If another dialog is open,
        // the question waits until it closes; the job waits too.)
        if let (Some(job), None) = (&mut self.job, &self.modal) {
            if let Some(q) = job.conflict.take() {
                self.modal = Some(Modal::Conflict(crate::dialogs::ConflictDialog::new(q)));
            }
        }
        #[cfg(target_os = "linux")]
        if let Some((dest, rx)) = &self.paste_rx {
            if let Ok(clip) = rx.try_recv() {
                let dest = dest.clone();
                self.paste_rx = None;
                match clip {
                    Some(c) => self.paste_into(Clip { paths: c.paths, cut: c.cut }, &dest),
                    None => self.set_status("There are no files on the clipboard", true),
                }
            }
        }
        if let Some(rx) = &self.size_rx {
            if let Ok((n, bytes)) = rx.try_recv() {
                self.size_rx = None;
                self.set_status(format!("{}: {}", plural(n, "item"), human_size(bytes)), false);
            }
        }
        if let Some(report) = self.checksum.as_ref().and_then(|(rx, _)| rx.try_recv().ok()) {
            self.checksum = None;
            self.status = None;
            if self.modal.is_none() {
                self.modal = Some(Modal::Message { title: report.title, text: report.text });
            }
        }
        if let Some(found) = self.update_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.update_rx = None;
            self.settings.newest_release = found.as_ref().map(|r| r.version.clone());
            self.update = found;
            self.dirty = true;
        }
        if let Some(r) = self.mount_job.as_ref().and_then(|j| j.rx.try_recv().ok()) {
            if let Some(job) = self.mount_job.take() {
                self.mount_done(job, r);
            }
        }
        for i in 0..2 {
            match self.panes[i].poll_loading() {
                Some(Ok(true)) => self.after_move(i, Ok(())),
                Some(Ok(false)) | None => {}
                Some(Err(e)) => self.set_status(e, true),
            }
        }
        if self.last_check.elapsed() > Duration::from_millis(1000) {
            self.last_check = Instant::now();
            for i in 0..2 {
                if let Some((dir, _)) = self.panes[i].watch_disk().filter(|(_, w)| *w > DEAD_CONNECTION) {
                    self.leave_dead_connection(i, &dir);
                }
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Ctrl+V seen on the Wayland keyboard (egui misses it when the clipboard
        // holds files but no text). Taken every frame so none is left for later.
        #[cfg(target_os = "linux")]
        let native_paste = self.native_drag.as_ref().is_some_and(|nd| nd.paste_pressed());
        #[cfg(not(target_os = "linux"))]
        let native_paste = false;
        // Tab, kept from egui's focus handling (see `raw_input_hook`).
        let held = std::mem::take(&mut self.held_keys);
        if !held.is_empty() {
            ctx.input_mut(|i| {
                for (k, e) in held.into_iter().enumerate() {
                    i.events.insert(k, e);
                }
            });
        }
        let editing = self.panes.iter().any(|p| p.editing_path.is_some());
        if self.modal.is_some() || editing {
            return;
        }
        // Keep keyboard focus off buttons so arrows, Space and Tab drive the panes.
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        // The mouse's back / forward buttons go up / down the folder tree.
        let (mouse_back, mouse_fwd) = ctx.input(|i| {
            (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2))
        });
        if mouse_back {
            self.actions.push(Action::RunId("go_up"));
        }
        if mouse_fwd {
            self.actions.push(Action::RunId("enter_folder"));
        }
        let (events, mods_now) = ctx.input(|i| (i.events.clone(), i.modifiers));
        let mut searching = !self.quick.is_empty() && self.quick_at.elapsed() < Duration::from_millis(1200);
        // Keys can arrive in bursts (fast typing, a slow frame): handle them in order,
        // and once a dialog opens, leave the rest of the burst for the dialog.
        let mut consumed = 0;
        let mut skip_text = false;
        while consumed < events.len() {
            let ev = events[consumed].clone();
            consumed += 1;
            match ev {
                Event::Key { key, pressed: true, modifiers, .. } => {
                    skip_text = false;
                    let plain = !modifiers.command && !modifiers.ctrl && !modifiers.alt;
                    if plain && !modifiers.shift && self.filter_key(key) {
                        continue;
                    }
                    if plain && self.move_key(key, modifiers.shift) {
                        self.quick.clear();
                        searching = false;
                        continue;
                    }
                    // While typing a name to jump to, "+" and "-" are part of the name.
                    if plain && searching && matches!(key, Key::Plus | Key::Minus) {
                        continue;
                    }
                    if let Some(cmd) = self.bindings.get(&Shortcut::from_event(key, modifiers)).copied() {
                        self.quick.clear();
                        searching = false;
                        self.run(ctx, cmd);
                        // The key's own character (like "+") isn't a name search.
                        skip_text = true;
                        if self.modal.is_some() || self.panes.iter().any(|p| p.editing_path.is_some()) {
                            if matches!(events.get(consumed), Some(Event::Text(_))) {
                                consumed += 1;
                            }
                            break;
                        }
                    }
                }
                // egui reports Ctrl+C / X / V as these rather than as keys.
                Event::Copy => self.run_id(ctx, "clip_copy"),
                Event::Cut => self.run_id(ctx, "clip_cut"),
                Event::Paste(_) => self.run_id(ctx, "clip_paste"),
                Event::Text(t) => {
                    if std::mem::take(&mut skip_text) {
                        continue;
                    }
                    // Some systems send invisible codes with Alt and similar keys: those aren't a name search.
                    if mods_now.command || mods_now.ctrl || mods_now.alt || t.trim().is_empty() || t.chars().any(char::is_control) {
                        continue;
                    }
                    let pane = &mut self.panes[self.active];
                    if pane.filtering {
                        let text = format!("{}{t}", pane.filter);
                        pane.set_filter(&text);
                        continue;
                    }
                    // Numpad * inverts the selection, like Double Commander.
                    if t == "*" && !searching {
                        self.panes[self.active].invert_selection();
                        continue;
                    }
                    if self.quick_at.elapsed() > Duration::from_millis(1200) {
                        self.quick.clear();
                    }
                    self.quick.push_str(&t);
                    self.quick_at = Instant::now();
                    let q = self.quick.clone();
                    if !self.panes[self.active].quick_find(&q) {
                        self.set_status(format!("No name matches “{q}”"), true);
                    }
                }
                _ => {}
            }
        }
        if native_paste && self.modal.is_none() {
            self.run_id(ctx, "clip_paste");
        }
        // Don't let scroll areas or buttons react to the keys handled here.
        ctx.input_mut(|i| {
            let rest = i.events.split_off(consumed.min(i.events.len()));
            i.events.retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_)));
            i.events.extend(rest);
        });
    }

    /// Keys that edit the Ctrl+S filter: Backspace takes a letter off, Esc clears it,
    /// Enter stops typing into it (and goes on to open as usual). "+" and "-" are
    /// part of the text while typing. Returns true when the key was used up.
    fn filter_key(&mut self, key: Key) -> bool {
        let pane = &mut self.panes[self.active];
        match key {
            Key::Escape if pane.filtering || !pane.filter.is_empty() => {
                pane.filtering = false;
                pane.set_filter("");
                true
            }
            Key::Backspace if pane.filtering => {
                let mut text = pane.filter.clone();
                text.pop();
                pane.set_filter(&text);
                true
            }
            Key::Enter if pane.filtering => {
                pane.filtering = false;
                false
            }
            Key::Plus | Key::Minus => pane.filtering,
            _ => false,
        }
    }

    /// Cursor movement and selection keys. Returns true when the key was used.
    fn move_key(&mut self, key: Key, shift: bool) -> bool {
        let quick_empty = self.quick.is_empty();
        let pane = &mut self.panes[self.active];
        let n = pane.entries.len() as isize;
        let page = ((pane.view_height / theme::row_height(self.settings.font_size)) as isize - 1).max(1);
        match key {
            Key::ArrowUp => pane.move_cursor(-1, shift),
            Key::ArrowDown => pane.move_cursor(1, shift),
            Key::PageUp => pane.move_cursor(-page, shift),
            Key::PageDown => pane.move_cursor(page, shift),
            Key::Home => pane.move_cursor(-n, shift),
            Key::End => pane.move_cursor(n, shift),
            Key::Space | Key::Insert if !shift => {
                let c = pane.cursor;
                pane.toggle_selected(c);
                pane.move_cursor(1, false);
            }
            Key::Escape if !shift => {
                if quick_empty {
                    pane.selected.clear();
                }
            }
            _ => return false,
        }
        true
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Activate(i) => {
                if self.active != i {
                    self.active = i;
                    self.quick.clear();
                }
            }
            Action::Run(c) => self.run(ctx, c),
            Action::RunId(id) => self.run_id(ctx, id),
            Action::Navigate(i, p) => {
                self.active = i;
                self.navigate(i, &p);
            }
            Action::Drop { from, to, paths, dest, mv } => self.drop_files(from, to, paths, dest, mv),
        }
    }

    fn lister_frame(&mut self, ctx: &egui::Context) {
        let Some(l) = self.lister.as_mut() else { return };
        let mut volume = self.settings.volume;
        let action = l.ui(ctx, &mut volume);
        let wrap = l.wrap;
        if volume != self.settings.volume || wrap != self.settings.lister_wrap {
            self.settings.volume = volume;
            self.settings.lister_wrap = wrap;
            self.dirty = true;
        }
        match action {
            crate::lister::ListerAction::None => {}
            crate::lister::ListerAction::Close => self.lister = None,
            crate::lister::ListerAction::Next => self.lister_step(1),
            crate::lister::ListerAction::Prev => self.lister_step(-1),
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    /// A file drag that starts in a pane is handed to the desktop straight away,
    /// so the compositor decides where it's dropped: on another app (even one
    /// overlapping ours) or back on a pane (which `raw_input_hook` passes on).
    fn hand_drag_to_desktop(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "linux")]
        {
            let Some(nd) = &self.native_drag else { return };
            if self.native_dragging || !nd.can_start() {
                return;
            }
            let Some(p) = egui::DragAndDrop::payload::<crate::ui::DragPayload>(ctx) else { return };
            // Files inside an archive aren't real files yet: those drags stay in the window.
            if self.panes[p.from].archive.is_none() && nd.start(&p.paths) {
                self.native_dragging = true;
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = ctx;
    }

    /// Ctrl+Q: shows details of what the cursor is on in the other pane (or stops).
    pub(crate) fn toggle_quick_view(&mut self) {
        self.quick_view = match self.quick_view {
            Some(_) => None,
            None => Some(crate::quickview::QuickView::new(self.other())),
        };
    }

    /// Keeps Quick View on the cursor of the pane it watches. Moving into the pane
    /// it covers (Tab, a click) closes it.
    fn follow_quick_view(&mut self, ctx: &egui::Context) {
        use crate::quickview::Target;
        let Some(qv) = &mut self.quick_view else { return };
        if qv.slot == self.active {
            self.quick_view = None;
            return;
        }
        let pane = &self.panes[1 - qv.slot];
        let target = pane.cursor_entry().map(|e| match &pane.archive {
            Some(a) => Target::InArchive { archive: a.file.clone(), entry: e.clone() },
            None => Target::Disk { path: e.path.clone(), up: e.is_up() },
        });
        qv.follow(target, ctx);
    }

    /// Files dropped onto the window from another app go to the pane they were
    /// dropped on (or the active pane when the position isn't known).
    /// Only some systems deliver these; see the README.
    fn files_dropped_in(&mut self, ctx: &egui::Context) {
        let (paths, pos) = ctx.input(|i| {
            (i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect::<Vec<_>>(), i.pointer.hover_pos())
        });
        if paths.is_empty() || self.modal.is_some() {
            return;
        }
        let to = pos.and_then(|p| self.pane_at(p)).unwrap_or(self.active);
        let dest = self.panes[to].path.clone();
        self.actions.push(Action::Drop { from: None, to, paths, dest, mv: false });
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        if let Some(l) = &self.lister {
            let name = l.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let title = format!("{name} — Lister — Duopane");
            if title != self.title {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                self.title = title;
            }
            return;
        }
        let p = &self.panes[self.active].path;
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        let title = format!("{name} — Duopane");
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for App {
    /// Runs while the window still exists: the Wayland thread has to finish (and
    /// its objects go) before winit closes the connection they use.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        #[cfg(target_os = "linux")]
        if let Some(mut nd) = self.native_drag.take() {
            nd.shutdown();
        }
    }

    /// While the desktop runs a drag, the window gets no pointer events; the
    /// drag's own events are turned into them here, so egui sees the drag move
    /// over the panes and be dropped (or end elsewhere) as if it were its own.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // In the main window, Tab switches panes. egui would also move its keyboard
        // focus to the first button on screen (F2 Rename) and flash a frame round
        // it, so Tab is held back from egui here and handed to `handle_keys`.
        let main_window = self.modal.is_none() && self.lister.is_none() && self.panes.iter().all(|p| p.editing_path.is_none());
        if main_window {
            let (tabs, rest) = std::mem::take(&mut raw.events).into_iter().partition(|e| matches!(e, Event::Key { key: Key::Tab, .. }));
            raw.events = rest;
            self.held_keys = tabs;
        }
        #[cfg(target_os = "linux")]
        if let Some(nd) = &self.native_drag {
            nd.set_move(raw.modifiers.shift);
            let events = nd.events();
            drag_events_to_input(events, raw, ctx.zoom_factor(), &mut self.native_dragging, &mut self.foreign_hover);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (ctx, raw);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Pictures in the viewer are scaled to what the graphics card can hold.
        crate::lister_load::set_max_texture_side(ctx.input(|i| i.max_texture_side));
        self.poll_background();
        if self.lister.is_some() {
            self.lister_frame(ctx);
            self.update_title(ctx);
            self.save_if_dirty();
            return;
        }
        self.handle_keys(ctx);
        self.files_dropped_in(ctx);
        self.follow_quick_view(ctx);
        self.bottom_bar(ctx);
        self.panes_ui(ctx);
        self.modal_ui(ctx);
        self.hand_drag_to_desktop(ctx);
        for a in std::mem::take(&mut self.actions) {
            self.apply(ctx, a);
        }
        self.update_title(ctx);
        self.save_if_dirty();
        let busy = self.job.is_some()
            || self.size_rx.is_some()
            || self.panes.iter().any(Pane::is_loading)
            || self.quick_view.as_ref().is_some_and(|q| q.busy());
        ctx.request_repaint_after(Duration::from_millis(if busy { 100 } else { 1000 }));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Files unpacked from archives for viewing are only needed while we run.
        if let Some(t) = crate::actions::temp_area_made() {
            let _ = std::fs::remove_dir_all(t);
        }
        self.store_pane_state();
        let _ = self.settings.save();
    }
}

/// Turns the desktop's drag events into egui input (see `App::raw_input_hook`).
/// `dragging`: one of our drags is in the desktop's hands; `hover`: files from
/// another app are over the window.
#[cfg(target_os = "linux")]
pub(crate) fn drag_events_to_input(
    events: Vec<crate::native_drag::DragEvent>,
    raw: &mut egui::RawInput,
    zoom: f32,
    dragging: &mut bool,
    hover: &mut bool,
) {
    use crate::native_drag::DragEvent;
    let to_points = |x: f64, y: f64| egui::pos2(x as f32, y as f32) / zoom;
    let mods = raw.modifiers;
    let release = |pos| Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: mods };
    for e in events {
        match e {
            DragEvent::Motion { x, y, ours } => {
                raw.events.push(Event::PointerMoved(to_points(x, y)));
                *hover = !ours;
            }
            DragEvent::Leave => {
                raw.events.push(Event::PointerGone);
                *hover = false;
            }
            DragEvent::DropOurs { x, y } => {
                let pos = to_points(x, y);
                raw.events.push(Event::PointerMoved(pos));
                raw.events.push(release(pos));
                *dragging = false;
            }
            DragEvent::DropFiles { paths, x, y } => {
                raw.events.push(Event::PointerMoved(to_points(x, y)));
                raw.dropped_files.extend(paths.into_iter().map(|p| egui::DroppedFile { path: Some(p), ..Default::default() }));
                *hover = false;
            }
            // Dropped in another app or cancelled: end egui's side of the drag,
            // released far away so no pane takes it.
            DragEvent::Ended if *dragging => {
                raw.events.push(Event::PointerGone);
                raw.events.push(release(egui::pos2(-10_000.0, -10_000.0)));
                *dragging = false;
            }
            DragEvent::Ended => {}
        }
    }
    if *hover {
        raw.hovered_files.push(egui::HoveredFile::default());
    }
}

/// Sets up dragging into other apps, when on Wayland (and not turned off with
/// `DUOPANE_NO_NATIVE_DRAG=1`).
#[cfg(target_os = "linux")]
fn native_drag(cc: &eframe::CreationContext<'_>) -> Option<crate::native_drag::NativeDrag> {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
    if std::env::var_os("DUOPANE_NO_NATIVE_DRAG").is_some_and(|v| v != "0") {
        return None;
    }
    let display = cc.display_handle().ok()?.as_raw();
    let window = cc.window_handle().ok()?.as_raw();
    crate::native_drag::NativeDrag::new(display, window, cc.egui_ctx.clone())
}

/// How long a network folder may keep us waiting before the pane leaves it.
const DEAD_CONNECTION: Duration = Duration::from_secs(8);

// ----- small utilities -----------------------------------------------------

pub fn subdirs(p: &Path, include_hidden: bool) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(p) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| include_hidden || !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .take(2000)
        .collect();
    v.sort();
    v
}

pub fn tilde(p: &Path) -> String {
    if let Some(h) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&h) {
            return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
        }
    }
    p.display().to_string()
}

pub fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(s)
}

pub fn describe(paths: &[PathBuf]) -> String {
    if paths.len() == 1 {
        let name = paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        format!("“{name}”")
    } else {
        fs_ops::count(paths.len())
    }
}

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn past_tense(label: &str) -> String {
    for (from, to) in [("Copying", "Copied"), ("Moving to trash", "Moved to trash"), ("Moving", "Moved"), ("Deleting", "Deleted"), ("Packing", "Packed"), ("Unpacking", "Unpacked"), ("Extracting", "Extracted")] {
        if let Some(rest) = label.strip_prefix(from) {
            return format!("{to}{rest}");
        }
    }
    label.to_string()
}
