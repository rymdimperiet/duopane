//! Pop-up dialogs: text input, confirmations, messages and the
//! searchable list used by "Go to" and the command palette.

use crate::app::{expand_tilde, subdirs, tilde, Action, App, CmdRef, InputPurpose, Modal};
use crate::fs_ops::{ConflictPolicy, FileInfo, Reply};
use crate::fuzzy;
use crate::theme;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Key, Modifiers, Sense};
use std::path::{Path, PathBuf};

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum PaletteKind {
    Commands,
    GoTo,
    /// "Open with" (Ctrl+Enter): apps for the files, then every other app.
    OpenWith,
}

#[derive(Clone)]
pub enum PaletteTarget {
    Cmd(CmdRef),
    Dir(PathBuf),
    App(std::sync::Arc<crate::open_with::DesktopApp>, std::sync::Arc<Vec<PathBuf>>),
}

#[derive(Clone)]
pub struct PaletteItem {
    pub title: String,
    pub detail: String,
    pub target: PaletteTarget,
    pub boost: i32,
}

pub struct PaletteState {
    kind: PaletteKind,
    query: String,
    computed_for: Option<String>,
    selected: usize,
    first: bool,
    base: Vec<PaletteItem>,
    shown: Vec<PaletteItem>,
    /// Hint in the empty search box, when the kind's own isn't enough.
    pub hint: Option<String>,
}

impl PaletteState {
    pub fn new(kind: PaletteKind, base: Vec<PaletteItem>) -> Self {
        PaletteState { kind, query: String::new(), computed_for: None, selected: 0, first: true, base, shown: Vec::new(), hint: None }
    }

    /// Re-ranks the list when the query changed.
    fn refresh(&mut self) {
        if self.computed_for.as_deref() == Some(self.query.as_str()) {
            return;
        }
        let q = self.query.trim().to_string();
        self.computed_for = Some(self.query.clone());
        self.selected = 0;
        if self.kind == PaletteKind::GoTo && (q.starts_with('/') || q.starts_with('~')) {
            self.shown = complete_path(&q);
            return;
        }
        if q.is_empty() && self.kind != PaletteKind::GoTo {
            self.shown = self.base.clone();
            return;
        }
        let mut scored: Vec<(i32, &PaletteItem)> = self
            .base
            .iter()
            .filter_map(|item| {
                let s = if self.kind == PaletteKind::GoTo {
                    let a = fuzzy::score(&q, &item.title).map(|s| s + 40);
                    let b = fuzzy::score(&q, &item.detail);
                    a.max(b)?
                } else {
                    fuzzy::score(&q, &item.title)?
                };
                Some((s + item.boost, item))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        let limit = if self.kind == PaletteKind::GoTo { 60 } else { 200 };
        self.shown = scored.into_iter().take(limit).map(|(_, i)| i.clone()).collect();
    }
}

/// Lists the folders inside a typed path, e.g. "~/Doc" -> ~/Documents.
fn complete_path(q: &str) -> Vec<PaletteItem> {
    let expanded = expand_tilde(q);
    let (parent, partial) = if q.ends_with('/') {
        (expanded.clone(), String::new())
    } else {
        (
            expanded.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")),
            expanded.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        )
    };
    let mut out = Vec::new();
    if expanded.is_dir() {
        out.push(dir_item(expanded.clone(), 0));
    }
    let mut subs: Vec<(i32, PathBuf)> = subdirs(&parent, partial.starts_with('.'))
        .into_iter()
        .filter(|p| p != &expanded)
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            fuzzy::score(&partial, &name).map(|s| (s, p))
        })
        .collect();
    subs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out.extend(subs.into_iter().take(60).map(|(_, p)| dir_item(p, 0)));
    out
}

pub fn dir_item(p: PathBuf, boost: i32) -> PaletteItem {
    let title = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
    PaletteItem { title, detail: tilde(&p), target: PaletteTarget::Dir(p), boost }
}

pub(crate) fn dialog<'a>(title: &str) -> egui::Window<'a> {
    egui::Window::new(title.to_string())
        .id(egui::Id::unique("modal_dialog"))
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .fade_in(false)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 120.0))
}

fn enter_esc(ctx: &egui::Context) -> (bool, bool) {
    ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)))
}

