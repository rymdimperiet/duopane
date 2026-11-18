//! Colour schemes, fonts and sizes.
//!
//! Pick a scheme and fonts in the Appearance dialog (F12). Single colours can be
//! overridden in the settings file under `custom_colors`, e.g. `"folder": "#ffcc00"`.
//! Keys: bg, pane, header, border, accent, text, dim, folder, symlink, selected,
//! cursor, cursor_inactive, error, ok, stripe (the darker/brighter every-other row).

use crate::settings::Settings;
use eframe::egui::{self, Color32, FontFamily, FontId, TextStyle};
use std::cell::Cell;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub light: bool,
    pub bg: Color32,
    pub pane: Color32,
    pub header: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub folder: Color32,
    pub symlink: Color32,
    pub selected: Color32,
    pub cursor: Color32,
    pub cursor_inactive: Color32,
    pub error: Color32,
    pub ok: Color32,
    /// Background of every other row (None = rows not striped).
    pub stripe: Option<Color32>,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

#[allow(clippy::too_many_arguments)]
const fn t(light: bool, bg: u32, pane: u32, header: u32, border: u32, accent: u32, text: u32, dim: u32,
           folder: u32, symlink: u32, selected: u32, cursor: u32, cursor_inactive: u32, error: u32, ok: u32) -> Theme {
    Theme {
        light, bg: hex(bg), pane: hex(pane), header: hex(header), border: hex(border), accent: hex(accent),
        text: hex(text), dim: hex(dim), folder: hex(folder), symlink: hex(symlink), selected: hex(selected),
        cursor: hex(cursor), cursor_inactive: hex(cursor_inactive), error: hex(error), ok: hex(ok), stripe: None,
    }
}

/// The built-in colour schemes. Add your own here.
pub const SCHEMES: &[(&str, Theme)] = &[
    ("Duopane Dark", t(false, 0x1b1d22, 0x21242a, 0x2a2e36, 0x343942, 0x4c9aff, 0xd6d9df, 0x8b919c,
                       0x8cc8ff, 0xb49cff, 0xffb454, 0x2b4f7d, 0x333841, 0xff6b6b, 0x7ed38c)),
    ("Duopane Light", t(true, 0xe9ecf0, 0xffffff, 0xdfe3e8, 0xc5cbd3, 0x2f6fd6, 0x1f2328, 0x646d78,
                        0x1d5fbf, 0x7a4fd6, 0xc2410c, 0xcfe0fb, 0xe3e7ec, 0xc62828, 0x2e7d32)),
    ("Commander Blue", t(false, 0x000060, 0x0000a8, 0x00007a, 0x5f87ff, 0x55ffff, 0x55ffff, 0x9fb4ff,
                         0xffffff, 0xff9fff, 0xffff55, 0x008080, 0x00008a, 0xff5555, 0x55ff55)),
    ("Solarized Dark", t(false, 0x002b36, 0x073642, 0x0b3f4c, 0x2a5561, 0x268bd2, 0x93a1a1, 0x657b83,
                         0x2aa198, 0x6c71c4, 0xb58900, 0x114d5e, 0x0b3f4c, 0xdc322f, 0x859900)),
    ("Nord", t(false, 0x2e3440, 0x3b4252, 0x434c5e, 0x4c566a, 0x88c0d0, 0xe5e9f0, 0xa3abb9,
               0x81a1c1, 0xb48ead, 0xebcb8b, 0x4c5f7a, 0x434c5e, 0xbf616a, 0xa3be8c)),
    ("Gruvbox Dark", t(false, 0x1d2021, 0x282828, 0x32302f, 0x504945, 0x83a598, 0xebdbb2, 0xa89984,
                       0x83a598, 0xd3869b, 0xfabd2f, 0x45403d, 0x3c3836, 0xfb4934, 0xb8bb26)),
    ("High Contrast", t(false, 0x000000, 0x000000, 0x1a1a1a, 0xffffff, 0xffff00, 0xffffff, 0xd0d0d0,
                        0x00ffff, 0xff80ff, 0xffff00, 0x0000c0, 0x303030, 0xff4040, 0x40ff40)),
];

thread_local! {
    static CURRENT: Cell<Theme> = const { Cell::new(SCHEMES[0].1) };
}

/// The colours in use right now.
pub fn c() -> Theme {
    CURRENT.with(|t| t.get())
}

/// Height of one file-list row for a given text size.
pub fn row_height(font_size: f32) -> f32 {
    (font_size * 1.6).round().max(22.0)
}

pub fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    u32::from_str_radix(s, 16).ok().map(hex)
}

