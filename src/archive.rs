//! Packing files into archives and unpacking them (Alt+F5 / Alt+F9),
//! plus a contents listing for the lister. ZIP, TAR.GZ and TAR are supported.

use crate::fs_ops::{Ctx, FileInfo};
use chrono::{Datelike, Timelike};
use std::cell::Cell;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Zip,
    TarGz,
    Tar,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Zip, Format::TarGz, Format::Tar];

    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::TarGz => "tar.gz",
            Format::Tar => "tar",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "ZIP",
            Format::TarGz => "TAR.GZ",
            Format::Tar => "TAR (no compression)",
        }
    }

    pub fn compresses(self) -> bool {
        self != Format::Tar
    }
}

const SUFFIXES: [&str; 4] = [".tar.gz", ".tgz", ".zip", ".tar"];

/// "photos.tar.gz" -> "photos"
pub fn strip_archive_ext(name: &str) -> &str {
    let lower = name.to_lowercase();
    for s in SUFFIXES {
        if lower.ends_with(s) && name.len() > s.len() {
            return &name[..name.len() - s.len()];
        }
    }
    name
}

pub fn is_archive(path: &Path) -> bool {
    let n = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    SUFFIXES.iter().any(|s| n.ends_with(s))
}

enum Kind {
    Dir,
    File,
    Symlink(PathBuf),
}

struct Item {
    abs: PathBuf,
    rel: String,
    kind: Kind,
    size: u64,
    mode: u32,
    modified: Option<std::time::SystemTime>,
}

/// Lists everything to pack; names are relative to each source's parent folder.
fn collect(sources: &[PathBuf], skip: &Path, ctx: &mut Ctx) -> Vec<Item> {
    fn walk(abs: &Path, base: &Path, skip: &Path, out: &mut Vec<Item>, ctx: &mut Ctx) {
        if abs == skip {
            return;
        }
        let meta = match fs::symlink_metadata(abs) {
            Ok(m) => m,
            Err(e) => return ctx.error(abs, e),
        };
        let rel = abs.strip_prefix(base).unwrap_or(abs).to_string_lossy().replace('\\', "/");
        #[cfg(unix)]
        let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
        #[cfg(not(unix))]
        let mode = 0o644;
        let ft = meta.file_type();
        let kind = if ft.is_symlink() {
            Kind::Symlink(fs::read_link(abs).unwrap_or_default())
        } else if ft.is_dir() {
            Kind::Dir
        } else {
            Kind::File
        };
        let is_dir = matches!(kind, Kind::Dir);
        let size = if matches!(kind, Kind::File) { meta.len() } else { 0 };
        out.push(Item { abs: abs.to_path_buf(), rel, kind, size, mode, modified: meta.modified().ok() });
        if is_dir {
            match fs::read_dir(abs) {
                Ok(rd) => {
                    let mut children: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
                    children.sort();
                    for c in children {
                        walk(&c, base, skip, out, ctx);
                    }
                }
                Err(e) => ctx.error(abs, e),
            }
        }
    }
    let mut out = Vec::new();
    for s in sources {
        let base = s.parent().unwrap_or(Path::new("/"));
        walk(s, base, skip, &mut out, ctx);
    }
    out
}

/// Packs `sources` into `archive`. Returns true when it finished without being cancelled.
pub fn pack(sources: &[PathBuf], archive: &Path, format: Format, level: u32, ctx: &mut Ctx) -> bool {
    let items = collect(sources, archive, ctx);
    ctx.total = items.iter().map(|i| i.size).sum();
    let result = match format {
        Format::Zip => pack_zip(&items, archive, level, ctx),
        Format::TarGz => File::create(archive).and_then(|f| {
            let enc = flate2::write::GzEncoder::new(BufWriter::new(f), flate2::Compression::new(level.min(9)));
            pack_tar(enc, &items, ctx)?.finish()?.flush()
        }),
        Format::Tar => File::create(archive).and_then(|f| pack_tar(BufWriter::new(f), &items, ctx)?.flush()),
    };
    if let Err(e) = result {
        ctx.error(archive, e);
    }
    !ctx.cancelled()
}

fn zip_time(t: Option<std::time::SystemTime>) -> Option<zip::DateTime> {
    let dt: chrono::DateTime<chrono::Local> = t?.into();
    zip::DateTime::from_date_and_time(dt.year().max(1980) as u16, dt.month() as u8, dt.day() as u8, dt.hour() as u8, dt.minute() as u8, dt.second() as u8).ok()
}

