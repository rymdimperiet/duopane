//! The "Find files" dialog (Ctrl+F / Alt+F7): type words, results appear as
//! they're found. Enter goes to the file; Ctrl+Enter shows all results in the
//! pane ("feed to listbox"). The searching itself is in `search.rs`.

use crate::pane::{human_size, ResultSet};
use crate::search::{Query, Search};
use crate::theme;
use eframe::egui::{self, pos2, vec2, Align2, FontId, Key, Modifiers, Sense};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Wait this long after the last key before searching (typing restarts it).
const TYPING_PAUSE: Duration = Duration::from_millis(150);

pub enum SearchOutcome {
    /// Show this file in its folder.
    Jump(PathBuf),
    /// Show all results in the pane.
    Feed(ResultSet),
    Close,
}

/// Where to search.
struct Scope {
    label: String,
    root: PathBuf,
    everywhere: bool,
}

pub struct SearchDialog {
    pub text: String,
    scopes: Vec<Scope>,
    scope: usize,
    search: Option<Search>,
    /// Set when the text changed and the search hasn't caught up yet.
    edited: Option<Instant>,
    selected: usize,
    scroll_to_selected: bool,
    /// The focused button (see [`BUTTONS`]), when the keyboard is down in the
    /// button row rather than in the results.
    button: Option<usize>,
    hidden: bool,
    first: bool,
}

/// The buttons under the results, with the keys that also press them.
const BUTTONS: [(&str, &str); 3] = [("Show all in the pane", "Ctrl+Enter"), ("Go to it", "Enter"), ("Close", "Esc")];

impl SearchDialog {
    /// A search starting in `folder` (Tab widens it to home, then everywhere),
    /// with the last query filled in and selected.
    pub fn new(folder: PathBuf, last: &str, hidden: bool) -> SearchDialog {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let mut scopes = vec![Scope { label: crate::app::tilde(&folder), root: folder.clone(), everywhere: false }];
        if folder != home {
            scopes.push(Scope { label: "Home".into(), root: home, everywhere: false });
        }
        scopes.push(Scope { label: "Everywhere".into(), root: PathBuf::from("/"), everywhere: true });
        let mut d = SearchDialog {
            text: last.to_string(),
            scopes,
            scope: 0,
            search: None,
            edited: None,
            selected: 0,
            scroll_to_selected: true,
            button: None,
            hidden,
            first: true,
        };
        d.restart();
        d
    }

    fn restart(&mut self) {
        self.edited = None;
        self.selected = 0;
        self.button = None;
        self.scroll_to_selected = true;
        let q = Query::parse(&self.text);
        self.search = if q.is_empty() {
            None
        } else {
            let s = &self.scopes[self.scope];
            Some(Search::start(q, s.root.clone(), s.everywhere, self.hidden))
        };
    }

