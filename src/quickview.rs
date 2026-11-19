//! Quick View (Ctrl+Q): the other pane shows what the cursor is on: its
//! properties, and whatever else there is to know (a photo's camera settings, a
//! song's tags, a PDF's page count, what's in an archive, the first lines of a
//! text file, a folder's total size).
//!
//! Everything is read on a background thread, after the cursor has rested a
//! moment, so scrolling through a folder never stalls.

use crate::pane::{human_size, Entry};
use crate::theme;
use eframe::egui::{self, vec2, Color32, ColorImage, Stroke};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

/// Wait this long after the cursor stops before reading a file.
const SETTLE: Duration = Duration::from_millis(120);

/// What the cursor is on.
#[derive(Clone, PartialEq)]
pub enum Target {
    /// A real file or folder.
    Disk { path: PathBuf, up: bool },
    /// An entry inside an archive being browsed.
    InArchive { archive: PathBuf, entry: Entry },
}

/// What's known about it, ready to show.
#[derive(Default)]
pub struct Info {
    pub name: String,
    pub sections: Vec<(String, Vec<(String, String)>)>,
    pub picture: Option<ColorImage>,
    pub text: Option<String>,
    pub is_dir: bool,
}

/// A folder's size, counted on another thread.
struct FolderCount {
    path: PathBuf,
    files: Arc<AtomicU64>,
    dirs: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

impl Drop for FolderCount {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct QuickView {
    /// The pane it's shown in (the other one is followed).
    pub slot: usize,
    target: Option<Target>,
    changed_at: Instant,
    started: bool,
    info: Option<Info>,
    rx: Option<mpsc::Receiver<Info>>,
    picture: Option<egui::TextureHandle>,
    count: Option<FolderCount>,
    /// Folder totals already counted: (files, folders, bytes).
    sizes: HashMap<PathBuf, (u64, u64, u64)>,
}

impl QuickView {
    pub fn new(slot: usize) -> QuickView {
        QuickView {
            slot,
            target: None,
            changed_at: Instant::now(),
            started: false,
            info: None,
            rx: None,
            picture: None,
            count: None,
            sizes: HashMap::new(),
        }
    }

    /// True while something is still being read or counted (keep repainting).
    pub fn busy(&self) -> bool {
        !self.started || self.rx.is_some() || self.count.as_ref().is_some_and(|c| !c.done.load(Ordering::Relaxed))
    }

    /// Follows the cursor: call every frame with what it's on.
    pub fn follow(&mut self, target: Option<Target>, ctx: &egui::Context) {
        if target != self.target {
            self.target = target;
            self.changed_at = Instant::now();
            self.started = false;
            self.rx = None;
            self.count = None;
        }
        if !self.started && self.changed_at.elapsed() >= SETTLE {
            self.started = true;
            self.start();
        }
        if let Some(rx) = &self.rx {
            if let Ok(info) = rx.try_recv() {
                self.picture = info.picture.clone().map(|img| ctx.load_texture("quick_view", img, egui::TextureOptions::LINEAR));
                self.info = Some(info);
                self.rx = None;
            }
        }
        if let Some(c) = &self.count {
            if c.done.load(Ordering::Relaxed) && !c.cancel.load(Ordering::Relaxed) {
                let total = (c.files.load(Ordering::Relaxed), c.dirs.load(Ordering::Relaxed), c.bytes.load(Ordering::Relaxed));
                self.sizes.insert(c.path.clone(), total);
            }
        }
    }

    fn start(&mut self) {
        let Some(t) = self.target.clone() else {
            self.info = None;
            self.picture = None;
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(gather(&t));
        });
        // A folder's total size: counted unless already known (not for ".." rows).
        if let Some(Target::Disk { path, up: false }) = &self.target {
            if crate::fs_ops::FileInfo::of(path).is_dir && !self.sizes.contains_key(path) {
                self.count = Some(count_folder(path.clone()));
            }
        }
    }