fn pack_zip(items: &[Item], archive: &Path, level: u32, ctx: &mut Ctx) -> io::Result<()> {
    let mut zw = zip::ZipWriter::new(BufWriter::new(File::create(archive)?));
    let base = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(level.clamp(1, 9) as i64))
        .large_file(false);
    let mut buf = vec![0u8; 1 << 20];
    for it in items {
        if ctx.cancelled() {
            break;
        }
        ctx.report(&it.abs, false);
        let mut opts = base.unix_permissions(it.mode);
        if let Some(t) = zip_time(it.modified) {
            opts = opts.last_modified_time(t);
        }
        let r: Result<(), zip::result::ZipError> = (|| {
            match &it.kind {
                Kind::Dir => zw.add_directory(format!("{}/", it.rel), opts)?,
                Kind::Symlink(target) => zw.add_symlink(it.rel.clone(), target.to_string_lossy().into_owned(), opts)?,
                Kind::File => {
                    let opts = if it.size >= 0xffff_ffff { opts.large_file(true) } else { opts };
                    zw.start_file(it.rel.clone(), opts)?;
                    let mut f = File::open(&it.abs)?;
                    loop {
                        if ctx.cancelled() {
                            break;
                        }
                        let n = f.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        zw.write_all(&buf[..n])?;
                        ctx.done += n as u64;
                        ctx.report(&it.abs, false);
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = r {
            ctx.error(&it.abs, e);
        }
    }
    zw.finish().map_err(io::Error::other)?.flush()
}

fn pack_tar<W: Write>(w: W, items: &[Item], ctx: &mut Ctx) -> io::Result<W> {
    let mut tb = tar::Builder::new(w);
    tb.follow_symlinks(false);
    for it in items {
        if ctx.cancelled() {
            break;
        }
        ctx.report(&it.abs, false);
        let r = match it.kind {
            Kind::Dir => tb.append_dir(&it.rel, &it.abs),
            _ => tb.append_path_with_name(&it.abs, &it.rel),
        };
        match r {
            Ok(()) => ctx.done += it.size,
            Err(e) => ctx.error(&it.abs, e),
        }
    }
    tb.into_inner()
}

/// Counts bytes read from the archive file, for progress.
struct Counting<R> {
    inner: R,
    count: Rc<Cell<u64>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

fn is_gzip(path: &Path) -> bool {
    let mut magic = [0u8; 2];
    File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == [0x1f, 0x8b]
}

fn is_zip(path: &Path) -> bool {
    let mut magic = [0u8; 2];
    File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"PK"
}

/// Total bytes `unpack` will report progress against.
pub fn unpack_size(archive: &Path) -> u64 {
    if is_zip(archive) {
        if let Ok(mut z) = File::open(archive).map_err(|e| e.to_string()).and_then(|f| zip::ZipArchive::new(f).map_err(|e| e.to_string())) {
            return (0..z.len()).filter_map(|i| z.by_index_raw(i).ok().map(|f| f.size())).sum();
        }
    }
    fs::metadata(archive).map(|m| m.len()).unwrap_or(0)
}

/// Where `archive` unpacks to: `dest` itself, or a folder in it named after the archive.
pub fn unpack_target(archive: &Path, dest: &Path, into_folder: bool) -> PathBuf {
    if into_folder {
        let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        dest.join(strip_archive_ext(&name))
    } else {
        dest.to_path_buf()
    }
}

/// Describes a file inside an archive, for the "already exists" question.
fn entry_info(archive: &Path, name: &str, size: u64, modified: Option<std::time::SystemTime>) -> FileInfo {
    FileInfo { shown: format!("{} › {name}", archive.display()), real: None, size, modified, is_dir: false }
}

fn tar_time(h: &tar::Header) -> Option<std::time::SystemTime> {
    h.mtime().ok().map(|s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s))
}

/// Unpacks `archive` into the folder `dest` (which must exist). Files that already
/// exist are settled by [`Ctx::place`] (asking, skipping, renaming…).
pub fn unpack(archive: &Path, dest: &Path, ctx: &mut Ctx) {
    let r = if is_zip(archive) { unpack_zip(archive, dest, ctx) } else { unpack_tar(archive, dest, ctx) };
    if let Err(e) = r {
        ctx.error(archive, e);
    }
}

fn unpack_zip(archive: &Path, dest: &Path, ctx: &mut Ctx) -> io::Result<()> {
    let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive)?)).map_err(io::Error::other)?;
    let mut buf = vec![0u8; 1 << 20];
    for i in 0..z.len() {
        if ctx.cancelled() {
            break;
        }
        let mut f = match z.by_index(i) {
            Ok(f) => f,
            Err(e) => {
                ctx.error(archive, e);
                continue;
            }
        };
        // enclosed_name refuses names like "../../etc/passwd".
        let Some(rel) = f.enclosed_name() else {
            ctx.error(archive, format!("skipped unsafe name {}", f.name()));
            continue;
        };
        let out = dest.join(&rel);
        ctx.report(&out, false);
        let r: io::Result<()> = (|| {
            no_links_on_the_way(dest, &rel)?;
            if f.is_dir() {
                return fs::create_dir_all(&out);
            }
            let info = entry_info(archive, &rel.to_string_lossy(), f.size(), f.last_modified().and_then(zip_to_system));
            let Some(out) = ctx.place(&info, &out) else {
                ctx.done += f.size();
                return Ok(());
            };
            no_links_on_the_way(dest, out.strip_prefix(dest).unwrap_or(&out))?;
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            #[cfg(unix)]
            if f.is_symlink() {
                let mut target = String::new();
                f.read_to_string(&mut target)?;
                let _ = fs::remove_file(&out);
                return std::os::unix::fs::symlink(target, &out);
            }
            let mut w = File::create(&out)?;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                w.write_all(&buf[..n])?;
                ctx.done += n as u64;
                ctx.report(&out, false);
                if ctx.cancelled() {
                    return Ok(());
                }
            }
            #[cfg(unix)]
            if let Some(mode) = f.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o7777));
            }
            if let Some(t) = f.last_modified().and_then(zip_to_system) {
                let _ = w.set_modified(t);
            }
            Ok(())
        })();
        if let Err(e) = r {
            ctx.error(&out, e);
        }
    }
    Ok(())
}

