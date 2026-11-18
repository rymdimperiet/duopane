//! Every command the app knows, with its default shortcuts.
//!
//! To add a command: add an entry to `builtin()` with a unique `id`, a title
//! (shown in the command palette) and a function that receives the app.
//! Shortcuts can be overridden in the settings file under `key_bindings`.

use crate::app::App;
use crate::pane::SortKey;
use eframe::egui::{self, Key, Modifiers};

pub type Run = fn(&mut App, &egui::Context);

pub struct Command {
    pub id: &'static str,
    pub title: &'static str,
    pub keys: &'static [&'static str],
    pub run: Run,
}

pub fn builtin() -> Vec<Command> {
    macro_rules! cmd {
        ($id:expr, $title:expr, [$($k:expr),*], $run:expr) => {
            Command { id: $id, title: $title, keys: &[$($k),*], run: $run }
        };
    }
    vec![
        cmd!("open", "Open", ["Enter"], |a, ctx| a.open_cursor(ctx)),
        cmd!("open_external", "Open with the system's app", ["Shift+Enter"], |a, _| a.open_external()),
        // Back/forward keys (and the mouse's back/forward buttons) move up and down the folder tree.
        cmd!("go_up", "Go up to the parent folder", ["Backspace", "Alt+Up", "Left", "Alt+Left"], |a, _| a.go_up()),
        cmd!("enter_folder", "Go down into the folder under the cursor", ["Right", "Alt+Right"], |a, _| a.enter_folder()),
        cmd!("switch_pane", "Switch to the other pane", ["Tab"], |a, _| a.switch_pane()),
        cmd!("go_to", "Go to folder…", ["Ctrl+P"], |a, ctx| a.open_goto(ctx)),
        cmd!("command_palette", "Show all commands…", ["Ctrl+Shift+P"], |a, _| a.open_palette()),
        cmd!("copy", "Copy to the other pane…", ["F5"], |a, _| a.ask_transfer(false)),
        cmd!("move", "Move to the other pane…", ["F6"], |a, _| a.ask_transfer(true)),
        cmd!("rename", "Rename…", ["F2", "Shift+F6"], |a, _| a.ask_rename()),
        cmd!("pack", "Pack files (compress)…", ["Alt+F5"], |a, _| a.ask_pack()),
        cmd!("unpack", "Unpack archive…", ["Alt+F9"], |a, _| a.ask_unpack()),
        cmd!("new_folder", "New folder…", ["F7"], |a, _| a.ask_new_folder()),
        cmd!("new_file", "New file…", ["Shift+F4"], |a, _| a.ask_new_file()),
        cmd!("view", "View file (lister)", ["F3"], |a, _| a.view_file()),
        cmd!("edit", "Edit file", ["F4"], |a, _| a.edit_cursor()),
        cmd!("delete", "Move to trash", ["F8", "Delete"], |a, _| a.ask_delete(false)),
        cmd!("delete_permanently", "Delete permanently", ["Shift+F8", "Shift+Delete"], |a, _| a.ask_delete(true)),
        cmd!("toggle_hidden", "Show / hide hidden files", ["Ctrl+Period", "Ctrl+H"], |a, _| a.toggle_hidden()),
        cmd!("reload", "Reload both panes", ["Ctrl+R"], |a, _| a.reload_all()),
        cmd!("select_all", "Select all", ["Ctrl+A"], |a, _| a.active_pane().select_all()),
        cmd!("deselect_all", "Deselect all", ["Ctrl+D"], |a, _| a.active_pane().selected.clear()),
        cmd!("invert_selection", "Invert selection", ["Ctrl+I"], |a, _| a.active_pane().invert_selection()),
        // Like Double Commander: numpad + selects by mask, numpad - deselects, numpad * inverts.
        cmd!("select_mask", "Select by mask…", ["Plus"], |a, _| a.ask_mask(true)),
        cmd!("unselect_mask", "Deselect by mask…", ["Minus"], |a, _| a.ask_mask(false)),
        cmd!("back", "History: previous folder", [], |a, _| a.back()),
        cmd!("forward", "History: next folder", [], |a, _| a.forward()),
        cmd!("open_in_left", "Open folder in the left pane", ["Ctrl+Left"], |a, _| a.open_in(0)),
        cmd!("open_in_right", "Open folder in the right pane", ["Ctrl+Right"], |a, _| a.open_in(1)),
        cmd!("mirror", "Show this folder in the other pane", ["Ctrl+M"], |a, _| a.mirror()),
        cmd!("swap_panes", "Swap the two panes", ["Ctrl+U"], |a, _| a.swap_panes()),
        cmd!("edit_path", "Type a location…", ["Ctrl+L"], |a, _| a.edit_path()),
        cmd!("copy_path", "Copy path to clipboard", ["Ctrl+Shift+C"], |a, ctx| a.copy_paths(ctx)),
        cmd!("open_terminal", "Open terminal here", ["F9"], |a, _| a.open_terminal()),
        cmd!("add_bookmark", "Bookmark this folder", ["Ctrl+B"], |a, _| a.add_bookmark()),
        cmd!("remove_bookmark", "Remove bookmark for this folder", [], |a, _| a.remove_bookmark()),
        cmd!("home", "Go to home folder", ["Alt+Home"], |a, _| a.go_home()),
        cmd!("sort_name", "Sort by name", [], |a, _| a.sort_by(SortKey::Name)),
        cmd!("sort_size", "Sort by size", [], |a, _| a.sort_by(SortKey::Size)),
        cmd!("sort_modified", "Sort by date modified", [], |a, _| a.sort_by(SortKey::Modified)),
        cmd!("sort_extension", "Sort by file type", [], |a, _| a.sort_by(SortKey::Extension)),
        cmd!("folder_size", "Calculate size of selection", ["Ctrl+Shift+S"], |a, _| a.calculate_size()),
        cmd!("cancel_job", "Cancel the running file operation", [], |a, _| a.cancel_job()),
        cmd!("edit_settings", "Edit settings file", ["Ctrl+Comma"], |a, _| a.edit_settings()),
        cmd!("reload_settings", "Reload settings file", [], |a, ctx| a.reload_settings(ctx)),
        cmd!("font_bigger", "Make text bigger", ["Ctrl+Plus", "Ctrl+Equals"], |a, ctx| a.change_font(ctx, 1.0)),
        cmd!("font_smaller", "Make text smaller", ["Ctrl+Minus"], |a, ctx| a.change_font(ctx, -1.0)),
        cmd!("appearance", "Appearance: colour scheme and fonts…", ["F12"], |a, _| a.open_appearance()),
        cmd!("help", "Keyboard shortcuts", ["F1"], |a, _| a.show_help()),
        cmd!("quit", "Quit", ["Ctrl+Q", "F10"], |_, ctx| ctx.send_viewport_cmd(egui::ViewportCommand::Close)),
    ]
}