    /// The folder size line: counted, counting, or nothing (not a folder).
    fn size_line(&self) -> Option<String> {
        let Some(Target::Disk { path, up: false }) = &self.target else { return None };
        let fmt = |(f, d, b): (u64, u64, u64)| {
            format!("{} ({})  ·  {} files, {} folders", human_size(b), bytes(b), group(f), group(d))
        };
        if let Some(t) = self.sizes.get(path) {
            return Some(fmt(*t));
        }
        let c = self.count.as_ref().filter(|c| &c.path == path)?;
        let now = (c.files.load(Ordering::Relaxed), c.dirs.load(Ordering::Relaxed), c.bytes.load(Ordering::Relaxed));
        Some(format!("Counting…  {}", fmt(now)))
    }

    /// Draws the panel in place of a pane.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let th = theme::c();
        let frame = egui::Frame::default().fill(th.pane).stroke(Stroke::new(1.0, th.border)).corner_radius(4.0);
        frame.show(ui, |ui| {
            let full = ui.available_size();
            ui.set_min_size(full);
            let (bar, _) = ui.allocate_exact_size(vec2(full.x, 30.0), egui::Sense::hover());
            ui.painter().rect_filled(bar, 4.0, th.header);
            let title = match &self.info {
                Some(i) if self.started && self.rx.is_none() => format!("Quick view  ·  {}", i.name),
                _ => "Quick view".into(),
            };
            ui.painter().text(bar.left_center() + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, title, egui::TextStyle::Body.resolve(ui.style()), th.text);
            ui.painter().text(bar.right_center() - vec2(10.0, 0.0), egui::Align2::RIGHT_CENTER, "Ctrl+Q closes", egui::TextStyle::Small.resolve(ui.style()), th.dim);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.set_width(full.x - 28.0);
                        self.body(ui);
                    });
                });
            });
        });
    }

    fn body(&self, ui: &mut egui::Ui) {
        let th = theme::c();
        if self.target.is_none() {
            ui.label(egui::RichText::new("Nothing under the cursor").color(th.dim));
            return;
        }
        // Still on the previous item's details while the new ones load: dim them.
        let stale = self.rx.is_some() || !self.started;
        let Some(info) = &self.info else {
            ui.spinner();
            return;
        };
        if let Some(tex) = &self.picture {
            let size = tex.size_vec2();
            let max = vec2(ui.available_width(), (ui.ctx().screen_rect().height() * 0.4).max(160.0));
            let scale = (max.x / size.x).min(max.y / size.y).min(1.0);
            ui.add(egui::Image::new((tex.id(), size * scale)).tint(if stale { Color32::from_gray(140) } else { Color32::WHITE }));
            ui.add_space(10.0);
        }
        let value_color = if stale { th.dim } else { th.text };
        for (i, (heading, rows)) in info.sections.iter().enumerate() {
            if i > 0 {
                ui.add_space(12.0);
            }
            ui.label(egui::RichText::new(heading).color(th.accent).strong());
            ui.add_space(4.0);
            egui::Grid::new(("qv", i)).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                for (k, v) in rows {
                    ui.label(egui::RichText::new(k).color(th.dim));
                    ui.add(egui::Label::new(egui::RichText::new(v).color(value_color)).wrap());
                    ui.end_row();
                }
                if i == 0 && info.is_dir {
                    if let Some(line) = self.size_line() {
                        ui.label(egui::RichText::new("Total size").color(th.dim));
                        ui.label(egui::RichText::new(line).color(value_color));
                        ui.end_row();
                    }
                }
            });
        }
        if let Some(text) = &info.text {
            ui.add_space(12.0);
            ui.label(egui::RichText::new("Beginning").color(th.accent).strong());
            ui.add_space(4.0);
            egui::Frame::default().fill(th.bg).inner_margin(8.0).corner_radius(3.0).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(text).monospace().color(value_color)).wrap());
            });
        }
    }
}