fn zip_to_system(t: zip::DateTime) -> Option<std::time::SystemTime> {
    let d = chrono::NaiveDate::from_ymd_opt(t.year() as i32, t.month() as u32, t.day() as u32)?;
    let dt = d.and_hms_opt(t.hour() as u32, t.minute() as u32, t.second() as u32)?;
    let local = dt.and_local_timezone(chrono::Local).single()?;
    Some(local.into())
}

fn unpack_tar(archive: &Path, dest: &Path, ctx: &mut Ctx) -> io::Result<()> {
    let count = Rc::new(Cell::new(0u64));
    let base = ctx.done;
    let file = Counting { inner: BufReader::new(File::open(archive)?), count: count.clone() };
    let reader: Box<dyn Read> = if is_gzip(archive) { Box::new(flate2::read::GzDecoder::new(file)) } else { Box::new(file) };
    let mut ar = tar::Archive::new(reader);
    ar.set_preserve_permissions(true);
    ar.set_preserve_mtime(true);
    for entry in ar.entries()? {
        if ctx.cancelled() {
            break;
        }
        let mut e = match entry {
            Ok(e) => e,
            Err(err) => {
                ctx.error(archive, err);
                break;
            }
        };
        let name = e.path().map(|p| p.into_owned()).unwrap_or_default();
        // Something already there? Settle it first (ask, skip, rename…).
        let rel = clean_name(&name.to_string_lossy());
        let is_dir = e.header().entry_type().is_dir();
        let existing = rel.as_ref().map(|r| dest.join(r)).filter(|o| !is_dir && o.symlink_metadata().is_ok());
        if let (Some(rel), Some(out)) = (&rel, existing) {
            let info = entry_info(archive, rel, e.header().size().unwrap_or(0), tar_time(e.header()));
            match ctx.place(&info, &out) {
                None => {
                    ctx.done = base + count.get();
                    continue;
                }
                Some(p) if p != out => {
                    // Renamed: unpack under the new name (same folder, checked like any other).
                    let r = no_links_on_the_way(dest, p.strip_prefix(dest).unwrap_or(&p)).and_then(|_| e.unpack(&p).map(|_| ()));
                    if let Err(err) = r {
                        ctx.error(&p, err);
                    }
                    ctx.done = base + count.get();
                    continue;
                }
                Some(_) => {}
            }
        }
        // unpack_in refuses to write outside `dest`.
        match e.unpack_in(dest) {
            Ok(true) => {}
            Ok(false) => ctx.error(&name, "skipped unsafe name"),
            Err(err) => ctx.error(&dest.join(&name), err),
        }
        ctx.done = base + count.get();
        ctx.report(&dest.join(&name), false);
    }
    Ok(())
}