impl App {
    pub(crate) fn modal_ui(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.modal.take() else { return };
        // Dim the window behind the dialog (not behind the drive menu, which
        // marks the panes' places and is better read next to them).
        if !matches!(modal, Modal::Places(_) | Modal::RowMenu { .. }) {
            ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("dim")))
                .rect_filled(ctx.content_rect(), 0.0, Color32::from_black_alpha(110));
        }

        let mut close = false;
        let mut input_ok = false;
        let mut pack_ok = false;
        let mut batch_ok = false;
        let mut unpack_ok = false;
        let mut start: Option<crate::app::Confirmed> = None;
        let mut pick: Option<PaletteTarget> = None;
        let mut places_pick = None;
        let mut permissions: Option<(PathBuf, u32)> = None;

        match &mut modal {
            Modal::Input { title, hint, text, select_chars, first, purpose } => {
                let password = matches!(purpose, InputPurpose::SftpPassword { .. });
                let (enter, esc) = enter_esc(ctx);
                input_ok = enter;
                close = esc;
                let id = egui::Id::unique("modal_input");
                if *first {
                    // Focus and pre-select before drawing, so keys typed straight
                    // after opening the dialog land in it.
                    let end = text.chars().count();
                    let (a, b) = match select_chars {
                        Some(n) => (0, *n),
                        None => (end, end),
                    };
                    let mut st = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(a),
                        egui::text::CCursor::new(b),
                    )));
                    st.store(ctx, id);
                    ctx.memory_mut(|m| m.request_focus(id));
                    *first = false;
                }
                dialog(title).show(ctx, |ui| {
                    ui.set_width(520.0);
                    let out = egui::TextEdit::singleline(text)
                        .id(id)
                        .password(password)
                        .hint_text(hint.as_str())
                        .desired_width(f32::INFINITY)
                        .show(ui);
                    if !out.response.has_focus() {
                        out.response.request_focus();
                    }
                    if !text.is_empty() || password {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(hint.as_str()).color(theme::c().dim).small());
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            input_ok = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        ui.label(egui::RichText::new("Enter to confirm, Esc to cancel").color(theme::c().dim).small());
                    });
                });
            }
            Modal::Confirm { title, message, buttons, focus } => {
                let (enter, esc) = enter_esc(ctx);
                let n = buttons.len().max(1);
                ctx.input_mut(|i| {
                    if i.consume_key(Modifiers::NONE, Key::ArrowRight) || i.consume_key(Modifiers::NONE, Key::Tab) {
                        *focus = (*focus + 1) % n;
                    }
                    if i.consume_key(Modifiers::NONE, Key::ArrowLeft) || i.consume_key(Modifiers::SHIFT, Key::Tab) {
                        *focus = (*focus + n - 1) % n;
                    }
                });
                let mut chosen: Option<usize> = if enter { Some(*focus) } else { None };
                dialog(title).show(ctx, |ui| {
                    ui.set_width(440.0);
                    ui.label(message.as_str());
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        for (i, (label, _)) in buttons.iter().enumerate() {
                            let b = if i == *focus {
                                egui::Button::new(egui::RichText::new(label.as_str()).strong())
                                    .fill(theme::c().cursor)
                                    .stroke(egui::Stroke::new(1.0_f32, theme::c().accent))
                            } else {
                                egui::Button::new(label.as_str())
                            };
                            if ui.add(b).clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("←/→ or Tab choose, Enter picks, Esc cancels").color(theme::c().dim).small());
                });
                if esc {
                    close = true;
                } else if let Some(i) = chosen {
                    close = true;
                    start = buttons.get_mut(i).and_then(|(_, s)| s.take());
                }
            }
            Modal::Places(m) => {
                let r = self.panes_rect.unwrap_or(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0)));
                // Just below the path bar, at each pane's left edge.
                let second = crate::ui::pane_rects(r, self.settings.pane_split, self.settings.stacked_panes).2;
                let at = [r.min + vec2(8.0, 34.0), second.min + vec2(8.0, 34.0)];
                if let Some(o) = m.ui(ctx, at) {
                    close = true;
                    places_pick = Some(o);
                }
            }
            Modal::RowMenu { at, selected, items } => {
                let (done, chosen) = row_menu_ui(ctx, *at, selected, items);
                close = done;
                if let Some(id) = chosen {
                    self.actions.push(Action::RunId(id));
                }
            }
            Modal::About => {
                let (enter, esc) = enter_esc(ctx);
                close = enter || esc;
                close |= about_ui(ctx, self.update.as_ref());
            }
            Modal::Message { title, text } => {
                let (enter, esc) = enter_esc(ctx);
                close = enter || esc;
                dialog(title).show(ctx, |ui| {
                    ui.set_width(560.0);
                    egui::ScrollArea::vertical().max_height(440.0).show(ui, |ui| {
                        ui.label(text.as_str());
                    });
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            }
            Modal::BatchRename { items, others, rules, first, .. } => {
                let (enter, esc) = enter_esc(ctx);
                close = esc;
                let name_id = egui::Id::unique("batch_name");
                if std::mem::take(first) || ctx.memory(|m| m.focused().is_none()) {
                    // Windows spend their first frame measuring themselves; keep asking.
                    ctx.memory_mut(|m| m.request_focus(name_id));
                }
                let th = theme::c();
                let title = format!("Batch rename {}", crate::app::plural(items.len(), "item"));
                dialog(&title).show(ctx, |ui| {
                    ui.set_width((ctx.content_rect().width() - 60.0).clamp(400.0, 900.0));
                    egui::Grid::new("batch").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut rules.name_mask).id(name_id).desired_width(300.0));
                        ui.end_row();
                        ui.label("Extension");
                        ui.add(egui::TextEdit::singleline(&mut rules.ext_mask).desired_width(300.0));
                        ui.end_row();
                        ui.label("Find");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut rules.find).desired_width(200.0));
                            ui.label("replace with");
                            ui.add(egui::TextEdit::singleline(&mut rules.replace).desired_width(200.0));
                            ui.checkbox(&mut rules.regex, "Regular expression");
                        });
                        ui.end_row();
                        ui.label("Case");
                        egui::ComboBox::from_id_salt("batch_case").selected_text(rules.case.label()).show_ui(ui, |ui| {
                            for c in crate::rename::Case::ALL {
                                ui.selectable_value(&mut rules.case, c, c.label());
                            }
                        });
                        ui.end_row();
                        ui.label("Counter [C]");
                        ui.horizontal(|ui| {
                            ui.label("start");
                            ui.add(egui::DragValue::new(&mut rules.counter_start));
                            ui.label("step");
                            ui.add(egui::DragValue::new(&mut rules.counter_step));
                            ui.label("digits");
                            ui.add(egui::DragValue::new(&mut rules.counter_width).range(1..=12));
                        });
                        ui.end_row();
                    });
                    ui.label(
                        egui::RichText::new("[N] name · [N2-5] its characters 2–5 · [E] extension · [C] counter · [P] folder · [Y] [M] [D] [h] [m] [s] date and time")
                            .color(th.dim)
                            .small(),
                    );
                    ui.separator();
                    let olds: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
                    let preview = crate::rename::new_names(items, rules);
                    let probs = match &preview {
                        Ok(n) => crate::rename::problems(&olds, n, others),
                        Err(_) => vec![Some("error"); olds.len()],
                    };
                    let ok = preview.is_ok() && probs.iter().all(|p| p.is_none());
                    // Two fixed columns; long names are shortened (hover for the whole name).
                    let total = ui.available_width();
                    let note_w = 170.0;
                    let col = ((total - note_w - 16.0) / 2.0).max(80.0);
                    let row = |ui: &mut egui::Ui, a: egui::RichText, b: egui::RichText, c: egui::RichText| {
                        ui.horizontal(|ui| {
                            let left = egui::Layout::left_to_right(egui::Align::Center);
                            for (text, w) in [(a, col), (b, col), (c, note_w)] {
                                ui.allocate_ui_with_layout(egui::vec2(w, 20.0), left, |ui| {
                                    ui.set_width(w);
                                    ui.add(egui::Label::new(text).truncate());
                                });
                            }
                        });
                    };
                    row(ui, egui::RichText::new("Old name").color(th.dim), egui::RichText::new("New name").color(th.dim), egui::RichText::new(""));
                    egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (i, old) in olds.iter().enumerate() {
                            let new = match &preview {
                                Ok(new) => {
                                    let color = if probs[i].is_some() { th.error } else if new[i] != *old { th.ok } else { th.text };
                                    egui::RichText::new(&new[i]).color(color)
                                }
                                Err(_) => egui::RichText::new(""),
                            };
                            let note = egui::RichText::new(probs.get(i).copied().flatten().unwrap_or("")).color(th.error).small();
                            row(ui, egui::RichText::new(old), new, note);
                        }
                    });
                    if let Err(e) = &preview {
                        ui.colored_label(th.error, e);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(ok, egui::Button::new("Rename")).clicked() {
                            batch_ok = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        ui.label(egui::RichText::new("Enter renames, Esc cancels · undo from the command palette").color(th.dim).small());
                    });
                    if enter && ok {
                        batch_ok = true;
                    }
                });
            }
            Modal::Pack { path, format, level, separate, delete_after, sources, first } => {
                let (enter, esc) = enter_esc(ctx);
                close = esc;
                pack_ok = enter;
                let id = egui::Id::unique("pack_path");
                if std::mem::take(first) || ctx.memory(|m| m.focused().is_none()) {
                    // Windows spend their first frame measuring themselves; keep asking.
                    ctx.memory_mut(|m| m.request_focus(id));
                }
                let title = format!("Pack {}", crate::app::describe(sources));
                dialog(&title).show(ctx, |ui| {
                    ui.set_width(560.0);
                    ui.label("Archive:");
                    ui.add(egui::TextEdit::singleline(path).id(id).desired_width(f32::INFINITY));
                    ui.add_space(8.0);
                    egui::Grid::new("pack").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                        ui.label("Format");
                        ui.horizontal(|ui| {
                            for f in crate::archive::Format::ALL {
                                if ui.radio(*format == f, f.label()).clicked() && *format != f {
                                    *format = f;
                                    let stem = crate::archive::strip_archive_ext(path.trim()).to_string();
                                    *path = format!("{stem}.{}", f.ext());
                                }
                            }
                        });
                        ui.end_row();
                        if format.compresses() {
                            ui.label("Compression");
                            ui.horizontal(|ui| {
                                for (l, name) in [(1, "Fast"), (6, "Normal"), (9, "Best")] {
                                    if ui.radio(*level == l, name).clicked() {
                                        *level = l;
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
                    ui.add_space(6.0);
                    if sources.len() > 1 {
                        ui.checkbox(separate, "One archive per item");
                    }
                    ui.checkbox(delete_after, "Delete the originals afterwards (move into the archive)");
                    let th = theme::c();
                    if !*separate && expand_tilde(path.trim()).exists() {
                        ui.label(egui::RichText::new("An archive with this name exists and will be replaced.").color(th.error).small());
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Pack").clicked() {
                            pack_ok = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        ui.label(egui::RichText::new("Enter to pack, Esc to cancel").color(th.dim).small());
                    });
                });
            }
            Modal::Unpack { archives, dest, into_folder, first } => {
                let (enter, esc) = enter_esc(ctx);
                close = esc;
                unpack_ok = enter;
                let id = egui::Id::unique("unpack_dest");
                if std::mem::take(first) || ctx.memory(|m| m.focused().is_none()) {
                    // Windows spend their first frame measuring themselves; keep asking.
                    ctx.memory_mut(|m| m.request_focus(id));
                }
                let title = format!("Unpack {}", crate::app::describe(archives));
                dialog(&title).show(ctx, |ui| {
                    ui.set_width(560.0);
                    ui.label("Unpack to:");
                    ui.add(egui::TextEdit::singleline(dest).id(id).desired_width(f32::INFINITY));
                    ui.add_space(6.0);
                    ui.checkbox(into_folder, "Unpack each archive into its own folder");
                    let th = theme::c();
                    if !*into_folder {
                        ui.label(egui::RichText::new("Files with the same names are replaced.").color(th.dim).small());
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Unpack").clicked() {
                            unpack_ok = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            }
            Modal::Appearance { ui_filter, mono_filter } => {
                let (_, esc) = enter_esc(ctx);
                close = esc;
                let mut changed = false;
                dialog("Appearance").show(ctx, |ui| {
                    ui.set_width(520.0);
                    egui::Grid::new("appearance").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                        ui.label("Colour scheme");
                        egui::ComboBox::from_id_salt("scheme").width(300.0).selected_text(self.settings.color_scheme.clone()).show_ui(ui, |ui| {
                            for (name, _) in theme::SCHEMES {
                                if ui.selectable_label(self.settings.color_scheme == *name, *name).clicked() {
                                    self.settings.color_scheme = name.to_string();
                                    changed = true;
                                }
                            }
                        });
                        ui.end_row();

                        let s = &mut self.settings;
                        ui.label("Interface font");
                        ui.horizontal(|ui| {
                            changed |= font_picker(ui, "ui_font", &mut s.ui_font, ui_filter, false);
                            changed |= weight_picker(ui, "ui_weight", &s.ui_font, &mut s.ui_font_weight);
                        });
                        ui.end_row();

                        ui.label("Monospace font");
                        ui.horizontal(|ui| {
                            changed |= font_picker(ui, "mono_font", &mut s.mono_font, mono_filter, true);
                            changed |= weight_picker(ui, "mono_weight", &s.mono_font, &mut s.mono_font_weight);
                        });
                        ui.end_row();

                        ui.label("Text size");
                        // Apply the size when the slider is let go: resizing while dragging moves
                        // the slider under the mouse and makes the window flicker.
                        let r = ui.add(egui::Slider::new(&mut self.settings.font_size, 9.0..=32.0).step_by(1.0));
                        changed |= r.drag_stopped() || (r.changed() && !r.dragged());
                        ui.end_row();

                        ui.label("Rows");
                        changed |= ui.checkbox(&mut self.settings.striped_rows, "Alternate row brightness").changed();
                        ui.end_row();

                        ui.label("Row contrast");
                        ui.add_enabled_ui(self.settings.striped_rows, |ui| {
                            ui.horizontal(|ui| {
                                let mut pct = theme::stripe_percent(&self.settings, theme::c().light);
                                let slider = egui::Slider::new(&mut pct, crate::settings::STRIPE_MIN..=crate::settings::STRIPE_MAX)
                                    .step_by(1.0)
                                    .suffix(" %");
                                if ui.add(slider).changed() {
                                    self.settings.stripe_percent = Some(pct);
                                    changed = true;
                                }
                                if self.settings.stripe_percent.is_some() && ui.small_button("Default").clicked() {
                                    self.settings.stripe_percent = None;
                                    changed = true;
                                }
                            });
                        });
                        ui.end_row();

                        ui.label("Files");
                        ui.vertical(|ui| {
                            changed |= ui.checkbox(&mut self.settings.color_file_types, "Colour files by kind").changed();
                            if self.settings.color_file_types {
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 10.0;
                                    for (cat, label) in crate::filetypes::Category::ALL.into_iter().zip(FILE_KIND_NAMES) {
                                        let col = theme::file_color(cat).unwrap_or(theme::c().text);
                                        ui.label(egui::RichText::new(label).color(col)).on_hover_text(format!("\"{}\" under custom_colors in the settings file", cat.key()));
                                    }
                                });
                            }
                        });
                        ui.end_row();

                        ui.label("Bottom bar");
                        if ui.checkbox(&mut self.settings.show_key_bar, "Show the function-key bar (F2 Rename, F3 View…)").changed() {
                            self.dirty = true;
                        }
                        ui.end_row();

                        ui.label("Panes");
                        if ui.checkbox(&mut self.settings.stacked_panes, "One above the other").changed() {
                            self.dirty = true;
                        }
                        ui.end_row();

                        ui.label("Stack");
                        if ui.checkbox(&mut self.settings.keep_stack, "Keep what's on it after a restart").changed() {
                            self.dirty = true;
                        }
                        ui.end_row();

                        ui.label("Folders");
                        if ui.checkbox(&mut self.settings.show_dotdot, "Show “..” at the top to go up a level").changed() {
                            for p in &mut self.panes {
                                p.show_dotdot = self.settings.show_dotdot;
                                p.apply_view();
                            }
                            self.dirty = true;
                        }
                        ui.end_row();

                        ui.label("Sort names");
                        let shown = self.settings.sort_language.map_or("System default", |c| c.label());
                        let mut choice = self.settings.sort_language;
                        egui::ComboBox::from_id_salt("sort_language").width(300.0).selected_text(shown).show_ui(ui, |ui| {
                            ui.selectable_value(&mut choice, None, "System default");
                            for c in crate::pane::Collation::ALL {
                                ui.selectable_value(&mut choice, Some(c), c.label());
                            }
                        });
                        if choice != self.settings.sort_language {
                            self.settings.sort_language = choice;
                            crate::pane::set_collation(choice);
                            for p in &mut self.panes {
                                p.apply_view();
                            }
                            self.dirty = true;
                        }
                        ui.end_row();
                    });
                    ui.add_space(10.0);
                    // A small preview of the colours.
                    let th = theme::c();
                    let kind = |c| theme::file_color(c).unwrap_or(th.text);
                    use crate::filetypes::Category;
                    let rows: [(&str, Color32, Option<Color32>); 6] = [
                        ("🗀  Documents", th.folder, None),
                        ("🗋  notes.txt", th.text, Some(th.cursor)),
                        ("🗋  photo.jpg", th.selected, None),
                        ("🗋  Night Train.flac", kind(Category::Music), None),
                        ("🗋  backup.zip", kind(Category::Archive), None),
                        ("🗋  link ↗", th.symlink, None),
                    ];
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0 + rows.len() as f32 * 22.0), Sense::hover());
                    let p = ui.painter_at(rect);
                    p.rect_filled(rect, 4.0, th.pane);
                    let font = FontId::proportional(self.settings.font_size);
                    for (i, (t, c, bg)) in rows.iter().enumerate() {
                        let r = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0 + i as f32 * 22.0), egui::vec2(rect.width() - 8.0, 22.0));
                        if let Some(stripe) = th.stripe.filter(|_| i % 2 == 1) {
                            p.rect_filled(r, 0.0, stripe);
                        }
                        if let Some(bg) = bg {
                            p.rect_filled(r, 3.0, *bg);
                        }
                        p.text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, *t, font.clone(), *c);
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Single colours can be changed under \"custom_colors\" in the settings file (Ctrl+,).").color(th.dim).small());
                    ui.add_space(6.0);
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
                if changed {
                    theme::apply(ctx, &self.settings);
                    self.dirty = true;
                }
            }
            Modal::Search(d) => {
                use crate::search_ui::SearchOutcome;
                match d.ui(ctx) {
                    None => {}
                    Some(o) => {
                        self.last_search = d.text.clone();
                        close = true;
                        match o {
                            SearchOutcome::Jump(p) => self.jump_to(&p),
                            SearchOutcome::Feed(set) => {
                                let r = self.panes[self.active].show_results(set);
                                self.after_move(self.active, r);
                            }
                            SearchOutcome::Close => {}
                        }
                    }
                }
            }
            Modal::Palette(st) => {
                let (c, p) = palette_ui(ctx, st);
                close = c;
                pick = p;
            }
            Modal::Conflict(d) => {
                if let Some(reply) = d.ui(ctx) {
                    d.q.answer(reply);
                    close = true;
                }
            }
            Modal::Properties(p) => match p.ui(ctx) {
                crate::properties::Outcome::Stay => {}
                crate::properties::Outcome::Close => close = true,
                crate::properties::Outcome::Apply(mode) => {
                    close = true;
                    permissions = Some((p.path.clone(), mode));
                }
            },
        }
        if let Some((path, mode)) = permissions {
            match crate::properties::apply(&path, mode) {
                Ok(()) => self.reload_all(),
                Err(e) => self.set_status(format!("Couldn't change the permissions: {e}"), true),
            }
        }

        if batch_ok && !close {
            if let Modal::BatchRename { dir, items, rules, .. } = modal {
                if let Ok(news) = crate::rename::new_names(&items, &rules) {
                    let olds = items.into_iter().map(|i| i.name).collect();
                    self.apply_batch_rename(dir, olds, news);
                }
            }
            return;
        }
        if pack_ok && !close {
            if let Modal::Pack { path, format, level, separate, delete_after, sources, .. } = modal {
                self.submit_pack(&path, format, level, separate, delete_after, sources);
            }
            return;
        }
        if unpack_ok && !close {
            if let Modal::Unpack { archives, dest, into_folder, .. } = modal {
                self.submit_unpack(archives, &dest, into_folder);
            }
            return;
        }
        if input_ok && !close {
            if let Modal::Input { purpose, text, .. } = modal {
                self.submit_input(purpose, text);
            }
            return;
        }
        if !close {
            self.modal = Some(modal);
        }
        match start {
            Some(crate::app::Confirmed::Job(spec)) => self.start_job(spec),
            Some(crate::app::Confirmed::ClearStack) => {
                self.settings.stack.clear();
                self.dirty = true;
            }
            None => {}
        }
        if let Some(o) = places_pick {
            self.places_outcome(o);
        }
        match pick {
            Some(PaletteTarget::Cmd(c)) => self.actions.push(Action::Run(c)),
            Some(PaletteTarget::Dir(p)) => self.actions.push(Action::Navigate(self.active, p)),
            Some(PaletteTarget::App(app, files)) => self.launch_with(&app, &files),
            None => {}
        }
    }
}

