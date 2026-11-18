//! Lister: working out what a file is and reading it, on a background thread.

use eframe::egui::{Color32, ColorImage};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::settings::ListerPlugin;

/// Runs a lister plugin and turns its output into something to show.
fn run_plugin(p: &ListerPlugin, path: &Path, dark: bool) -> Result<Loaded, String> {
    let quoted = format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    let cmd = p.command.replace("{file}", &quoted);
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .env("DUOPANE_FILE", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Read output on threads so a chatty command can't block; give up after 30 s.
    let mut out = child.stdout.take().ok_or("no output")?;
    let mut err = child.stderr.take().ok_or("no output")?;
    let out_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out.read_to_end(&mut v);
        v
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("took longer than 30 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    if !status.success() || stdout.is_empty() {
        return Err(stderr.lines().next().unwrap_or("the command failed").to_string());
    }
    if p.output.eq_ignore_ascii_case("image") {
        let img = image::load_from_memory(&stdout).map_err(|e| e.to_string())?;
        let orig = [img.width(), img.height()];
        let rgba = img.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        return Ok(Loaded::Image(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), orig));
    }
    let (text, _) = decode_text(&stdout, false);
    let syntax = if p.syntax.is_empty() { "txt" } else { p.syntax.as_str() };
    Ok(Loaded::Text(make_text(&text, syntax, format!("via plugin “{}”", p.name), dark)))
}

pub const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "ico", "tif", "tiff", "webp", "pnm", "pbm", "pgm", "ppm", "tga", "qoi"];
pub const AUDIO_EXT: &[&str] = &["mp3", "flac", "ogg", "oga", "opus", "wav", "m4a", "aac", "wma", "aif", "aiff", "ape", "mka", "wv"];
pub const VIDEO_EXT: &[&str] = &["mp4", "mkv", "webm", "avi", "mov", "m4v", "wmv", "flv", "mpg", "mpeg", "ts", "m2ts", "3gp", "ogv"];
const DOC_EXT: &[&str] = &["docx", "odt", "rtf"];

/// Text bigger than this is cut off; hex shows up to this much.
const READ_LIMIT: u64 = 16 * 1024 * 1024;
/// Code colouring is skipped for files bigger than this.
const HIGHLIGHT_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Preview: picture, PDF, media or text, whichever fits.
    Auto,
    Text,
    Hex,
}

pub struct TextDoc {
    pub lines: Vec<String>,
    /// Coloured pieces per line (when code colouring applied).
    pub spans: Option<Vec<Vec<(Color32, String)>>>,
    pub kind: String,
    pub note: String,
}

pub enum Loaded {
    Text(TextDoc),
    Hex(Vec<u8>, bool),
    Image(ColorImage, [u32; 2]),
    Pdf { pages: usize },
    Media { video: bool },
    Error(String),
}

pub fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn have(tool: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(tool).is_file()))
        .unwrap_or(false)
}

