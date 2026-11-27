//! Copy, move and delete, run on a background thread so the window stays responsive.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

/// What to do when something already exists where a file is going.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConflictPolicy {
    /// Ask each time (the conflict dialog).
    Ask,
    Overwrite,
    Skip,
    /// Overwrite when the existing file is older than the new one, otherwise skip.
    OverwriteOlder,
    /// Overwrite when the existing file is smaller than the new one, otherwise skip.
    OverwriteSmaller,
    /// Overwrite when the existing file is larger than the new one, otherwise skip.
    OverwriteLarger,
    /// Give the new file a free name ("report copy.pdf").
    RenameNew,
    /// Give the existing file a free name, then put the new one in its place.
    RenameExisting,
}

/// One side of a conflict: the file that's there, or the one about to replace it.
#[derive(Clone, Debug)]
pub struct FileInfo {
    /// Where it is, as shown to the user ("/a/b.txt", or "x.zip › docs/b.txt").
    pub shown: String,
    /// The real file on disk, when there is one (not for files inside archives).
    pub real: Option<PathBuf>,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    pub is_dir: bool,
}

impl FileInfo {
    /// Describes a file or folder on disk (links not followed).
    pub fn of(path: &Path) -> FileInfo {
        let m = fs::symlink_metadata(path).ok();
        FileInfo {
            shown: path.display().to_string(),
            real: Some(path.to_path_buf()),
            size: m.as_ref().filter(|m| m.is_file()).map_or(0, |m| m.len()),
            modified: m.as_ref().and_then(|m| m.modified().ok()),
            is_dir: m.as_ref().is_some_and(|m| m.is_dir()),
        }
    }
}

/// The answer to one conflict question.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Overwrite,
    Skip,
    /// Put the new file here instead.
    RenameTo(PathBuf),
    /// Use this rule for this and every later conflict in the job.
    All(ConflictPolicy),
    /// Stop the whole job.
    Cancel,
}

/// A job waiting for the user: `new` would replace `existing`. Answer through [`Conflict::answer`].
pub struct Conflict {
    pub new: FileInfo,
    pub existing: FileInfo,
    /// Where the new file was going (the existing file's path).
    pub target: PathBuf,
    reply: mpsc::Sender<Reply>,
}

impl Conflict {
    pub fn answer(&self, r: Reply) {
        let _ = self.reply.send(r);
    }

    /// A question not tied to a running job, and where its answer arrives.
    #[cfg(test)]
    pub fn for_test(new: FileInfo, existing: FileInfo, target: PathBuf) -> (Conflict, mpsc::Receiver<Reply>) {
        let (reply, rx) = mpsc::channel();
        (Conflict { new, existing, target, reply }, rx)
    }
}

#[derive(Clone, Debug)]
pub enum JobSpec {
    /// Copy (or move, when `mv`) each `(source, destination)` pair.
    Transfer { mv: bool, items: Vec<(PathBuf, PathBuf)>, policy: ConflictPolicy },
    Delete { paths: Vec<PathBuf>, permanent: bool },
    /// Pack `sources` into one archive; optionally delete the originals afterwards.
    Pack { sources: Vec<PathBuf>, archive: PathBuf, format: crate::archive::Format, level: u32, delete_after: bool },
    /// Unpack archives into `dest` (into a folder named after each archive when `into_folder`).
    Unpack { archives: Vec<PathBuf>, dest: PathBuf, into_folder: bool, policy: ConflictPolicy },
    /// Copy entries out of an archive being browsed (paths inside the archive).
    Extract { archive: PathBuf, items: Vec<String>, base: String, dest: PathBuf, policy: ConflictPolicy },
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
    Done { errors: Vec<String>, cancelled: bool, copied: Vec<PathBuf> },
    Conflict(Conflict),
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
    /// Set while the job waits for an answer about a file that already exists.
    pub conflict: Option<Conflict>,
    /// What a move or delete job takes away from where it was (so the stack can
    /// let go of it).
    pub moved: Vec<PathBuf>,
    /// A copy job's sources, and the ones copied whole (nothing skipped or failed).
    pub sources: Vec<PathBuf>,
    pub copied: Vec<PathBuf>,
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
                JobMsg::Done { errors, cancelled, copied } => {
                    self.finished = Some((errors, cancelled));
                    self.copied = copied;
                }
                JobMsg::Conflict(c) => self.conflict = Some(c),
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
    let moved = match &spec {
        JobSpec::Transfer { mv: true, items, .. } => items.iter().map(|(s, _)| s.clone()).collect(),
        JobSpec::Delete { paths, .. } => paths.clone(),
        _ => Vec::new(),
    };
    let sources = match &spec {
        JobSpec::Transfer { mv: false, items, .. } => items.iter().map(|(s, _)| s.clone()).collect(),
        _ => Vec::new(),
    };
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
        conflict: None,
        moved,
        sources,
        copied: Vec::new(),
    }
}