    /// Draws the dialog; returns what to do once the user picks something.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<SearchOutcome> {
        if let Some(s) = &mut self.search {
            s.poll();
            if s.running() {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        if self.edited.is_some_and(|t| t.elapsed() >= TYPING_PAUSE) {
            self.restart();
        } else if self.edited.is_some() {
            ctx.request_repaint_after(TYPING_PAUSE);
        }
        let n = self.search.as_ref().map_or(0, |s| s.hits.len());
        let on_buttons = self.button.is_some();
        let (esc, tab, down, up, pgdn, pgup, left, right, ctrl_enter, enter) = ctx.input_mut(|i| {
            let mut k = |m, key| i.consume_key(m, key);
            (
                k(Modifiers::NONE, Key::Escape),
                k(Modifiers::NONE, Key::Tab),
                k(Modifiers::NONE, Key::ArrowDown),
                k(Modifiers::NONE, Key::ArrowUp),
                k(Modifiers::NONE, Key::PageDown),
                k(Modifiers::NONE, Key::PageUp),
                // Left / Right only move between the buttons; in the results they
                // stay with the text box.
                on_buttons && k(Modifiers::NONE, Key::ArrowLeft),
                on_buttons && k(Modifiers::NONE, Key::ArrowRight),
                k(Modifiers::COMMAND, Key::Enter),
                k(Modifiers::NONE, Key::Enter),
            )
        });
        if esc {
            return Some(SearchOutcome::Close);
        }
        if ctrl_enter {
            return self.feed();
        }
        if tab {
            self.scope = (self.scope + 1) % self.scopes.len();
            self.edited = Some(Instant::now() - TYPING_PAUSE);
        }
        // With no results only "Close" can be pressed.
        let usable = |b: usize| n > 0 || b == 2;
        match self.button {
            None if down && (n == 0 || self.selected + 1 == n) => self.button = Some(if n > 0 { 0 } else { 2 }),
            None if down => self.move_list(1, n),
            Some(_) if up => self.button = None,
            None if up => self.move_list(-1, n),
            _ => {}
        }
        if self.button.is_none() {
            if pgdn {
                self.move_list(12, n);
            }
            if pgup {
                self.move_list(-12, n);
            }
        }
        if let Some(b) = self.button {
            let step = |from: usize, d: usize| (1..=3).map(|k| (from + d * k) % 3).find(|&b| usable(b)).unwrap_or(from);
            if right {
                self.button = Some(step(b, 1));
            }
            if left {
                self.button = Some(step(b, 2));
            }
        }
        if enter {
            return match self.button {
                Some(b) => self.press(b),
                None => self.jump(),
            };
        }
        let mut outcome = None;

        crate::dialogs::dialog("Find files").show(ctx, |ui| {
            ui.set_width(760.0);
            let id = egui::Id::new("search_text");
            if self.first {
                // Select the last search, so typing replaces it.
                let mut st = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
                let end = self.text.chars().count();
                st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(end))));
                st.store(ctx, id);
                self.first = false;
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.text)
                    .id(id)
                    .hint_text("Words to find, e.g.  holiday jpg")
                    .desired_width(f32::INFINITY),
            );
            if !r.has_focus() {
                r.request_focus();
            }
            if r.changed() {
                self.edited = Some(Instant::now());
                self.button = None;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("In:").color(theme::c().dim));
                for i in 0..self.scopes.len() {
                    if ui.selectable_label(i == self.scope, self.scopes[i].label.as_str()).clicked() && i != self.scope {
                        self.scope = i;
                        self.restart();
                    }
                }
                ui.label(egui::RichText::new("(Tab)").color(theme::c().dim).small());
            });
            ui.add_space(4.0);
            self.status_line(ui);
            ui.add_space(6.0);
            if let Some(o) = self.results_list(ui) {
                outcome = Some(o);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let th = theme::c();
                for (b, (label, key)) in BUTTONS.iter().enumerate() {
                    let text = egui::RichText::new(format!("{label}   ")).color(th.text);
                    let mut job = egui::text::LayoutJob::default();
                    text.append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Center);
                    egui::RichText::new(*key).small().color(th.dim).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Center);
                    let mut button = egui::Button::new(job);
                    if self.button == Some(b) {
                        button = button.fill(th.cursor).stroke(egui::Stroke::new(1.0_f32, th.accent));
                    }
                    if ui.add_enabled(n > 0 || b == 2, button).clicked() {
                        outcome = self.press(b);
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Every word must appear, in any order.  ↓ past the last result reaches the buttons.\nFilters:  *.pdf   >10mb  <1kb   @today  @week  @month  @2025  @2025-11   @folders  @files",
                )
                .color(theme::c().dim)
                .small(),
            );
        });
        outcome
    }

    fn status_line(&self, ui: &mut egui::Ui) {
        let q = Query::parse(&self.text);
        let dim = theme::c().dim;
        let text = match &self.search {
            None if q.is_empty() => "Type to search".to_string(),
            None => String::new(),
            Some(s) => {
                let found = match s.found {
                    0 if s.running() => "Searching…".to_string(),
                    0 => "Nothing found".to_string(),
                    1 => "1 found".to_string(),
                    n if n > s.hits.len() => format!("{n} found (showing the best {})", s.hits.len()),
                    n => format!("{n} found"),
                };
                let mut parts = vec![found];
                if s.walking {
                    parts.push(format!("{} looked at…", s.looked_at.load(std::sync::atomic::Ordering::Relaxed)));
                }
                if self.scopes[self.scope].everywhere {
                    parts.push(match crate::search::index_age() {
                        Some(age) => format!("from the file index, updated {} ago", ago(age)),
                        None => "searching everywhere needs plocate installed".into(),
                    });
                }
                parts.join("  ·  ")
            }
        };
        ui.horizontal(|ui| {
            if self.search.as_ref().is_some_and(Search::running) {
                ui.spinner();
            }
            ui.label(egui::RichText::new(text).color(dim));
            if !q.unknown.is_empty() {
                ui.label(egui::RichText::new(format!("Not understood: {}", q.unknown.join(" "))).color(theme::c().error));
            }
        });
    }

    fn results_list(&mut self, ui: &mut egui::Ui) -> Option<SearchOutcome> {
        let s = self.search.as_ref()?;
        let font = FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
        let small = egui::TextStyle::Small.resolve(ui.style());
        let row_h = theme::row_height(font.size);
        let n = s.hits.len();
        let mut area = egui::ScrollArea::vertical().max_height(row_h * 14.0).auto_shrink([false, true]);
        if self.scroll_to_selected {
            area = area.vertical_scroll_offset((self.selected as f32 * row_h - row_h * 6.0).max(0.0));
            self.scroll_to_selected = false;
        }
        let mut picked = None;
        let mut jump = false;
        area.show_rows(ui, row_h, n, |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let h = &s.hits[i];
                let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let p = ui.painter_at(rect);
                let th = theme::c();
                if i == self.selected {
                    p.rect_filled(rect, 3.0, th.cursor);
                } else if r.hovered() {
                    p.rect_filled(rect, 3.0, th.text.gamma_multiply(0.05));
                }
                let y = rect.center().y;
                let name = h.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let color = if h.is_dir {
                    th.folder
                } else {
                    crate::filetypes::category(&name, false).and_then(theme::file_color).unwrap_or(th.text)
                };
                p.text(pos2(rect.left() + 8.0, y), Align2::LEFT_CENTER, if h.is_dir { "🗀" } else { "🗋" }, font.clone(), color);
                let name_r = p.text(pos2(rect.left() + 32.0, y), Align2::LEFT_CENTER, &name, font.clone(), color);
                // Where it is (below the searched folder), dimmed, then size and date on the right.
                let parent = h.path.parent().unwrap_or(&h.path);
                let place = match parent.strip_prefix(&s.root) {
                    Ok(rel) if !self.scopes[self.scope].everywhere => {
                        if rel.as_os_str().is_empty() { String::new() } else { rel.display().to_string() }
                    }
                    _ => crate::app::tilde(parent),
                };
                let right = format!(
                    "{}   {}",
                    if h.is_dir { String::new() } else { human_size(h.size) },
                    h.modified.map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
                );
                let right_r = p.text(pos2(rect.right() - 8.0, y), Align2::RIGHT_CENTER, right, small.clone(), th.dim);
                let clip = egui::Rect::from_min_max(pos2(name_r.right() + 12.0, rect.top()), pos2(right_r.left() - 12.0, rect.bottom()));
                p.with_clip_rect(clip).text(pos2(clip.left(), y), Align2::LEFT_CENTER, place, small.clone(), th.dim);
                if r.clicked() {
                    picked = Some(i);
                }
                if r.double_clicked() {
                    jump = true;
                }
            }
        });
        if let Some(i) = picked {
            self.selected = i;
        }
        if jump {
            return self.jump();
        }
        None
    }

    /// Moves the selection in the results by `d` rows (of `n`).
    fn move_list(&mut self, d: isize, n: usize) {
        if n > 0 {
            self.selected = (self.selected as isize + d).clamp(0, n as isize - 1) as usize;
            self.scroll_to_selected = true;
        }
    }

    fn press(&self, button: usize) -> Option<SearchOutcome> {
        match button {
            0 => self.feed(),
            1 => self.jump(),
            _ => Some(SearchOutcome::Close),
        }
    }

    fn jump(&self) -> Option<SearchOutcome> {
        let h = self.search.as_ref()?.hits.get(self.selected)?;
        Some(SearchOutcome::Jump(h.path.clone()))
    }

    fn feed(&self) -> Option<SearchOutcome> {
        let s = self.search.as_ref()?;
        if s.hits.is_empty() {
            return None;
        }
        Some(SearchOutcome::Feed(ResultSet {
            query: self.text.trim().to_string(),
            root: s.root.clone(),
            paths: s.hits.iter().map(|h| h.path.clone()).collect(),
        }))
    }
}