/// Counts files, folders and bytes under `path`, on another thread.
fn count_folder(path: PathBuf) -> FolderCount {
    let c = FolderCount {
        path: path.clone(),
        files: Arc::default(),
        dirs: Arc::default(),
        bytes: Arc::default(),
        done: Arc::default(),
        cancel: Arc::default(),
    };
    let (files, dirs, bytes, done, cancel) = (c.files.clone(), c.dirs.clone(), c.bytes.clone(), c.done.clone(), c.cancel.clone());
    std::thread::spawn(move || {
        let walker = ignore::WalkBuilder::new(&path)
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .follow_links(false)
            .build_parallel();
        walker.run(|| {
            Box::new(|entry| {
                if cancel.load(Ordering::Relaxed) {
                    return ignore::WalkState::Quit;
                }
                let Ok(e) = entry else { return ignore::WalkState::Continue };
                if e.depth() == 0 {
                    return ignore::WalkState::Continue;
                }
                match e.metadata() {
                    Ok(m) if m.is_dir() => {
                        dirs.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(m) => {
                        files.fetch_add(1, Ordering::Relaxed);
                        bytes.fetch_add(m.len(), Ordering::Relaxed);
                    }
                    Err(_) => {}
                }
                ignore::WalkState::Continue
            })
        });
        done.store(true, Ordering::Relaxed);
    });
    c
}

// ----- gathering details --------------------------------------------------------

fn gather(t: &Target) -> Info {
    match t {
        Target::InArchive { archive, entry } => {
            let mut rows = vec![("Kind".to_string(), kind(Path::new(&entry.name), entry.is_dir))];
            if !entry.is_dir {
                rows.push(("Size".into(), format!("{} ({})", human_size(entry.size), bytes(entry.size))));
            }
            if let Some(m) = entry.modified {
                rows.push(("Modified".into(), when(m)));
            }
            rows.push(("Inside".into(), crate::app::tilde(archive)));
            Info { name: entry.name.clone(), sections: vec![("General".into(), rows)], is_dir: entry.is_dir, ..Default::default() }
        }
        Target::Disk { path, .. } => gather_disk(path),
    }
}

fn gather_disk(path: &Path) -> Info {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let Ok(lmeta) = std::fs::symlink_metadata(path) else {
        return Info { name, sections: vec![("General".into(), vec![("".into(), "Can't be read (gone, or no permission)".into())])], ..Default::default() };
    };
    let link = lmeta.file_type().is_symlink().then(|| std::fs::read_link(path).ok()).flatten();
    let meta = if link.is_some() { std::fs::metadata(path).unwrap_or(lmeta) } else { lmeta };
    let is_dir = meta.is_dir();
    let mut info = Info { name, is_dir, ..Default::default() };

    let mut general = vec![("Kind".to_string(), kind(path, is_dir))];
    general.push(("Where".into(), path.parent().map(crate::app::tilde).unwrap_or_default()));
    if let Some(l) = &link {
        general.push(("Link to".into(), l.display().to_string()));
    }
    if is_dir {
        if let Ok(rd) = std::fs::read_dir(path) {
            let (mut files, mut dirs) = (0u64, 0u64);
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    dirs += 1;
                } else {
                    files += 1;
                }
            }
            general.push(("Contains".into(), format!("{} files, {} folders", group(files), group(dirs))));
        }
    } else {
        general.push(("Size".into(), format!("{} ({})", human_size(meta.len()), bytes(meta.len()))));
    }
    if let Ok(t) = meta.modified() {
        general.push(("Modified".into(), when(t)));
    }
    if let Ok(t) = meta.created() {
        general.push(("Created".into(), when(t)));
    }
    if let Ok(t) = meta.accessed() {
        general.push(("Accessed".into(), when(t)));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        general.push(("Permissions".into(), permissions(meta.mode(), is_dir)));
        general.push(("Owner".into(), format!("{}  ·  group {}", user_name(meta.uid()), group_name(meta.gid()))));
    }
    info.sections.push(("General".into(), general));
    if is_dir {
        return info;
    }

    let ext = crate::lister_load::ext(path);
    if is_picture(&ext) {
        picture_details(path, &mut info);
    } else if is_media(&ext) {
        media_details(path, &mut info);
    } else if ext == "pdf" {
        pdf_details(path, &mut info);
    } else if crate::archive::is_archive(path) {
        archive_details(path, &mut info);
    } else {
        info.text = text_start(path);
    }
    info
}

fn is_picture(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "gif" | "bmp" | "ico" | "tif" | "tiff" | "webp" | "pnm" | "pbm" | "pgm" | "ppm" | "tga" | "qoi")
}