pub fn load(path: &Path, mode: Mode, dark: bool, plugins: &[ListerPlugin]) -> Loaded {
    let e = ext(path);
    if mode == Mode::Auto {
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        for p in plugins {
            if p.extensions.iter().any(|x| name.ends_with(&format!(".{}", x.trim_start_matches('.').to_lowercase()))) {
                match run_plugin(p, path, dark) {
                    Ok(l) => return l,
                    // Fall back to the built-in viewers; say why in the text note if we end up there.
                    Err(err) => eprintln!("lister plugin {}: {err}", p.name),
                }
            }
        }
        if crate::archive::is_archive(path) {
            if let Ok(list) = crate::archive::listing(path) {
                let mut doc = make_text(&list, "txt", "Alt+F9 unpacks · 3 for hex".into(), dark);
                doc.kind = "Archive".into();
                return Loaded::Text(doc);
            }
        }
        if IMAGE_EXT.contains(&e.as_str()) {
            return load_image(path);
        }
        if e == "pdf" {
            if have("pdftoppm") && have("pdfinfo") {
                return match pdf_pages(path) {
                    Some(pages) => Loaded::Pdf { pages },
                    None => Loaded::Error("This PDF couldn't be read.".into()),
                };
            }
            if have("pdftotext") {
                return pdf_text(path, dark);
            }
            return Loaded::Error("PDF preview needs poppler (pdftoppm, pdfinfo). Install it, or press 3 for hex.".into());
        }
        if AUDIO_EXT.contains(&e.as_str()) {
            return Loaded::Media { video: false };
        }
        if VIDEO_EXT.contains(&e.as_str()) {
            return Loaded::Media { video: true };
        }
    }
    if mode != Mode::Hex && DOC_EXT.contains(&e.as_str()) {
        match extract_doc(path, &e) {
            Ok(t) => return Loaded::Text(make_text(&t, "txt", "Text taken from the document".into(), dark)),
            Err(err) if mode == Mode::Text => return Loaded::Error(err),
            Err(_) => {}
        }
    }
    let (bytes, truncated) = match read_limited(path) {
        Ok(x) => x,
        Err(e) => return Loaded::Error(format!("Can't read the file: {e}")),
    };
    if mode == Mode::Hex || (mode == Mode::Auto && looks_binary(&bytes)) {
        return Loaded::Hex(bytes, truncated);
    }
    let (text, enc) = decode_text(&bytes, truncated);
    let mut note = enc.to_string();
    if truncated {
        note.push_str(" · showing the first 16 MB");
    }
    Loaded::Text(make_text(&text, &e, note, dark))
}

fn read_limited(path: &Path) -> std::io::Result<(Vec<u8>, bool)> {
    let f = File::open(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    f.take(READ_LIMIT).read_to_end(&mut buf)?;
    Ok((buf, len > READ_LIMIT))
}

fn looks_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(8192)];
    if head.starts_with(&[0xff, 0xfe]) || head.starts_with(&[0xfe, 0xff]) {
        return false; // UTF-16 text
    }
    head.contains(&0)
}

/// Returns the text and the name of the encoding used.
fn decode_text(bytes: &[u8], truncated: bool) -> (String, &'static str) {
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return (String::from_utf8_lossy(rest).into_owned(), "UTF-8");
    }
    let utf16 = |b: &[u8], le: bool| -> String {
        let units: Vec<u16> = b
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return (utf16(rest, true), "UTF-16");
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return (utf16(rest, false), "UTF-16 BE");
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "UTF-8"),
        // Cut off in the middle of a character by the size limit?
        Err(e) if truncated && e.error_len().is_none() => (String::from_utf8_lossy(bytes).into_owned(), "UTF-8"),
        Err(_) => (bytes.iter().map(|&b| b as char).collect(), "Latin-1"),
    }
}

fn syntaxes() -> &'static syntect::parsing::SyntaxSet {
    static SS: OnceLock<syntect::parsing::SyntaxSet> = OnceLock::new();
    SS.get_or_init(syntect::parsing::SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static syntect::highlighting::ThemeSet {
    static TS: OnceLock<syntect::highlighting::ThemeSet> = OnceLock::new();
    TS.get_or_init(syntect::highlighting::ThemeSet::load_defaults)
}

fn expand_tabs(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).replace('\t', "    ")
}

pub fn make_text(text: &str, ext: &str, note: String, dark: bool) -> TextDoc {
    use syntect::easy::HighlightLines;
    use syntect::util::LinesWithEndings;
    let ss = syntaxes();
    let first = text.lines().next().unwrap_or("");
    let syntax = ss
        .find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_first_line(first))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let plain = syntax.name == "Plain Text";
    let kind = if plain { "Text".to_string() } else { syntax.name.clone() };
    if plain || text.len() > HIGHLIGHT_LIMIT {
        let lines = text.lines().map(expand_tabs).collect();
        let mut note = note;
        if !plain {
            note.push_str(" · too big to colour");
        }
        return TextDoc { lines, spans: None, kind, note };
    }
    let theme = &themes().themes[if dark { "base16-ocean.dark" } else { "InspiredGitHub" }];
    let mut h = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    for line in LinesWithEndings::from(text) {
        let pieces = h.highlight_line(line, ss).unwrap_or_default();
        let row: Vec<(Color32, String)> = pieces
            .into_iter()
            .map(|(st, s)| (Color32::from_rgb(st.foreground.r, st.foreground.g, st.foreground.b), s.replace('\t', "    ")))
            .map(|(c, s)| (c, s.trim_end_matches(['\n', '\r']).to_string()))
            .filter(|(_, s)| !s.is_empty())
            .collect();
        lines.push(expand_tabs(line));
        spans.push(row);
    }
    TextDoc { lines, spans: Some(spans), kind, note }
}

