//! The stack: files and folders gathered from anywhere (Ctrl+G), shown in a
//! frame under the panes until they're dealt with. It only holds paths, never
//! the files themselves, so nothing can be lost through it. The list is kept in
//! the settings file (`stack`) and survives a restart.

use crate::theme;
use eframe::egui::{self, pos2, vec2, Align2, Key, Modifiers, Sense, Stroke};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::pane::Entry;

/// Where things from the stack go: the active pane's folder.
pub struct Dest {
    pub path: PathBuf,
    /// "right pane", "top pane"…
    pub side: &'static str,
}

/// One look at the items, taken in the background: a network folder must not
/// hold up the window.
struct Look {
    /// Each item, with what was found (None: not found).
    found: Vec<(PathBuf, Option<Entry>)>,
    /// The folder looked in for names already taken, and those items.
    dest: Option<PathBuf>,
    taken: HashSet<PathBuf>,
    free: Option<u64>,
}

/// The frame's state between frames; the list itself lives in the settings.
#[derive(Default)]
pub struct StackView {
    /// None: not found (or not answering).
    info: HashMap<PathBuf, Option<Entry>>,
    looking: Option<mpsc::Receiver<Look>>,
    last_look: Option<Instant>,
    looked_at: Vec<PathBuf>,
    looked_dest: Option<PathBuf>,
    /// Items whose name is already in the destination folder.
    taken: HashSet<PathBuf>,
    /// Free space where they go.
    free: Option<u64>,
    /// What's in each folder on the stack, added up once in the background.
    dir_sizes: HashMap<PathBuf, u64>,
    sizing: Option<mpsc::Receiver<(PathBuf, u64)>>,
    /// The keyboard is in the frame (Ctrl+Tab) rather than in a pane.
    pub focused: bool,
    pub cursor: usize,
    pub selected: HashSet<PathBuf>,
    /// Bring the cursor's row into view on the next frame (it moved by key).
    scroll_to_cursor: bool,
    /// How many rows fit, for Page Up / Down.
    rows_shown: usize,
    /// Where the cursor's row was drawn (the keyboard's menu opens there).
    pub cursor_rect: Option<egui::Rect>,
}

/// What a key or a click in the frame asks for.
#[derive(Debug, PartialEq)]
pub enum StackAction {
    /// The key was used and nothing more is needed.
    Used,
    /// Items came off the stack.
    Changed,
    /// Back to the panes (Tab, Esc).
    Leave,
    /// The Clear button: empty the stack, once that's confirmed.
    Clear,
    /// The frame was clicked: the keyboard goes there.
    Focus,
    /// Open the cursor's item (double-click), as Enter does.
    Open,
    /// The menu for the cursor's item (right-click).
    Menu(egui::Pos2),
    /// Show this item in the active pane.
    Reveal(PathBuf),
    /// Copy (or move) these to the left (0) or right (1) pane.
    Send { to: usize, mv: bool, paths: Vec<PathBuf> },
}

/// Ctrl+G: puts `paths` on the stack, or takes them off when all of them are
/// on it already. Returns how many went on and how many came off.
pub fn toggle(stack: &mut Vec<PathBuf>, paths: Vec<PathBuf>) -> (usize, usize) {
    if !paths.is_empty() && paths.iter().all(|p| stack.contains(p)) {
        let before = stack.len();
        stack.retain(|p| !paths.contains(p));
        return (0, before - stack.len());
    }
    let mut added = 0;
    for p in paths {
        if !stack.contains(&p) {
            stack.push(p);
            added += 1;
        }
    }
    (added, 0)
}

/// The commands that work while the stack has the keyboard: the ones on files
/// (they take the stack's items), and the window-wide ones. The rest act on a
/// pane, and the keyboard isn't there.
pub const WORKS_HERE: &[&str] = &[
    "open", "open_external", "row_menu", "open_with", "view", "edit", "rename", "copy_in_place", "copy", "move",
    "delete", "delete_permanently", "properties", "checksum", "copy_path", "clip_copy", "clip_cut", "drag_out",
    "pack", "unpack", "folder_size", "stack", "stack_reveal", "stack_focus", "switch_pane", "stack_clear",
    "command_palette", "go_to", "edit_settings", "help", "about", "quit", "toggle_key_bar", "toggle_stacked",
];

