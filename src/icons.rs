//! Small file-type icons in front of names, like eza's.
//!
//! They come from Nerd Fonts' icon sets. Only the icons used here are bundled,
//! in `assets/icons.ttf`; after adding one, run `sh packaging/make-icon-font.sh`.

use crate::filetypes::Category;

/// The icon for a list entry.
pub fn icon(name: &str, is_dir: bool, executable: bool, is_symlink: bool) -> char {
    if is_dir {
        return if is_symlink { '\u{f482}' } else { '\u{e5ff}' };
    }
    if is_symlink {
        return '\u{f481}';
    }
    let lower = name.to_lowercase();
    match lower.as_str() {
        "makefile" | "justfile" | "cmakelists.txt" => return '\u{f0ad}',
        "dockerfile" | "containerfile" => return '\u{f308}',
        "pkgbuild" => return '\u{f303}',
        ".gitignore" | ".gitattributes" | ".gitmodules" => return '\u{f1d3}',
        _ => {}
    }
    let ext = match lower.rfind('.') {
        Some(i) if i > 0 => &lower[i + 1..],
        _ => "",
    };
    let specific = match ext {
        "pdf" => '\u{f1c1}',
        "doc" | "docx" | "odt" | "rtf" | "pages" => '\u{f1c2}',
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => '\u{f1c3}',
        "ppt" | "pptx" | "odp" => '\u{f1c4}',
        "txt" | "log" => '\u{f15c}',
        "md" | "markdown" => '\u{f48a}',
        "epub" | "mobi" | "azw3" | "djvu" => '\u{f02d}',
        "rs" => '\u{e7a8}',
        "py" | "ipynb" => '\u{e606}',
        "js" | "mjs" | "cjs" | "jsx" => '\u{e74e}',
        "ts" | "tsx" => '\u{e628}',
        "html" | "htm" => '\u{f13b}',
        "css" | "scss" | "sass" | "less" => '\u{e749}',
        "json" | "jsonc" => '\u{e60b}',
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => '\u{f489}',
        "c" | "h" => '\u{e61e}',
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => '\u{e61d}',
        "go" => '\u{e626}',
        "java" | "jar" | "kt" | "kts" => '\u{e738}',
        "rb" | "gem" => '\u{e739}',
        "php" => '\u{e73d}',
        "lua" => '\u{e620}',
        "vim" => '\u{e62b}',
        "yaml" | "yml" | "toml" | "ini" | "conf" | "cfg" => '\u{e615}',
        "lock" => '\u{f023}',
        "nix" => '\u{f313}',
        "sql" | "db" | "sqlite" | "sqlite3" => '\u{f1c0}',
        "iso" | "img" | "dmg" => '\u{e271}',
        "deb" => '\u{e77d}',
        "exe" | "msi" => '\u{f17a}',
        "appimage" | "run" => '\u{f17c}',
        "ttf" | "otf" | "woff" | "woff2" => '\u{f031}',
        _ => '\0',
    };
    if specific != '\0' {
        return specific;
    }
    match crate::filetypes::category(name, executable) {
        Some(Category::Picture) => '\u{f1c5}',
        Some(Category::Video) => '\u{f03d}',
        Some(Category::Music) => '\u{f001}',
        Some(Category::Document) => '\u{f15c}',
        Some(Category::Archive) => '\u{f410}',
        Some(Category::Code) => '\u{f1c9}',
        Some(Category::Program) => '\u{f489}',
        Some(Category::Temporary) => '\u{f1da}',
        None => '\u{f15b}',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_in_the_bundled_font() {
        let ctx = eframe::egui::Context::default();
        crate::theme::apply_fonts(&ctx, ("", 400), ("", 400));
        let _ = ctx.run(Default::default(), |_| {});
        let src = include_str!("icons.rs");
        let font = crate::theme::icon_font(&ctx, 14.0).expect("the icon font is loaded");
        let mut n = 0;
        for part in src.split("'\\u{").skip(1) {
            let hex = &part[..part.find('}').unwrap()];
            let c = char::from_u32(u32::from_str_radix(hex, 16).unwrap()).unwrap();
            if c != '\0' {
                assert!(ctx.fonts(|f| f.has_glyph(&font, c)), "icon U+{hex} is missing: run packaging/make-icon-font.sh");
                n += 1;
            }
        }
        assert!(n > 40);
        assert_eq!(icon("Report.PDF", false, false, false), '\u{f1c1}');
        assert_eq!(icon("photo.jpg", false, false, false), '\u{f1c5}');
        assert_eq!(icon("src", true, false, false), '\u{e5ff}');
        assert_eq!(icon("notes", false, false, false), '\u{f15b}');
    }
}