/// Applies the scheme, custom colours, fonts and text size from the settings.
pub fn apply(ctx: &egui::Context, s: &Settings) {
    let mut th = SCHEMES.iter().find(|(n, _)| *n == s.color_scheme).map(|x| x.1).unwrap_or(SCHEMES[0].1);
    for (k, v) in &s.custom_colors {
        let Some(col) = parse_hex(v) else { continue };
        let slot = match k.as_str() {
            "bg" => &mut th.bg,
            "pane" => &mut th.pane,
            "header" => &mut th.header,
            "border" => &mut th.border,
            "accent" => &mut th.accent,
            "text" => &mut th.text,
            "dim" => &mut th.dim,
            "folder" => &mut th.folder,
            "symlink" => &mut th.symlink,
            "selected" => &mut th.selected,
            "cursor" => &mut th.cursor,
            "cursor_inactive" => &mut th.cursor_inactive,
            "error" => &mut th.error,
            "ok" => &mut th.ok,
            _ => continue,
        };
        *slot = col;
    }
    th.stripe = s.striped_rows.then(|| {
        s.custom_colors
            .get("stripe")
            .and_then(|v| parse_hex(v))
            .unwrap_or_else(|| th.text.gamma_multiply(if th.light { 0.08 } else { 0.06 }))
    });
    CURRENT.with(|x| x.set(th));

    let mut v = if th.light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = th.bg;
    v.window_fill = th.header;
    v.extreme_bg_color = th.pane;
    v.faint_bg_color = th.pane;
    v.selection.bg_fill = th.cursor;
    v.selection.stroke.color = th.text;
    v.hyperlink_color = th.accent;
    v.window_stroke.color = th.border;
    v.override_text_color = Some(th.text);
    v.widgets.noninteractive.bg_stroke.color = th.border;
    v.widgets.inactive.weak_bg_fill = th.cursor_inactive;
    v.widgets.inactive.bg_fill = th.border; // also slider tracks and checkboxes
    v.widgets.hovered.weak_bg_fill = th.cursor;
    v.widgets.hovered.bg_fill = th.cursor;
    v.widgets.active.weak_bg_fill = th.cursor;
    ctx.set_visuals(v);

    apply_fonts(ctx, &s.ui_font, &s.mono_font);

    let size = s.font_size;
    ctx.style_mut(|st| {
        st.text_styles.insert(TextStyle::Body, FontId::new(size, FontFamily::Proportional));
        st.text_styles.insert(TextStyle::Button, FontId::new(size, FontFamily::Proportional));
        st.text_styles.insert(TextStyle::Monospace, FontId::new(size, FontFamily::Monospace));
        st.text_styles.insert(TextStyle::Small, FontId::new(size - 2.0, FontFamily::Proportional));
        st.text_styles.insert(TextStyle::Heading, FontId::new(size + 4.0, FontFamily::Proportional));
    });
}

thread_local! {
    static LOADED_FONTS: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
}

/// Puts the chosen system fonts first; egui's built-in fonts stay as fallback (icons, emoji).
fn apply_fonts(ctx: &egui::Context, ui_font: &str, mono_font: &str) {
    let key = (ui_font.to_string(), mono_font.to_string());
    if LOADED_FONTS.with(|l| l.borrow().as_ref() == Some(&key)) {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    for (name, family, slot) in [(ui_font, FontFamily::Proportional, "user-ui"), (mono_font, FontFamily::Monospace, "user-mono")] {
        if name.is_empty() {
            continue;
        }
        if let Some((bytes, index)) = load_family(name) {
            let mut fd = egui::FontData::from_owned(bytes);
            fd.index = index;
            fonts.font_data.insert(slot.into(), Arc::new(fd));
            fonts.families.entry(family).or_default().insert(0, slot.into());
        }
    }
    ctx.set_fonts(fonts);
    LOADED_FONTS.with(|l| *l.borrow_mut() = Some(key));
}

fn db() -> &'static fontdb::Database {
    static DB: OnceLock<fontdb::Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut d = fontdb::Database::new();
        d.load_system_fonts();
        d
    })
}

/// Font families installed on this computer (only monospaced ones with `mono_only`).
pub fn font_families(mono_only: bool) -> &'static [String] {
    static ALL: OnceLock<Vec<String>> = OnceLock::new();
    static MONO: OnceLock<Vec<String>> = OnceLock::new();
    let build = |mono: bool| {
        let mut v: Vec<String> = db()
            .faces()
            .filter(|f| !mono || f.monospaced)
            .filter_map(|f| f.families.first().map(|(n, _)| n.clone()))
            .collect();
        v.sort_by_key(|a| a.to_lowercase());
        v.dedup();
        v
    };
    if mono_only {
        MONO.get_or_init(|| build(true))
    } else {
        ALL.get_or_init(|| build(false))
    }
}

fn load_family(name: &str) -> Option<(Vec<u8>, u32)> {
    let families = [fontdb::Family::Name(name)];
    let q = fontdb::Query { families: &families, ..Default::default() };
    let id = db().query(&q)?;
    db().with_face_data(id, |data, index| (data.to_vec(), index))
}