fn is_media(ext: &str) -> bool {
    matches!(
        ext,
        "mp3" | "flac" | "ogg" | "opus" | "wav" | "m4a" | "aac" | "wma" | "aiff" | "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "wmv" | "mpg" | "mpeg" | "ts"
    )
}

/// A picture small enough to show (turned the right way up).
fn thumbnail(img: image::DynamicImage) -> ColorImage {
    let img = img.thumbnail(900, 900).to_rgba8();
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

fn picture_details(path: &Path, info: &mut Info) {
    use image::ImageDecoder;
    let mut rows = Vec::new();
    if let Ok((w, h)) = image::image_dimensions(path) {
        rows.push(("Dimensions".into(), format!("{w} × {h} pixels ({:.1} megapixels)", w as f64 * h as f64 / 1e6)));
    }
    let decoded = image::ImageReader::open(path).ok().and_then(|r| r.with_guessed_format().ok()).and_then(|r| r.into_decoder().ok()).and_then(|mut d| {
        let o = d.orientation().ok();
        let mut img = image::DynamicImage::from_decoder(d).ok()?;
        if let Some(o) = o {
            img.apply_orientation(o);
        }
        Some(img)
    });
    info.picture = decoded.map(thumbnail);
    if !rows.is_empty() {
        info.sections.push(("Picture".into(), rows));
    }
    let exif = exif_rows(path);
    if !exif.is_empty() {
        info.sections.push(("Camera".into(), exif));
    }
}

/// The interesting EXIF fields of a photo, as (label, value).
fn exif_rows(path: &Path) -> Vec<(String, String)> {
    use exif::{In, Tag};
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(ex) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)) else { return Vec::new() };
    let get = |tag| ex.get_field(tag, In::PRIMARY).map(|f| f.display_value().with_unit(&ex).to_string().trim_matches('"').trim().to_string());
    let mut rows = Vec::new();
    let camera = [get(Tag::Make), get(Tag::Model)].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    if !camera.is_empty() {
        rows.push(("Camera".into(), camera));
    }
    for (tag, label) in [
        (Tag::LensModel, "Lens"),
        (Tag::DateTimeOriginal, "Taken"),
        (Tag::ExposureTime, "Exposure"),
        (Tag::FNumber, "Aperture"),
        (Tag::PhotographicSensitivity, "ISO"),
        (Tag::FocalLength, "Focal length"),
        (Tag::FocalLengthIn35mmFilm, "35 mm equivalent"),
        (Tag::Flash, "Flash"),
        (Tag::Software, "Software"),
    ] {
        if let Some(v) = get(tag).filter(|v| !v.is_empty()) {
            rows.push((label.into(), v));
        }
    }
    let coord = |tag, reference| -> Option<f64> {
        let f = ex.get_field(tag, In::PRIMARY)?;
        let exif::Value::Rational(v) = &f.value else { return None };
        let deg = v.first()?.to_f64() + v.get(1).map_or(0.0, |m| m.to_f64() / 60.0) + v.get(2).map_or(0.0, |s| s.to_f64() / 3600.0);
        let r = get(reference).unwrap_or_default();
        Some(if r.starts_with('S') || r.starts_with('W') { -deg } else { deg })
    };
    if let (Some(lat), Some(lon)) = (coord(Tag::GPSLatitude, Tag::GPSLatitudeRef), coord(Tag::GPSLongitude, Tag::GPSLongitudeRef)) {
        rows.push(("Location".into(), format!("{lat:.5}, {lon:.5}")));
    }
    rows
}