/// 50 000 s -> "14 h".
fn ago(d: Duration) -> String {
    let m = d.as_secs() / 60;
    match m {
        0..=59 => format!("{m} min"),
        60..=2879 => format!("{} h", m / 60),
        _ => format!("{} days", m / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_keys() {
        let base = std::env::temp_dir().join(format!("duopane-search-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("Pictures")).unwrap();
        std::fs::write(base.join("Pictures/holiday.jpg"), b"x").unwrap();
        std::fs::write(base.join("Pictures/holiday-2.jpg"), b"x").unwrap();
        let ctx = egui::Context::default();
        let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let frame = |d: &mut SearchDialog, events: Vec<egui::Event>| {
            let mut out = None;
            let _ = ctx.run(egui::RawInput { events, ..Default::default() }, |ctx| out = d.ui(ctx));
            out
        };
        let open = || {
            let mut d = SearchDialog::new(base.clone(), "holiday jpg", false);
            let t = Instant::now();
            while d.search.as_ref().is_none_or(|s| s.walking) {
                frame(&mut d, vec![]);
                assert!(t.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(5));
            }
            frame(&mut d, vec![]);
            d
        };

        let mut d = open();
        assert_eq!(d.search.as_ref().unwrap().hits.len(), 2);
        match frame(&mut d, vec![key(Key::ArrowDown, Modifiers::NONE), key(Key::Enter, Modifiers::NONE)]) {
            Some(SearchOutcome::Jump(p)) => assert_eq!(p, base.join("Pictures/holiday-2.jpg")),
            _ => panic!("Enter goes to the selected result"),
        }
        let mut d = open();
        match frame(&mut d, vec![key(Key::Enter, Modifiers::COMMAND)]) {
            Some(SearchOutcome::Feed(set)) => {
                assert_eq!(set.paths.len(), 2);
                assert_eq!(set.root, base);
                assert_eq!(set.query, "holiday jpg");
            }
            _ => panic!("Ctrl+Enter shows all in the pane"),
        }
        // Down past the last result reaches the buttons; Enter presses the focused one.
        let mut d = open();
        frame(&mut d, vec![key(Key::ArrowDown, Modifiers::NONE)]);
        frame(&mut d, vec![key(Key::ArrowDown, Modifiers::NONE)]);
        assert_eq!(d.button, Some(0));
        match frame(&mut d, vec![key(Key::Enter, Modifiers::NONE)]) {
            Some(SearchOutcome::Feed(set)) => assert_eq!(set.paths.len(), 2),
            _ => panic!("Enter on \"Show all in the pane\""),
        }
        let mut d = open();
        for k in [Key::ArrowDown, Key::ArrowDown, Key::ArrowLeft] {
            frame(&mut d, vec![key(k, Modifiers::NONE)]);
        }
        assert_eq!(d.button, Some(2), "Left wraps round to Close");
        frame(&mut d, vec![key(Key::ArrowUp, Modifiers::NONE)]);
        assert_eq!((d.button, d.selected), (None, 1), "Up goes back to the results");
        assert!(matches!(frame(&mut d, vec![key(Key::Enter, Modifiers::NONE)]), Some(SearchOutcome::Jump(_))));

        let mut d = open();
        frame(&mut d, vec![key(Key::Tab, Modifiers::NONE)]);
        assert_eq!(d.scope, 1, "Tab widens where to search");
        assert!(matches!(frame(&mut d, vec![key(Key::Escape, Modifiers::NONE)]), Some(SearchOutcome::Close)));
        let _ = std::fs::remove_dir_all(&base);
    }
}