/// "Overwrite: … With file: …", asked while a copy, move or unpack runs, with
/// Double Commander's choices. Arrow keys move between the buttons, Enter picks,
/// Esc cancels the job. The focus starts on Skip, so a stray Enter loses nothing.
pub struct ConflictDialog {
    pub q: crate::fs_ops::Conflict,
    /// The button with the keyboard focus (see [`CHOICES`]).
    focus: usize,
    /// Showing the less common choices under "Other".
    more: bool,
    /// `Some(name)` while typing a new name for the incoming file.
    rename: Option<String>,
    compare: Compare,
}

enum Compare {
    NotYet,
    Running(std::sync::mpsc::Receiver<String>),
    Done(String),
}

#[derive(Clone, Copy)]
enum Choice {
    Reply(fn() -> Reply),
    Rename,
    Compare,
    Other,
}

/// Where the keyboard focus starts: the "Skip" button.
const SKIP: usize = 1;

/// The buttons, three to a row like Double Commander's; the last row shows under "Other".
const CHOICES: [(&str, Choice); 13] = [
    ("Overwrite", Choice::Reply(|| Reply::Overwrite)),
    ("Skip", Choice::Reply(|| Reply::Skip)),
    ("Rename…", Choice::Rename),
    ("Overwrite All", Choice::Reply(|| Reply::All(ConflictPolicy::Overwrite))),
    ("Skip All", Choice::Reply(|| Reply::All(ConflictPolicy::Skip))),
    ("Compare by content", Choice::Compare),
    ("Overwrite All Older", Choice::Reply(|| Reply::All(ConflictPolicy::OverwriteOlder))),
    ("Cancel", Choice::Reply(|| Reply::Cancel)),
    ("Other", Choice::Other),
    ("Overwrite All Smaller", Choice::Reply(|| Reply::All(ConflictPolicy::OverwriteSmaller))),
    ("Overwrite All Larger", Choice::Reply(|| Reply::All(ConflictPolicy::OverwriteLarger))),
    ("Rename All New", Choice::Reply(|| Reply::All(ConflictPolicy::RenameNew))),
    ("Rename All Existing", Choice::Reply(|| Reply::All(ConflictPolicy::RenameExisting))),
];