/// An item as a pane row, for the commands that work on one.
pub fn entry_of(p: &Path) -> Option<Entry> {
    let lmeta = std::fs::symlink_metadata(p).ok()?;
    let is_symlink = lmeta.file_type().is_symlink();
    let meta = if is_symlink { std::fs::metadata(p).unwrap_or(lmeta) } else { lmeta };
    Some(Entry {
        name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
        path: p.to_path_buf(),
        is_dir: meta.is_dir(),
        is_symlink,
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified: meta.modified().ok(),
        executable: crate::pane::is_executable(&meta),
    })
}

/// Something was renamed: the stack follows it (and anything inside it).
pub fn renamed(stack: &mut [PathBuf], from: &Path, to: &Path) -> bool {
    let mut changed = false;
    for p in stack.iter_mut() {
        if let Ok(rest) = p.strip_prefix(from) {
            *p = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
            changed = true;
        }
    }
    changed
}

impl StackView {
    /// What was last found for an item: Some(None) when it wasn't there, None
    /// when it hasn't been looked at yet.
    pub fn known(&self, p: &Path) -> Option<Option<Entry>> {
        self.info.get(p).cloned()
    }

    /// The selected items in stack order, or else the one under the cursor.
    pub fn targets(&self, stack: &[PathBuf]) -> Vec<PathBuf> {
        let picked: Vec<PathBuf> = stack.iter().filter(|p| self.selected.contains(*p)).cloned().collect();
        if !picked.is_empty() {
            return picked;
        }
        stack.get(self.cursor).cloned().into_iter().collect()
    }

    /// Keeps the cursor and selection on what's still there.
    fn tidy(&mut self, stack: &[PathBuf]) {
        self.cursor = self.cursor.min(stack.len().saturating_sub(1));
        self.selected.retain(|p| stack.contains(p));
        if stack.is_empty() {
            self.focused = false;
        }
    }

    fn move_cursor(&mut self, stack: &[PathBuf], delta: isize, extend: bool) {
        let n = stack.len() as isize;
        if n == 0 {
            return;
        }
        let from = self.cursor as isize;
        let to = (from + delta).clamp(0, n - 1);
        if extend {
            let (a, b) = if from <= to { (from, to) } else { (to, from) };
            for i in a..=b {
                self.selected.insert(stack[i as usize].clone());
            }
        }
        self.cursor = to as usize;
        self.scroll_to_cursor = true;
    }

