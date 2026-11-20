//! Drawing the main window: the two panes and the bars at the bottom.

use crate::app::{plural, Action, App};
use crate::pane::{human_size, Pane, SortKey};
use crate::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Key, Sense, Stroke};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Files being dragged with the mouse from one of the panes.
pub(crate) struct DragPayload {
    pub from: usize,
    pub paths: Vec<PathBuf>,
}

impl App {
    pub(crate) fn bottom_bar(&mut self, ctx: &egui::Context) {
        if self.settings.show_key_bar {
            self.key_bar(ctx);
        }
        self.status_bar(ctx);
    }

    /// The function-key bar at the very bottom (can be hidden in Appearance).
    fn key_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("keys")
            .frame(egui::Frame::default().fill(theme::c().header).inner_margin(egui::Margin::symmetric(8, 4)))
            .show(ctx, |ui| {
                // Holding Alt, Shift or Ctrl shows what the F-keys do with that key,
                // like Double Commander's function-key bar.
                let m = ui.input(|i| i.modifiers);
                if m.alt || m.shift || m.ctrl || m.command {
                    self.modifier_key_bar(ui, m);
                    return;
                }
                {
                    let keys: [(&str, &str, &'static str); 10] = [
                        ("F2", "Rename", "rename"),
                        ("F3", "View", "view"),
                        ("F4", "Edit", "edit"),
                        ("F5", "Copy", "copy"),
                        ("F6", "Move", "move"),
                        ("F7", "New folder", "new_folder"),
                        ("F8", "Trash", "delete"),
                        ("Ctrl+P", "Go to", "go_to"),
                        ("Ctrl+Shift+P", "Commands", "command_palette"),
                        ("F12", "Appearance", "appearance"),
                    ];
                    let entries = keys.iter().map(|(k, label, id)| (k.to_string(), label.to_string(), Action::RunId(id))).collect();
                    self.key_row(ui, entries, None);
                }
            });

    }

    /// The function-key bar while a modifier key is held: every F-key that has a
    /// command with exactly these modifiers (from the current key bindings).
    fn modifier_key_bar(&mut self, ui: &mut egui::Ui, m: egui::Modifiers) {
        use crate::app::CmdRef;
        use crate::commands::Shortcut;
        use egui::Key;
        const F_KEYS: [Key; 12] =
            [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12];
        let mut entries: Vec<(String, String, CmdRef)> = Vec::new();
        for key in F_KEYS {
            let sc = Shortcut::from_event(key, m);
            if let Some(&cmd) = self.bindings.get(&sc) {
                let label = match cmd {
                    CmdRef::Builtin(i) => short_label(self.commands[i].id).map(str::to_string).unwrap_or_else(|| {
                        self.commands[i].title.trim_end_matches('…').to_string()
                    }),
                    CmdRef::Custom(i) => self.settings.custom_commands.get(i).map(|c| c.title.clone()).unwrap_or_default(),
                };
                entries.push((sc.label(), label, cmd));
            }
        }
        let empty = entries.is_empty().then(|| {
            let held = Shortcut { key: Key::F1, ctrl: m.ctrl || m.command, shift: m.shift, alt: m.alt }.label();
            let held = held.trim_end_matches("F1").trim_end_matches('+').to_string();
            format!("No F-key commands with {held}")
        });
        let entries = entries.into_iter().map(|(k, label, cmd)| (k, label, Action::Run(cmd))).collect();
        self.key_row(ui, entries, empty);
    }

    /// One line of key + button pairs, always the same height. Pairs that don't fit
    /// are left out and "…" shows there are more (widen the window to see them).
    fn key_row(&mut self, ui: &mut egui::Ui, entries: Vec<(String, String, Action)>, empty: Option<String>) {
        let th = theme::c();
        let small = egui::TextStyle::Small.resolve(ui.style());
        let body = egui::TextStyle::Body.resolve(ui.style());
        let gap = ui.spacing().item_spacing.x;
        let pad = ui.spacing().button_padding.x * 2.0;
        let width = |ui: &egui::Ui, t: &str, f: &FontId| ui.fonts(|fo| fo.layout_no_wrap(t.to_string(), f.clone(), Color32::WHITE).size().x);
        let more_w = width(ui, "…", &body) + gap + 4.0;
        ui.horizontal(|ui| {
            ui.set_min_height(ui.spacing().interact_size.y);
            if let Some(msg) = empty {
                ui.label(egui::RichText::new(msg).color(th.dim).small());
                return;
            }
            let n = entries.len();
            for (i, (k, label, action)) in entries.into_iter().enumerate() {
                let w = width(ui, &k, &small) + width(ui, &label, &body) + pad + gap * 3.0 + 8.0;
                let reserve = if i + 1 < n { more_w } else { 0.0 };
                if w + reserve > ui.available_width() {
                    ui.label(egui::RichText::new("…").color(th.dim));
                    break;
                }
                ui.label(egui::RichText::new(k).color(th.dim).small());
                if ui.add(egui::Button::new(label).small()).clicked() {
                    self.actions.push(action);
                }
                ui.add_space(8.0);
            }
        });
    }