pub(crate) struct Ctx {
    tx: mpsc::Sender<JobMsg>,
    cancel: Arc<AtomicBool>,
    pub(crate) done: u64,
    pub(crate) total: u64,
    errors: Vec<String>,
    last_report: Instant,
    /// How conflicts are settled; changes when the user picks an "All" answer.
    pub(crate) policy: ConflictPolicy,
    /// Moving across drives: delete each original once it has been copied.
    moving: bool,
    /// Files left alone because of a conflict (Skip, or "overwrite older" saying no).
    skipped: usize,
    /// Copies that went all the way: nothing skipped, no errors.
    whole: Vec<PathBuf>,
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

    /// Where `new` should be written, given that it's going to `dst`. Returns `dst`
    /// when nothing is there (or two folders meet: they're merged), another path when
    /// renamed, and `None` to skip it. Settles conflicts by the job's policy, asking
    /// the user when that's [`ConflictPolicy::Ask`].
    pub(crate) fn place(&mut self, new: &FileInfo, dst: &Path) -> Option<PathBuf> {
        let Ok(m) = dst.symlink_metadata() else { return Some(dst.to_path_buf()) };
        if new.is_dir && m.is_dir() {
            return Some(dst.to_path_buf());
        }
        if self.policy == ConflictPolicy::RenameExisting {
            return self.rename_existing(dst);
        }
        let existing = FileInfo::of(dst);
        let reply = match decide(self.policy, new, &existing, dst) {
            Some(r) => r,
            None => self.ask(new, existing, dst),
        };
        match reply {
            Reply::Overwrite => Some(dst.to_path_buf()),
            Reply::Skip => {
                self.skipped += 1;
                None
            }
            Reply::RenameTo(p) => self.place(new, &p),
            Reply::All(ConflictPolicy::Ask) | Reply::Cancel => {
                self.cancel.store(true, Ordering::Relaxed);
                None
            }
            Reply::All(policy) => {
                self.policy = policy;
                self.place(new, dst)
            }
        }
    }

    /// Moves the existing file aside to a free name, freeing `dst` for the new one.
    fn rename_existing(&mut self, dst: &Path) -> Option<PathBuf> {
        match fs::rename(dst, unique_copy_name(dst)) {
            Ok(()) => Some(dst.to_path_buf()),
            Err(e) => {
                self.error(dst, format!("couldn't rename the existing file: {e}"));
                None
            }
        }
    }

    /// Sends the question to the window and waits for the answer (or for Cancel).
    fn ask(&mut self, new: &FileInfo, existing: FileInfo, dst: &Path) -> Reply {
        let (reply, answers) = mpsc::channel();
        let q = Conflict { new: new.clone(), existing, target: dst.to_path_buf(), reply };
        if self.tx.send(JobMsg::Conflict(q)).is_err() {
            return Reply::Cancel;
        }
        loop {
            match answers.recv_timeout(Duration::from_millis(100)) {
                Ok(r) => return r,
                Err(mpsc::RecvTimeoutError::Timeout) if !self.cancelled() => {}
                Err(_) => return Reply::Cancel,
            }
        }
    }
}