    /// A key while the frame has the keyboard. None: not one of the stack's keys.
    /// Ctrl+arrow copies to the pane that way (Shift as well: moves); F5 / F6 to
    /// the pane last used (`active`). With the panes one above the other
    /// (`stacked`), Ctrl+↑ / ↓ do it instead. Nothing here touches the files
    /// themselves except through the Copy / Move dialog.
    pub fn key(&mut self, stack: &mut Vec<PathBuf>, key: Key, m: Modifiers, active: usize, stacked: bool) -> Option<StackAction> {
        self.tidy(stack);
        let ctrl = m.ctrl || m.command;
        let page = self.rows_shown.saturating_sub(1).max(1) as isize;
        let n = stack.len() as isize;
        let send = |to| Some(StackAction::Send { to, mv: m.shift, paths: self.targets(stack) });
        match key {
            Key::ArrowLeft if ctrl && !m.alt => return send(0),
            Key::ArrowRight if ctrl && !m.alt => return send(1),
            Key::ArrowUp if ctrl && !m.alt && stacked => return send(0),
            Key::ArrowDown if ctrl && !m.alt && stacked => return send(1),
            // Side by side, Ctrl+↑ / ↓ are just ↑ / ↓ here (Ctrl+↓ is what puts things
            // on the stack, so it's an easy key to reach for).
            Key::ArrowUp | Key::ArrowDown if ctrl && !m.alt => {
                self.move_cursor(stack, if key == Key::ArrowUp { -1 } else { 1 }, m.shift);
                return Some(StackAction::Used);
            }
            Key::F5 | Key::F6 if !ctrl && !m.alt && !m.shift => {
                return Some(StackAction::Send { to: active, mv: key == Key::F6, paths: self.targets(stack) })
            }
            _ => {}
        }
        if m.alt {
            return None;
        }
        match key {
            Key::A if ctrl => {
                if m.shift {
                    self.selected.clear();
                } else {
                    self.selected = stack.iter().cloned().collect();
                }
            }
            // Ctrl+Shift+G (clear the whole stack) is the window's, not this.
            Key::G if ctrl && !m.shift => return self.take_off(stack),
            _ if ctrl => return None,
            Key::ArrowUp => self.move_cursor(stack, -1, m.shift),
            Key::ArrowDown => self.move_cursor(stack, 1, m.shift),
            Key::PageUp => self.move_cursor(stack, -page, m.shift),
            Key::PageDown => self.move_cursor(stack, page, m.shift),
            Key::Home => self.move_cursor(stack, -n, m.shift),
            Key::End => self.move_cursor(stack, n, m.shift),
            Key::Space | Key::Insert if !m.shift => {
                if let Some(p) = stack.get(self.cursor) {
                    if !self.selected.remove(p) {
                        self.selected.insert(p.clone());
                    }
                }
                self.move_cursor(stack, 1, false);
            }
            Key::Delete | Key::Backspace => return self.take_off(stack),
            Key::Escape if !self.selected.is_empty() => self.selected.clear(),
            Key::Escape | Key::Tab => return Some(StackAction::Leave),
            _ => return None,
        }
        Some(StackAction::Used)
    }

    /// Delete / Ctrl+G in the frame: the items come off the stack (the files
    /// stay where they are).
    fn take_off(&mut self, stack: &mut Vec<PathBuf>) -> Option<StackAction> {
        let gone = self.targets(stack);
        if gone.is_empty() {
            return Some(StackAction::Used);
        }
        stack.retain(|p| !gone.contains(p));
        self.tidy(stack);
        Some(StackAction::Changed)
    }