const MORE_HINTS: [&str; 4] = [
    "Replace existing files only when they're smaller than the new ones; skip the rest",
    "Replace existing files only when they're larger than the new ones; skip the rest",
    "Give every new file a free name (“name copy.ext”), keeping the existing ones",
    "Rename every existing file aside (“name copy.ext”), then put the new one in its place",
];

impl ConflictDialog {
    pub fn new(q: crate::fs_ops::Conflict) -> Self {
        ConflictDialog { q, focus: SKIP, more: false, rename: None, compare: Compare::NotYet }
    }

    fn can_compare(&self) -> bool {
        let (a, b) = (&self.q.new, &self.q.existing);
        a.real.is_some() && b.real.is_some() && !a.is_dir && !b.is_dir
    }

    fn visible(&self) -> usize {
        if self.more { CHOICES.len() } else { 9 }
    }

    /// Draws the dialog. Returns the answer once one is picked.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Reply> {
        if let Compare::Running(rx) = &self.compare {
            match rx.try_recv() {
                Ok(msg) => self.compare = Compare::Done(msg),
                Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
            }
        }
        if self.rename.is_some() {
            return self.rename_ui(ctx);
        }
        let n = self.visible();
        let mut picked = ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowRight) || i.consume_key(Modifiers::NONE, Key::Tab) {
                self.focus = (self.focus + 1) % n;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft) || i.consume_key(Modifiers::SHIFT, Key::Tab) {
                self.focus = (self.focus + n - 1) % n;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) && self.focus + 3 < n {
                self.focus += 3;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) && self.focus >= 3 {
                self.focus -= 3;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                return Some(7); // Cancel
            }
            (i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Space)).then_some(self.focus)
        });
        let title = if self.q.existing.is_dir { "Folder already exists" } else { "File already exists" };
        dialog(title).show(ctx, |ui| {
            ui.set_width(620.0);
            describe_side(ui, "Overwrite:", &self.q.existing);
            ui.add_space(10.0);
            describe_side(ui, "With:", &self.q.new);
            match &self.compare {
                Compare::NotYet => {}
                Compare::Running(_) => {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Comparing…").color(theme::c().dim));
                }
                Compare::Done(msg) => {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(msg).color(theme::c().accent));
                }
            }
            ui.add_space(14.0);
            let w = (ui.available_width() - 2.0 * 10.0) / 3.0;
            egui::Grid::new("conflict_buttons").spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                for (i, (label, choice)) in CHOICES.iter().enumerate().take(self.visible()) {
                    let enabled = !matches!(choice, Choice::Compare) || self.can_compare();
                    let label = if matches!(choice, Choice::Other) { if self.more { "Other ▴" } else { "Other ▾" } } else { label };
                    let mut b = egui::Button::new(label).min_size(vec2(w, 30.0));
                    if i == self.focus {
                        b = b.fill(theme::c().cursor).stroke(egui::Stroke::new(1.0_f32, theme::c().accent));
                    }
                    let mut r = ui.add_enabled(enabled, b);
                    if i >= 9 {
                        r = r.on_hover_text(MORE_HINTS[i - 9]);
                    }
                    if r.clicked() {
                        picked = Some(i);
                    }
                    if i % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Arrow keys choose, Enter picks, Esc cancels the whole job").color(theme::c().dim).small());
        });
        match CHOICES.get(picked?)?.1 {
            Choice::Reply(r) => Some(r()),
            Choice::Rename => {
                let name = crate::fs_ops::unique_copy_name(&self.q.target).file_name().map(|n| n.to_string_lossy().into_owned());
                self.rename = Some(name.unwrap_or_default());
                None
            }
            Choice::Compare => {
                if self.can_compare() {
                    self.start_compare();
                }
                None
            }
            Choice::Other => {
                self.more = !self.more;
                self.focus = if self.more { 9 } else { 8 };
                None
            }
        }
    }

    fn start_compare(&mut self) {
        let (Some(a), Some(b)) = (self.q.new.real.clone(), self.q.existing.real.clone()) else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use crate::fs_ops::Sameness;
            let msg = match crate::fs_ops::compare_files(&a, &b) {
                Ok(Sameness::Identical) => "Identical: the two files have the same contents.".to_string(),
                Ok(Sameness::DifferentSize) => "Different: the files aren't the same size.".to_string(),
                Ok(Sameness::DifferAt(at)) => format!("Different: they first differ at byte {}.", group_digits(at)),
                Err(e) => format!("Couldn't compare them: {e}"),
            };
            let _ = tx.send(msg);
        });
        self.compare = Compare::Running(rx);
    }

    /// Typing a new name for the incoming file.
    fn rename_ui(&mut self, ctx: &egui::Context) -> Option<Reply> {
        let (enter, esc) = enter_esc(ctx);
        let text = self.rename.as_mut()?;
        let bad = text.contains('/') || text.trim().is_empty();
        let mut ok = enter && !bad;
        let mut back = esc;
        dialog("Rename the new file").show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(format!("“{}” already exists. Put the new one there as:", self.q.target.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
            ui.add_space(6.0);
            let r = ui.add(egui::TextEdit::singleline(text).id(egui::Id::unique("conflict_rename")).desired_width(f32::INFINITY));
            if !r.has_focus() {
                r.request_focus();
            }
            if bad && !text.is_empty() {
                ui.label(egui::RichText::new("A name can't contain “/”").color(theme::c().error).small());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!bad, egui::Button::new("OK")).clicked() {
                    ok = true;
                }
                if ui.button("Back").clicked() {
                    back = true;
                }
            });
        });
        if back {
            self.rename = None;
            return None;
        }
        if ok {
            let name = self.rename.take()?;
            let dir = self.q.target.parent().map(Path::to_path_buf).unwrap_or_default();
            return Some(Reply::RenameTo(dir.join(name.trim())));
        }
        None
    }
}