    /// Messages, progress of file operations and the name search.
    fn status_bar(&mut self, ctx: &egui::Context) {
        // Only take up room when there's something to say.
        let searching = !self.quick.is_empty() && self.quick_at.elapsed() < Duration::from_millis(1200);
        let message = self.status.as_ref().is_some_and(|s| s.at.elapsed() < Duration::from_secs(if s.error { 8 } else { 5 }));
        if self.job.is_none() && !searching && !message {
            return;
        }
        // A small note floating over the bottom-left of the panes, so nothing in the
        // window moves when a message comes and goes.
        let area = self.panes_rect.unwrap_or_else(|| ctx.screen_rect());
        let pos = egui::pos2(area.left() + 16.0, area.bottom() - 34.0);
        egui::Area::new(egui::Id::new("status_note"))
            .order(egui::Order::Foreground)
            .pivot(Align2::LEFT_BOTTOM)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(theme::c().header)
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(job) = &self.job {
                        let in_bytes = job.label.starts_with("Copying")
                            || job.label.starts_with("Extracting")
                            || job.label.starts_with("Packing")
                            || job.label.starts_with("Unpacking")
                            || (job.label.starts_with("Moving") && !job.label.contains("trash"));
                        let text = if in_bytes {
                            format!("{} — {} of {}", job.label, human_size(job.done), human_size(job.total))
                        } else {
                            format!("{} — {} of {}", job.label, job.done, job.total)
                        };
                        ui.add(egui::ProgressBar::new(job.fraction()).desired_width(320.0).text(text));
                        let current = Path::new(&job.current)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        ui.label(egui::RichText::new(current).color(theme::c().dim).small());
                        if ui.button("Cancel").clicked() {
                            self.actions.push(Action::RunId("cancel_job"));
                        }
                        if !self.queue.is_empty() {
                            ui.label(egui::RichText::new(format!("+{} queued", self.queue.len())).color(theme::c().dim));
                        }
                    } else if !self.quick.is_empty() && self.quick_at.elapsed() < Duration::from_millis(1200) {
                        ui.label(egui::RichText::new(format!("Find: {}", self.quick)).color(theme::c().accent));
                    } else if let Some(s) = &self.status {
                        if s.at.elapsed() < Duration::from_secs(if s.error { 8 } else { 5 }) {
                            ui.label(egui::RichText::new(&s.text).color(if s.error { theme::c().error } else { theme::c().ok }));
                        }
                    }
                });
                    });
            });
    }

    pub(crate) fn panes_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(theme::c().bg).inner_margin(egui::Margin { left: 6, right: 6, top: 6, bottom: 2 }))
            .show(ctx, |ui| {
                self.panes_rect = Some(ui.max_rect());
                ui.spacing_mut().item_spacing.x = 6.0;
                let active = self.active;
                let b0 = self.settings.bookmarks.contains(&self.panes[0].path);
                let b1 = self.settings.bookmarks.contains(&self.panes[1].path);
                let (left, right) = self.panes.split_at_mut(1);
                let actions = &mut self.actions;
                let quick_view = &mut self.quick_view;
                ui.columns(2, |cols| {
                    let panes = [(&mut left[0], b0), (&mut right[0], b1)];
                    for (idx, (col, (pane, bookmarked))) in cols.iter_mut().zip(panes).enumerate() {
                        match quick_view {
                            Some(qv) if qv.slot == idx => qv.ui(col),
                            _ => pane_ui(col, pane, idx, active == idx, bookmarked, actions),
                        }
                    }
                });
            });
        drag_label(ctx);
    }
}