/// Guards against an archive that first makes a symlink ("link" -> ~/.ssh) and then
/// writes through it ("link/authorized_keys"), landing outside `dest`: refuses when a
/// folder on the way from `dest` to `rel` is a symlink, and removes a symlink sitting
/// at `rel` itself so the new entry replaces it instead of writing to where it points.
fn no_links_on_the_way(dest: &Path, rel: &Path) -> io::Result<()> {
    let mut cur = dest.to_path_buf();
    let mut parts = rel.components().peekable();
    while let Some(part) = parts.next() {
        cur.push(part);
        let Ok(m) = fs::symlink_metadata(&cur) else { return Ok(()) }; // the rest doesn't exist yet
        if m.file_type().is_symlink() {
            if parts.peek().is_some() {
                return Err(io::Error::other("refused to write through a symbolic link"));
            }
            fs::remove_file(&cur)?;
        }
    }
    Ok(())
}

// ----- browsing archives like folders ---------------------------------------

/// "./a//b/" -> "a/b"; refuses names that climb out ("..") or are empty.
pub fn clean_name(n: &str) -> Option<String> {
    let parts: Vec<&str> = n.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() || parts.contains(&"..") {
        return None;
    }
    Some(parts.join("/"))
}

fn parent_of(p: &str) -> &str {
    p.rsplit_once('/').map(|(a, _)| a).unwrap_or("")
}

pub struct IndexEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// Everything inside an archive, so it can be shown like folders.
pub struct Index {
    pub entries: Vec<IndexEntry>,
    children: std::collections::HashMap<String, Vec<usize>>,
}

impl Index {
    pub fn is_dir(&self, p: &str) -> bool {
        p.is_empty() || self.entries.iter().any(|e| e.is_dir && e.path == p)
    }

    pub fn children(&self, dir: &str) -> impl Iterator<Item = &IndexEntry> {
        self.children.get(dir).into_iter().flatten().map(|&i| &self.entries[i])
    }

    /// Total size of an entry (a folder counts everything inside it).
    pub fn size_of(&self, p: &str) -> u64 {
        let prefix = format!("{p}/");
        self.entries.iter().filter(|e| !e.is_dir && (e.path == p || e.path.starts_with(&prefix))).map(|e| e.size).sum()
    }
}

pub fn build_index(archive: &Path) -> Result<Index, String> {
    let mut raw: Vec<IndexEntry> = Vec::new();
    if is_zip(archive) {
        let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
        for i in 0..z.len() {
            let f = z.by_index_raw(i).map_err(|e| e.to_string())?;
            let Some(path) = clean_name(f.name()) else { continue };
            raw.push(IndexEntry { path, is_dir: f.is_dir(), size: f.size(), modified: f.last_modified().and_then(zip_to_system) });
        }
    } else {
        let file = BufReader::new(File::open(archive).map_err(|e| e.to_string())?);
        let reader: Box<dyn Read> = if is_gzip(archive) { Box::new(flate2::read::GzDecoder::new(file)) } else { Box::new(file) };
        let mut ar = tar::Archive::new(reader);
        for e in ar.entries().map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            let h = e.header();
            let name = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let Some(path) = clean_name(&name) else { continue };
            let modified = h.mtime().ok().map(|m| std::time::UNIX_EPOCH + std::time::Duration::from_secs(m));
            raw.push(IndexEntry { path, is_dir: h.entry_type().is_dir(), size: h.size().unwrap_or(0), modified });
        }
    }
    // Archives often leave out folder entries; add the ones implied by file paths.
    let mut seen: std::collections::HashSet<String> = raw.iter().filter(|e| e.is_dir).map(|e| e.path.clone()).collect();
    let mut extra = Vec::new();
    for e in &raw {
        let mut p = parent_of(&e.path);
        while !p.is_empty() && seen.insert(p.to_string()) {
            extra.push(IndexEntry { path: p.to_string(), is_dir: true, size: 0, modified: None });
            p = parent_of(p);
        }
    }
    raw.extend(extra);
    // Later entries with the same name replace earlier ones (as unpacking would).
    let mut last: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, e) in raw.iter().enumerate() {
        last.insert(e.path.clone(), i);
    }
    let entries: Vec<IndexEntry> = raw.into_iter().enumerate().filter(|(i, e)| last.get(&e.path) == Some(i)).map(|(_, e)| e).collect();
    let mut children: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        children.entry(parent_of(&e.path).to_string()).or_default().push(i);
    }
    Ok(Index { entries, children })
}