/// "Overwrite:" + path + "77 232 bytes, 2026-11-18 14:59:18".
fn describe_side(ui: &mut egui::Ui, heading: &str, f: &FileInfo) {
    ui.label(egui::RichText::new(heading).color(theme::c().dim));
    ui.add(egui::Label::new(egui::RichText::new(&f.shown).color(theme::c().text)).wrap());
    let what = if f.is_dir { "Folder".to_string() } else { format!("{} bytes", group_digits(f.size)) };
    let when = f
        .modified
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown date".into());
    ui.label(egui::RichText::new(format!("{what}, {when}")).color(theme::c().dim));
}

/// 77232 -> "77 232" (thin groups, like Double Commander shows sizes).
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// Draws the searchable list. Returns (close, picked item).
fn palette_ui(ctx: &egui::Context, st: &mut PaletteState) -> (bool, Option<PaletteTarget>) {
    let (down, up, pgdn, pgup, enter, esc, tab) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::PageDown),
            i.consume_key(Modifiers::NONE, Key::PageUp),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
            i.consume_key(Modifiers::NONE, Key::Tab),
        )
    });
    st.refresh();
    let len = st.shown.len();
    let mut moved = false;
    if len > 0 {
        let before = st.selected;
        if down {
            st.selected = (st.selected + 1) % len;
        }
        if up {
            st.selected = (st.selected + len - 1) % len;
        }
        if pgdn {
            st.selected = (st.selected + 10).min(len - 1);
        }
        if pgup {
            st.selected = st.selected.saturating_sub(10);
        }
        moved = before != st.selected;
    }
    // Tab completes a typed path in "Go to".
    let mut completed = false;
    if tab && st.kind == PaletteKind::GoTo {
        if let Some(PaletteTarget::Dir(p)) = st.shown.get(st.selected).map(|i| i.target.clone()) {
            let mut q = tilde(&p);
            if !q.ends_with('/') {
                q.push('/');
            }
            st.query = q;
            completed = true;
        }
    }
    let mut close = esc;
    let mut pick = if enter { st.shown.get(st.selected).map(|i| i.target.clone()) } else { None };
    let hint = match (&st.hint, st.kind) {
        (Some(h), _) => h.as_str(),
        (None, PaletteKind::OpenWith) => "Type to find an app…",
        (None, PaletteKind::Commands) => "Type to find a command…",
        (None, PaletteKind::GoTo) => "Part of a folder name, or a path like ~/Documents (Tab completes)",
    };
    let resp = egui::Window::new("palette")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .fade_in(false)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 40.0))
        .frame(egui::Frame::window(&ctx.global_style()).fill(theme::c().header).inner_margin(8.0))
        .show(ctx, |ui| {
            ui.set_width(640.0);
            if st.first {
                // Focus before drawing so keys typed straight away land in the search box.
                ui.ctx().memory_mut(|m| m.request_focus(egui::Id::unique("palette_text")));
            }
            let mut out = egui::TextEdit::singleline(&mut st.query)
                .id(egui::Id::unique("palette_text"))
                .hint_text(hint)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Heading)
                .show(ui);
            if st.first || !out.response.has_focus() {
                out.response.request_focus();
                st.first = false;
            }
            if completed {
                let end = st.query.chars().count();
                out.state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(end))));
                out.state.store(ui.ctx(), out.response.id);
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(400.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if st.shown.is_empty() {
                    ui.label(egui::RichText::new("No matches").color(theme::c().dim));
                }
                let size = ui.style().text_styles[&egui::TextStyle::Body].size;
                let font = FontId::proportional(size);
                let small = FontId::proportional(size - 1.5);
                for (i, item) in st.shown.iter().enumerate() {
                    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                    let painter = ui.painter_at(rect);
                    if i == st.selected {
                        painter.rect_filled(rect, 3.0, theme::c().cursor);
                        if moved {
                            r.scroll_to_me(None);
                        }
                    } else if r.hovered() {
                        painter.rect_filled(rect, 3.0, theme::c().cursor_inactive);
                    }
                    let y = rect.center().y;
                    // The name wins; the path gets what's left, losing its start first
                    // since its end (the name and its parents) is what tells folders apart.
                    let room = rect.width() - 16.0;
                    let (title, _) = crate::ui::fit_name(ui, &item.title, "", true, room * 0.7, &font);
                    let title_w = text_width(ui, &title, &font);
                    let detail = fit_end(ui, &item.detail, room - title_w - 24.0, &small);
                    painter.text(pos2(rect.left() + 8.0, y), Align2::LEFT_CENTER, title, font.clone(), theme::c().text);
                    painter.text(pos2(rect.right() - 8.0, y), Align2::RIGHT_CENTER, detail, small.clone(), theme::c().dim);
                    if r.clicked() {
                        pick = Some(item.target.clone());
                    }
                }
            });
        });
    // Clicking outside closes the palette.
    if let Some(r) = resp {
        let outside = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !r.response.rect.contains(p))
        });
        close |= outside;
    }
    if pick.is_some() {
        close = true;
    }
    (close, pick)
}