/// Draws one pane: path bar, column headers, the list and a summary line.
fn pane_ui(ui: &mut egui::Ui, pane: &mut Pane, idx: usize, active: bool, bookmarked: bool, actions: &mut Vec<Action>) {
    let frame = egui::Frame::default()
        .fill(theme::c().pane)
        .stroke(Stroke::new(1.0_f32, if active { theme::c().accent } else { theme::c().border }))
        .corner_radius(4.0);
    let resp = frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let full_w = ui.available_width();
        let body_size = ui.style().text_styles[&egui::TextStyle::Body].size;
        let font = FontId::proportional(body_size);
        let small = FontId::proportional(body_size - 1.5);

        path_bar(ui, pane, idx, active, bookmarked, full_w, actions);

        // Size the columns from the text they hold, so they never run into each other.
        let widths = ui.fonts(|f| {
            let w = |s: &str| f.layout_no_wrap(s.to_string(), small.clone(), Color32::WHITE).size().x;
            ColWidths { size: w("888.8 MB") + 16.0, date: w("8888-88-88 88:88").max(w("Modified") + 14.0) + 28.0 }
        });

        // --- column headers ---
        let (head_rect, _) = ui.allocate_exact_size(vec2(full_w, theme::row_height(body_size)), Sense::hover());
        let cols = columns(head_rect, &widths);
        for (rect, key, label, left) in [
            (cols.0, SortKey::Name, "Name", true),
            (cols.1, SortKey::Size, "Size", false),
            (cols.2, SortKey::Modified, "Modified", false),
        ] {
            let r = ui.interact(rect, egui::Id::new(("head", idx, label)), Sense::click());
            let sorted = pane.sort == key || (left && pane.sort == SortKey::Extension);
            // Leave room on the right of right-aligned labels for the sort arrow.
            let (x, align) = if left {
                (rect.left() + name_indent(body_size), Align2::LEFT_CENTER)
            } else {
                (rect.right() - if sorted { 16.0 } else { 4.0 }, Align2::RIGHT_CENTER)
            };
            let color = if r.hovered() || sorted { theme::c().text } else { theme::c().dim };
            let shown = if left && pane.sort == SortKey::Extension { "Name (by type)" } else { label };
            let text_rect = ui.painter().text(pos2(x, rect.center().y), align, shown, small.clone(), color);
            if sorted {
                sort_arrow(ui.painter(), pos2(text_rect.right() + 8.0, rect.center().y), pane.ascending, color);
            }
            if r.clicked() {
                pane.set_sort(key);
                actions.push(Action::Activate(idx));
            }
        }
        ui.painter().hline(head_rect.x_range(), head_rect.bottom(), Stroke::new(1.0_f32, theme::c().border));

        // --- the list ---
        let footer_h = 22.0;
        let row_h = theme::row_height(body_size);
        // Show whole rows only, and keep the separator a few pixels below the list,
        // so the line never runs through a half-visible name.
        let gap = 4.0;
        let room = (ui.available_height() - footer_h - gap).max(row_h);
        let list_h = (room / row_h).floor().max(1.0) * row_h;
        let spare = room - list_h;
        let n = pane.entries.len();
        let mut area = egui::ScrollArea::vertical()
            .id_salt(("list", idx))
            .auto_shrink([false, false])
            .max_height(list_h)
            .min_scrolled_height(list_h);
        if pane.scroll_to_cursor {
            // Scroll just enough to keep the cursor row visible.
            let top = pane.cursor as f32 * row_h;
            let bottom = top + row_h;
            let vh = pane.view_height.max(row_h);
            let mut off = pane.scroll_offset;
            if top < off {
                off = top;
            } else if bottom > off + vh {
                off = bottom - vh;
            }
            area = area.vertical_scroll_offset(off.max(0.0));
            pane.scroll_to_cursor = false;
        }
        let out = area.show_rows(ui, row_h, n.max(1), |ui, range| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if n == 0 {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                let (msg, color) = match &pane.error {
                    Some(e) => (e.as_str(), theme::c().error),
                    None => ("This folder is empty", theme::c().dim),
                };
                ui.painter().text(pos2(rect.left() + name_indent(body_size), rect.center().y), Align2::LEFT_CENTER, msg, small.clone(), color);
                return;
            }
            for i in range {
                row_ui(ui, pane, i, idx, active, &font, &small, &widths, row_h, actions);
            }
        });
        pane.scroll_offset = out.state.offset.y;
        pane.view_height = out.inner_rect.height();

        // --- footer ---
        ui.add_space(spare + gap);
        let (foot_rect, _) = ui.allocate_exact_size(vec2(full_w, footer_h), Sense::hover());
        ui.painter().hline(foot_rect.x_range(), foot_rect.top(), Stroke::new(1.0_f32, theme::c().border));
        let (dirs, files, sel, bytes) = pane.summary();
        let text = if sel > 0 {
            format!("{sel} of {} selected · {}", dirs + files, human_size(bytes))
        } else {
            format!("{} · {}", plural(dirs, "folder"), plural(files, "file"))
        };
        let hidden = if pane.show_hidden { "" } else { "   (hidden files not shown: Ctrl+.)" };
        let (text, hidden) = match pane.loading_path() {
            Some(p) => (format!("Loading {}…", crate::app::tilde(p)), ""),
            None => (text, hidden),
        };
        ui.painter().text(
            pos2(foot_rect.left() + 10.0, foot_rect.center().y),
            Align2::LEFT_CENTER,
            format!("{text}{hidden}"),
            small.clone(),
            if sel > 0 { theme::c().selected } else { theme::c().dim },
        );
    });
    if resp.response.contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
        actions.push(Action::Activate(idx));
    }
    // Files dragged over from the other pane (or from another app) go into this
    // pane's folder. Folder rows take drops themselves (see `row_ui`).
    let from_other = |p: &DragPayload| p.from != idx;
    let incoming = resp.response.dnd_hover_payload::<DragPayload>().is_some_and(|p| from_other(&p))
        || (resp.response.contains_pointer() && ui.input(|i| !i.raw.hovered_files.is_empty()));
    if incoming && pane.archive.is_none() {
        ui.painter().rect_stroke(resp.response.rect, 4.0, Stroke::new(2.0_f32, theme::c().accent), egui::StrokeKind::Inside);
    }
    if let Some(p) = resp.response.dnd_release_payload::<DragPayload>().filter(|p| from_other(p)) {
        let mv = ui.input(|i| i.modifiers.shift);
        actions.push(Action::Drop { from: Some(p.from), to: idx, paths: p.paths.clone(), dest: pane.path.clone(), mv });
    }
}

