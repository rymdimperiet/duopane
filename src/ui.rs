//! Drawing the main window: the two panes and the bars at the bottom.

use crate::app::{plural, Action, App};
use crate::pane::{human_size, Pane, SortKey};
use crate::settings::ColumnWidths;
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
        // F-keys, then number keys for commands no F-key has.
        const KEYS: [Key; 22] = [
            Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12,
            Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9, Key::Num0,
        ];
        let mut entries: Vec<(String, String, CmdRef)> = Vec::new();
        for key in KEYS {
            let sc = Shortcut::from_event(key, m);
            if let Some(&cmd) = self.bindings.get(&sc).filter(|c| !entries.iter().any(|e| e.2 == **c)) {
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
            format!("No commands on F-keys or numbers with {held}")
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
        let width = |ui: &egui::Ui, t: &str, f: &FontId| width_height(ui, t, f).0;
        let more_w = width(ui, "…", &body) + gap + 4.0;
        // As tall as the buttons, also when there are none, so the panes above
        // don't move when a modifier key changes what the bar shows.
        let height = ui.spacing().interact_size.y.max(width_height(ui, "Copy", &body).1);
        // A newer version shows at the right end.
        let update = self.update.as_ref().map(|u| (format!("Duopane {} is out", u.version), u.url.clone()));
        let update_w = update.as_ref().map_or(0.0, |(t, _)| width(ui, t, &body) + pad + gap * 2.0);
        ui.horizontal(|ui| {
            ui.set_min_height(height);
            if let Some(msg) = empty {
                ui.label(egui::RichText::new(msg).color(th.dim).small());
            } else {
                let n = entries.len();
                for (i, (k, label, action)) in entries.into_iter().enumerate() {
                    let w = width(ui, &k, &small) + width(ui, &label, &body) + pad + gap * 3.0 + 8.0;
                    let reserve = if i + 1 < n { more_w } else { 0.0 } + update_w;
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
            }
            if let Some((text, url)) = &update {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let b = egui::Button::new(egui::RichText::new(text).color(th.accent)).small();
                    if ui.add(b).on_hover_text("A newer version is available: open its download page").clicked() {
                        let _ = open::that_detached(url);
                    }
                });
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
                let active = self.active;
                let b0 = self.settings.bookmarks.contains(&self.panes[0].path);
                let b1 = self.settings.bookmarks.contains(&self.panes[1].path);
                let (left, right) = self.panes.split_at_mut(1);
                let actions = &mut self.actions;
                let quick_view = &mut self.quick_view;
                let (split, columns, stacked) = (self.settings.pane_split, self.settings.columns, self.settings.stacked_panes);
                let rects = pane_rects(ui.max_rect(), split, stacked);
                let panes = [(&mut left[0], b0), (&mut right[0], b1)];
                for (idx, (pane, bookmarked)) in panes.into_iter().enumerate() {
                    let col = &mut ui.new_child(egui::UiBuilder::new().max_rect([rects.0, rects.2][idx]));
                    match quick_view {
                        Some(qv) if qv.slot == idx => qv.ui(col),
                        _ => pane_ui(col, pane, idx, active == idx, bookmarked, &mut self.settings.columns, actions),
                    }
                }
                pane_divider(ui, rects.1, ui.max_rect(), stacked, &mut self.settings.pane_split, actions);
                if self.settings.pane_split != split || self.settings.columns != columns {
                    self.dirty = true;
                }
            });
        drag_label(ctx);
    }

    /// Which pane a point is over.
    pub(crate) fn pane_at(&self, p: egui::Pos2) -> Option<usize> {
        let r = self.panes_rect?;
        let stacked = self.settings.stacked_panes;
        let mid = pane_rects(r, self.settings.pane_split, stacked).1.center();
        Some(usize::from(if stacked { p.y > mid.y } else { p.x > mid.x }))
    }
}

/// The room between the panes, which is also the handle for moving it.
const DIVIDER: f32 = 6.0;
/// The narrowest a pane can be made, side by side and stacked.
const MIN_PANE: f32 = 220.0;
const MIN_PANE_HEIGHT: f32 = 120.0;