    /// Looks the items up again every few seconds, and at once when the list (or
    /// where they'd go) changed. Folders are added up once, one after another.
    fn refresh(&mut self, stack: &[PathBuf], dest: Option<&Path>, ctx: &egui::Context) {
        if let Some(rx) = &self.sizing {
            loop {
                match rx.try_recv() {
                    Ok((p, n)) => {
                        self.dir_sizes.insert(p, n);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.sizing = None;
                        break;
                    }
                }
            }
        }
        if let Some(rx) = &self.looking {
            match rx.try_recv() {
                Ok(look) => {
                    self.info = look.found.into_iter().collect();
                    self.taken = look.taken;
                    self.free = look.free;
                    self.looked_dest = look.dest;
                    self.looking = None;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => self.looking = None,
            }
        }
        if self.sizing.is_none() {
            let to_count: Vec<PathBuf> =
                stack.iter().filter(|p| !self.dir_sizes.contains_key(*p) && self.info.get(*p).is_some_and(|e| e.as_ref().is_some_and(|e| e.is_dir))).cloned().collect();
            if !to_count.is_empty() {
                let (tx, rx) = mpsc::channel();
                let waker = ctx.clone();
                std::thread::spawn(move || {
                    for p in to_count {
                        let n = crate::fs_ops::tree_size(&p);
                        if tx.send((p, n)).is_err() {
                            return;
                        }
                        waker.request_repaint();
                    }
                });
                self.sizing = Some(rx);
            }
        }
        let due = self.last_look.is_none_or(|t| t.elapsed() > Duration::from_secs(3));
        if !due && self.looked_at == stack && self.looked_dest.as_deref() == dest {
            return;
        }
        self.last_look = Some(Instant::now());
        self.looked_at = stack.to_vec();
        self.looked_dest = dest.map(Path::to_path_buf);
        let paths = stack.to_vec();
        let dest = dest.map(Path::to_path_buf);
        let (tx, rx) = mpsc::channel();
        let waker = ctx.clone();
        std::thread::spawn(move || {
            let found = paths.iter().map(|p| (p.clone(), entry_of(p))).collect();
            let (taken, free) = match &dest {
                Some(d) => (already_there(&paths, d), crate::places::space(d).map(|s| s.0)),
                None => (HashSet::new(), None),
            };
            let _ = tx.send(Look { found, dest, taken, free });
            waker.request_repaint();
        });
        self.looking = Some(rx);
        ctx.request_repaint_after(Duration::from_secs(3));
    }

    /// An item's size: a file's own, or what's in a folder (once it's added up).
    fn size_of(&self, p: &Path) -> Option<u64> {
        match self.info.get(p)? {
            Some(e) if e.is_dir => self.dir_sizes.get(p).copied(),
            Some(e) => Some(e.size),
            None => None,
        }
    }

    /// The frame under the panes, while anything is on the stack. `dest` is where
    /// its items would go. Returns whether the list changed, and anything a click
    /// asked for.
    pub fn ui(&mut self, ctx: &egui::Context, stack: &mut Vec<PathBuf>, font_size: f32, dest: Option<Dest>) -> (bool, Option<StackAction>) {
        self.tidy(stack);
        if stack.is_empty() {
            return (false, None);
        }
        self.refresh(stack, dest.as_ref().map(|d| d.path.as_path()), ctx);
        // What was found for an older destination doesn't count for this one.
        let current = dest.is_some() && self.looked_dest.as_deref() == dest.as_ref().map(|d| d.path.as_path());
        let th = theme::c();
        let row_h = (font_size * 1.55).round();
        let header_h = row_h + 4.0;
        let mut changed = false;
        let mut action = None;
        egui::TopBottomPanel::bottom("stack")
            .resizable(true)
            .default_height(header_h + row_h * 4.0 + 12.0)
            .height_range((header_h + row_h + 12.0)..=(header_h + row_h * 20.0 + 12.0))
            .frame(egui::Frame::default().fill(th.bg).inner_margin(egui::Margin { left: 6, right: 6, top: 4, bottom: 4 }))
            .show_separator_line(false)
            .show(ctx, |ui| {
                // The accent border while the keyboard is here, as an active pane has.
                let edge = if self.focused { th.accent } else { th.border };
                let frame = egui::Frame::default().fill(th.pane).stroke(Stroke::new(1.0_f32, edge)).corner_radius(4.0);
                frame.show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    let small = egui::TextStyle::Small.resolve(ui.style());
                    let body = egui::FontId::proportional(font_size);
                    // Header: what's on it, where it would go, and a way to empty it.
                    let total: u64 = stack.iter().filter_map(|p| self.size_of(p)).sum();
                    let counting = self.sizing.is_some();
                    let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), header_h), Sense::hover());
                    ui.painter().rect_filled(head, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, th.header);
                    let items = if stack.len() == 1 { "1 item".to_string() } else { format!("{} items", stack.len()) };
                    let size = match (total, counting) {
                        (0, false) => String::new(),
                        (0, true) => "  ·  counting…".to_string(),
                        (n, c) => format!("  ·  {}{}", crate::pane::human_size(n), if c { "…" } else { "" }),
                    };
                    let title = if self.focused { th.accent } else { th.text };
                    let y = head.center().y;
                    let title_r = ui.painter().text(pos2(head.left() + 10.0, y), Align2::LEFT_CENTER, format!("Stack  ·  {items}{size}"), body.clone(), title);
                    let clear_w = 54.0;
                    let clear = egui::Rect::from_center_size(pos2(head.right() - 8.0 - clear_w / 2.0, y), vec2(clear_w, header_h - 8.0));
                    let mut room = egui::Rect::from_min_max(pos2(title_r.right() + 20.0, head.top()), pos2(clear.left() - 12.0, head.bottom()));
                    let width_of = |ui: &egui::Ui, t: &str| ui.fonts(|f| f.layout_no_wrap(t.to_string(), small.clone(), th.dim).size().x);
                    // Where it goes, and whether it fits there.
                    if let Some(d) = &dest {
                        let free = self.free.filter(|_| current);
                        let too_big = free.is_some_and(|f| total > f);
                        let space = match free {
                            Some(f) if too_big => format!("  ·  won't fit: {} free", crate::pane::human_size(f)),
                            Some(f) => format!("  ·  {} free", crate::pane::human_size(f)),
                            None => String::new(),
                        };
                        let after = format!(" ({}){space}", d.side);
                        let path_w = room.width() - width_of(ui, "→ ") - width_of(ui, &after);
                        let path = crate::dialogs::fit_end(ui, &crate::app::tilde(&d.path), path_w, &small);
                        if !path.is_empty() {
                            let color = if too_big { th.error } else { th.dim };
                            let r = ui.painter().text(room.left_center(), Align2::LEFT_CENTER, format!("→ {path}{after}"), small.clone(), color);
                            room.min.x = r.right() + 24.0;
                        }
                    }
                    let hint = if self.focused { "Ctrl+← / → copies there · Shift moves · Del takes off · Tab goes back" } else { "Ctrl+↓ adds · Ctrl+Tab goes here" };
                    if room.width() >= width_of(ui, hint) {
                        ui.painter().text(room.right_center(), Align2::RIGHT_CENTER, hint, small.clone(), th.dim);
                    }
                    if ui.put(clear, egui::Button::new(egui::RichText::new("Clear").small())).on_hover_text("Empty the stack (Ctrl+Shift+G; the files stay where they are)").clicked() {
                        action = Some(StackAction::Clear);
                    }
                    self.rows_shown = ((ui.available_height() / row_h) as usize).max(1);
                    let scroll_to = std::mem::take(&mut self.scroll_to_cursor);
                    // The items, newest last.
                    let mut remove = None;
                    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(!self.focused).show(ui, |ui| {
                        let icons = theme::icon_font(ui.ctx(), font_size);
                        for (i, p) in stack.iter().enumerate() {
                            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                            if scroll_to && i == self.cursor {
                                ui.scroll_to_rect(rect, None);
                            }
                            if i == self.cursor {
                                self.cursor_rect = Some(rect);
                            }
                            if !ui.is_rect_visible(rect) {
                                continue;
                            }
                            let info = self.info.get(p).cloned();
                            let missing = matches!(info, Some(None));
                            let info = info.flatten();
                            let paint = ui.painter_at(rect);
                            if let Some(stripe) = th.stripe.filter(|_| i % 2 == 1) {
                                paint.rect_filled(rect, 0.0, stripe);
                            }
                            let selected = self.selected.contains(p);
                            let bg = rect.shrink2(vec2(2.0, 0.0));
                            if self.focused && i == self.cursor {
                                paint.rect_filled(bg, 3.0, th.cursor);
                            } else if selected {
                                paint.rect_filled(bg, 3.0, th.selected.gamma_multiply(0.12));
                            } else if resp.hovered() {
                                paint.rect_filled(bg, 3.0, th.text.gamma_multiply(0.05));
                            }
                            let is_dir = info.as_ref().is_some_and(|i| i.is_dir);
                            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
                            let color = if selected { th.selected } else if missing { th.dim } else if is_dir { th.folder } else { th.text };
                            let y = rect.center().y;
                            match &icons {
                                Some(f) => {
                                    let (exe, link) = info.as_ref().map_or((false, false), |e| (e.executable, e.is_symlink));
                                    let icon = crate::icons::icon(&name, is_dir, exe, link);
                                    paint.text(pos2(rect.left() + 10.0, y), Align2::LEFT_CENTER, icon, f.clone(), color.gamma_multiply(0.85));
                                }
                                None => {
                                    paint.text(pos2(rect.left() + 10.0, y), Align2::LEFT_CENTER, if is_dir { "🗀" } else { "🗋" }, body.clone(), color.gamma_multiply(0.8));
                                }
                            }
                            let x_rect = egui::Rect::from_center_size(pos2(rect.right() - 16.0, y), vec2(row_h, row_h));
                            let right = x_rect.left() - 10.0;
                            let name_left = rect.left() + 10.0 + font_size * 1.6;
                            let folder = p.parent().map(crate::app::tilde).unwrap_or_default();
                            let size = if missing {
                                "not found".to_string()
                            } else {
                                match self.size_of(p) {
                                    Some(n) => crate::pane::human_size(n),
                                    None if is_dir && counting => "…".to_string(),
                                    None => String::new(),
                                }
                            };
                            // Its name is already where it would go (or it's in that very folder).
                            let in_dest = current && dest.as_ref().is_some_and(|d| p.parent() == Some(d.path.as_path()));
                            let taken = current && self.taken.contains(p);
                            // A mark rather than words, so the name keeps its room; the tooltip says it.
                            let tag = if in_dest { Some(("✓", th.dim)) } else if taken { Some(("⚠", th.error)) } else { None };
                            let wide = rect.width() >= 640.0;
                            let name_right = if wide {
                                // Columns: Name | From | Size | Modified.
                                let date_left = right - 130.0;
                                let size_right = date_left - 14.0;
                                let size_left = size_right - 80.0;
                                let from_left = name_left + ((size_left - name_left) * 0.45).max(120.0);
                                if let Some(t) = info.as_ref().and_then(|e| e.modified) {
                                    let dt: chrono::DateTime<chrono::Local> = t.into();
                                    paint.text(pos2(right, y), Align2::RIGHT_CENTER, dt.format("%Y-%m-%d %H:%M").to_string(), small.clone(), th.dim);
                                }
                                paint.with_clip_rect(egui::Rect::from_x_y_ranges(size_left..=size_right, rect.y_range())).text(pos2(size_right, y), Align2::RIGHT_CENTER, size, small.clone(), th.dim);
                                let from_w = size_left - 14.0 - from_left;
                                let shown = crate::dialogs::fit_end(ui, &folder, from_w, &small);
                                paint.text(pos2(from_left, y), Align2::LEFT_CENTER, shown, small.clone(), th.dim);
                                from_left - 14.0
                            } else {
                                let tail_w = paint.text(pos2(right, y), Align2::RIGHT_CENTER, size, small.clone(), th.dim).width();
                                right - tail_w - 16.0
                            };
                            // The tag at the end of the name's room, the name in what's left.
                            let name_right = match tag {
                                Some((t, c)) => {
                                    let r = paint.text(pos2(name_right, y), Align2::RIGHT_CENTER, t, body.clone(), c);
                                    r.left() - 10.0
                                }
                                None => name_right,
                            };
                            let name_clip = egui::Rect::from_x_y_ranges(name_left..=name_right.max(name_left), rect.y_range());
                            let name_r = paint.with_clip_rect(name_clip).text(pos2(name_left, y), Align2::LEFT_CENTER, &name, body.clone(), color);
                            if !wide {
                                // Where it lives, dimmed, in what room is left.
                                let folder_left = name_r.right() + 14.0;
                                if folder_left < name_right {
                                    let clip = egui::Rect::from_min_max(pos2(folder_left, rect.top()), pos2(name_right, rect.bottom()));
                                    paint.with_clip_rect(clip).text(pos2(folder_left, y), Align2::LEFT_CENTER, folder, small.clone(), th.dim);
                                }
                            }
                            let x = ui.interact(x_rect, ui.id().with(("stack_x", i)), Sense::click());
                            if resp.hovered() || x.hovered() {
                                let c = if x.hovered() { th.error } else { th.dim };
                                paint.text(x_rect.center(), Align2::CENTER_CENTER, "×", body.clone(), c);
                            }
                            if x.on_hover_text("Take it off the stack (the file stays where it is)").clicked() {
                                remove = Some(i);
                            } else if resp.double_clicked() {
                                self.cursor = i;
                                action = Some(StackAction::Open);
                            } else if resp.secondary_clicked() {
                                self.cursor = i;
                                let at = resp.interact_pointer_pos().unwrap_or(rect.left_bottom());
                                action = Some(StackAction::Menu(at));
                            } else if resp.clicked() {
                                // A click puts the keyboard here; Ctrl+click picks it too.
                                self.cursor = i;
                                if ui.input(|inp| inp.modifiers.command || inp.modifiers.ctrl) && !self.selected.remove(p) {
                                    self.selected.insert(p.clone());
                                }
                                action = Some(StackAction::Focus);
                            }
                            let note = match (tag, &dest) {
                                (Some(("⚠", _)), Some(d)) => format!("\n⚠ “{name}” is already in {}: copying asks what to do", crate::app::tilde(&d.path)),
                                (Some(("✓", _)), Some(d)) => format!("\n✓ It's in {} already", crate::app::tilde(&d.path)),
                                _ => String::new(),
                            };
                            let _ = resp.on_hover_text_at_pointer(format!("{}{note}\nDouble-click opens it · right-click for more · Ctrl+← / → copies it to that pane", p.display()));
                        }
                    });
                    if let Some(i) = remove {
                        stack.remove(i);
                        changed = true;
                    }
                    self.tidy(stack);
                });
            });
        (changed, action)
    }
}

