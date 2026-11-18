//! Copy, move and delete, run on a background thread so the window stays responsive.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConflictPolicy {
    Overwrite,
    Skip,
}

#[derive(Clone, Debug)]
pub enum JobSpec {
    /// Copy (or move, when `mv`) each `(source, destination)` pair.
    Transfer { mv: bool, items: Vec<(PathBuf, PathBuf)>, policy: ConflictPolicy },
    Delete { paths: Vec<PathBuf>, permanent: bool },
    /// Pack `sources` into one archive; optionally delete the originals afterwards.
    Pack { sources: Vec<PathBuf>, archive: PathBuf, format: crate::archive::Format, level: u32, delete_after: bool },
    /// Unpack archives into `dest` (into a folder named after each archive when `into_folder`).
    Unpack { archives: Vec<PathBuf>, dest: PathBuf, into_folder: bool },
    /// Copy entries out of an archive being browsed (paths inside the archive).
    Extract { archive: PathBuf, items: Vec<String>, base: String, dest: PathBuf },
}

impl JobSpec {
    pub fn label(&self) -> String {
        match self {
            JobSpec::Transfer { mv, items, .. } => {
                format!("{} {}", if *mv { "Moving" } else { "Copying" }, count(items.len()))
            }
            JobSpec::Delete { paths, permanent } => format!(
                "{} {}",
                if *permanent { "Deleting" } else { "Moving to trash" },
                count(paths.len())
            ),
            JobSpec::Pack { sources, archive, .. } => format!(
                "Packing {} into {}",
                count(sources.len()),
                archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ),
            JobSpec::Extract { items, .. } => format!("Extracting {}", count(items.len())),
            JobSpec::Unpack { archives, .. } => {
                format!("Unpacking {}", if archives.len() == 1 { "1 archive".to_string() } else { format!("{} archives", archives.len()) })
            }
        }
    }
}

pub fn count(n: usize) -> String {
    if n == 1 {
        "1 item".into()
    } else {
        format!("{n} items")
    }
}

enum JobMsg {
    Progress { done: u64, total: u64, current: String },
    Done { errors: Vec<String>, cancelled: bool },
}

pub struct JobHandle {
    rx: mpsc::Receiver<JobMsg>,
    pub cancel: Arc<AtomicBool>,
    pub label: String,
    pub done: u64,
    pub total: u64,
    pub current: String,
    /// Set once the job has ended: (errors, cancelled)
    pub finished: Option<(Vec<String>, bool)>,
}

impl JobHandle {
    /// Reads new progress messages. Returns true once the job has finished.
    pub fn poll(&mut self) -> bool {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                JobMsg::Progress { done, total, current } => {
                    self.done = done;
                    self.total = total;
                    self.current = current;
                }
                JobMsg::Done { errors, cancelled } => self.finished = Some((errors, cancelled)),
            }
        }
        self.finished.is_some()
    }

    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32
        }
    }
}

pub fn start(spec: JobSpec) -> JobHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let label = spec.label();
    let c = cancel.clone();
    thread::spawn(move || run(spec, tx, c));
    JobHandle {
        rx,
        cancel,
        label,
        done: 0,
        total: 0,
        current: String::new(),
        finished: None,
    }
}

pub(crate) struct Ctx {
    tx: mpsc::Sender<JobMsg>,
    cancel: Arc<AtomicBool>,
    pub(crate) done: u64,
    pub(crate) total: u64,
    errors: Vec<String>,
    last_report: Instant,
}

impl Ctx {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub(crate) fn report(&mut self, current: &Path, force: bool) {
        if force || self.last_report.elapsed() > Duration::from_millis(60) {
            self.last_report = Instant::now();
            let _ = self.tx.send(JobMsg::Progress {
                done: self.done,
                total: self.total,
                current: current.display().to_string(),
            });
        }
    }

    pub(crate) fn error(&mut self, path: &Path, e: impl std::fmt::Display) {
        self.errors.push(format!("{}: {}", path.display(), e));
    }
}