fn text_width(ui: &egui::Ui, s: &str, font: &FontId) -> f32 {
    ui.ctx().fonts_mut(|f| f.layout_no_wrap(s.to_string(), font.clone(), egui::Color32::WHITE).size().x)
}

/// Shortens text to fit `max_w` by cutting its start ("…/photos/2024"); empty if nothing fits.
pub(crate) fn fit_end(ui: &egui::Ui, text: &str, max_w: f32, font: &FontId) -> String {
    if text_width(ui, text, font) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let tail = |n: usize| format!("…{}", chars[chars.len() - n..].iter().collect::<String>());
    // The most characters of the end that fit.
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if text_width(ui, &tail(mid), font) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 { String::new() } else { tail(lo) }
}

/// A searchable list of installed fonts. Returns true when the choice changed.
/// One entry in the keyboard's row menu: the command, its title and its keys.
pub struct MenuItem {
    pub id: &'static str,
    pub title: String,
    pub keys: String,
}

/// The keyboard's version of a row's right-click menu: arrows, Enter, Esc, or the
/// first letter of an item. Returns (close, the command chosen).
fn row_menu_ui(ctx: &egui::Context, at: egui::Pos2, selected: &mut usize, items: &[MenuItem]) -> (bool, Option<&'static str>) {
    let n = items.len();
    if n == 0 {
        return (true, None);
    }
    let (esc, enter, up, down, home, end, typed) = ctx.input_mut(|i| {
        let mut k = |key| i.consume_key(Modifiers::NONE, key);
        let keys = (k(Key::Escape), k(Key::Enter), k(Key::ArrowUp), k(Key::ArrowDown), k(Key::Home) || k(Key::PageUp), k(Key::End) || k(Key::PageDown));
        let typed = i.events.iter().find_map(|e| match e {
            egui::Event::Text(t) => t.chars().next(),
            _ => None,
        });
        (keys.0, keys.1, keys.2, keys.3, keys.4, keys.5, typed)
    });
    if esc {
        return (true, None);
    }
    if enter {
        return (true, Some(items[*selected].id));
    }
    if down {
        *selected = (*selected + 1) % n;
    }
    if up {
        *selected = (*selected + n - 1) % n;
    }
    if home {
        *selected = 0;
    }
    if end {
        *selected = n - 1;
    }
    // A letter moves to the next item starting with it.
    if let Some(c) = typed.filter(|c| c.is_alphanumeric()).map(|c| c.to_lowercase().next().unwrap_or(c)) {
        let starts = |i: usize| items[i].title.to_lowercase().starts_with(c);
        if let Some(i) = (1..=n).map(|d| (*selected + d) % n).find(|&i| starts(i)) {
            *selected = i;
        }
    }
    let th = theme::c();
    let mut chosen = None;
    let r = egui::Area::new(egui::Id::unique("row_menu")).order(egui::Order::Foreground).fixed_pos(at).constrain(true).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(340.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            let font = FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
            let small = egui::TextStyle::Small.resolve(ui.style());
            let row_h = theme::row_height(font.size);
            for (i, item) in items.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let paint = ui.painter_at(rect);
                if i == *selected {
                    paint.rect_filled(rect, 3.0, th.cursor);
                } else if resp.hovered() {
                    paint.rect_filled(rect, 3.0, th.text.gamma_multiply(0.05));
                }
                paint.text(pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, &item.title, font.clone(), th.text);
                paint.text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, &item.keys, small.clone(), th.dim);
                if resp.clicked() {
                    chosen = Some(item.id);
                }
            }
        });
    });
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !r.response.rect.contains(p)));
    (outside || chosen.is_some(), chosen)
}

