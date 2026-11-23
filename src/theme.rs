//! Colour schemes, fonts and sizes.
//!
//! Pick a scheme and fonts in the Appearance dialog (F12). Single colours can be
//! overridden in the settings file under `custom_colors`, e.g. `"folder": "#ffcc00"`.
//! Keys: bg, pane, header, border, accent, text, dim, folder, symlink, selected,
//! cursor, cursor_inactive, error, ok, stripe (the darker/brighter every-other row),
//! and the file kinds: picture, video, music, document, archive, code, program,
//! temporary (see `filetypes.rs`).

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
    /// Colours for the kinds of file, in `Category::ALL` order (None = not coloured).
    pub files: Option<[Color32; 8]>,
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
        files: None,
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

/// Each scheme's colours for the kinds of file: picture, video, music, document,
/// archive, code, program, temporary. Each is readable on the scheme's background
/// (WCAG contrast 4.5, temporary 3) and clearly apart from its folder, selection,
/// link and text colours and from each other; the scheme's own palette colours are
/// used where they pass. Change with care: test the same way before swapping one.
const FILE_COLORS: &[(&str, [u32; 8])] = &[
    ("Duopane Dark", [0xf5a3c7, 0xeb91f3, 0x6fd6c8, 0xefe2be, 0xf28b82, 0xa8d98b, 0xe6e36b, 0x6b717c]),
    ("Duopane Light", [0xc2185b, 0x8f23a4, 0x0f857b, 0x6d4c41, 0xd5393e, 0x2e7d32, 0x8b7504, 0x8d949a]),
    ("Commander Blue", [0xff55ff, 0xe2a7e2, 0x56fbd1, 0xd6c4b3, 0xfb6456, 0x55ff55, 0xafff5f, 0x8787af]),
    ("Solarized Dark", [0xfb60a0, 0xbb90da, 0x05a7c7, 0xeee8d5, 0xfb6c56, 0x60ac39, 0xc6b353, 0x707d89]),
    ("Nord", [0xfdb4df, 0xcfa7ec, 0x94d5db, 0xd1c0a9, 0xf39599, 0xa3be8c, 0xf0a882, 0x848fa4]),
    ("Gruvbox Dark", [0xfdc3d7, 0xdb94db, 0x8ec07c, 0xbb9e81, 0xfa563d, 0xb8bb26, 0xfe8019, 0x7c6f64]),
    ("High Contrast", [0xff80c0, 0xc383f1, 0x00ff80, 0xe0e0a0, 0xff6060, 0x84fc79, 0xff9000, 0x80878e]),
];

/// The colour for a kind of file, if files are coloured by kind.
pub fn file_color(cat: crate::filetypes::Category) -> Option<Color32> {
    let i = crate::filetypes::Category::ALL.iter().position(|c| *c == cat)?;
    c().files.map(|f| f[i])
}

thread_local! {
    static CURRENT: Cell<Theme> = const { Cell::new(SCHEMES[0].1) };
}

/// The colours in use right now.
pub fn c() -> Theme {
    CURRENT.with(|t| t.get())
}