/// The answer a policy gives without asking, or `None` when the user must be asked.
fn decide(policy: ConflictPolicy, new: &FileInfo, existing: &FileInfo, dst: &Path) -> Option<Reply> {
    let overwrite_if = |yes: bool| if yes { Reply::Overwrite } else { Reply::Skip };
    Some(match policy {
        ConflictPolicy::Ask | ConflictPolicy::RenameExisting => return None,
        ConflictPolicy::Overwrite => Reply::Overwrite,
        ConflictPolicy::Skip => Reply::Skip,
        ConflictPolicy::OverwriteOlder => {
            overwrite_if(matches!((existing.modified, new.modified), (Some(a), Some(b)) if a < b))
        }
        ConflictPolicy::OverwriteSmaller => overwrite_if(existing.size < new.size),
        ConflictPolicy::OverwriteLarger => overwrite_if(existing.size > new.size),
        ConflictPolicy::RenameNew => Reply::RenameTo(unique_copy_name(dst)),
    })
}

fn run(spec: JobSpec, tx: mpsc::Sender<JobMsg>, cancel: Arc<AtomicBool>) {
    let policy = match &spec {
        JobSpec::Transfer { policy, .. } | JobSpec::Unpack { policy, .. } | JobSpec::Extract { policy, .. } => *policy,
        _ => ConflictPolicy::Ask,
    };
    let mut ctx = Ctx { tx, cancel, done: 0, total: 0, errors: Vec::new(), last_report: Instant::now(), policy, moving: false, skipped: 0, whole: Vec::new() };
    match spec {
        JobSpec::Transfer { mv, items, .. } => {
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
                // The same file under another name (a hard link, a bind mount, a symlink
                // to it): copying would empty it, moving would delete it.
                if same_file(src, dst) {
                    ctx.error(src, "source and destination are the same file");
                    ctx.done += size;
                    continue;
                }
                if inside(dst, src) {
                    ctx.error(src, "can't put a folder inside itself");
                    continue;
                }
                if !mv {
                    let before = (ctx.errors.len(), ctx.skipped);
                    copy_tree(src, dst, &mut ctx, false);
                    if (ctx.errors.len(), ctx.skipped) == before && !ctx.cancelled() {
                        ctx.whole.push(src.clone());
                    }
                    continue;
                }
                let Some(dst) = ctx.place(&FileInfo::of(src), dst) else {
                    ctx.done += size;
                    continue;
                };
                if !(is_real_dir(src) && is_real_dir(&dst)) {
                    // Same drive: a rename does it at once (and replaces a file atomically).
                    if !clear_way(src, &dst, &mut ctx) {
                        continue;
                    }
                    if fs::rename(src, &dst).is_ok() {
                        ctx.done += size;
                        continue;
                    }
                }
                // Another drive, or merging into a folder that's there: copy, deleting
                // each original once it's copied (so anything skipped stays put).
                ctx.moving = true;
                copy_tree(src, &dst, &mut ctx, true);
                ctx.moving = false;
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
        JobSpec::Extract { archive, items, base, dest, .. } => {
            crate::archive::extract(&archive, &items, &base, &dest, &mut ctx);
        }
        JobSpec::Unpack { archives, dest, into_folder, .. } => {
            ctx.total = archives.iter().map(|a| crate::archive::unpack_size(a)).sum();
            for a in &archives {
                if ctx.cancelled() {
                    break;
                }
                let target = crate::archive::unpack_target(a, &dest, into_folder);
                if let Err(e) = fs::create_dir_all(&target) {
                    ctx.error(&target, e);
                    continue;
                }
                crate::archive::unpack(a, &target, &mut ctx);
            }
        }
    }
    let cancelled = ctx.cancelled();
    let _ = ctx.tx.send(JobMsg::Done { errors: ctx.errors, cancelled, copied: ctx.whole });
}

/// True when `a` and `b` name the same directory entry (same device and inode,
/// links not followed), e.g. "Readme" and "README" on a drive that ignores case.
pub fn same_entry(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let id = |p: &Path| fs::symlink_metadata(p).ok().map(|m| (m.dev(), m.ino()));
        id(a).is_some() && id(a) == id(b)
    }
    #[cfg(not(unix))]
    {
        matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
    }
}