/// While files are dragged, a small label by the pointer says what's dragged, and
/// the pointer says whether a drop copies (with a plus) or moves (Shift held),
/// like Double Commander. The label never changes size, so it doesn't jump.
/// (egui itself cancels the drag on Escape or a release over nothing. During a
/// drag run by the desktop, the desktop draws the pointer; see `native_drag.rs`.)
fn drag_label(ctx: &egui::Context) {
    let Some(p) = egui::DragAndDrop::payload::<DragPayload>(ctx) else { return };
    let Some(pos) = ctx.pointer_interact_pos() else { return };
    let mv = ctx.input(|i| i.modifiers.shift);
    ctx.set_cursor_icon(if mv { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Copy });
    let what = crate::app::describe(&p.paths);
    egui::Area::new(egui::Id::new("drag_label"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + vec2(16.0, 12.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(theme::c().header).show(ui, |ui| {
                ui.label(egui::RichText::new(what).color(theme::c().text));
            });
        });
}

/// The home folder, as a real path (it may be reached through a link, e.g.
/// /home -> /var/home), so it matches the panes' paths.
fn home_dir() -> Option<&'static Path> {
    static HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| dirs::home_dir().map(|h| std::fs::canonicalize(&h).unwrap_or(h))).as_deref()
}

/// The clickable folder path at the top of a pane (Ctrl+L to type one).
fn path_bar(ui: &mut egui::Ui, pane: &mut Pane, idx: usize, active: bool, bookmarked: bool, w: f32, actions: &mut Vec<Action>) {
    let (bar_rect, _) = ui.allocate_exact_size(vec2(w, 30.0), Sense::hover());
    ui.painter().rect_filled(bar_rect, 4.0, theme::c().header);
    let mut bar = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bar_rect.shrink2(vec2(8.0, 3.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    if let Some(text) = &mut pane.editing_path {
        let r = bar.add(egui::TextEdit::singleline(text).id(egui::Id::new(("path_edit", idx))).desired_width(f32::INFINITY));
        let (enter, esc) = bar.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if r.lost_focus() || esc {
            if enter && !esc {
                actions.push(Action::Navigate(idx, crate::app::expand_tilde(text.trim())));
            }
            pane.editing_path = None;
        } else if !r.has_focus() {
            r.request_focus();
        }
        return;
    }
    if let Some(r) = &pane.results {
        let n = pane.entries.iter().filter(|e| !e.is_up()).count();
        let text = egui::RichText::new(format!("🔍  Search results for “{}” in {}  ·  {}", r.query, crate::app::tilde(&r.root), plural(n, "item")))
            .color(if active { theme::c().text } else { theme::c().dim });
        let b = bar.add(egui::Button::new(text).frame(false)).on_hover_text("Back to the folder (Backspace)");
        if b.clicked() {
            actions.push(Action::Navigate(idx, r.root.clone()));
        }
        return;
    }
    egui::ScrollArea::horizontal()
        .id_salt(("crumbs", idx))
        .stick_to_right(true)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(&mut bar, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let parts: Vec<PathBuf> = pane.path.ancestors().map(Path::to_path_buf).collect();
                let n = parts.len();
                // Inside the home folder the path starts at "~" (like everywhere else in
                // Duopane), rather than spelling out /home/<name>.
                let home = home_dir().filter(|h| pane.path.starts_with(h) && h.parent().is_some());
                let skip = home.map_or(0, |h| h.ancestors().count() - 1);
                for (i, p) in parts.into_iter().rev().enumerate().skip(skip) {
                    let label = if i == 0 {
                        "/".to_string()
                    } else if i == skip {
                        "~".to_string()
                    } else {
                        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                    };
                    let last = i + 1 == n;
                    let color = if last && active { theme::c().text } else { theme::c().dim };
                    let mut text = egui::RichText::new(label).color(color);
                    if last {
                        text = text.strong();
                    }
                    if ui.add(egui::Button::new(text).frame(false)).clicked() {
                        actions.push(Action::Navigate(idx, p.clone()));
                    }
                    if !last && i > 0 {
                        ui.label(egui::RichText::new("›").color(theme::c().dim));
                    }
                }
                if bookmarked {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("★").color(theme::c().selected)).on_hover_text("Bookmarked");
                }
            });
        });
}

