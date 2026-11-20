//! The settings file, stored as readable JSON so it can be edited by hand
//! (command palette: "Edit settings file").

use crate::pane::SortKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// A command you define yourself, run through the shell.
///
/// Placeholders in `run` are replaced with shell-quoted values:
/// `{path}` the item under the cursor, `{dir}` this pane's folder,
/// `{other}` the other pane's folder, `{selected}` all selected items.
/// The same values are also available as the environment variables
/// `DUOPANE_PATH`, `DUOPANE_DIR`, `DUOPANE_OTHER` and `DUOPANE_SELECTED` (one per line).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CustomCommand {
    pub title: String,
    pub run: String,
    #[serde(default)]
    pub keys: Vec<String>,
}

/// A lister plugin: a command that turns a file into text or a picture for F3.
///
/// `{file}` in `command` is replaced with the shell-quoted path (also in `$DUOPANE_FILE`).
/// `output` is "text" (the command prints text; `syntax` picks code colouring, e.g. "md")
/// or "image" (the command prints a PNG/JPEG picture).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ListerPlugin {
    pub name: String,
    pub extensions: Vec<String>,
    pub command: String,
    #[serde(default = "default_output")]
    pub output: String,
    #[serde(default)]
    pub syntax: String,
}

fn default_output() -> String {
    "text".into()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub left_path: Option<PathBuf>,
    pub right_path: Option<PathBuf>,
    pub show_hidden: bool,
    pub left_sort: SortKey,
    pub left_ascending: bool,
    pub right_sort: SortKey,
    pub right_ascending: bool,
    pub font_size: f32,
    /// Colour scheme name (see theme.rs), e.g. "Duopane Dark", "Nord".
    pub color_scheme: String,
    /// Single colour overrides: key -> "#rrggbb" (keys listed in theme.rs).
    pub custom_colors: BTreeMap<String, String>,
    /// Font family for the interface; empty = built-in.
    pub ui_font: String,
    /// Monospaced font family (lister text and hex); empty = built-in.
    pub mono_font: String,
    /// Font weights, 100 (thin) to 900 (black); 400 is regular, 700 bold.
    pub ui_font_weight: u16,
    pub mono_font_weight: u16,
    /// Lister: wrap long lines.
    pub lister_wrap: bool,
    /// Alternate the brightness of rows in the file lists.
    pub striped_rows: bool,
    /// Colour files by kind (pictures, music, archives…).
    pub color_file_types: bool,
    /// How different striped rows look, in percent of the text colour laid over
    /// the background. None: the colour scheme's own (6 dark, 8 light).
    pub stripe_percent: Option<f32>,
    /// Show the function-key bar (F2 Rename, F3 View, ...) at the bottom.
    pub show_key_bar: bool,
    /// Show a ".." row at the top of every folder.
    pub show_dotdot: bool,
    /// Lister: playback volume, 0.0 to 1.0.
    pub volume: f32,
    pub confirm_delete: bool,
    /// Program used by "Edit" (F4). Empty: the system's default app.
    pub editor: String,
    /// Terminal used by "Open terminal here" (F9). Empty: detect one.
    pub terminal: String,
    pub bookmarks: Vec<PathBuf>,
    /// Masks used with "Select by mask" (numpad +), most recent first.
    pub mask_history: Vec<String>,
    /// Folders you've opened and how often (used to rank "Go to").
    pub visited: BTreeMap<String, u32>,
    /// Override default shortcuts: command id -> list of keys, e.g. "copy": ["F5", "Ctrl+Shift+C"].
    pub key_bindings: BTreeMap<String, Vec<String>>,
    pub custom_commands: Vec<CustomCommand>,
    /// Extra viewers for the lister (F3), tried before the built-in ones.
    pub lister_plugins: Vec<ListerPlugin>,
}

/// The range of the "Row contrast" slider, in percent.
pub const STRIPE_MIN: f32 = 1.0;
pub const STRIPE_MAX: f32 = 30.0;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            left_path: None,
            right_path: None,
            show_hidden: false,
            left_sort: SortKey::Name,
            left_ascending: true,
            right_sort: SortKey::Name,
            right_ascending: true,
            font_size: 14.0,
            color_scheme: "Duopane Dark".into(),
            custom_colors: BTreeMap::new(),
            ui_font: String::new(),
            mono_font: String::new(),
            ui_font_weight: 400,
            mono_font_weight: 400,
            lister_wrap: false,
            striped_rows: true,
            color_file_types: true,
            stripe_percent: None,
            show_key_bar: true,
            show_dotdot: true,
            volume: 0.8,
            confirm_delete: true,
            editor: String::new(),
            terminal: String::new(),
            bookmarks: Vec::new(),
            mask_history: Vec::new(),
            visited: BTreeMap::new(),
            key_bindings: BTreeMap::new(),
            lister_plugins: vec![ListerPlugin {
                name: "SVG picture".into(),
                extensions: vec!["svg".into(), "svgz".into()],
                command: "rsvg-convert --width 1600 --keep-aspect-ratio {file}".into(),
                output: "image".into(),
                syntax: String::new(),
            }],
            custom_commands: vec![CustomCommand {
                title: "Open this folder in the system file browser".into(),
                run: "xdg-open {dir}".into(),
                keys: vec![],
            }],
        }
    }
}

impl Settings {
    pub fn file() -> PathBuf {
        dirs::config_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("duopane")
            .join("settings.json")
    }

    /// Loads the settings; on a broken file returns defaults plus an error message.
    pub fn load() -> (Settings, Option<String>) {
        let path = Self::file();
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(mut s) => {
                    s.font_size = s.font_size.clamp(9.0, 32.0);
                    s.stripe_percent = s.stripe_percent.map(|p| p.clamp(STRIPE_MIN, STRIPE_MAX));
                    s.ui_font_weight = s.ui_font_weight.clamp(100, 900);
                    s.mono_font_weight = s.mono_font_weight.clamp(100, 900);
                    (s, None)
                }
                Err(e) => (Settings::default(), Some(format!("Settings file has an error ({e}); using defaults"))),
            },
            Err(_) => (Settings::default(), None),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::file();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn record_visit(&mut self, path: &std::path::Path) {
        let key = path.display().to_string();
        *self.visited.entry(key).or_insert(0) += 1;
        if self.visited.len() > 600 {
            // Forget the least-visited folders.
            let mut counts: Vec<(String, u32)> = self.visited.iter().map(|(k, v)| (k.clone(), *v)).collect();
            counts.sort_by_key(|(_, v)| *v);
            for (k, _) in counts.into_iter().take(100) {
                self.visited.remove(&k);
            }
        }
    }
}