/// True when `a` and `b` are the same file on disk, though their paths may differ:
/// the same entry, or both resolving to the same file through links.
pub fn same_file(a: &Path, b: &Path) -> bool {
    same_entry(a, b) || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// True when `dst` lies inside the folder `src` (following links in either path).
fn inside(dst: &Path, src: &Path) -> bool {
    if dst.starts_with(src) {
        return true;
    }
    let Ok(src) = fs::canonicalize(src) else { return false };
    // `dst` itself usually doesn't exist yet: resolve its nearest existing ancestor.
    let mut cur = Some(dst);
    while let Some(c) = cur {
        if let Ok(real) = fs::canonicalize(c) {
            return real.starts_with(&src);
        }
        cur = c.parent();
    }
    false
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

/// Before writing `src` to `dst`: if something of another kind is there (a file
/// where a folder goes, or the other way round) it's removed; the user has already
/// agreed to replace it. Two files are left alone: the copy replaces the old one
/// atomically. Returns false (with an error noted) if the way couldn't be cleared.
fn clear_way(src: &Path, dst: &Path, ctx: &mut Ctx) -> bool {
    let Ok(old) = dst.symlink_metadata() else { return true };
    let Ok(new) = src.symlink_metadata() else { return true };
    if (old.is_dir() && new.is_dir()) || (old.is_file() && new.is_file()) {
        return true;
    }
    match remove_path(dst) {
        Ok(()) => true,
        Err(e) => {
            ctx.error(dst, e);
            false
        }
    }
}

/// Copies a file, link or folder. Unless `placed` (the conflict was already
/// settled), something already at `dst` is handled by [`Ctx::place`]. While
/// `ctx.moving`, each original is deleted once copied.
fn copy_tree(src: &Path, dst: &Path, ctx: &mut Ctx, placed: bool) {
    if ctx.cancelled() {
        return;
    }
    let meta = match fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(e) => return ctx.error(src, e),
    };
    let dst = if placed {
        dst.to_path_buf()
    } else {
        match ctx.place(&FileInfo::of(src), dst) {
            Some(d) => d,
            None => {
                ctx.done += tree_size(src);
                return;
            }
        }
    };
    if !clear_way(src, &dst, ctx) {
        return;
    }
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = match fs::read_link(src) {
            Ok(t) => t,
            Err(e) => return ctx.error(src, e),
        };
        if dst.symlink_metadata().is_ok() {
            if let Err(e) = remove_path(&dst) {
                return ctx.error(&dst, e);
            }
        }
        #[cfg(unix)]
        let res = std::os::unix::fs::symlink(&target, &dst);
        #[cfg(not(unix))]
        let res = fs::copy(src, &dst).map(|_| ());
        let _ = target;
        match res {
            Ok(()) => forget_original(src, ctx),
            Err(e) => ctx.error(&dst, e),
        }
    } else if ft.is_dir() {
        if !dst.exists() {
            // Private while it fills up; its own permissions come at the end.
            let mut b = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            if let Err(e) = b.create(&dst) {
                return ctx.error(&dst, e);
            }
        }
        let children = match fs::read_dir(src) {
            Ok(rd) => rd,
            Err(e) => return ctx.error(src, e),
        };
        for child in children.flatten() {
            copy_tree(&child.path(), &dst.join(child.file_name()), ctx, false);
            if ctx.cancelled() {
                let _ = fs::set_permissions(&dst, plain_permissions(&meta));
                return;
            }
        }
        let _ = fs::set_permissions(&dst, plain_permissions(&meta));
        if ctx.moving {
            // Only goes if everything in it was moved; skipped or failed files keep it.
            let _ = fs::remove_dir(src);
        }
    } else if copy_file(src, &dst, &meta, ctx) {
        forget_original(src, ctx);
    }
}