fn run(spec: JobSpec, tx: mpsc::Sender<JobMsg>, cancel: Arc<AtomicBool>) {
    let mut ctx = Ctx { tx, cancel, done: 0, total: 0, errors: Vec::new(), last_report: Instant::now() };
    match spec {
        JobSpec::Transfer { mv, items, policy } => {
            let sizes: Vec<u64> = items.iter().map(|(s, _)| tree_size(s)).collect();
            ctx.total = sizes.iter().sum();
            for ((src, dst), size) in items.iter().zip(sizes) {
                if ctx.cancelled() {
                    break;
                }
                ctx.report(src, true);
                if src == dst {
                    ctx.done += size;
                    continue;
                }
                if dst.starts_with(src) {
                    ctx.error(src, "can't put a folder inside itself");
                    continue;
                }
                let exists = dst.symlink_metadata().is_ok();
                if exists && policy == ConflictPolicy::Skip {
                    ctx.done += size;
                    continue;
                }
                if mv {
                    let both_dirs = exists && is_real_dir(src) && is_real_dir(dst);
                    if !both_dirs {
                        if exists {
                            if let Err(e) = remove_path(dst) {
                                ctx.error(dst, e);
                                continue;
                            }
                        }
                        if fs::rename(src, dst).is_ok() {
                            ctx.done += size;
                            continue;
                        }
                    }
                    // Different drive, or merging two folders: copy, then delete the original.
                    let before = ctx.errors.len();
                    copy_tree(src, dst, &mut ctx);
                    if ctx.errors.len() == before && !ctx.cancelled() {
                        if let Err(e) = remove_path(src) {
                            ctx.error(src, e);
                        }
                    }
                } else {
                    copy_tree(src, dst, &mut ctx);
                }
            }
        }
        JobSpec::Delete { paths, permanent } => {
            ctx.total = paths.len() as u64;
            for p in &paths {
                if ctx.cancelled() {
                    break;
                }
                ctx.report(p, true);
                let res = if permanent {
                    remove_path(p).map_err(|e| e.to_string())
                } else {
                    trash::delete(p).map_err(|e| e.to_string())
                };
                if let Err(e) = res {
                    ctx.error(p, e);
                }
                ctx.done += 1;
            }
        }
        JobSpec::Pack { sources, archive, format, level, delete_after } => {
            let finished = crate::archive::pack(&sources, &archive, format, level, &mut ctx);
            if !finished {
                let _ = fs::remove_file(&archive); // cancelled: don't leave half an archive
            } else if delete_after && ctx.errors.is_empty() {
                for s in &sources {
                    if let Err(e) = remove_path(s) {
                        ctx.error(s, e);
                    }
                }
            }
        }
        JobSpec::Extract { archive, items, base, dest } => {
            crate::archive::extract(&archive, &items, &base, &dest, &mut ctx);
        }
        JobSpec::Unpack { archives, dest, into_folder } => {
            ctx.total = archives.iter().map(|a| crate::archive::unpack_size(a)).sum();
            for a in &archives {
                if ctx.cancelled() {
                    break;
                }
                let target = if into_folder {
                    let name = a.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    dest.join(crate::archive::strip_archive_ext(&name))
                } else {
                    dest.clone()
                };
                if let Err(e) = fs::create_dir_all(&target) {
                    ctx.error(&target, e);
                    continue;
                }
                crate::archive::unpack(a, &target, &mut ctx);
            }
        }
    }
    let cancelled = ctx.cancelled();
    let _ = ctx.tx.send(JobMsg::Done { errors: ctx.errors, cancelled });
}

fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// Total size in bytes of a file or folder (symlinks are not followed).
pub fn tree_size(p: &Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
            .unwrap_or(0),
        Ok(m) if m.file_type().is_symlink() => 0,
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

pub fn remove_path(p: &Path) -> std::io::Result<()> {
    if is_real_dir(p) {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

fn copy_tree(src: &Path, dst: &Path, ctx: &mut Ctx) {
    if ctx.cancelled() {
        return;
    }
    let meta = match fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(e) => return ctx.error(src, e),
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = match fs::read_link(src) {
            Ok(t) => t,
            Err(e) => return ctx.error(src, e),
        };
        if dst.symlink_metadata().is_ok() {
            if let Err(e) = remove_path(dst) {
                return ctx.error(dst, e);
            }
        }
        #[cfg(unix)]
        let res = std::os::unix::fs::symlink(&target, dst);
        #[cfg(not(unix))]
        let res = fs::copy(src, dst).map(|_| ());
        let _ = target;
        if let Err(e) = res {
            ctx.error(dst, e);
        }
    } else if ft.is_dir() {
        if dst.symlink_metadata().is_ok() && !is_real_dir(dst) {
            if let Err(e) = remove_path(dst) {
                return ctx.error(dst, e);
            }
        }
        if !dst.exists() {
            if let Err(e) = fs::create_dir(dst) {
                return ctx.error(dst, e);
            }
        }
        let children = match fs::read_dir(src) {
            Ok(rd) => rd,
            Err(e) => return ctx.error(src, e),
        };
        for child in children.flatten() {
            copy_tree(&child.path(), &dst.join(child.file_name()), ctx);
            if ctx.cancelled() {
                return;
            }
        }
        let _ = fs::set_permissions(dst, meta.permissions());
    } else {
        copy_file(src, dst, &meta, ctx);
    }
}

fn copy_file(src: &Path, dst: &Path, meta: &fs::Metadata, ctx: &mut Ctx) {
    let result = (|| -> std::io::Result<bool> {
        let mut input = File::open(src)?;
        let mut output = File::create(dst)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if ctx.cancelled() {
                return Ok(false);
            }
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
            ctx.done += n as u64;
            ctx.report(src, false);
        }
        output.set_permissions(meta.permissions())?;
        if let Ok(t) = meta.modified() {
            let _ = output.set_modified(t);
        }
        Ok(true)
    })();
    match result {
        Ok(true) => {}
        Ok(false) => {
            let _ = fs::remove_file(dst); // cancelled: don't leave half a file behind
        }
        Err(e) => ctx.error(src, e),
    }
}

/// "report.pdf" -> "report copy.pdf", "report copy 2.pdf", ...
pub fn unique_copy_name(dst: &Path) -> PathBuf {
    let parent = dst.parent().unwrap_or(Path::new("/"));
    let stem = dst.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = dst.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let (stem, ext) = if is_real_dir(dst) || stem.is_empty() {
        (dst.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), String::new())
    } else {
        (stem, ext)
    };
    for i in 1.. {
        let name = if i == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {i}{ext}") };
        let candidate = parent.join(name);
        if candidate.symlink_metadata().is_err() {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(mut h: JobHandle) -> Vec<String> {
        while !h.poll() {
            thread::sleep(Duration::from_millis(5));
        }
        h.finished.unwrap().0
    }

    #[test]
    fn copy_and_move_folder() {
        let base = std::env::temp_dir().join(format!("duopane-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let src = base.join("a");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/f.txt"), b"hello").unwrap();
        let out = base.join("b");
        fs::create_dir_all(&out).unwrap();

        let errs = wait(start(JobSpec::Transfer {
            mv: false,
            items: vec![(src.clone(), out.join("a"))],
            policy: ConflictPolicy::Overwrite,
        }));
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(fs::read(out.join("a/sub/f.txt")).unwrap(), b"hello");

        let errs = wait(start(JobSpec::Transfer {
            mv: true,
            items: vec![(src.clone(), out.join("moved"))],
            policy: ConflictPolicy::Overwrite,
        }));
        assert!(errs.is_empty(), "{errs:?}");
        assert!(!src.exists());
        assert!(out.join("moved/sub/f.txt").exists());

        let errs = wait(start(JobSpec::Delete { paths: vec![out.join("moved")], permanent: true }));
        assert!(errs.is_empty());
        assert!(!out.join("moved").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_names() {
        let base = std::env::temp_dir().join(format!("duopane-names-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("r.txt"), b"").unwrap();
        assert_eq!(unique_copy_name(&base.join("r.txt")), base.join("r copy.txt"));
        let _ = fs::remove_dir_all(&base);
    }
}