/// Copies entries out of an archive. `items` are paths inside the archive (folders
/// bring everything under them); `base` is the archive folder being looked at, so
/// "docs/a.txt" seen from "docs" lands as dest/a.txt.
pub fn extract(archive: &Path, items: &[String], base: &str, dest: &Path, ctx: &mut Ctx) {
    let wanted = |n: &str| items.iter().any(|i| n == i || (n.len() > i.len() && n.starts_with(i.as_str()) && n.as_bytes()[i.len()] == b'/'));
    let target = |n: &str| -> PathBuf {
        let rel = if base.is_empty() { n } else { n.strip_prefix(base).map(|r| r.trim_start_matches('/')).unwrap_or(n) };
        dest.join(rel)
    };
    if let Ok(idx) = build_index(archive) {
        ctx.total = idx.entries.iter().filter(|e| !e.is_dir && wanted(&e.path)).map(|e| e.size).sum();
    }
    let r: io::Result<()> = (|| {
        if is_zip(archive) {
            let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive)?)).map_err(io::Error::other)?;
            let mut buf = vec![0u8; 1 << 20];
            for i in 0..z.len() {
                if ctx.cancelled() {
                    break;
                }
                let mut f = z.by_index(i).map_err(io::Error::other)?;
                let Some(name) = clean_name(f.name()) else { continue };
                if !wanted(&name) {
                    continue;
                }
                let out = target(&name);
                ctx.report(&out, false);
                if let Err(err) = no_links_on_the_way(dest, out.strip_prefix(dest).unwrap_or(&out)) {
                    ctx.error(&out, err);
                    continue;
                }
                if f.is_dir() {
                    fs::create_dir_all(&out)?;
                    continue;
                }
                let info = entry_info(archive, &name, f.size(), f.last_modified().and_then(zip_to_system));
                let Some(out) = ctx.place(&info, &out) else {
                    ctx.done += f.size();
                    continue;
                };
                if let Some(p) = out.parent() {
                    fs::create_dir_all(p)?;
                }
                let mut w = File::create(&out)?;
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    w.write_all(&buf[..n])?;
                    ctx.done += n as u64;
                    ctx.report(&out, false);
                }
                #[cfg(unix)]
                if let Some(mode) = f.unix_mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o7777));
                }
                if let Some(t) = f.last_modified().and_then(zip_to_system) {
                    let _ = w.set_modified(t);
                }
            }
        } else {
            let file = BufReader::new(File::open(archive)?);
            let reader: Box<dyn Read> = if is_gzip(archive) { Box::new(flate2::read::GzDecoder::new(file)) } else { Box::new(file) };
            let mut ar = tar::Archive::new(reader);
            for e in ar.entries()? {
                if ctx.cancelled() {
                    break;
                }
                let mut e = e?;
                let name = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let Some(name) = clean_name(&name) else { continue };
                if !wanted(&name) {
                    continue;
                }
                let out = target(&name);
                if let Err(err) = no_links_on_the_way(dest, out.strip_prefix(dest).unwrap_or(&out)) {
                    ctx.error(&out, err);
                    continue;
                }
                if let Some(p) = out.parent() {
                    fs::create_dir_all(p)?;
                }
                let size = e.header().size().unwrap_or(0);
                let out = if e.header().entry_type().is_dir() {
                    out
                } else {
                    match ctx.place(&entry_info(archive, &name, size, tar_time(e.header())), &out) {
                        Some(p) => p,
                        None => {
                            ctx.done += size;
                            continue;
                        }
                    }
                };
                if let Err(err) = e.unpack(&out) {
                    ctx.error(&out, err);
                }
                ctx.done += size;
                ctx.report(&out, false);
            }
        }
        Ok(())
    })();
    if let Err(e) = r {
        ctx.error(archive, e);
    }
}