/// The permissions a copy gets: the original's, without setuid and setgid (as `cp`
/// does). Run as root, copying someone's program could otherwise make it setuid root.
/// Folders keep the sticky bit.
pub fn plain_permissions(meta: &fs::Metadata) -> fs::Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::Permissions::from_mode(plain_mode(meta.permissions().mode(), meta.is_dir()))
    }
    #[cfg(not(unix))]
    meta.permissions()
}

/// Permission bits without setuid and setgid; folders keep the sticky bit.
pub fn plain_mode(mode: u32, is_dir: bool) -> u32 {
    mode & if is_dir { 0o1777 } else { 0o777 }
}

/// While moving: deletes the original of something that has just been copied.
fn forget_original(src: &Path, ctx: &mut Ctx) {
    if ctx.moving {
        if let Err(e) = fs::remove_file(src) {
            ctx.error(src, format!("copied, but the original couldn't be removed: {e}"));
        }
    }
}

/// Copies into a temporary file next to `dst`, then renames it over `dst`, so an
/// existing file is only replaced once the copy is complete: a failed or cancelled
/// copy leaves it untouched.
fn copy_file(src: &Path, dst: &Path, meta: &fs::Metadata, ctx: &mut Ctx) -> bool {
    let tmp = part_name(dst);
    let result = (|| -> std::io::Result<bool> {
        let mut input = File::open(src)?;
        // Only we can read it until it's complete; then it gets the original's permissions.
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut output = opts.open(&tmp)?;
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
        output.set_permissions(plain_permissions(meta))?;
        if let Ok(t) = meta.modified() {
            let _ = output.set_modified(t);
        }
        drop(output);
        fs::rename(&tmp, dst)?;
        Ok(true)
    })();
    match result {
        Ok(true) => true,
        Ok(false) => {
            let _ = fs::remove_file(&tmp); // cancelled: don't leave half a file behind
            false
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            ctx.error(src, e);
            false
        }
    }
}

/// "dir/report.pdf" -> "dir/.report.pdf.duopane-part" (unused name, same folder so
/// the final rename stays on one drive).
fn part_name(dst: &Path) -> PathBuf {
    let parent = dst.parent().unwrap_or(Path::new("/"));
    let name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let base = format!(".{name}.duopane-part");
    let mut candidate = parent.join(&base);
    let mut i = 1;
    while candidate.symlink_metadata().is_ok() {
        i += 1;
        candidate = parent.join(format!("{base}{i}"));
    }
    candidate
}

/// How two files compare, byte for byte.
#[derive(Debug, PartialEq, Eq)]
pub enum Sameness {
    Identical,
    DifferentSize,
    /// The offset of the first byte that differs.
    DifferAt(u64),
}