/// A key plus modifiers, e.g. "Ctrl+Shift+P".
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Shortcut {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Shortcut {
    pub fn from_event(key: Key, m: Modifiers) -> Self {
        Shortcut { key, ctrl: m.command || m.ctrl, shift: m.shift, alt: m.alt }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut s = Shortcut { key: Key::Escape, ctrl: false, shift: false, alt: false };
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        // "Ctrl++" means Ctrl and the plus key.
        let (mods, key) = if text.ends_with("++") {
            (&parts[..parts.len().saturating_sub(2)], "Plus")
        } else {
            let (k, m) = parts.split_last()?;
            (m, *k)
        };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" => s.ctrl = true,
                "shift" => s.shift = true,
                "alt" | "option" => s.alt = true,
                "" => {}
                _ => return None,
            }
        }
        let alias = match key.to_ascii_lowercase().as_str() {
            "up" => "ArrowUp",
            "down" => "ArrowDown",
            "left" => "ArrowLeft",
            "right" => "ArrowRight",
            "del" => "Delete",
            "esc" => "Escape",
            "ins" => "Insert",
            "pgup" => "PageUp",
            "pgdn" | "pgdown" => "PageDown",
            "return" => "Enter",
            "." | "period" => "Period",
            "," | "comma" => "Comma",
            "-" | "minus" => "Minus",
            "=" | "equals" => "Equals",
            "plus" => "Plus",
            _ => key,
        };
        s.key = Key::from_name(alias).or_else(|| {
            // Accept lowercase letters and names like "f5".
            let upper = alias.to_ascii_uppercase();
            Key::from_name(&upper).or_else(|| {
                let mut c = alias.chars();
                let cap: String = c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default();
                Key::from_name(&cap)
            })
        })?;
        Some(s)
    }

    pub fn label(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        let k = match self.key {
            Key::ArrowUp => "Up",
            Key::ArrowDown => "Down",
            Key::ArrowLeft => "Left",
            Key::ArrowRight => "Right",
            Key::Period => ".",
            Key::Comma => ",",
            Key::Plus => "+",
            Key::Minus => "-",
            Key::Equals => "=",
            other => other.name(),
        };
        out.push_str(k);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shortcuts() {
        let s = Shortcut::parse("Ctrl+Shift+P").unwrap();
        assert!(s.ctrl && s.shift && !s.alt);
        assert_eq!(s.key, Key::P);
        assert_eq!(Shortcut::parse("F5").unwrap().key, Key::F5);
        assert_eq!(Shortcut::parse("Ctrl+.").unwrap().key, Key::Period);
        assert_eq!(Shortcut::parse("Alt+Left").unwrap().key, Key::ArrowLeft);
        assert_eq!(Shortcut::parse("Ctrl+Period").unwrap().label(), "Ctrl+.");
    }

    #[test]
    fn every_default_shortcut_parses() {
        for c in builtin() {
            for k in c.keys {
                assert!(Shortcut::parse(k).is_some(), "{} has bad key {k}", c.id);
            }
        }
    }
}