/// The first pane, the divider and the second pane: side by side, or with
/// `stacked` one above the other.
pub(crate) fn pane_rects(r: egui::Rect, split: f32, stacked: bool) -> (egui::Rect, egui::Rect, egui::Rect) {
    if stacked {
        let room = (r.height() - DIVIDER).max(0.0);
        let y = (r.top() + room * split.clamp(0.0, 1.0)).round();
        return (
            egui::Rect::from_min_max(r.min, pos2(r.right(), y)),
            egui::Rect::from_min_max(pos2(r.left(), y), pos2(r.right(), y + DIVIDER)),
            egui::Rect::from_min_max(pos2(r.left(), y + DIVIDER), r.max),
        );
    }
    let room = (r.width() - DIVIDER).max(0.0);
    let x = (r.left() + room * split.clamp(0.0, 1.0)).round();
    let divider = egui::Rect::from_min_max(pos2(x, r.top()), pos2(x + DIVIDER, r.bottom()));
    (
        egui::Rect::from_min_max(r.min, pos2(x, r.bottom())),
        divider,
        egui::Rect::from_min_max(pos2(x + DIVIDER, r.top()), r.max),
    )
}

/// Dragging the gap between the panes moves it; a double click puts it back in the middle.
/// Ctrl+double-click resets every size (the "reset_layout" command).
fn pane_divider(ui: &mut egui::Ui, rect: egui::Rect, area: egui::Rect, stacked: bool, split: &mut f32, actions: &mut Vec<Action>) {
    let grab = if stacked { vec2(0.0, 2.0) } else { vec2(2.0, 0.0) };
    let r = ui.interact(rect.expand2(grab), egui::Id::new("pane_divider"), Sense::click_and_drag());
    let (length, least_px, start) = if stacked { (area.height(), MIN_PANE_HEIGHT, area.top()) } else { (area.width(), MIN_PANE, area.left()) };
    let room = (length - DIVIDER).max(1.0);
    let least = (least_px / room).min(0.5);
    if r.dragged() {
        if let Some(p) = r.interact_pointer_pos() {
            let at = if stacked { p.y } else { p.x };
            *split = ((at - DIVIDER / 2.0 - start) / room).clamp(least, 1.0 - least);
        }
    }
    if r.double_clicked() {
        if ui.input(|i| i.modifiers.ctrl || i.modifiers.command) {
            actions.push(Action::RunId("reset_layout"));
        }
        *split = 0.5;
    }
    if r.hovered() || r.dragged() {
        let stroke = Stroke::new(2.0_f32, theme::c().dim.gamma_multiply(0.6));
        if stacked {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            ui.painter().hline(area.x_range().shrink(area.width() * 0.3), rect.center().y, stroke);
        } else {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            ui.painter().vline(rect.center().x, area.y_range().shrink(area.height() * 0.3), stroke);
        }
    }
    let _ = r.on_hover_text_at_pointer("Drag to move. Double-click: back to the middle.\nCtrl+double-click: reset the columns too.");
}