/// Compares two files' contents (for "Compare by content").
pub fn compare_files(a: &Path, b: &Path) -> std::io::Result<Sameness> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(Sameness::DifferentSize);
    }
    let (mut fa, mut fb) = (std::io::BufReader::new(File::open(a)?), std::io::BufReader::new(File::open(b)?));
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    let mut offset = 0u64;
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(Sameness::Identical);
        }
        fb.read_exact(&mut bb[..n])?;
        if let Some(i) = ba[..n].iter().zip(&bb[..n]).position(|(x, y)| x != y) {
            return Ok(Sameness::DifferAt(offset + i as u64));
        }
        offset += n as u64;
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

    /// The stack lets go of a copy only when all of it got there: a file skipped
    /// because of a conflict (here, inside a folder) keeps its folder on the stack.
    #[test]
    fn copies_report_what_went_whole() {
        let base = std::env::temp_dir().join(format!("duopane-whole-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/dir")).unwrap();
        fs::create_dir_all(base.join("dst/dir")).unwrap();
        fs::write(base.join("src/a.txt"), b"a").unwrap();
        fs::write(base.join("src/dir/b.txt"), b"new").unwrap();
        fs::write(base.join("dst/dir/b.txt"), b"old").unwrap();
        let (a, dir) = (base.join("src/a.txt"), base.join("src/dir"));
        let mut h = start(JobSpec::Transfer {
            mv: false,
            items: vec![(a.clone(), base.join("dst/a.txt")), (dir.clone(), base.join("dst/dir"))],
            policy: ConflictPolicy::Skip,
        });
        while !h.poll() {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(h.sources, [a.clone(), dir]);
        assert_eq!(h.copied, [a]);
        assert_eq!(fs::read(base.join("dst/dir/b.txt")).unwrap(), b"old");
        let _ = fs::remove_dir_all(&base);
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

    #[cfg(unix)]
    #[test]
    fn copies_keep_permissions_but_not_setuid() {
        use std::os::unix::fs::PermissionsExt;
        let base = fresh("perms");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        for (name, mode) in [("private", 0o600), ("program", 0o6755)] {
            fs::write(src.join(name), "x").unwrap();
            fs::set_permissions(src.join(name), fs::Permissions::from_mode(mode)).unwrap();
        }
        fs::set_permissions(&src, fs::Permissions::from_mode(0o750)).unwrap();
        let errs = wait(start(JobSpec::Transfer { mv: false, items: vec![(src.clone(), base.join("out"))], policy: ConflictPolicy::Overwrite }));
        assert!(errs.is_empty(), "{errs:?}");
        let mode = |p: &str| fs::metadata(base.join(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("out/private"), 0o600);
        assert_eq!(mode("out/program"), 0o755, "setuid and setgid are dropped");
        assert_eq!(mode("out"), 0o750, "the folder gets its own permissions in the end");
        assert_eq!(plain_mode(0o1777, true), 0o1777, "folders keep the sticky bit");
        let _ = fs::remove_dir_all(&base);
    }

    fn fresh(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("duopane-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn part_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("duopane-part"))
            .collect()
    }

    #[test]
    fn overwrite_replaces_only_when_complete() {
        let base = fresh("overwrite");
        fs::write(base.join("new.txt"), b"new contents").unwrap();
        fs::write(base.join("old.txt"), b"precious").unwrap();

        // A cancelled overwrite must leave the old file exactly as it was.
        let (tx, _rx) = mpsc::channel();
        let mut ctx = Ctx {
            tx,
            cancel: Arc::new(AtomicBool::new(true)),
            done: 0,
            total: 0,
            errors: Vec::new(),
            last_report: Instant::now(),
            policy: ConflictPolicy::Overwrite,
            moving: false,
            skipped: 0,
            whole: Vec::new(),
        };
        let meta = fs::metadata(base.join("new.txt")).unwrap();
        copy_file(&base.join("new.txt"), &base.join("old.txt"), &meta, &mut ctx);
        assert_eq!(fs::read(base.join("old.txt")).unwrap(), b"precious");
        assert!(part_files(&base).is_empty());

        // A completed overwrite replaces it, leaving no temporary file.
        let errs = wait(start(JobSpec::Transfer {
            mv: false,
            items: vec![(base.join("new.txt"), base.join("old.txt"))],
            policy: ConflictPolicy::Overwrite,
        }));
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(fs::read(base.join("old.txt")).unwrap(), b"new contents");
        assert!(part_files(&base).is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn same_file_under_another_path_is_left_alone() {
        let base = fresh("samefile");
        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("f.txt"), b"keep me").unwrap();
        std::os::unix::fs::symlink(&real, base.join("link")).unwrap();
        fs::hard_link(real.join("f.txt"), real.join("hard.txt")).unwrap();

        for mv in [false, true] {
            for other in [base.join("link/f.txt"), real.join("hard.txt")] {
                let errs = wait(start(JobSpec::Transfer {
                    mv,
                    items: vec![(real.join("f.txt"), other.clone())],
                    policy: ConflictPolicy::Overwrite,
                }));
                assert_eq!(errs.len(), 1, "mv={mv} {other:?}: {errs:?}");
                assert_eq!(fs::read(real.join("f.txt")).unwrap(), b"keep me", "mv={mv} {other:?}");
            }
        }
        // A folder copied into itself through a link.
        let errs = wait(start(JobSpec::Transfer {
            mv: false,
            items: vec![(real.clone(), base.join("link/inner"))],
            policy: ConflictPolicy::Overwrite,
        }));
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(!real.join("inner").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn same_entry_tells_case_twins_apart() {
        let base = fresh("case");
        fs::write(base.join("Readme"), b"a").unwrap();
        fs::write(base.join("readme"), b"b").unwrap();
        // Two files on Linux (or the same one on a case-ignoring drive, where
        // only one write survives): never two *different* files reported as one.
        let distinct = fs::read(base.join("Readme")).unwrap() != fs::read(base.join("readme")).unwrap();
        assert_eq!(same_entry(&base.join("Readme"), &base.join("readme")), !distinct);
        assert!(same_entry(&base.join("readme"), &base.join("./readme")));
        let _ = fs::remove_dir_all(&base);
    }

    /// Runs a job, answering each conflict question with `answer`.
    /// Returns (errors, cancelled, the names asked about).
    fn run_answering(spec: JobSpec, mut answer: impl FnMut(&Conflict) -> Reply) -> (Vec<String>, bool, Vec<String>) {
        let mut h = start(spec);
        let mut asked = Vec::new();
        let t = Instant::now();
        while !h.poll() {
            if let Some(q) = h.conflict.take() {
                asked.push(q.target.file_name().unwrap().to_string_lossy().into_owned());
                q.answer(answer(&q));
            }
            assert!(t.elapsed() < Duration::from_secs(10), "job never finished");
            thread::sleep(Duration::from_millis(2));
        }
        let (errors, cancelled) = h.finished.unwrap();
        (errors, cancelled, asked)
    }

    /// base/src/{a,b,c}.txt = "new", base/dst/src/{a,b,c}.txt = "old".
    fn clash(name: &str) -> PathBuf {
        let base = fresh(name);
        for dir in ["src", "dst/src"] {
            fs::create_dir_all(base.join(dir)).unwrap();
            for f in ["a.txt", "b.txt", "c.txt"] {
                fs::write(base.join(dir).join(f), if dir == "src" { "new" } else { "old" }).unwrap();
            }
        }
        base
    }

    fn read(p: PathBuf) -> String {
        fs::read_to_string(p).unwrap()
    }

    #[test]
    fn asks_about_each_file_inside_merged_folders() {
        let base = clash("ask-each");
        let spec = JobSpec::Transfer { mv: false, items: vec![(base.join("src"), base.join("dst/src"))], policy: ConflictPolicy::Ask };
        let d = base.join("dst/src");
        let (errs, cancelled, asked) = run_answering(spec, |q| match q.target.file_name().unwrap().to_str().unwrap() {
            "a.txt" => Reply::Skip,
            "b.txt" => Reply::RenameTo(q.target.with_file_name("b2.txt")),
            _ => Reply::Overwrite,
        });
        assert!(errs.is_empty() && !cancelled, "{errs:?}");
        let mut asked = asked;
        asked.sort();
        assert_eq!(asked, ["a.txt", "b.txt", "c.txt"]);
        assert_eq!((read(d.join("a.txt")), read(d.join("b.txt")), read(d.join("b2.txt")), read(d.join("c.txt"))), ("old".into(), "old".into(), "new".into(), "new".into()));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_all_answer_covers_the_rest_and_cancel_stops() {
        let base = clash("ask-all");
        let spec = || JobSpec::Transfer { mv: false, items: vec![(base.join("src"), base.join("dst/src"))], policy: ConflictPolicy::Ask };
        let (errs, cancelled, asked) = run_answering(spec(), |_| Reply::All(ConflictPolicy::Overwrite));
        assert!(errs.is_empty() && !cancelled);
        assert_eq!(asked.len(), 1, "one question, then the rule");
        assert!(["a.txt", "b.txt", "c.txt"].iter().all(|f| read(base.join("dst/src").join(f)) == "new"));
        let _ = fs::remove_dir_all(&base);

        let base = clash("ask-cancel");
        let spec = JobSpec::Transfer { mv: false, items: vec![(base.join("src"), base.join("dst/src"))], policy: ConflictPolicy::Ask };
        let (_, cancelled, asked) = run_answering(spec, |_| Reply::Cancel);
        assert!(cancelled);
        assert_eq!(asked.len(), 1);
        assert!(["a.txt", "b.txt", "c.txt"].iter().all(|f| read(base.join("dst/src").join(f)) == "old"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rules_older_and_rename_existing() {
        let base = clash("rules");
        let d = base.join("dst/src");
        let set_time = |p: PathBuf, secs: u64| {
            File::options().write(true).open(p).unwrap().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(secs)).unwrap()
        };
        for f in ["a.txt", "b.txt", "c.txt"] {
            set_time(base.join("src").join(f), 2_000_000_000);
        }
        set_time(d.join("a.txt"), 1_000_000_000); // older: replaced
        set_time(d.join("b.txt"), 2_100_000_000); // newer: kept
        set_time(d.join("c.txt"), 2_000_000_000); // same age: kept
        let spec = JobSpec::Transfer { mv: false, items: vec![(base.join("src"), d.clone())], policy: ConflictPolicy::OverwriteOlder };
        let (errs, _, asked) = run_answering(spec, |_| unreachable!("a rule never asks"));
        assert!(errs.is_empty() && asked.is_empty(), "{errs:?}");
        assert_eq!((read(d.join("a.txt")), read(d.join("b.txt")), read(d.join("c.txt"))), ("new".into(), "old".into(), "old".into()));

        let spec = JobSpec::Transfer { mv: false, items: vec![(base.join("src/b.txt"), d.join("b.txt"))], policy: ConflictPolicy::RenameExisting };
        let (errs, _, _) = run_answering(spec, |_| unreachable!());
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!((read(d.join("b.txt")), read(d.join("b copy.txt"))), ("new".into(), "old".into()));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn moving_keeps_what_was_skipped() {
        let base = clash("move-skip");
        let spec = JobSpec::Transfer { mv: true, items: vec![(base.join("src"), base.join("dst/src"))], policy: ConflictPolicy::Ask };
        let (errs, _, _) = run_answering(spec, |q| if q.target.ends_with("a.txt") { Reply::Skip } else { Reply::Overwrite });
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(read(base.join("src/a.txt")), "new", "a skipped file stays where it was");
        assert!(!base.join("src/b.txt").exists() && !base.join("src/c.txt").exists());
        assert_eq!(read(base.join("dst/src/a.txt")), "old");
        assert_eq!(read(base.join("dst/src/b.txt")), "new");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn size_rules_and_comparing() {
        let info = |size, modified| FileInfo { shown: String::new(), real: None, size, modified, is_dir: false };
        let (small, big) = (info(1, None), info(9, None));
        let d = Path::new("/x");
        assert_eq!(decide(ConflictPolicy::OverwriteSmaller, &big, &small, d), Some(Reply::Overwrite));
        assert_eq!(decide(ConflictPolicy::OverwriteSmaller, &small, &big, d), Some(Reply::Skip));
        assert_eq!(decide(ConflictPolicy::OverwriteLarger, &small, &big, d), Some(Reply::Overwrite));
        assert_eq!(decide(ConflictPolicy::OverwriteOlder, &small, &big, d), Some(Reply::Skip), "unknown dates: keep");
        assert_eq!(decide(ConflictPolicy::Ask, &small, &big, d), None);

        let base = fresh("compare");
        fs::write(base.join("a"), b"hello world").unwrap();
        fs::write(base.join("b"), b"hello world").unwrap();
        fs::write(base.join("c"), b"hello there").unwrap();
        fs::write(base.join("d"), b"hi").unwrap();
        assert_eq!(compare_files(&base.join("a"), &base.join("b")).unwrap(), Sameness::Identical);
        assert_eq!(compare_files(&base.join("a"), &base.join("c")).unwrap(), Sameness::DifferAt(6));
        assert_eq!(compare_files(&base.join("a"), &base.join("d")).unwrap(), Sameness::DifferentSize);
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