/// The kinds of file as named in Appearance, in `Category::ALL` order.
const FILE_KIND_NAMES: [&str; 8] = ["Pictures", "Video", "Music", "Documents", "Archives", "Code", "Programs", "Temporary"];

/// Where the source lives (shown as a link in the About box).
const REPOSITORY: &str = "https://github.com/rymdimperiet/duopane";

/// The About box: icon, version, build details, credits. Returns true when closed.
fn about_ui(ctx: &egui::Context, update: Option<&crate::update::Release>) -> bool {
    let th = theme::c();
    let mut close = false;
    egui::Window::new("About Duopane")
        .id(egui::Id::unique("modal_dialog"))
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .order(egui::Order::Foreground)
        .fade_in(false)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_width(340.0);
            ui.vertical_centered(|ui| {
                ui.add_space(14.0);
                ui.add(egui::Image::from_bytes("bytes://duopane-icon.png", include_bytes!("../packaging/duopane.png")).fit_to_exact_size(vec2(96.0, 96.0)));
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Duopane").heading().strong().color(th.text));
                ui.label(egui::RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(th.text));
                if let Some(u) = update {
                    if ui.link(format!("Version {} is out: download it", u.version)).on_hover_text(&u.url).clicked() {
                        let _ = open::that_detached(&u.url);
                    }
                }
                ui.label(egui::RichText::new("A keyboard-driven dual-pane file manager").color(th.dim));
                ui.add_space(10.0);
                for line in [
                    format!("Commit {} · built {}", env!("DUOPANE_COMMIT"), env!("DUOPANE_BUILD_DATE")),
                    format!("{} build for {}", env!("DUOPANE_PROFILE"), env!("DUOPANE_TARGET")),
                    env!("DUOPANE_RUSTC").to_string(),
                ] {
                    ui.label(egui::RichText::new(line).small().color(th.dim));
                }
                ui.add_space(10.0);
                if ui.link("Source code on GitHub").on_hover_text(REPOSITORY).clicked() {
                    let _ = open::that_detached(REPOSITORY);
                }
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Coded with Opus 5.5 Medium").color(th.text));
                ui.label(
                    egui::RichText::new("Standing on the blue shoulders of John Socha's Norton Commander (1986).")
                        .italics()
                        .color(th.dim),
                );
                ui.add_space(6.0);
                ui.label(egui::RichText::new("MIT licence").small().color(th.dim));
                ui.add_space(12.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
                ui.add_space(8.0);
            });
        });
    close
}

/// The weight of a chosen font (Light, Regular, Bold…), from those installed.
/// Hidden for the built-in font and for families with a single weight.
fn weight_picker(ui: &mut egui::Ui, id: &str, family: &str, weight: &mut u16) -> bool {
    let weights = if family.is_empty() { Vec::new() } else { theme::font_weights(family) };
    if weights.len() < 2 {
        return false;
    }
    // Show what will actually be used: the installed weight nearest the setting.
    let nearest = *weights.iter().min_by_key(|w| w.abs_diff(*weight)).unwrap_or(&400);
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).width(120.0).selected_text(theme::weight_name(nearest)).show_ui(ui, |ui| {
        for w in weights {
            if ui.selectable_label(w == nearest, format!("{} ({w})", theme::weight_name(w))).clicked() {
                *weight = w;
                changed = true;
            }
        }
    });
    changed
}

