//! Sorting files into a few kinds, for colouring them in the lists.
//!
//! The groups follow the ones terminal tools have settled on (vivid's file
//! types database, eza, Midnight Commander): pictures, video, music, documents,
//! archives, code, programs and temporary files. Eight is plenty: more colours
//! than that stop telling you anything at a glance.

/// What kind of file something is, by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Picture,
    Video,
    Music,
    Document,
    Archive,
    Code,
    Program,
    Temporary,
}

impl Category {
    pub const ALL: [Category; 8] = [
        Category::Picture,
        Category::Video,
        Category::Music,
        Category::Document,
        Category::Archive,
        Category::Code,
        Category::Program,
        Category::Temporary,
    ];

    /// The key used for it under `custom_colors` in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Category::Picture => "picture",
            Category::Video => "video",
            Category::Music => "music",
            Category::Document => "document",
            Category::Archive => "archive",
            Category::Code => "code",
            Category::Program => "program",
            Category::Temporary => "temporary",
        }
    }
}

const PICTURE: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "tif", "tiff", "svg", "svgz", "ico", "heic", "heif", "avif", "jxl", "qoi",
    "tga", "pnm", "pbm", "pgm", "ppm", "psd", "xcf", "kra", "ora", "eps", "ai", "raw", "dng", "cr2", "cr3", "nef", "arw",
    "orf", "rw2", "raf", "exr", "hdr",
];
const VIDEO: &[&str] =
    &["mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "mpg", "mpeg", "m2ts", "mts", "flv", "ogv", "3gp", "vob", "rmvb"];
const MUSIC: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "wav", "m4a", "aac", "wma", "aif", "aiff", "alac", "ape", "wv", "mka", "mid", "midi",
    "dsf", "dff", "m3u", "m3u8", "pls", "cue",
];
const DOCUMENT: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "csv", "tsv", "ppt", "pptx", "odp", "odg", "epub", "mobi",
    "azw3", "djvu", "txt", "md", "markdown", "org", "rst", "tex", "pages", "numbers",
];
const ARCHIVE: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zst", "tzst", "lz", "lzma", "lz4", "7z", "rar", "cab", "cpio",
    "iso", "img", "dmg", "deb", "rpm", "apk", "jar", "war", "whl", "gem", "pkg", "zpaq",
];
const CODE: &[&str] = &[
    "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "java", "kt", "kts",
    "go", "rb", "php", "swift", "m", "mm", "lua", "pl", "pm", "sh", "bash", "zsh", "fish", "ps1", "html", "htm", "css",
    "scss", "sass", "less", "json", "jsonc", "yaml", "yml", "toml", "xml", "ini", "conf", "cfg", "sql", "vue", "svelte",
    "zig", "nim", "hs", "ml", "ex", "exs", "erl", "clj", "scala", "dart", "r", "jl", "gd", "glsl", "wgsl", "cmake", "mk",
    "gradle", "nix", "lock", "proto", "graphql", "ipynb",
];
const PROGRAM: &[&str] = &["appimage", "run", "exe", "msi", "com", "bat", "cmd", "flatpakref", "out"];
const TEMPORARY: &[&str] = &["tmp", "temp", "bak", "bkp", "swp", "swo", "old", "orig", "part", "crdownload", "partial"];

/// Whole names (no extension) that are code all the same.
const CODE_NAMES: &[&str] = &["makefile", "dockerfile", "containerfile", "justfile", "cmakelists.txt", "pkgbuild", "rakefile", "gemfile"];

/// The kind of a file, from its name; `executable` is whether it can be run (the
/// permission bit). Folders have no kind (they have their own colour).
pub fn category(name: &str, executable: bool) -> Option<Category> {
    let lower = name.to_lowercase();
    if lower.ends_with('~') || lower.ends_with(".duopane-part") || (lower.starts_with(".#") && lower.len() > 2) {
        return Some(Category::Temporary);
    }
    if CODE_NAMES.contains(&lower.as_str()) {
        return Some(Category::Code);
    }
    // ".bashrc" is a hidden name, not an extension.
    let ext = match lower.rfind('.') {
        Some(i) if i > 0 => &lower[i + 1..],
        _ => "",
    };
    for (list, cat) in [
        (PICTURE, Category::Picture),
        (VIDEO, Category::Video),
        (MUSIC, Category::Music),
        (DOCUMENT, Category::Document),
        (ARCHIVE, Category::Archive),
        (CODE, Category::Code),
        (PROGRAM, Category::Program),
        (TEMPORARY, Category::Temporary),
    ] {
        if list.contains(&ext) {
            return Some(cat);
        }
    }
    // Runnable files count as programs only without an extension: on drives from
    // Windows (NTFS, exFAT) every file can look runnable.
    (executable && ext.is_empty()).then_some(Category::Program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(category("IMG_4411.JPG", false), Some(Category::Picture));
        assert_eq!(category("Boat trip.mp4", false), Some(Category::Video));
        assert_eq!(category("01 - Night Train.flac", false), Some(Category::Music));
        assert_eq!(category("Budget 2026.ods", false), Some(Category::Document));
        assert_eq!(category("aurora-0.4.1.tar.gz", false), Some(Category::Archive));
        assert_eq!(category("main.rs", false), Some(Category::Code));
        assert_eq!(category("app.ts", false), Some(Category::Code), "TypeScript, not video");
        assert_eq!(category("clip.m2ts", false), Some(Category::Video));
        assert_eq!(category("Makefile", false), Some(Category::Code));
        assert_eq!(category("Duopane-x86_64.AppImage", false), Some(Category::Program));
        assert_eq!(category("notes.txt~", false), Some(Category::Temporary));
        assert_eq!(category(".report.pdf.duopane-part", false), Some(Category::Temporary));
        assert_eq!(category("duopane", true), Some(Category::Program), "runnable, no extension");
        assert_eq!(category("scene.blend", true), None, "runnable but with an unknown extension (NTFS)");
        assert_eq!(category(".bashrc", false), None, "a hidden name isn't an extension");
        assert_eq!(category("README", false), None);
    }
}