/// Draws one pane: path bar, column headers, the list and a summary line.
fn pane_ui(ui: &mut egui::Ui, pane: &mut Pane, idx: usize, active: bool, bookmarked: bool, set: &mut ColumnWidths, actions: &mut Vec<Action>) {
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

        // Columns are as wide as the text they hold, unless dragged wider or narrower.
        let (fit_size, fit_date) = ui.fonts(|f| {
            let w = |s: &str| f.layout_no_wrap(s.to_string(), small.clone(), Color32::WHITE).size().x;
            (w("888.8 MB") + 16.0, w("8888-88-88 88:88").max(w("Modified") + 14.0) + 28.0)
        });
        let date = set.date.unwrap_or(fit_date);
        let widths = ColWidths {
            size: set.size.unwrap_or(fit_size),
            date,
            // Too narrow for the time: show the day alone.
            date_format: if date < fit_date - 16.0 { "%Y-%m-%d" } else { "%Y-%m-%d %H:%M" },
        };

        // --- column headers ---
        let (head_rect, _) = ui.allocate_exact_size(vec2(full_w, theme::row_height(body_size)), Sense::hover());
        let cols = columns(head_rect, &widths);
        for (rect, key, label, left) in [
            (cols.0, SortKey::Name, "Name", true),
            (cols.1, SortKey::Size, "Size", false),
            (cols.2, SortKey::Modified, "Modified", false),
        ] {
            let r = ui.interact(rect, egui::Id::new(("head", idx, label)), Sense::click());
            // The Name column also stands for "type" (sorting by type keeps names in view).
            let here = |k: SortKey| k == key || (left && k == SortKey::Extension);
            let sorted = here(pane.sort);
            let second = pane.then.filter(|(k, _)| !sorted && here(*k));
            // Leave room on the right of right-aligned labels for the sort arrow (and the "2").
            let (x, align) = if left {
                (rect.left() + name_indent(body_size), Align2::LEFT_CENTER)
            } else {
                (rect.right() - if sorted { 16.0 } else if second.is_some() { 26.0 } else { 4.0 }, Align2::RIGHT_CENTER)
            };
            let color = if r.hovered() || sorted { theme::c().text } else { theme::c().dim };
            let shown = match (left, pane.sort, pane.then) {
                (true, SortKey::Extension, _) => "Name (by type)",
                (true, _, Some((SortKey::Extension, _))) => "Name (then type)",
                _ => label,
            };
            let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
            let text_rect = painter.text(pos2(x, rect.center().y), align, shown, small.clone(), color);
            let arrow_at = pos2(text_rect.right() + 8.0, rect.center().y);
            if sorted {
                sort_arrow(&painter, arrow_at, pane.ascending, color);
            } else if let Some((_, up)) = second {
                // The second sort: a dimmer arrow with a small 2.
                let dim = theme::c().dim;
                sort_arrow(&painter, arrow_at, up, dim);
                let tiny = FontId::proportional(small.size * 0.75);
                painter.text(arrow_at + vec2(5.0, -2.0), Align2::LEFT_CENTER, "2", tiny, dim);
            }
            let r = r.on_hover_text("Click to sort by this, again to reverse. Ctrl+click: sort by this second.");
            if r.clicked() {
                actions.push(Action::Activate(idx));
                if ui.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                    actions.push(Action::RunId(match key {
                        SortKey::Name => "then_sort_name",
                        SortKey::Size => "then_sort_size",
                        SortKey::Modified => "then_sort_modified",
                        SortKey::Extension => "then_sort_extension",
                    }));
                } else {
                    pane.set_sort(key);
                }
            }
        }
        column_handles(ui, head_rect, &cols, set, idx, actions);
        ui.painter().hline(head_rect.x_range(), head_rect.bottom(), Stroke::new(1.0_f32, theme::c().border));

        // --- the list ---
        let footer_h = 22.0;
        let row_h = theme::row_height(body_size);
        // The list fills the room down to the footer; the last row may be cut off
        // (scrolling to the cursor always shows its row whole). It stops a few
        // pixels short of the footer's line, so the line never crosses a name.
        let gap = 4.0;
        let list_h = (ui.available_height() - footer_h - gap).max(row_h);
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
            pane.cursor_rect = None;
            for i in range {
                row_ui(ui, pane, i, idx, active, &font, &small, &widths, row_h, actions);
            }
        });
        pane.scroll_offset = out.state.offset.y;
        pane.view_height = out.inner_rect.height();

        // --- footer ---
        ui.add_space(gap);
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
        // The Ctrl+S filter comes first, with a text cursor while it's being typed.
        let mut x = foot_rect.left() + 10.0;
        if pane.filtering || !pane.filter.is_empty() {
            let typing = if pane.filtering { "▏" } else { "" };
            let r = ui.painter().text(
                pos2(x, foot_rect.center().y),
                Align2::LEFT_CENTER,
                format!("Filter: {}{typing}", pane.filter),
                small.clone(),
                theme::c().accent,
            );
            x = r.right() + 14.0;
        }
        let hidden = if pane.filter.is_empty() { hidden } else { "   (Esc shows all)" };
        ui.painter().text(
            pos2(x, foot_rect.center().y),
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

/// The right-click menu of a file or folder (also Shift+Enter), as command IDs and
/// titles. "Open" names the app Enter will use.
pub(crate) fn row_menu(name: &str, is_dir: bool, in_archive: bool) -> Vec<(&'static str, String)> {
    let mut items = Vec::new();
    if is_dir {
        items.push(("open", "Open".to_string()));
    } else if !in_archive && crate::archive::is_archive(std::path::Path::new(name)) {
        // Enter steps inside; the archive app is one item further down.
        items.push(("open", "Open (browse inside)".to_string()));
        let app = crate::open_with::default_app(name, false).map_or_else(|| "the system's app".to_string(), |a| a.name.clone());
        items.push(("open_external", format!("Open ({app})")));
    } else {
        items.push(("open", crate::open_with::default_app(name, false).map_or_else(|| "Open".to_string(), |a| format!("Open ({})", a.name))));
    }
    items.extend(ROW_MENU.iter().map(|&(id, t)| (id, t.to_string())));
    if !is_dir {
        items.push(("checksum", "Checksum".to_string()));
    }
    items.push(("properties", "Properties…".to_string()));
    items
}

const ROW_MENU: &[(&str, &str)] = &[
    ("open_with", "Open with…"),
    ("edit", "Edit"),
    ("rename", "Rename…"),
    ("copy_in_place", "Copy here as…"),
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
];

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
    match theme::icon_font(ui.ctx(), font.size) {
        Some(icons) => {
            let icon = crate::icons::icon(&e.name, e.is_dir, e.executable, e.is_symlink);
            painter.text(pos2(c_name.left() + 8.0, y), Align2::LEFT_CENTER, icon, icons, color.gamma_multiply(0.85));
        }
        None => {
            painter.text(pos2(c_name.left() + 8.0, y), Align2::LEFT_CENTER, if e.is_dir { "🗀" } else { "🗋" }, font.clone(), color.gamma_multiply(0.8));
        }
    }
    let name_x = c_name.left() + name_indent(font.size);
    let suffix = if e.is_symlink { " ↗" } else { "" };
    let (shown, truncated) = fit_name(ui, &e.name, suffix, e.is_dir, c_name.right() - name_x - 4.0, font);
    painter.with_clip_rect(c_name).text(pos2(name_x, y), Align2::LEFT_CENTER, shown, font.clone(), color);
    let r = if truncated { r.on_hover_text(&e.name) } else { r };
    if !e.is_dir {
        painter.with_clip_rect(c_size).text(pos2(c_size.right() - 4.0, y), Align2::RIGHT_CENTER, human_size(e.size), small.clone(), theme::c().dim);
    }
    if let Some(t) = e.modified {
        let dt: chrono::DateTime<chrono::Local> = t.into();
        let s = dt.format(widths.date_format).to_string();
        painter.with_clip_rect(c_date).text(pos2(c_date.right() - 4.0, y), Align2::RIGHT_CENTER, s, small.clone(), theme::c().dim);
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
    if i == pane.cursor {
        // Where the keyboard's menu (Shift+Enter) opens.
        pane.cursor_rect = Some(bg.intersect(ui.clip_rect()));
    }
    r.context_menu(|ui| {
        let Some(e) = pane.entries.get(i) else { return };
        for (id, title) in row_menu(&e.name, e.is_dir, pane.archive.is_some()) {
            if ui.button(title).clicked() {
                actions.push(Action::RunId(id));
                ui.close_menu();
            }
        }
    });
}

/// The size of a line of text (a small button is exactly this tall).
fn width_height(ui: &egui::Ui, text: &str, font: &FontId) -> (f32, f32) {
    let s = ui.fonts(|f| f.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE).size());
    (s.x, s.y)
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

/// Widths of the size and date columns, and how dates are written.
struct ColWidths {
    size: f32,
    date: f32,
    date_format: &'static str,
}

/// The narrowest a column can be dragged, and the least room left for names.
const MIN_COLUMN: f32 = 20.0;
const MIN_NAMES: f32 = 80.0;

/// The edges between the header's columns: dragging one sets the width of the
/// column on its right (names take what's left); a double click fits it to its text.
fn column_handles(ui: &mut egui::Ui, head: egui::Rect, cols: &(egui::Rect, egui::Rect, egui::Rect), set: &mut ColumnWidths, idx: usize, actions: &mut Vec<Action>) {
    let right = head.right() - 6.0;
    for (n, x) in [(0, cols.1.left()), (1, cols.2.left())] {
        let handle = egui::Rect::from_center_size(pos2(x, head.center().y), vec2(8.0, head.height()));
        let r = ui.interact(handle, egui::Id::new(("column_edge", idx, n)), Sense::click_and_drag());
        let strong = r.hovered() || r.dragged();
        let color = if strong { theme::c().text.gamma_multiply(0.6) } else { theme::c().border };
        ui.painter().vline(x, head.y_range().shrink(head.height() * 0.25), Stroke::new(1.0_f32, color));
        if strong {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if r.dragged() {
            if let Some(p) = r.interact_pointer_pos() {
                let room = right - head.left() - MIN_NAMES - 8.0;
                if n == 0 {
                    let most = (room - (right - cols.2.left())).max(MIN_COLUMN);
                    set.size = Some((cols.2.left() - p.x).clamp(MIN_COLUMN, most).round());
                } else {
                    let most = (room - cols.1.width()).max(MIN_COLUMN);
                    set.date = Some((right - p.x).clamp(MIN_COLUMN, most).round());
                }
            }
        }
        if r.double_clicked() {
            if ui.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                actions.push(Action::RunId("reset_layout"));
            }
            if n == 0 {
                set.size = None;
            } else {
                set.date = None;
            }
        }
        let _ = r.on_hover_text_at_pointer("Drag to resize. Double-click: fit to the text.\nCtrl+double-click: reset all columns and the panes.");
    }
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
        "places_left" => "Left drive",
        "places_right" => "Right drive",
        "open_terminal" => "Terminal",
        "quit" => "Quit",
        "appearance" => "Appearance",
        "sort_name" => "Sort: name",
        "sort_extension" => "Sort: type",
        "sort_modified" => "Sort: date",
        "sort_size" => "Sort: size",
        "then_sort_name" => "Then: name",
        "then_sort_extension" => "Then: type",
        "then_sort_modified" => "Then: date",
        "then_sort_size" => "Then: size",
        "compare" => "Compare",
        "copy_in_place" => "Copy as",
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
                let mut widths = ColumnWidths::default();
                ui.columns(2, |cols| {
                    pane_ui(&mut cols[0], &mut l[0], 0, true, false, &mut widths, actions);
                    pane_ui(&mut cols[1], &mut r[0], 1, false, false, &mut widths, actions);
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

    /// Going into a folder and back out again leaves the folder on the same row
    /// on screen, not wherever the least scrolling would put it.
    #[test]
    fn coming_back_up_keeps_the_folder_in_place() {
        let base = std::env::temp_dir().join(format!("duopane-scrollback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for i in 0..120 {
            std::fs::create_dir_all(base.join(format!("folder {i:03}"))).unwrap();
        }
        let base = base.canonicalize().unwrap();
        let mut panes = [Pane::new(&base, false, false, SortKey::Name, true), Pane::new(&base, false, false, SortKey::Name, true)];
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        let settle = |panes: &mut [Pane; 2], actions: &mut Vec<Action>| {
            while panes[0].is_loading() {
                let _ = panes[0].poll_loading();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            for _ in 0..3 {
                frame(&ctx, panes, actions, vec![]);
            }
        };
        settle(&mut panes, &mut actions);
        // Move down a screenful and a half: the cursor ends up at the bottom edge,
        // then scroll so it sits higher up, as after paging back up a little.
        panes[0].set_cursor(60);
        settle(&mut panes, &mut actions);
        panes[0].set_cursor(50);
        settle(&mut panes, &mut actions);
        let before = (panes[0].cursor, panes[0].scroll_offset);
        assert!(before.1 > 0.0, "the list is scrolled");
        assert!(panes[0].cursor_rect.is_some(), "the keyboard menu knows where the cursor row is");
        let name = panes[0].entries[50].name.clone();
        let _ = panes[0].navigate(&base.join(&name));
        settle(&mut panes, &mut actions);
        let _ = panes[0].go_up();
        settle(&mut panes, &mut actions);
        assert_eq!(panes[0].entries[panes[0].cursor].name, name, "the cursor is on the folder we left");
        assert_eq!((panes[0].cursor, panes[0].scroll_offset), before, "and the list is scrolled as it was");
        // A folder entered afresh still starts at the top.
        let _ = panes[0].navigate(&base.join("folder 119"));
        settle(&mut panes, &mut actions);
        assert_eq!(panes[0].scroll_offset, 0.0);
        let _ = std::fs::remove_dir_all(&base);
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

    /// Presses at `from`, drags to `to` and lets go, one frame per step, drawing `draw` each time.
    fn drag_across(ctx: &egui::Context, from: Pos2, to: Pos2, mut draw: impl FnMut(&mut egui::Ui)) {
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        let steps = [vec![Event::PointerMoved(from)], vec![button(from, true)], vec![Event::PointerMoved(from.lerp(to, 0.5))], vec![Event::PointerMoved(to)], vec![button(to, false)]];
        for events in steps {
            let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0))), events, ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| draw(ui));
            });
        }
    }

    #[test]
    fn moving_the_pane_divider() {
        let ctx = egui::Context::default();
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0));
        let mut split = 0.5;
        let at = pane_rects(area, split, false).1.center();
        drag_across(&ctx, at, at - vec2(200.0, 0.0), |ui| pane_divider(ui, pane_rects(area, split, false).1, area, false, &mut split, &mut Vec::new()));
        let left = pane_rects(area, split, false).0;
        assert!((left.width() - 297.0).abs() < 2.0, "left pane is {}", left.width());
        // It can't squeeze a pane to nothing.
        let at = pane_rects(area, split, false).1.center();
        drag_across(&ctx, at, pos2(5.0, at.y), |ui| pane_divider(ui, pane_rects(area, split, false).1, area, false, &mut split, &mut Vec::new()));
        assert!(pane_rects(area, split, false).0.width() >= MIN_PANE - 1.0);
    }

    #[test]
    fn stacked_panes_divide_up_and_down() {
        let ctx = egui::Context::default();
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0));
        let (top, gap, bottom) = pane_rects(area, 0.5, true);
        assert_eq!((top.width(), bottom.width()), (1000.0, 1000.0));
        assert!(top.bottom() <= gap.top() && gap.bottom() <= bottom.top());
        let mut split = 0.5;
        let at = pane_rects(area, split, true).1.center();
        drag_across(&ctx, at, at - vec2(0.0, 100.0), |ui| pane_divider(ui, pane_rects(area, split, true).1, area, true, &mut split, &mut Vec::new()));
        let top = pane_rects(area, split, true).0;
        assert!((top.height() - 197.0).abs() < 2.0, "top pane is {}", top.height());
        let at = pane_rects(area, split, true).1.center();
        drag_across(&ctx, at, pos2(at.x, 5.0), |ui| pane_divider(ui, pane_rects(area, split, true).1, area, true, &mut split, &mut Vec::new()));
        assert!(pane_rects(area, split, true).0.height() >= MIN_PANE_HEIGHT - 1.0);
    }

    #[test]
    fn dragging_column_edges() {
        let ctx = egui::Context::default();
        let head = Rect::from_min_size(pos2(0.0, 100.0), vec2(500.0, 20.0));
        let mut set = ColumnWidths::default();
        let fit = |set: &ColumnWidths| ColWidths { size: set.size.unwrap_or(70.0), date: set.date.unwrap_or(120.0), date_format: "" };
        // The edge between names and sizes, dragged left: a wider Size column.
        let edge = columns(head, &fit(&set)).1.left_center();
        drag_across(&ctx, edge, edge - vec2(30.0, 0.0), |ui| column_handles(ui, head, &columns(head, &fit(&set)), &mut set, 0, &mut Vec::new()));
        assert_eq!(set, ColumnWidths { size: Some(100.0), date: None });
        // The edge between sizes and dates, dragged right: a narrower date column, same size column.
        let edge = columns(head, &fit(&set)).2.left_center();
        drag_across(&ctx, edge, edge + vec2(50.0, 0.0), |ui| column_handles(ui, head, &columns(head, &fit(&set)), &mut set, 0, &mut Vec::new()));
        assert_eq!(set, ColumnWidths { size: Some(100.0), date: Some(70.0) });
        // Names always keep some room.
        let edge = columns(head, &fit(&set)).1.left_center();
        drag_across(&ctx, edge, pos2(0.0, edge.y), |ui| column_handles(ui, head, &columns(head, &fit(&set)), &mut set, 0, &mut Vec::new()));
        assert!(columns(head, &fit(&set)).0.width() >= MIN_NAMES);
    }


    #[test]
    fn ctrl_double_click_resets_everything() {
        let ctx = egui::Context::default();
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0));
        let mut split = 0.3;
        let mut actions = Vec::new();
        let at = pane_rects(area, split, false).1.center();
        let ctrl = egui::Modifiers::CTRL;
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: ctrl };
        for (n, events) in [vec![Event::PointerMoved(at)], vec![button(true)], vec![button(false)], vec![button(true)], vec![button(false)]].into_iter().enumerate() {
            let input = RawInput { screen_rect: Some(area), events, modifiers: ctrl, time: Some(n as f64 * 0.05), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| pane_divider(ui, pane_rects(area, split, false).1, area, false, &mut split, &mut actions));
            });
        }
        assert_eq!(split, 0.5);
        assert!(matches!(actions[..], [Action::RunId("reset_layout")]));
    }

}
