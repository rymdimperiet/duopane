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
}

pub enum InputPurpose {
    Rename(PathBuf),
    NewFolder,
    NewFile,
    Transfer { mv: bool, sources: Vec<PathBuf> },
    /// Select (or deselect) by mask.
    Mask { select: bool },
    /// Copy entries out of the archive being browsed.
    Extract { archive: PathBuf, items: Vec<String>, base: String },
}

pub enum Modal {
    Input { title: String, hint: String, text: String, purpose: InputPurpose, select_chars: Option<usize>, first: bool },
    Confirm { title: String, message: String, buttons: Vec<(String, Option<JobSpec>)> },
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
    /// The F3 viewer, when open (it takes over the window).
    pub(crate) lister: Option<crate::lister::Lister>,
    /// The last batch rename: folder and (new name, old name) pairs, for undo.
    pub(crate) last_batch: Option<(PathBuf, Vec<(String, String)>)>,
    /// Where the two panes were drawn last frame (the status note floats over it).
    pub(crate) panes_rect: Option<egui::Rect>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Vec<PathBuf>) -> Self {
        let (settings, settings_error) = Settings::load();
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
            lister: None,
            last_batch: None,
            panes_rect: None,
        };
        app.rebuild_bindings();
        if let Some(e) = settings_error {
            app.set_status(e, true);
        }
        app
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

    pub(crate) fn after_move(&mut self, idx: usize, res: Result<(), String>) {
        match res {
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
        if self.job.is_some() {
            self.set_status(format!("Queued: {}", spec.label()), false);
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
                self.reload_all();
                if let Some(next) = self.queue.pop_front() {
                    self.job = Some(fs_ops::start(next));
                }
            }
        }
        if let Some(rx) = &self.size_rx {
            if let Ok((n, bytes)) = rx.try_recv() {
                self.size_rx = None;
                self.set_status(format!("{}: {}", plural(n, "item"), human_size(bytes)), false);
            }
        }
        if self.last_check.elapsed() > Duration::from_millis(1000) {
            self.last_check = Instant::now();
            for p in &mut self.panes {
                if p.changed_on_disk() {
                    p.reload();
                }
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
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
                Event::Text(t) => {
                    if std::mem::take(&mut skip_text) {
                        continue;
                    }
                    // Some systems send invisible codes with Alt and similar keys: those aren't a name search.
                    if mods_now.command || mods_now.ctrl || mods_now.alt || t.trim().is_empty() || t.chars().any(char::is_control) {
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
        // Don't let scroll areas or buttons react to the keys handled here.
        ctx.input_mut(|i| {
            let rest = i.events.split_off(consumed.min(i.events.len()));
            i.events.retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_)));
            i.events.extend(rest);
        });
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background();
        if self.lister.is_some() {
            self.lister_frame(ctx);
            self.update_title(ctx);
            self.save_if_dirty();
            return;
        }
        self.handle_keys(ctx);
        self.bottom_bar(ctx);
        self.panes_ui(ctx);
        self.modal_ui(ctx);
        for a in std::mem::take(&mut self.actions) {
            self.apply(ctx, a);
        }
        self.update_title(ctx);
        self.save_if_dirty();
        let busy = self.job.is_some() || self.size_rx.is_some();
        ctx.request_repaint_after(Duration::from_millis(if busy { 100 } else { 1000 }));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Files unpacked from archives for viewing are only needed while we run.
        let _ = std::fs::remove_dir_all(crate::actions::temp_area());
        self.store_pane_state();
        let _ = self.settings.save();
    }
}

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