#[allow(clippy::too_many_arguments)]
fn row_ui(
    ui: &mut egui::Ui,
    pane: &mut Pane,
    i: usize,
    idx: usize,
    active: bool,
    font: &FontId,
    small: &FontId,
    widths: &ColWidths,
    row_h: f32,
    actions: &mut Vec<Action>,
) {
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
    let Some(e) = pane.entries.get(i) else { return };
    let selected = pane.selected.contains(&e.name);
    let painter = ui.painter_at(rect);
    let bg = rect.shrink2(vec2(2.0, 0.0));
    if let Some(stripe) = theme::c().stripe.filter(|_| i % 2 == 1) {
        painter.rect_filled(rect, 0.0, stripe);
    }
    if i == pane.cursor {
        painter.rect_filled(bg, 3.0, if active { theme::c().cursor } else { theme::c().cursor_inactive });
    } else if selected {
        painter.rect_filled(bg, 3.0, theme::c().selected.gamma_multiply(0.12));
    } else if r.hovered() {
        painter.rect_filled(bg, 3.0, theme::c().text.gamma_multiply(0.05));
    }
    let kind = || crate::filetypes::category(&e.name, e.executable).and_then(theme::file_color);
    let color = if selected {
        theme::c().selected
    } else if e.is_dir {
        theme::c().folder
    } else if e.is_symlink {
        theme::c().symlink
    } else {
        kind().unwrap_or(theme::c().text)
    };
    let y = rect.center().y;
    let (c_name, c_size, c_date) = columns(rect, widths);
    let icon = if e.is_dir { "🗀" } else { "🗋" };
    painter.text(pos2(c_name.left() + 8.0, y), Align2::LEFT_CENTER, icon, font.clone(), color.gamma_multiply(0.8));
    let name_x = c_name.left() + name_indent(font.size);
    let suffix = if e.is_symlink { " ↗" } else { "" };
    let (shown, truncated) = fit_name(ui, &e.name, suffix, e.is_dir, c_name.right() - name_x - 4.0, font);
    painter.with_clip_rect(c_name).text(pos2(name_x, y), Align2::LEFT_CENTER, shown, font.clone(), color);
    let r = if truncated { r.on_hover_text(&e.name) } else { r };
    if !e.is_dir {
        painter.text(pos2(c_size.right() - 4.0, y), Align2::RIGHT_CENTER, human_size(e.size), small.clone(), theme::c().dim);
    }
    if let Some(t) = e.modified {
        let dt: chrono::DateTime<chrono::Local> = t.into();
        let s = dt.format("%Y-%m-%d %H:%M").to_string();
        painter.text(pos2(c_date.right() - 4.0, y), Align2::RIGHT_CENTER, s, small.clone(), theme::c().dim);
    }

    // Dragging a selected row takes the whole selection; any other row goes alone.
    if r.drag_started() {
        let paths = if selected { pane.targets() } else if e.is_up() { Vec::new() } else { vec![e.path.clone()] };
        if !paths.is_empty() {
            egui::DragAndDrop::set_payload(ui.ctx(), DragPayload { from: idx, paths });
        }
    }
    // A folder row takes a drop into that folder ("..": into the parent).
    if e.is_dir {
        let into_itself = |p: &DragPayload| p.paths.contains(&e.path);
        if r.dnd_hover_payload::<DragPayload>().is_some_and(|p| !into_itself(&p)) {
            painter.rect_stroke(bg, 3.0, Stroke::new(2.0_f32, theme::c().accent), egui::StrokeKind::Inside);
        }
        if let Some(p) = r.dnd_release_payload::<DragPayload>().filter(|p| !into_itself(p)) {
            let mv = ui.input(|i| i.modifiers.shift);
            actions.push(Action::Drop { from: Some(p.from), to: idx, paths: p.paths.clone(), dest: e.path.clone(), mv });
        }
    }
    if r.clicked() {
        actions.push(Action::Activate(idx));
        let m = ui.input(|inp| inp.modifiers);
        if m.command || m.ctrl {
            pane.toggle_selected(i);
        } else if m.shift {
            let c = pane.cursor;
            pane.select_range(c, i);
        }
        pane.cursor = i;
    }
    if r.double_clicked() {
        pane.cursor = i;
        actions.push(Action::RunId("open"));
    }
    if r.secondary_clicked() {
        pane.cursor = i;
        actions.push(Action::Activate(idx));
    }
    r.context_menu(|ui| {
        for (id, title) in [
            ("open", "Open"),
            ("edit", "Edit"),
            ("rename", "Rename…"),
            ("copy", "Copy to other pane…"),
            ("move", "Move to other pane…"),
            ("clip_copy", "Copy"),
            ("clip_cut", "Cut"),
            ("clip_paste", "Paste here"),
            ("copy_path", "Copy path"),
            ("drag_out", "Drag to another app…"),
            ("pack", "Pack (compress)…"),
            ("unpack", "Unpack…"),
            ("folder_size", "Calculate size"),
            ("delete", "Move to trash"),
        ] {
            if ui.button(title).clicked() {
                actions.push(Action::RunId(id));
                ui.close_menu();
            }
        }
    });
}