fn load_image(path: &Path) -> Loaded {
    let img = match image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        Ok(r) => match r.decode() {
            Ok(i) => i,
            Err(e) => return Loaded::Error(format!("Can't show this picture: {e}")),
        },
        Err(e) => return Loaded::Error(format!("Can't read the picture: {e}")),
    };
    let orig = [img.width(), img.height()];
    // Graphics cards can't hold enormous textures; scale those down for display.
    let img = if img.width() > 8192 || img.height() > 8192 { img.thumbnail(8192, 8192) } else { img };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Loaded::Image(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), orig)
}

fn pdf_pages(path: &Path) -> Option<usize> {
    let out = Command::new("pdfinfo").arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|l| l.strip_prefix("Pages:").and_then(|n| n.trim().parse().ok()))
}

fn pdf_text(path: &Path, dark: bool) -> Loaded {
    match Command::new("pdftotext").args(["-layout"]).arg(path).arg("-").output() {
        Ok(o) if o.status.success() => {
            let t = String::from_utf8_lossy(&o.stdout).into_owned();
            Loaded::Text(make_text(&t, "txt", "Text taken from the PDF".into(), dark))
        }
        _ => Loaded::Error("This PDF couldn't be read.".into()),
    }
}

/// Renders one PDF page (1-based) to a picture about `width` pixels wide.
pub fn render_pdf_page(path: &Path, page: usize, width: u32) -> Result<ColorImage, String> {
    let n = page.to_string();
    let out = Command::new("pdftoppm")
        .args(["-f", &n, "-l", &n, "-scale-to-x", &width.to_string(), "-scale-to-y", "-1", "-png"])
        .arg(path)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let img = image::load_from_memory(&out.stdout).map_err(|e| e.to_string())?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

// ----- Office documents ------------------------------------------------------

fn extract_doc(path: &Path, ext: &str) -> Result<String, String> {
    match ext {
        "docx" => zip_xml_text(path, "word/document.xml"),
        "odt" => zip_xml_text(path, "content.xml"),
        "rtf" => {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            Ok(rtf_text(&String::from_utf8_lossy(&bytes)))
        }
        _ => Err("Unknown document type".into()),
    }
}

fn zip_xml_text(path: &Path, member: &str) -> Result<String, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let mut xml = String::new();
    z.by_name(member).map_err(|e| e.to_string())?.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    Ok(xml_text(&xml))
}

/// Keeps the text of Word / OpenDocument XML, one paragraph per line.
fn xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        out.push_str(&unescape(&rest[..lt]));
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        let name = tag.trim_start_matches('/').split([' ', '/']).next().unwrap_or("");
        match name {
            "w:p" | "text:p" | "text:h" if tag.starts_with('/') => out.push('\n'),
            "text:p" | "text:h" if tag.ends_with('/') => out.push('\n'),
            "w:br" | "w:cr" | "text:line-break" => out.push('\n'),
            "w:tab" | "text:tab" => out.push('\t'),
            "text:s" => out.push(' '),
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let Some(semi) = rest[amp..].find(';') else {
            out.push_str(&rest[amp..]);
            return out;
        };
        let ent = &rest[amp + 1..amp + semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&rest[amp..amp + semi + 1]),
        }
        rest = &rest[amp + semi + 1..];
    }
    out.push_str(rest);
    out
}