/// The items whose name is already taken in `dest` (not counting ones that are
/// in `dest` themselves). Hidden files count: they'd be overwritten all the same.
fn already_there(items: &[PathBuf], dest: &Path) -> HashSet<PathBuf> {
    items
        .iter()
        .filter(|p| p.parent() != Some(dest))
        .filter(|p| p.file_name().is_some_and(|n| dest.join(n).symlink_metadata().is_ok()))
        .cloned()
        .collect()
}

/// The folder to show for an item, and the name to put the cursor on.
pub fn reveal_target(p: &Path) -> Option<(PathBuf, String)> {
    Some((p.parent()?.to_path_buf(), p.file_name()?.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws the frame headlessly and clicks the first item's ×.
    #[test]
    fn the_frame_shows_and_x_takes_an_item_off() {
        use egui::{Event, PointerButton, RawInput, Rect};
        let ctx = egui::Context::default();
        let (a, b) = (PathBuf::from("/nowhere/a.txt"), PathBuf::from("/nowhere/b"));
        let mut stack = vec![a, b.clone()];
        let mut view = StackView::default();
        let mut x_at = None;
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        for step in 0..4 {
            let events = match (step, x_at) {
                (1, Some(p)) => vec![Event::PointerMoved(p)],
                (2, Some(p)) => vec![button(p, true)],
                (3, Some(p)) => vec![button(p, false)],
                _ => vec![],
            };
            let input = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 600.0))), events, ..Default::default() };
            let mut panel = Rect::NOTHING;
            let _ = ctx.run(input, |ctx| {
                let (changed, _) = view.ui(ctx, &mut stack, 14.0, None);
                assert_eq!(changed, step == 3, "only the click changes the list");
                panel = ctx.available_rect();
                egui::CentralPanel::default().show(ctx, |_| {});
            });
            // The first row is just under the header; its × is at the right end.
            assert!(panel.bottom() < 600.0, "the frame takes room under the panes");
            let row_h = (14.0_f32 * 1.55).round();
            x_at = Some(pos2(1000.0 - 6.0 - 1.0 - 16.0, panel.bottom() + 4.0 + 1.0 + row_h + 4.0 + row_h / 2.0));
        }
        assert_eq!(stack, [b]);
    }

    #[test]
    fn keys_in_the_stack() {
        let (a, b, c) = (PathBuf::from("/x/a"), PathBuf::from("/y/b"), PathBuf::from("/z/c"));
        let mut s = vec![a.clone(), b.clone(), c.clone()];
        let mut v = StackView { focused: true, rows_shown: 5, ..Default::default() };
        let none = Modifiers::NONE;
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Default::default() };
        // Nothing picked: the cursor's item goes. Ctrl+← copies left, with Shift moves right.
        assert_eq!(v.key(&mut s, Key::ArrowLeft, ctrl, 1, false), Some(StackAction::Send { to: 0, mv: false, paths: vec![a.clone()] }));
        assert_eq!(v.key(&mut s, Key::Space, none, 1, false), Some(StackAction::Used));
        assert_eq!(v.cursor, 1);
        assert_eq!(v.key(&mut s, Key::End, none, 1, false), Some(StackAction::Used));
        assert_eq!(v.key(&mut s, Key::Insert, none, 1, false), Some(StackAction::Used));
        // Picked items go in stack order.
        assert_eq!(v.key(&mut s, Key::ArrowRight, ctrl_shift, 1, false), Some(StackAction::Send { to: 1, mv: true, paths: vec![a.clone(), c.clone()] }));
        assert_eq!(v.key(&mut s, Key::F6, none, 0, false), Some(StackAction::Send { to: 0, mv: true, paths: vec![a.clone(), c.clone()] }));
        // Enter is left for the window: it opens, as in a pane.
        assert_eq!(v.key(&mut s, Key::Enter, none, 0, false), None);
        // Esc lets go of the picks first, then leaves.
        assert_eq!(v.key(&mut s, Key::Escape, none, 0, false), Some(StackAction::Used));
        assert!(v.selected.is_empty());
        assert_eq!(v.key(&mut s, Key::Escape, none, 0, false), Some(StackAction::Leave));
        assert_eq!(v.key(&mut s, Key::Tab, none, 0, false), Some(StackAction::Leave));
        // Delete takes it off the stack; the cursor stays on the list.
        assert_eq!(v.key(&mut s, Key::Delete, none, 0, false), Some(StackAction::Changed));
        assert_eq!(s, [a.clone(), b.clone()]);
        assert_eq!(v.cursor, 1);
        // Ctrl+Shift+G is left for "Clear the stack" (which asks first).
        assert_eq!(v.key(&mut s, Key::G, ctrl_shift, 0, false), None);
        assert_eq!(s.len(), 2);
        // Ctrl+A picks everything; Ctrl+G takes it all off and the keyboard leaves.
        assert_eq!(v.key(&mut s, Key::A, ctrl, 0, false), Some(StackAction::Used));
        assert_eq!(v.key(&mut s, Key::G, ctrl, 0, false), Some(StackAction::Changed));
        assert!(s.is_empty() && !v.focused);
        // Other keys are left for the window.
        assert_eq!(v.key(&mut s, Key::F3, none, 0, false), None);
    }

    /// The ⚠ mark ("already there"): a name taken in the destination (hidden ones
    /// too), but not an item that's in that very folder.
    #[test]
    fn names_already_in_the_destination() {
        let base = std::env::temp_dir().join(format!("duopane-stack-there-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("from")).unwrap();
        std::fs::create_dir_all(base.join("to")).unwrap();
        for f in ["from/a.txt", "from/.hidden", "from/new.txt", "to/a.txt", "to/.hidden", "to/here.txt"] {
            std::fs::write(base.join(f), b"x").unwrap();
        }
        let items: Vec<PathBuf> = ["from/a.txt", "from/.hidden", "from/new.txt", "to/here.txt"].iter().map(|f| base.join(f)).collect();
        let taken = already_there(&items, &base.join("to"));
        assert_eq!(taken, [base.join("from/a.txt"), base.join("from/.hidden")].into_iter().collect());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ctrl_g_adds_and_takes_off() {
        let (a, b, c) = (PathBuf::from("/x/a"), PathBuf::from("/y/b"), PathBuf::from("/z/c"));
        let mut s = Vec::new();
        assert_eq!(toggle(&mut s, vec![a.clone(), b.clone()]), (2, 0));
        // Some new, some there already: the new ones are added, nothing taken off.
        assert_eq!(toggle(&mut s, vec![b.clone(), c.clone()]), (1, 0));
        assert_eq!(s, [a.clone(), b.clone(), c.clone()]);
        // All on it already: they come off.
        assert_eq!(toggle(&mut s, vec![a.clone(), c.clone()]), (0, 2));
        assert_eq!(s, std::slice::from_ref(&b));
        assert_eq!(toggle(&mut s, vec![]), (0, 0));
        assert_eq!(reveal_target(&b), Some((PathBuf::from("/y"), "b".into())));
        // A rename (of the item, or a folder it's in) carries the stack along.
        let mut s = vec![PathBuf::from("/y/b"), PathBuf::from("/y/b/inner.txt"), PathBuf::from("/y/bee")];
        assert!(renamed(&mut s, Path::new("/y/b"), Path::new("/y/c")));
        assert_eq!(s, [PathBuf::from("/y/c"), PathBuf::from("/y/c/inner.txt"), PathBuf::from("/y/bee")]);
    }
}