/// How strong the row stripes are, in percent (the setting, or the scheme's default).
pub fn stripe_percent(s: &Settings, light: bool) -> f32 {
    s.stripe_percent.unwrap_or(if light { 8.0 } else { 6.0 })
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
    // File kinds: the scheme's colours (light schemes fall back to Duopane Light's,
    // dark ones to Duopane Dark's), each overridable by its key in custom_colors.
    th.files = s.color_file_types.then(|| {
        let fallback = if th.light { FILE_COLORS[1].1 } else { FILE_COLORS[0].1 };
        let base = FILE_COLORS.iter().find(|(n, _)| *n == s.color_scheme).map_or(fallback, |x| x.1);
        let mut out = base.map(hex);
        for (i, cat) in crate::filetypes::Category::ALL.iter().enumerate() {
            if let Some(col) = s.custom_colors.get(cat.key()).and_then(|v| parse_hex(v)) {
                out[i] = col;
            }
        }
        out
    });
    th.stripe = s.striped_rows.then(|| {
        s.custom_colors
            .get("stripe")
            .and_then(|v| parse_hex(v))
            .unwrap_or_else(|| th.text.gamma_multiply(stripe_percent(s, th.light) / 100.0))
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

    apply_fonts(ctx, (&s.ui_font, s.ui_font_weight), (&s.mono_font, s.mono_font_weight));

    let size = s.font_size;
    ctx.style_mut(|st| {
        st.text_styles.insert(TextStyle::Body, FontId::new(size, FontFamily::Proportional));
        st.text_styles.insert(TextStyle::Button, FontId::new(size, FontFamily::Proportional));
        st.text_styles.insert(TextStyle::Monospace, FontId::new(size, FontFamily::Monospace));
        st.text_styles.insert(TextStyle::Small, FontId::new(size - 2.0, FontFamily::Proportional));
        st.text_styles.insert(TextStyle::Heading, FontId::new(size + 4.0, FontFamily::Proportional));
    });
}

/// A font family and weight, e.g. ("Inter", 500).
type FontChoice<'a> = (&'a str, u16);
/// The interface and monospace fonts last loaded (so unchanged fonts aren't reloaded).
type LoadedFonts = ((String, u16), (String, u16));

thread_local! {
    static LOADED_FONTS: std::cell::RefCell<Option<LoadedFonts>> = const { std::cell::RefCell::new(None) };
}

/// Puts the chosen system fonts first; egui's built-in fonts stay as fallback (icons, emoji).
fn apply_fonts(ctx: &egui::Context, ui_font: FontChoice, mono_font: FontChoice) {
    let key = ((ui_font.0.to_string(), ui_font.1), (mono_font.0.to_string(), mono_font.1));
    if LOADED_FONTS.with(|l| l.borrow().as_ref() == Some(&key)) {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    for ((name, weight), family, slot) in [(ui_font, FontFamily::Proportional, "user-ui"), (mono_font, FontFamily::Monospace, "user-mono")] {
        if name.is_empty() {
            continue;
        }
        if let Some((bytes, index)) = load_family(name, weight) {
            let mut fd = egui::FontData::from_owned(bytes);
            fd.index = index;
            fonts.font_data.insert(slot.into(), Arc::new(fd));
            fonts.families.entry(family).or_default().insert(0, slot.into());
        }
    }
    // Neither the chosen fonts nor egui's own cover every script, so file names in
    // Japanese, Arabic and so on would show as empty boxes without these.
    for (slot, fd) in fallback_fonts() {
        fonts.font_data.insert(slot.clone(), fd.clone());
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(slot.clone());
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

/// The weights (100–900) installed for a font family, upright normal-width faces
/// only, lightest first. A variable font shows as one weight: egui can't pick others from it.
pub fn font_weights(name: &str) -> Vec<u16> {
    let mut v: Vec<u16> = db()
        .faces()
        .filter(|f| f.style == fontdb::Style::Normal && f.stretch == fontdb::Stretch::Normal)
        .filter(|f| f.families.iter().any(|(n, _)| n == name))
        .map(|f| f.weight.0)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// "Bold" for 700, and so on.
pub fn weight_name(w: u16) -> &'static str {
    match w {
        0..=149 => "Thin",
        150..=249 => "Extra Light",
        250..=349 => "Light",
        350..=449 => "Regular",
        450..=549 => "Medium",
        550..=649 => "Semi Bold",
        650..=749 => "Bold",
        750..=849 => "Extra Bold",
        _ => "Black",
    }
}

/// The font file for a family, as close to `weight` as it comes (upright).
/// For each script egui can't draw on its own, the first of these fonts that is installed.
/// Japanese variants come first among the CJK fonts; they cover Chinese and Korean too.
const FALLBACKS: &[&[&str]] = &[
    &["Noto Sans CJK JP", "Source Han Sans JP", "Noto Sans JP", "Source Han Sans", "Noto Sans CJK SC", "Noto Sans CJK KR", "WenQuanYi Zen Hei", "Droid Sans Fallback", "IPAGothic", "VL Gothic", "Takao Gothic", "Yu Gothic", "Meiryo", "MS Gothic", "Microsoft YaHei", "Hiragino Sans", "PingFang SC", "Arial Unicode MS"],
    &["Noto Sans Arabic", "Noto Naskh Arabic", "DejaVu Sans", "Segoe UI", "Geeza Pro"],
    &["Noto Sans Hebrew", "DejaVu Sans", "Segoe UI", "Arial Hebrew"],
    &["Noto Sans Thai", "Noto Sans Thai Looped", "Leelawadee UI", "Thonburi"],
    &["Noto Sans Devanagari", "Nirmala UI", "Kohinoor Devanagari"],
    &["Noto Sans Symbols 2", "DejaVu Sans", "Segoe UI Symbol"],
    &["Noto Sans Armenian", "Noto Serif Armenian", "DejaVu Sans"],
    &["Noto Sans Georgian", "Noto Serif Georgian", "DejaVu Sans"],
    &["Noto Sans Ethiopic", "Noto Serif Ethiopic"],
    &["Noto Sans Bengali", "Noto Serif Bengali"],
    &["Noto Sans Tamil", "Noto Serif Tamil"],
    &["Noto Sans Telugu", "Noto Serif Telugu"],
    &["Noto Sans Kannada", "Noto Serif Kannada"],
    &["Noto Sans Malayalam", "Noto Serif Malayalam"],
    &["Noto Sans Gujarati", "Noto Serif Gujarati"],
    &["Noto Sans Gurmukhi", "Noto Serif Gurmukhi"],
    &["Noto Sans Sinhala", "Noto Serif Sinhala"],
    &["Noto Sans Khmer", "Noto Serif Khmer"],
    &["Noto Sans Lao", "Noto Serif Lao"],
    &["Noto Sans Myanmar", "Noto Serif Myanmar"],
    &["Noto Serif Tibetan", "Noto Sans Tibetan"],
];

/// The installed fallback fonts, read once (a CJK font is tens of megabytes).
fn fallback_fonts() -> &'static [(String, Arc<egui::FontData>)] {
    static FONTS: OnceLock<Vec<(String, Arc<egui::FontData>)>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut out: Vec<(String, Arc<egui::FontData>)> = Vec::new();
        for group in FALLBACKS {
            let Some(name) = group.iter().find(|n| has_family(n)) else { continue };
            let slot = format!("fallback-{name}");
            if out.iter().any(|(s, _)| *s == slot) {
                continue;
            }
            if let Some((bytes, index)) = load_family(name, 400) {
                let mut fd = egui::FontData::from_owned(bytes);
                fd.index = index;
                out.push((slot, Arc::new(fd)));
            }
        }
        out
    })
}

fn has_family(name: &str) -> bool {
    db().faces().any(|f| f.families.iter().any(|(n, _)| n == name))
}

fn load_family(name: &str, weight: u16) -> Option<(Vec<u8>, u32)> {
    let families = [fontdb::Family::Name(name)];
    let q = fontdb::Query { families: &families, weight: fontdb::Weight(weight), ..Default::default() };
    let id = db().query(&q)?;
    db().with_face_data(id, |data, index| (data.to_vec(), index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_names_have_glyphs_when_a_cjk_font_is_installed() {
        if !FALLBACKS[0].iter().any(|n| has_family(n)) {
            return; // nothing to fall back on here (e.g. a bare CI machine)
        }
        let ctx = egui::Context::default();
        apply_fonts(&ctx, ("", 400), ("", 400));
        let _ = ctx.run(Default::default(), |_| {});
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let id = FontId::new(14.0, family);
            assert!(ctx.fonts(|f| f.has_glyphs(&id, "日本語のファイル 한글")));
        }
    }
}