/// A rough RTF-to-text conversion: keeps paragraphs, tabs and accented letters.
fn rtf_text(rtf: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = rtf.chars().collect();
    let mut i = 0;
    let mut depth = 0usize;
    let mut skip_from: Option<usize> = None; // group depth whose contents are skipped
    let mut uc_skip = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => {
                depth += 1;
                i += 1;
            }
            '}' => {
                if skip_from == Some(depth) {
                    skip_from = None;
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    break;
                }
                let n = chars[i];
                if n == '\'' {
                    let hx: String = chars.get(i + 1..i + 3).map(|s| s.iter().collect()).unwrap_or_default();
                    if skip_from.is_none() {
                        if let Ok(b) = u8::from_str_radix(&hx, 16) {
                            out.push(b as char);
                        }
                    }
                    i += 3;
                } else if n.is_ascii_alphabetic() {
                    let start = i;
                    while i < chars.len() && chars[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    let nstart = i;
                    if i < chars.len() && (chars[i] == '-' || chars[i].is_ascii_digit()) {
                        i += 1;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                    let num: Option<i32> = chars[nstart..i].iter().collect::<String>().parse().ok();
                    if i < chars.len() && chars[i] == ' ' {
                        i += 1;
                    }
                    match word.as_str() {
                        "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "header" | "footer" | "object" => {
                            if skip_from.is_none() {
                                skip_from = Some(depth);
                            }
                        }
                        "par" | "line" | "sect" | "page" if skip_from.is_none() => out.push('\n'),
                        "tab" if skip_from.is_none() => out.push('\t'),
                        "uc" => uc_skip = num.unwrap_or(1).max(0) as usize,
                        "u" if skip_from.is_none() => {
                            if let Some(v) = num {
                                let v = if v < 0 { v + 65536 } else { v } as u32;
                                if let Some(ch) = char::from_u32(v) {
                                    out.push(ch);
                                }
                            }
                            // Skip the replacement character(s) that follow.
                            let mut k = uc_skip.max(1);
                            while k > 0 && i < chars.len() && chars[i] != '\\' && chars[i] != '{' && chars[i] != '}' {
                                i += 1;
                                k -= 1;
                            }
                        }
                        _ => {}
                    }
                } else {
                    if n == '*' && skip_from.is_none() {
                        skip_from = Some(depth);
                    } else if skip_from.is_none() && matches!(n, '\\' | '{' | '}') {
                        out.push(n);
                    }
                    i += 1;
                }
            }
            '\r' | '\n' => i += 1,
            _ => {
                if skip_from.is_none() {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_run() {
        let dir = std::env::temp_dir().join(format!("duopane-plug-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("it's.abc");
        std::fs::write(&f, b"raw").unwrap();
        let text = ListerPlugin {
            name: "upper".into(),
            extensions: vec!["abc".into()],
            command: "tr a-z A-Z < {file}".into(),
            output: "text".into(),
            syntax: String::new(),
        };
        match load(&f, Mode::Auto, true, &[text]) {
            Loaded::Text(d) => assert_eq!(d.lines, vec!["RAW".to_string()]),
            _ => panic!("expected text from the plugin"),
        }
        if have("rsvg-convert") {
            let svg = dir.join("p.svg");
            std::fs::write(&svg, br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"#).unwrap();
            let plugins = crate::settings::Settings::default().lister_plugins;
            assert!(matches!(load(&svg, Mode::Auto, true, &plugins), Loaded::Image(..)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn xml_paragraphs() {
        let x = "<w:p><w:r><w:t>Hello &amp; welcome</w:t></w:r></w:p><w:p><w:r><w:t>Line</w:t><w:tab/><w:t>2</w:t></w:r></w:p>";
        assert_eq!(xml_text(x), "Hello & welcome\nLine\t2\n");
    }

    #[test]
    fn rtf_basic() {
        let r = concat!(r"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\f0 Hello\par Caf\'e9 \", "u8364?\\tab x}");
        assert_eq!(rtf_text(r), "Hello\nCafé €\tx");
    }

    #[test]
    fn encodings() {
        assert_eq!(decode_text(b"abc", false).1, "UTF-8");
        assert_eq!(decode_text(&[0x63, 0x61, 0x66, 0xe9], false).0, "café");
        assert!(looks_binary(&[0x7f, 0x45, 0x4c, 0x46, 0, 1]));
        let doc = make_text("fn main() {}\n", "rs", String::new(), true);
        assert_eq!(doc.kind, "Rust");
        assert!(doc.spans.is_some());
    }
}