fn media_details(path: &Path, info: &mut Info) {
    let Some(m) = crate::media::probe(path) else {
        info.sections.push(("Media".into(), vec![("".into(), "Install ffmpeg (ffprobe) to see length and tags".into())]));
        return;
    };
    let mut rows = Vec::new();
    if let Some(d) = m.duration {
        rows.push(("Length".into(), crate::media::fmt_time(d)));
    }
    if m.has_video {
        rows.push(("Picture".into(), format!("{} × {}", m.width, m.height)));
    }
    if !m.summary.is_empty() {
        rows.push(("Format".into(), m.summary.clone()));
    }
    info.sections.push((if m.has_video { "Video" } else { "Audio" }.into(), rows));
    if !m.tags.is_empty() {
        info.sections.push(("Tags".into(), m.tags.clone()));
    }
    // A cover for music; a frame a little way in for video.
    let picture = if m.has_video {
        let at = m.duration.map_or(1.0, |d| (d * 0.1).min(30.0));
        std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-ss", &format!("{at:.2}"), "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-vf", "scale=900:-2", "-f", "image2pipe", "-vcodec", "png", "-"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    } else if m.has_cover {
        crate::media::cover_art(path)
    } else {
        None
    };
    info.picture = picture.and_then(|b| image::load_from_memory(&b).ok()).map(thumbnail);
}

fn pdf_details(path: &Path, info: &mut Info) {
    let Ok(out) = std::process::Command::new("pdfinfo").arg(path).output() else {
        info.sections.push(("PDF".into(), vec![("".into(), "Install poppler (pdfinfo) to see pages and title".into())]));
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !v.is_empty() && ["Title", "Author", "Subject", "Creator", "Producer", "Pages", "Page size", "PDF version", "Encrypted"].contains(&k.as_str()))
        .collect();
    if !rows.is_empty() {
        info.sections.push(("PDF".into(), rows));
    }
}

fn archive_details(path: &Path, info: &mut Info) {
    let rows = match crate::archive::build_index(path) {
        Ok(idx) => {
            let files = idx.entries.iter().filter(|e| !e.is_dir).count() as u64;
            let dirs = idx.entries.iter().filter(|e| e.is_dir).count() as u64;
            let size: u64 = idx.entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            vec![
                ("Contains".into(), format!("{} files, {} folders", group(files), group(dirs))),
                ("Unpacked".into(), format!("{} ({})", human_size(size), bytes(size))),
                ("".into(), "Enter opens it like a folder".into()),
            ]
        }
        Err(e) => vec![("".into(), e)],
    };
    info.sections.push(("Archive".into(), rows));
}

/// The first lines of a text file (None for anything that isn't text).
fn text_start(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0u8; 16 * 1024];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    let buf = &buf[..n];
    if n == 0 || buf.contains(&0) {
        return None;
    }
    let valid = match std::str::from_utf8(buf) {
        Ok(s) => s,
        // Cut at the last whole character (a read can end mid-character).
        Err(e) if e.valid_up_to() > n.saturating_sub(4) => std::str::from_utf8(&buf[..e.valid_up_to()]).ok()?,
        Err(_) => return None,
    };
    let lines: Vec<&str> = valid.lines().take(60).collect();
    Some(lines.join("\n"))
}

// ----- small helpers --------------------------------------------------------------

/// What kind of thing it is, from its name ("JPEG picture", "Folder"…).
fn kind(path: &Path, is_dir: bool) -> String {
    if is_dir {
        return "Folder".into();
    }
    let ext = crate::lister_load::ext(path);
    let k = match ext.as_str() {
        "jpg" | "jpeg" => "JPEG picture",
        "png" => "PNG picture",
        "gif" => "GIF picture",
        "webp" => "WebP picture",
        "svg" => "SVG drawing",
        "tif" | "tiff" => "TIFF picture",
        "bmp" => "BMP picture",
        "heic" | "heif" => "HEIF picture",
        "mp3" => "MP3 audio",
        "flac" => "FLAC audio",
        "ogg" | "opus" => "Ogg audio",
        "wav" => "WAV audio",
        "m4a" | "aac" => "AAC audio",
        "mp4" | "m4v" => "MP4 video",
        "mkv" => "Matroska video",
        "webm" => "WebM video",
        "avi" => "AVI video",
        "mov" => "QuickTime video",
        "pdf" => "PDF document",
        "md" => "Markdown text",
        "txt" => "Plain text",
        "odt" | "docx" | "doc" => "Text document",
        "ods" | "xlsx" | "xls" => "Spreadsheet",
        "odp" | "pptx" | "ppt" => "Presentation",
        "zip" => "ZIP archive",
        "gz" | "tgz" => "Gzip archive",
        "tar" => "Tar archive",
        "iso" => "Disc image",
        "rs" => "Rust source",
        "py" => "Python script",
        "sh" => "Shell script",
        "html" | "htm" => "Web page",
        "css" => "Style sheet",
        "js" => "JavaScript",
        "json" => "JSON data",
        "toml" | "yaml" | "yml" | "ini" | "conf" => "Settings file",
        "ttf" | "otf" | "woff" | "woff2" => "Font",
        "" => "File",
        _ => return format!("{} file", ext.to_uppercase()),
    };
    k.into()
}