/// Shortens a name to fit `max_w`, cutting the start of the name so the end and the
/// extension stay visible ("holiday-pho…summer.jpg"). Returns (text, was shortened).
pub(crate) fn fit_name(ui: &egui::Ui, name: &str, suffix: &str, is_dir: bool, max_w: f32, font: &FontId) -> (String, bool) {
    let width = |s: &str| ui.fonts(|f| f.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x);
    let full = format!("{name}{suffix}");
    if max_w <= 0.0 || width(&full) <= max_w {
        return (full, false);
    }
    let chars: Vec<char> = name.chars().collect();
    // Keep the extension (files only) plus a few characters before it.
    let ext_len = if is_dir {
        0
    } else {
        name.rfind('.').filter(|&i| i > 0).map(|i| name[i..].chars().count()).unwrap_or(0)
    };
    let build = |head: usize, tail: usize| -> String {
        let h: String = chars[..head].iter().collect();
        let t: String = chars[chars.len() - tail..].iter().collect();
        format!("{h}…{t}{suffix}")
    };
    // The most characters of the start that fit while keeping `tail` characters of the end.
    let best_head = |tail: usize| -> Option<usize> {
        if width(&build(0, tail)) > max_w {
            return None;
        }
        let (mut lo, mut hi) = (0usize, chars.len().saturating_sub(tail));
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if width(&build(mid, tail)) <= max_w {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some(lo)
    };
    // Prefer keeping a bit of the end with the extension; when space is tight keep
    // fewer end characters (down to just the extension) so the start stays readable.
    let len = chars.len();
    let mut tails = vec![(ext_len + 6).min(len), (ext_len + 2).min(len), ext_len.min(len)];
    tails.dedup();
    for &tail in &tails {
        if let Some(head) = best_head(tail) {
            if head >= 4.min(len - tail) {
                return (build(head, tail), true);
            }
        }
    }
    // Very little room: as much of the start as fits, then the extension if possible.
    for &tail in tails.iter().rev() {
        if let Some(head) = best_head(tail) {
            return (build(head, tail), true);
        }
    }
    let (mut lo, mut hi) = (0usize, len);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let t: String = chars[..mid].iter().collect();
        if width(&format!("{t}…")) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    (format!("{}…", chars[..lo].iter().collect::<String>()), true)
}

/// A small triangle pointing up (ascending) or down.
fn sort_arrow(painter: &egui::Painter, c: egui::Pos2, up: bool, color: Color32) {
    let (w, h) = (4.0, 3.5);
    let pts = if up {
        vec![pos2(c.x - w, c.y + h), pos2(c.x + w, c.y + h), pos2(c.x, c.y - h)]
    } else {
        vec![pos2(c.x - w, c.y - h), pos2(c.x + w, c.y - h), pos2(c.x, c.y + h)]
    };
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Where names start, leaving room for the icon at any text size.
fn name_indent(font_size: f32) -> f32 {
    10.0 + font_size * 1.3
}

/// Widths of the size and date columns, measured from the current font.
struct ColWidths {
    size: f32,
    date: f32,
}

/// Splits a row into the name, size and date columns.
fn columns(rect: egui::Rect, w: &ColWidths) -> (egui::Rect, egui::Rect, egui::Rect) {
    let date_left = rect.right() - w.date - 6.0;
    let size_left = date_left - w.size;
    let name = egui::Rect::from_min_max(rect.min, pos2((size_left - 8.0).max(rect.left() + 40.0), rect.bottom()));
    let size = egui::Rect::from_min_max(pos2(size_left, rect.top()), pos2(date_left, rect.bottom()));
    let date = egui::Rect::from_min_max(pos2(date_left, rect.top()), pos2(rect.right() - 6.0, rect.bottom()));
    (name, size, date)
}

/// Short names for the function-key bar (other commands use their title).
fn short_label(id: &str) -> Option<&'static str> {
    Some(match id {
        "help" => "Help",
        "about" => "About",
        "rename" => "Rename",
        "view" => "View",
        "edit" => "Edit",
        "copy" => "Copy",
        "move" => "Move",
        "new_folder" => "New folder",
        "new_file" => "New file",
        "delete" => "Trash",
        "delete_permanently" => "Delete",
        "pack" => "Pack",
        "unpack" => "Unpack",
        "open_terminal" => "Terminal",
        "quit" => "Quit",
        "appearance" => "Appearance",
        "sort_name" => "Sort: name",
        "sort_extension" => "Sort: type",
        "sort_modified" => "Sort: date",
        "sort_size" => "Sort: size",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::SortKey;
    use egui::{Event, PointerButton, Pos2, RawInput, Rect};

    /// Draws both panes for one frame (no window needed) with the given input events.
    fn frame(ctx: &egui::Context, panes: &mut [Pane; 2], actions: &mut Vec<Action>, events: Vec<Event>) {
        frame_with(ctx, panes, actions, events, egui::Modifiers::NONE);
    }

    fn frame_with(ctx: &egui::Context, panes: &mut [Pane; 2], actions: &mut Vec<Action>, events: Vec<Event>, modifiers: egui::Modifiers) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0))),
            events,
            modifiers,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let (l, r) = panes.split_at_mut(1);
                ui.columns(2, |cols| {
                    pane_ui(&mut cols[0], &mut l[0], 0, true, false, actions);
                    pane_ui(&mut cols[1], &mut r[0], 1, false, false, actions);
                });
            });
            drag_label(ctx);
        });
    }

    /// Presses at `from`, moves in steps to `to`, releases there (Shift held when `shift`).
    fn drag(ctx: &egui::Context, panes: &mut [Pane; 2], actions: &mut Vec<Action>, from: Pos2, to: Pos2, shift: bool) {
        let modifiers = egui::Modifiers { shift, ..Default::default() };
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers };
        frame_with(ctx, panes, actions, vec![Event::PointerMoved(from), button(from, true)], modifiers);
        for k in 1..=5 {
            let p = from + (to - from) * (k as f32 / 5.0);
            frame_with(ctx, panes, actions, vec![Event::PointerMoved(p)], modifiers);
        }
        frame_with(ctx, panes, actions, vec![button(to, false)], modifiers);
        frame(ctx, panes, actions, vec![]);
    }

    #[test]
    fn drag_rows_between_panes_and_onto_folders() {
        let base = std::env::temp_dir().join(format!("duopane-dnd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("left/sub")).unwrap();
        std::fs::write(base.join("left/b.txt"), b"b").unwrap();
        std::fs::create_dir_all(base.join("right")).unwrap();
        let base = base.canonicalize().unwrap();
        let mut panes = [
            Pane::new(&base.join("left"), false, false, SortKey::Name, true),
            Pane::new(&base.join("right"), false, false, SortKey::Name, true),
        ];
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        frame(&ctx, &mut panes, &mut actions, vec![]);

        // Rows: "sub" then "b.txt", below the path bar and the column headers.
        let row_h = theme::row_height(ctx.style().text_styles[&egui::TextStyle::Body].size);
        let row = |i: usize| pos2(150.0, 8.0 + 30.0 + row_h + (i as f32 + 0.5) * row_h);
        // Make sure the rows are where this test thinks: clicking row 1 puts the cursor there.
        let click = |pressed| Event::PointerButton { pos: row(1), button: PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(&ctx, &mut panes, &mut actions, vec![Event::PointerMoved(row(1)), click(true)]);
        frame(&ctx, &mut panes, &mut actions, vec![click(false)]);
        assert_eq!(panes[0].cursor_entry().unwrap().name, "b.txt");
        actions.clear();

        // b.txt onto the right pane: copy there.
        drag(&ctx, &mut panes, &mut actions, row(1), pos2(750.0, 400.0), false);
        let drops: Vec<_> = actions.iter().filter_map(|a| match a { Action::Drop { from, to, paths, dest, mv } => Some((*from, *to, paths.clone(), dest.clone(), *mv)), _ => None }).collect();
        assert_eq!(drops, vec![(Some(0), 1, vec![base.join("left/b.txt")], base.join("right"), false)]);
        actions.clear();

        // b.txt onto the "sub" folder row with Shift: move into it.
        drag(&ctx, &mut panes, &mut actions, row(1), row(0), true);
        let drops: Vec<_> = actions.iter().filter_map(|a| match a { Action::Drop { to, dest, mv, .. } => Some((*to, dest.clone(), *mv)), _ => None }).collect();
        assert_eq!(drops, vec![(0, base.join("left/sub"), true)]);
        actions.clear();

        // Released over its own pane's empty area: nothing happens, and no drag is left hanging.
        drag(&ctx, &mut panes, &mut actions, row(1), pos2(150.0, 450.0), false);
        assert!(!actions.iter().any(|a| matches!(a, Action::Drop { .. })));
        assert!(!egui::DragAndDrop::has_any_payload(&ctx));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Once a drag is in the desktop's hands, egui only sees what
    /// `drag_events_to_input` makes of the desktop's events.
    #[cfg(target_os = "linux")]
    #[test]
    fn drags_run_by_the_desktop() {
        use crate::native_drag::DragEvent;
        let base = std::env::temp_dir().join(format!("duopane-dnd-native-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("left")).unwrap();
        std::fs::write(base.join("left/b.txt"), b"b").unwrap();
        std::fs::create_dir_all(base.join("right")).unwrap();
        let base = base.canonicalize().unwrap();
        let mut panes = [
            Pane::new(&base.join("left"), false, false, SortKey::Name, true),
            Pane::new(&base.join("right"), false, false, SortKey::Name, true),
        ];
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        frame(&ctx, &mut panes, &mut actions, vec![]);
        let row_h = theme::row_height(ctx.style().text_styles[&egui::TextStyle::Body].size);
        let row0 = pos2(150.0, 8.0 + 30.0 + row_h * 1.5);
        let press = |pressed| Event::PointerButton { pos: row0, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        // The desktop's events, as egui input for one frame.
        let desktop = |ctx: &egui::Context, panes: &mut [Pane; 2], actions: &mut Vec<Action>, events: Vec<DragEvent>, dragging: &mut bool| {
            let mut raw = RawInput::default();
            crate::app::drag_events_to_input(events, &mut raw, 1.0, dragging, &mut false);
            frame(ctx, panes, actions, raw.events);
        };

        for (end, expect_drop) in [(DragEvent::DropOurs { x: 750.0, y: 300.0 }, true), (DragEvent::Ended, false)] {
            actions.clear();
            // Start dragging b.txt in egui; the desktop then takes over (the window
            // loses the pointer), the drag comes back over the right pane, and ends.
            frame(&ctx, &mut panes, &mut actions, vec![Event::PointerMoved(row0), press(true)]);
            frame(&ctx, &mut panes, &mut actions, vec![Event::PointerMoved(row0 + vec2(30.0, 0.0))]);
            assert!(egui::DragAndDrop::has_any_payload(&ctx));
            let mut dragging = true;
            desktop(&ctx, &mut panes, &mut actions, vec![DragEvent::Leave], &mut dragging);
            desktop(&ctx, &mut panes, &mut actions, vec![DragEvent::Motion { x: 700.0, y: 300.0, ours: true }], &mut dragging);
            assert!(egui::DragAndDrop::has_any_payload(&ctx), "the drag survives the desktop taking over");
            if !expect_drop {
                // Dropped in an app on top of ours: the desktop says Leave, then Ended.
                desktop(&ctx, &mut panes, &mut actions, vec![DragEvent::Leave], &mut dragging);
            }
            desktop(&ctx, &mut panes, &mut actions, vec![end], &mut dragging);
            frame(&ctx, &mut panes, &mut actions, vec![]);
            let drops: Vec<_> = actions.iter().filter_map(|a| match a { Action::Drop { to, dest, .. } => Some((*to, dest.clone())), _ => None }).collect();
            if expect_drop {
                assert_eq!(drops, vec![(1, base.join("right"))]);
            } else {
                assert!(drops.is_empty(), "{drops:?}");
            }
            assert!(!dragging);
            assert!(!egui::DragAndDrop::has_any_payload(&ctx), "nothing left hanging");
            // And the next click is an ordinary click again.
            actions.clear();
            frame(&ctx, &mut panes, &mut actions, vec![Event::PointerMoved(row0), press(true)]);
            frame(&ctx, &mut panes, &mut actions, vec![press(false)]);
            assert!(actions.iter().any(|a| matches!(a, Action::Activate(0))));
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