/// Unpacks one file from an archive to `out` (used to view or open it).
pub fn extract_one(archive: &Path, inner: &str, out: &Path) -> Result<(), String> {
    if let Some(p) = out.parent() {
        fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    if is_zip(archive) {
        let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
        for i in 0..z.len() {
            let mut f = z.by_index(i).map_err(|e| e.to_string())?;
            if clean_name(f.name()).as_deref() == Some(inner) && !f.is_dir() {
                let mut w = File::create(out).map_err(|e| e.to_string())?;
                io::copy(&mut f, &mut w).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
    } else {
        let file = BufReader::new(File::open(archive).map_err(|e| e.to_string())?);
        let reader: Box<dyn Read> = if is_gzip(archive) { Box::new(flate2::read::GzDecoder::new(file)) } else { Box::new(file) };
        let mut ar = tar::Archive::new(reader);
        for e in ar.entries().map_err(|e| e.to_string())? {
            let mut e = e.map_err(|e| e.to_string())?;
            let name = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            if clean_name(&name).as_deref() == Some(inner) {
                e.unpack(out).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
    }
    Err(format!("{inner} isn't in the archive"))
}

/// A text listing of an archive's contents, for the lister.
pub fn listing(archive: &Path) -> Result<String, String> {
    let mut out = String::new();
    let mut files = 0u64;
    let mut bytes = 0u64;
    let line = |out: &mut String, size: Option<u64>, when: String, name: &str| {
        let s = size.map(crate::pane::human_size).unwrap_or_else(|| "<folder>".into());
        out.push_str(&format!("{s:>10}  {when:16}  {name}\n"));
    };
    if is_zip(archive) {
        let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
        for i in 0..z.len() {
            let f = z.by_index_raw(i).map_err(|e| e.to_string())?;
            let when = f
                .last_modified()
                .map(|t| format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year(), t.month(), t.day(), t.hour(), t.minute()))
                .unwrap_or_default();
            if f.is_dir() {
                line(&mut out, None, when, f.name());
            } else {
                files += 1;
                bytes += f.size();
                line(&mut out, Some(f.size()), when, f.name());
            }
        }
    } else {
        let file = BufReader::new(File::open(archive).map_err(|e| e.to_string())?);
        let reader: Box<dyn Read> = if is_gzip(archive) { Box::new(flate2::read::GzDecoder::new(file)) } else { Box::new(file) };
        let mut ar = tar::Archive::new(reader);
        for e in ar.entries().map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            let h = e.header();
            let name = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let when = h
                .mtime()
                .ok()
                .and_then(|m| chrono::DateTime::from_timestamp(m as i64, 0))
                .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            if h.entry_type().is_dir() {
                line(&mut out, None, when, &name);
            } else {
                let size = h.size().unwrap_or(0);
                files += 1;
                bytes += size;
                line(&mut out, Some(size), when, &name);
            }
        }
    }
    Ok(format!("{files} files, {} unpacked\n\n{out}", crate::pane::human_size(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_ops::{start, ConflictPolicy, JobSpec};

    fn wait(mut h: crate::fs_ops::JobHandle) -> Vec<String> {
        while !h.poll() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        h.finished.unwrap().0
    }

    #[test]
    fn round_trip() {
        for format in Format::ALL {
            let base = std::env::temp_dir().join(format!("duopane-arc-{}-{}", std::process::id(), format.ext()));
            let _ = fs::remove_dir_all(&base);
            let src = base.join("Photos");
            fs::create_dir_all(src.join("2024/empty")).unwrap();
            fs::write(src.join("2024/a.txt"), b"hello").unwrap();
            fs::write(base.join("loose.txt"), b"loose").unwrap();
            let arc = base.join(format!("out.{}", format.ext()));
            let errs = wait(start(JobSpec::Pack {
                sources: vec![src.clone(), base.join("loose.txt")],
                archive: arc.clone(),
                format,
                level: 6,
                delete_after: false,
            }));
            assert!(errs.is_empty(), "{format:?} pack: {errs:?}");
            assert!(listing(&arc).unwrap().contains("Photos/2024/a.txt"));
            let dest = base.join("x");
            fs::create_dir_all(&dest).unwrap();
            let errs = wait(start(JobSpec::Unpack { archives: vec![arc.clone()], dest: dest.clone(), into_folder: true, policy: ConflictPolicy::Overwrite }));
            assert!(errs.is_empty(), "{format:?} unpack: {errs:?}");
            let root = dest.join("out");
            assert_eq!(fs::read(root.join("Photos/2024/a.txt")).unwrap(), b"hello");
            assert_eq!(fs::read(root.join("loose.txt")).unwrap(), b"loose");
            assert!(root.join("Photos/2024/empty").is_dir());
            let _ = fs::remove_dir_all(&base);
        }
    }

    #[test]
    fn browse_and_extract() {
        let base = std::env::temp_dir().join(format!("duopane-browse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/docs/deep")).unwrap();
        fs::write(base.join("src/docs/deep/x.txt"), b"x").unwrap();
        fs::write(base.join("src/top.txt"), b"top").unwrap();
        for format in [Format::Zip, Format::TarGz] {
            let arc = base.join(format!("a.{}", format.ext()));
            let mut h = start(JobSpec::Pack { sources: vec![base.join("src")], archive: arc.clone(), format, level: 6, delete_after: false });
            while !h.poll() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let idx = build_index(&arc).unwrap();
            let root: Vec<&str> = idx.children("").map(|e| e.path.as_str()).collect();
            assert_eq!(root, vec!["src"]);
            assert!(idx.is_dir("src/docs/deep"));
            let mut kids: Vec<&str> = idx.children("src").map(|e| e.path.as_str()).collect();
            kids.sort();
            assert_eq!(kids, vec!["src/docs", "src/top.txt"]);
            // Copy "docs" out while looking at "src": it lands as dest/docs/...
            let dest = base.join(format!("out-{}", format.ext()));
            fs::create_dir_all(&dest).unwrap();
            let errs = wait(start(JobSpec::Extract { archive: arc.clone(), items: vec!["src/docs".into()], base: "src".into(), dest: dest.clone(), policy: ConflictPolicy::Overwrite }));
            assert!(errs.is_empty(), "{errs:?}");
            assert_eq!(fs::read(dest.join("docs/deep/x.txt")).unwrap(), b"x");
            let one = base.join("one.txt");
            extract_one(&arc, "src/top.txt", &one).unwrap();
            assert_eq!(fs::read(&one).unwrap(), b"top");
        }
        assert_eq!(clean_name("./a//b/"), Some("a/b".into()));
        assert_eq!(clean_name("../evil"), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unpack_skips_or_replaces_existing() {
        let base = std::env::temp_dir().join(format!("duopane-skip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/sub")).unwrap();
        fs::write(base.join("src/sub/a.txt"), b"from archive").unwrap();
        fs::write(base.join("src/sub/b.txt"), b"from archive").unwrap();
        for format in [Format::Zip, Format::TarGz] {
            let arc = base.join(format!("a.{}", format.ext()));
            wait(start(JobSpec::Pack { sources: vec![base.join("src/sub")], archive: arc.clone(), format, level: 6, delete_after: false }));
            for (policy, expect) in [(ConflictPolicy::Skip, &b"mine"[..]), (ConflictPolicy::Overwrite, &b"from archive"[..])] {
                let dest = base.join(format!("out-{}-{policy:?}", format.ext()));
                fs::create_dir_all(dest.join("sub")).unwrap();
                fs::write(dest.join("sub/a.txt"), b"mine").unwrap();
                let errs = wait(start(JobSpec::Unpack { archives: vec![arc.clone()], dest: dest.clone(), into_folder: false, policy }));
                assert!(errs.is_empty(), "{format:?} {policy:?}: {errs:?}");
                assert_eq!(fs::read(dest.join("sub/a.txt")).unwrap(), expect, "{format:?} {policy:?}");
                assert_eq!(fs::read(dest.join("sub/b.txt")).unwrap(), b"from archive", "{format:?} {policy:?}");

                let dest = base.join(format!("ex-{}-{policy:?}", format.ext()));
                fs::create_dir_all(dest.join("sub")).unwrap();
                fs::write(dest.join("sub/a.txt"), b"mine").unwrap();
                let errs = wait(start(JobSpec::Extract { archive: arc.clone(), items: vec!["sub".into()], base: String::new(), dest: dest.clone(), policy }));
                assert!(errs.is_empty(), "{format:?} {policy:?}: {errs:?}");
                assert_eq!(fs::read(dest.join("sub/a.txt")).unwrap(), expect, "extract {format:?} {policy:?}");
                assert_eq!(fs::read(dest.join("sub/b.txt")).unwrap(), b"from archive", "extract {format:?} {policy:?}");
            }
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unpack_asks_and_can_rename() {
        use crate::fs_ops::Reply;
        let base = std::env::temp_dir().join(format!("duopane-unask-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/sub")).unwrap();
        fs::write(base.join("src/sub/a.txt"), b"from archive").unwrap();
        for format in [Format::Zip, Format::TarGz] {
            let arc = base.join(format!("a.{}", format.ext()));
            wait(start(JobSpec::Pack { sources: vec![base.join("src/sub")], archive: arc.clone(), format, level: 6, delete_after: false }));
            let dest = base.join(format!("out-{}", format.ext()));
            fs::create_dir_all(dest.join("sub")).unwrap();
            fs::write(dest.join("sub/a.txt"), b"mine").unwrap();
            let mut h = start(JobSpec::Unpack { archives: vec![arc.clone()], dest: dest.clone(), into_folder: false, policy: ConflictPolicy::Ask });
            let mut asked = 0;
            while !h.poll() {
                if let Some(q) = h.conflict.take() {
                    asked += 1;
                    assert!(q.new.shown.contains("a.txt") && q.new.real.is_none(), "{}", q.new.shown);
                    assert_eq!(q.new.size, 12);
                    q.answer(Reply::RenameTo(dest.join("sub/a (from archive).txt")));
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert!(h.finished.unwrap().0.is_empty());
            assert_eq!(asked, 1, "{format:?}");
            assert_eq!(fs::read(dest.join("sub/a.txt")).unwrap(), b"mine");
            assert_eq!(fs::read(dest.join("sub/a (from archive).txt")).unwrap(), b"from archive", "{format:?}");
        }
        let _ = fs::remove_dir_all(&base);
    }

    /// An archive with "link" -> `outside`, then "link/evil.txt" and "trap" -> `outside`/victim.txt,
    /// then a plain file "trap": none of it may touch `outside`.
    fn hostile(base: &Path, outside: &Path, format: Format) -> PathBuf {
        let victim = outside.join("victim.txt");
        let arc = base.join(format!("hostile.{}", format.ext()));
        match format {
            Format::Zip => {
                let mut zw = zip::ZipWriter::new(File::create(&arc).unwrap());
                let o = zip::write::SimpleFileOptions::default();
                zw.add_symlink("link", outside.to_string_lossy(), o).unwrap();
                zw.start_file("link/evil.txt", o).unwrap();
                zw.write_all(b"pwned").unwrap();
                zw.add_symlink("trap", victim.to_string_lossy(), o).unwrap();
                zw.start_file("./trap", o).unwrap();
                zw.write_all(b"pwned").unwrap();
                zw.finish().unwrap();
            }
            _ => {
                let mut tb = tar::Builder::new(File::create(&arc).unwrap());
                let link = |tb: &mut tar::Builder<File>, name: &str, to: &Path| {
                    let mut h = tar::Header::new_gnu();
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    h.set_mode(0o777);
                    tb.append_link(&mut h, name, to).unwrap();
                };
                let file = |tb: &mut tar::Builder<File>, name: &str| {
                    let mut h = tar::Header::new_gnu();
                    h.set_size(5);
                    h.set_mode(0o644);
                    tb.append_data(&mut h, name, &b"pwned"[..]).unwrap();
                };
                link(&mut tb, "link", outside);
                file(&mut tb, "link/evil.txt");
                link(&mut tb, "trap", &victim);
                file(&mut tb, "trap");
                tb.finish().unwrap();
            }
        }
        arc
    }

    #[cfg(unix)]
    #[test]
    fn hostile_archives_stay_inside() {
        let base = std::env::temp_dir().join(format!("duopane-hostile-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let outside = base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("victim.txt"), b"precious").unwrap();
        for format in [Format::Zip, Format::Tar] {
            let arc = hostile(&base, &outside, format);
            let unpack_to = base.join(format!("unpack-{}", format.ext()));
            let extract_to = base.join(format!("extract-{}", format.ext()));
            fs::create_dir_all(&unpack_to).unwrap();
            fs::create_dir_all(&extract_to).unwrap();
            wait(start(JobSpec::Unpack { archives: vec![arc.clone()], dest: unpack_to.clone(), into_folder: false, policy: ConflictPolicy::Overwrite }));
            wait(start(JobSpec::Extract {
                archive: arc.clone(),
                items: vec!["link".into(), "trap".into()],
                base: String::new(),
                dest: extract_to.clone(),
                policy: ConflictPolicy::Overwrite,
            }));
            assert!(!outside.join("evil.txt").exists(), "{format:?} wrote through a link");
            assert_eq!(fs::read(outside.join("victim.txt")).unwrap(), b"precious", "{format:?} overwrote the link target");
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn names() {
        assert_eq!(strip_archive_ext("a.TAR.GZ"), "a");
        assert_eq!(strip_archive_ext("b.zip"), "b");
        assert_eq!(strip_archive_ext("c.txt"), "c.txt");
        assert!(is_archive(Path::new("x.tgz")));
    }
}