fn when(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 77232 -> "77 232 bytes"
fn bytes(n: u64) -> String {
    format!("{} bytes", group(n))
}

fn group(n: u64) -> String {
    crate::dialogs::group_digits(n)
}

/// 0o100644 -> "rw-r--r-- (644)"
#[cfg(unix)]
fn permissions(mode: u32, is_dir: bool) -> String {
    let mut s = String::new();
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    let what = if is_dir { "" } else if mode & 0o111 != 0 { "  ·  runnable" } else { "" };
    format!("{s} ({:o}){what}", mode & 0o7777)
}

/// Names for user and group numbers, from /etc/passwd and /etc/group.
#[cfg(unix)]
fn names(file: &str) -> &'static HashMap<u32, String> {
    use std::sync::OnceLock;
    static USERS: OnceLock<HashMap<u32, String>> = OnceLock::new();
    static GROUPS: OnceLock<HashMap<u32, String>> = OnceLock::new();
    let cell = if file == "/etc/passwd" { &USERS } else { &GROUPS };
    cell.get_or_init(|| {
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.split(':');
                let name = f.next()?.to_string();
                let id = f.nth(1)?.parse().ok()?;
                Some((id, name))
            })
            .collect()
    })
}

#[cfg(unix)]
fn user_name(uid: u32) -> String {
    names("/etc/passwd").get(&uid).cloned().unwrap_or_else(|| uid.to_string())
}

#[cfg(unix)]
fn group_name(gid: u32) -> String {
    names("/etc/group").get(&gid).cloned().unwrap_or_else(|| gid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_of_files_and_folders() {
        let dir = std::env::temp_dir().join(format!("duopane-qv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), "first line\nsecond line\n").unwrap();
        std::fs::write(dir.join("sub/deeper/b.bin"), vec![0u8; 5000]).unwrap();
        image::RgbImage::from_pixel(40, 30, image::Rgb([200, 100, 50])).save(dir.join("pic.png")).unwrap();

        let rows = |info: &Info| info.sections.iter().flat_map(|(_, r)| r.clone()).collect::<HashMap<_, _>>();
        let text = gather_disk(&dir.join("sub/a.txt"));
        assert_eq!(rows(&text)["Kind"], "Plain text");
        assert!(rows(&text)["Size"].contains("23 bytes"));
        assert!(rows(&text).contains_key("Permissions") || cfg!(not(unix)));
        assert_eq!(text.text.as_deref(), Some("first line\nsecond line"));
        assert!(gather_disk(&dir.join("sub/deeper/b.bin")).text.is_none(), "binary files have no text preview");

        let pic = gather_disk(&dir.join("pic.png"));
        assert!(rows(&pic)["Dimensions"].starts_with("40 × 30"));
        assert!(pic.picture.is_some());

        let folder = gather_disk(&dir.join("sub"));
        assert!(folder.is_dir);
        assert_eq!(rows(&folder)["Contains"], "1 files, 1 folders");

        let c = count_folder(dir.join("sub"));
        let t = Instant::now();
        while !c.done.load(Ordering::Relaxed) {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!((c.files.load(Ordering::Relaxed), c.dirs.load(Ordering::Relaxed), c.bytes.load(Ordering::Relaxed)), (2, 1, 5023));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn permission_text() {
        assert_eq!(permissions(0o100644, false), "rw-r--r-- (644)");
        assert_eq!(permissions(0o100755, false), "rwxr-xr-x (755)  ·  runnable");
        assert_eq!(permissions(0o040755, true), "rwxr-xr-x (755)");
    }
}