fn font_picker(ui: &mut egui::Ui, id: &str, value: &mut String, filter: &mut String, mono: bool) -> bool {
    let mut changed = false;
    let shown = if value.is_empty() { "Built-in".to_string() } else { value.clone() };
    egui::ComboBox::from_id_salt(id).width(300.0).height(360.0).selected_text(shown).show_ui(ui, |ui| {
        ui.add(egui::TextEdit::singleline(filter).hint_text("Filter…").desired_width(f32::INFINITY));
        if ui.selectable_label(value.is_empty(), "Built-in").clicked() {
            value.clear();
            changed = true;
        }
        let f = filter.to_lowercase();
        let mut list = theme::font_families(mono);
        if list.is_empty() {
            list = theme::font_families(false);
        }
        for name in list.iter().filter(|n| f.is_empty() || n.to_lowercase().contains(&f)).take(400) {
            if ui.selectable_label(value == name, name.as_str()).clicked() {
                *value = name.clone();
                changed = true;
            }
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_ops::Conflict;

    /// Opens the conflict dialog, presses `keys` one frame each, and returns the
    /// answer it gave (if any).
    fn press(keys: &[Key]) -> Option<Reply> {
        let info = |p: &str| FileInfo::of(Path::new(p));
        let (q, _rx) = Conflict::for_test(info("/new/x.txt"), info("/old/x.txt"), PathBuf::from("/old/x.txt"));
        let mut d = ConflictDialog::new(q);
        let ctx = egui::Context::default();
        let mut answer = None;
        let mut frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput { events, ..Default::default() };
            ctx.run_pass(input, |ctx| {
                if let Some(r) = d.ui(ctx) {
                    answer = Some(r);
                }
            }).drop_without_applying_deltas();
        };
        frame(vec![]);
        for &key in keys {
            frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        }
        answer
    }

    #[test]
    fn keyboard_row_menu() {
        let ctx = egui::Context::default();
        let menu = crate::ui::row_menu("notes", false, false);
        let items: Vec<MenuItem> = menu.iter().map(|(id, t)| MenuItem { id, title: t.clone(), keys: String::new() }).collect();
        let run = |events: Vec<egui::Event>, selected: &mut usize| {
            let mut out = (false, None);
            let input = egui::RawInput { events, ..Default::default() };
            ctx.run_pass(input, |ctx| out = row_menu_ui(ctx, egui::pos2(10.0, 10.0), selected, &items)).drop_without_applying_deltas();
            out
        };
        let key = |key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        let mut sel = 0;
        assert_eq!(run(vec![], &mut sel), (false, None));
        assert_eq!(run(vec![key(Key::ArrowDown)], &mut sel), (false, None));
        assert_eq!(run(vec![key(Key::Enter)], &mut sel), (true, Some("open_with")));
        // Up from the top wraps to the last item; a letter jumps to the next item starting with it.
        let mut sel = 0;
        run(vec![key(Key::ArrowUp)], &mut sel);
        assert_eq!(menu[sel].0, "properties");
        run(vec![egui::Event::Text("r".into())], &mut sel);
        assert_eq!(menu[sel].0, "rename");
        assert_eq!(run(vec![key(Key::Escape)], &mut sel), (true, None));
    }

    #[test]
    fn long_paths_lose_their_start() {
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let font = FontId::proportional(14.0);
                let path = "~/.cache/duopane/mounts/per@host/.cache/com.bitwarden.desktop";
                assert_eq!(fit_end(ui, path, 10_000.0, &font), path);
                let short = fit_end(ui, path, 250.0, &font);
                assert!(short.starts_with('…') && short.ends_with("com.bitwarden.desktop"), "{short}");
                assert!(text_width(ui, &short, &font) <= 250.0);
                assert_eq!(fit_end(ui, path, 1.0, &font), "");
            });
        }).drop_without_applying_deltas();
    }

    #[test]
    fn about_box() {
        let date = env!("DUOPANE_BUILD_DATE");
        assert!(date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-', "{date}");
        eprintln!("about: {} | {} | {} | {} | {}", env!("DUOPANE_COMMIT"), date, env!("DUOPANE_PROFILE"), env!("DUOPANE_TARGET"), env!("DUOPANE_RUSTC"));
        // It draws, and stays open until closed.
        let ctx = egui::Context::default();
        let mut closed = true;
        ctx.run_pass(egui::RawInput::default(), |ctx| closed = about_ui(ctx, None)).drop_without_applying_deltas();
        assert!(!closed);
    }

    #[test]
    fn conflict_dialog_keys() {
        // The focus starts on Skip: a stray Enter never overwrites.
        assert_eq!(press(&[Key::Enter]), Some(Reply::Skip));
        assert_eq!(press(&[Key::ArrowLeft, Key::Enter]), Some(Reply::Overwrite));
        assert_eq!(press(&[Key::ArrowDown, Key::Enter]), Some(Reply::All(ConflictPolicy::Skip)));
        assert_eq!(press(&[Key::ArrowLeft, Key::ArrowDown, Key::ArrowDown, Key::Enter]), Some(Reply::All(ConflictPolicy::OverwriteOlder)));
        assert_eq!(press(&[Key::Escape]), Some(Reply::Cancel));
        // "Other" opens the extra row with the focus on its first button.
        let other = [Key::ArrowDown, Key::ArrowDown, Key::ArrowRight, Key::Enter];
        assert_eq!(press(&[&other[..], &[Key::Enter]].concat()), Some(Reply::All(ConflictPolicy::OverwriteSmaller)));
        assert_eq!(press(&[&other[..], &[Key::ArrowRight, Key::ArrowRight, Key::ArrowRight, Key::Enter]].concat()), Some(Reply::All(ConflictPolicy::RenameExisting)));
        // Rename suggests a free name; Enter accepts it, Esc goes back to the choices.
        assert_eq!(press(&[Key::ArrowRight, Key::Enter, Key::Enter]), Some(Reply::RenameTo(PathBuf::from("/old/x copy.txt"))));
        assert_eq!(press(&[Key::ArrowRight, Key::Enter, Key::Escape]), None);
        // Compare can't run on files that don't exist, so nothing happens.
        assert_eq!(press(&[Key::ArrowDown, Key::ArrowRight, Key::Enter]), None);
    }
}
