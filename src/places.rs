//! The drive menu (Alt+F1 for the left pane, Alt+F2 for the right), where the
//! old commanders let you pick a drive letter. Here it lists the usual places,
//! bookmarks, mounted drives (USB sticks, other disks, network and cloud
//! mounts) and drives that are plugged in but not mounted yet.
//!
//! The list itself comes from `/proc/self/mountinfo`, which is instant. Free
//! space and drive names (from `lsblk`) arrive from background threads, because
//! asking a network mount that has gone away can hang for a long time.

use crate::app::tilde;
use crate::pane::human_size;
use crate::theme;
use eframe::egui::{self, pos2, vec2, Align2, FontId, Key, Modifiers, Pos2, Sense};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Places,
    Bookmarks,
    Drives,
    NotMounted,
    Network,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Places => "Places",
            Section::Bookmarks => "Bookmarks",
            Section::Drives => "Drives",
            Section::NotMounted => "Not mounted",
            Section::Network => "Network",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub section: Section,
    pub name: String,
    pub detail: String,
    /// Where it opens; `None` for a drive that has to be mounted first.
    pub path: Option<PathBuf>,
    /// The block device (`/dev/sdb1`), when there is one.
    pub device: Option<String>,
    /// A FUSE mount (sshfs, rclone, …): unmounted with `fusermount`.
    pub fuse: bool,
    /// Whether Duopane offers to unmount it (things the user mounted, not the system).
    pub can_unmount: bool,
    /// Free and total bytes, once known.
    pub space: Option<(u64, u64)>,
    /// An SFTP server to connect to (`user@host:/folder`); empty for "Connect to SFTP…".
    pub sftp: Option<String>,
    /// How to mount it again, for a mount that's down.
    pub again: Option<Again>,
}

/// How to bring back a mount that isn't mounted now.
#[derive(Clone, Debug, PartialEq)]
pub enum Again {
    /// A systemd user service that mounts it (`systemctl --user start`).
    Unit(String),
    /// The command line that mounted it before Duopane unmounted it.
    Command { args: Vec<String>, point: PathBuf },
}

/// A mount Duopane unmounted, remembered (in the settings) so it can be mounted again.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Remount {
    pub name: String,
    pub point: PathBuf,
    pub command: Vec<String>,
}

/// What a mount or unmount did.
#[derive(Debug)]
pub enum Done {
    Mounted(PathBuf),
    /// Unmounted; with how to mount it again when Duopane should remember that.
    Unmounted(Option<Remount>),
}

impl Place {
    fn folder(section: Section, name: &str, path: PathBuf) -> Place {
        Place { section, name: name.into(), detail: tilde(&path), path: Some(path), device: None, fuse: false, can_unmount: false, space: None, sftp: None, again: None }
    }
}

/// One line of `/proc/self/mountinfo`, the parts we use.
#[derive(Debug, PartialEq)]
struct Mount {
    point: PathBuf,
    fstype: String,
    source: String,
    /// The mount's options and the filesystem's, e.g. "user_id=1000".
    options: String,
}

/// The network mount (sshfs, NFS, SMB, …) that `path` is on: its folder and what
/// it's a connection to, e.g. "me@nas:/srv".
pub fn network_mount_of(path: &Path) -> Option<(PathBuf, String)> {
    #[cfg(target_os = "linux")]
    {
        let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
        parse_mountinfo(&info)
            .into_iter()
            .filter(|m| path.starts_with(&m.point))
            .max_by_key(|m| m.point.as_os_str().len())
            .filter(|m| NETWORK.contains(&m.fstype.as_str()))
            .map(|m| (m.point, m.source))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        None
    }
}

/// Mount points escape spaces and a few other characters as octal (`\040`).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let l: Vec<&str> = left.split(' ').collect();
            let r: Vec<&str> = right.split(' ').collect();
            Some(Mount {
                point: PathBuf::from(unescape(l.get(4)?)),
                fstype: r.first()?.to_string(),
                source: unescape(r.get(1)?),
                options: format!("{},{}", l.get(5).unwrap_or(&""), r.get(2).unwrap_or(&"")),
            })
        })
        .collect()
}

/// FUSE filesystems that are part of the desktop rather than something to browse.
const FUSE_NOISE: &[&str] = &["fuse.portal", "fuse.gvfsd-fuse", "fuse.snapfuse", "fuse.lxcfs", "fuse.xdg-document-portal", "fuse.appimagelauncherfs", "fuse.jetbrains-toolbox"];
const NETWORK: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "9p", "virtiofs", "davfs", "fuse.sshfs", "fuse.rclone", "fuse.davfs2", "fuse.s3fs"];

/// The folders the system keeps for itself, even when they're separate partitions.
fn system_folder(p: &Path) -> bool {
    ["/boot", "/efi", "/var", "/usr", "/tmp", "/snap", "/nix", "/opt", "/srv", "/proc", "/sys", "/dev"].iter().any(|s| p.starts_with(s))
        || (p.starts_with("/run") && !p.starts_with("/run/media") && !p.starts_with("/run/mount") && !is_gvfs(p))
}

fn is_gvfs(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.starts_with("/run/user/") && s.contains("/gvfs/")
}

/// The mounts worth showing: real disks, network shares and FUSE mounts, without
/// the root filesystem's subvolumes and bind mounts (on btrfs, `/home`, `/var/log`
/// and friends are all mounts of the same partition as `/`).
fn drives(mounts: &[Mount], home: &Path, uid: Option<u32>) -> Vec<Place> {
    let root_source = mounts.iter().rev().find(|m| m.point == Path::new("/")).map(|m| m.source.clone());
    let mut out: Vec<Place> = Vec::new();
    for m in mounts {
        let disk = m.source.starts_with("/dev/") && !m.source.starts_with("/dev/loop") && !m.source.starts_with("/dev/zram");
        let fuse = m.fstype.starts_with("fuse.") && !FUSE_NOISE.contains(&m.fstype.as_str());
        let network = NETWORK.contains(&m.fstype.as_str());
        if !(disk || fuse || network) || m.point == Path::new("/") || system_folder(&m.point) {
            continue;
        }
        if disk && Some(&m.source) == root_source.as_ref() {
            continue;
        }
        // Home is already under Places, and a folder above it (a separate /home) says little more.
        if home.starts_with(&m.point) {
            continue;
        }
        // Mounted twice (or remounted): keep the last one.
        out.retain(|p| p.path.as_deref() != Some(&m.point));
        let name = m.point.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| tilde(&m.point));
        let kind = match m.fstype.as_str() {
            "fuse.sshfs" => format!("SFTP ({})", m.source),
            "fuse.rclone" => format!("rclone ({})", m.source.trim_end_matches(':')),
            t if network => t.to_string(),
            "fuseblk" => String::new(),
            t => t.trim_start_matches("fuse.").to_string(),
        };
        let detail = if kind.is_empty() { tilde(&m.point) } else { format!("{}  ·  {kind}", tilde(&m.point)) };
        let mine = uid.is_some_and(|u| m.options.split(',').any(|o| o == format!("user_id={u}")));
        let removable_place = m.point.starts_with("/run/media") || is_gvfs(&m.point);
        out.push(Place {
            section: Section::Drives,
            name,
            detail,
            path: Some(m.point.clone()),
            device: disk.then(|| m.source.clone()),
            fuse,
            can_unmount: removable_place || (fuse && mine),
            space: None,
            sftp: None,
            again: None,
        });
    }
    out
}

/// What `lsblk` tells us about the block devices.
#[derive(Debug, Default)]
pub struct Devices {
    /// Device path → its label, for naming mounted drives.
    pub labels: HashMap<String, String>,
    /// Device paths of plugged-in (USB, card reader) drives.
    pub removable: Vec<String>,
    pub unmounted: Vec<Place>,
}

/// Filesystems that a person may want to open even on an internal drive.
const BROWSABLE: &[&str] = &["ntfs", "exfat", "vfat", "ext4", "ext3", "ext2", "btrfs", "xfs", "f2fs", "hfsplus", "apfs", "udf", "iso9660"];

fn parse_lsblk(json: &str) -> Devices {
    let mut d = Devices::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return d };
    let Some(list) = v["blockdevices"].as_array() else { return d };
    // `lsblk` lists the model on the disk only; partitions inherit it.
    let mut model = String::new();
    for b in list {
        let s = |k: &str| b[k].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let (Some(path), kind) = (s("path"), s("type").unwrap_or_default()) else { continue };
        if kind == "disk" {
            model = [s("vendor"), s("model")].into_iter().flatten().collect::<Vec<_>>().join(" ");
        }
        let label = s("label");
        if let Some(l) = &label {
            d.labels.insert(path.clone(), l.clone());
        }
        let plugged = b["hotplug"].as_bool().unwrap_or(false) || b["rm"].as_bool().unwrap_or(false);
        if plugged {
            d.removable.push(path.clone());
        }
        let Some(fstype) = s("fstype") else { continue };
        let mounted = b["mountpoints"].as_array().is_some_and(|m| m.iter().any(|p| p.is_string()));
        let size = b["size"].as_u64().unwrap_or(0);
        let wanted = plugged || (BROWSABLE.contains(&fstype.as_str()) && size >= 2_000_000_000);
        if mounted || !wanted || !(kind == "part" || kind == "disk" || kind == "rom") {
            continue;
        }
        let name = label.unwrap_or_else(|| format!("{} drive", human_size(size)));
        let mut detail = vec![];
        if !model.is_empty() {
            detail.push(model.clone());
        }
        detail.push(format!("{path}  ·  {fstype}  ·  {}", human_size(size)));
        d.unmounted.push(Place {
            section: Section::NotMounted,
            name,
            detail: detail.join("  ·  "),
            path: None,
            device: Some(path),
            fuse: false,
            can_unmount: false,
            space: None,
            sftp: None,
            again: None,
        });
    }
    d
}

/// Free and total bytes of the filesystem holding `p`. Can block (dead network mounts).
#[cfg(unix)]
#[allow(clippy::useless_conversion)]
pub(crate) fn space(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `s` a properly sized, writable struct.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    // The field types differ between systems (u32 on some), hence `from`.
    let unit = u64::from(s.f_frsize);
    Some((u64::from(s.f_bavail) * unit, u64::from(s.f_blocks) * unit))
}

#[cfg(not(unix))]
pub(crate) fn space(_: &Path) -> Option<(u64, u64)> {
    None
}

enum Update {
    Space(PathBuf, (u64, u64)),
    Devices(Devices),
}

/// Everything for the menu, gathered without waiting on any drive.
pub fn gather(bookmarks: &[PathBuf], sftp_recent: &[String], remounts: &[Remount]) -> Vec<Place> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let mut out = vec![Place::folder(Section::Places, "Home", home.clone())];
    #[cfg(not(windows))]
    out.push(Place::folder(Section::Places, "Root", PathBuf::from("/")));
    let trash = dirs::data_dir().map(|d| d.join("Trash/files"));
    if let Some(t) = trash.filter(|t| t.is_dir()) {
        out.push(Place::folder(Section::Places, "Trash", t));
    }
    for b in bookmarks {
        let name = b.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| tilde(b));
        out.push(Place::folder(Section::Bookmarks, &name, b.clone()));
    }
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") {
        let uid = std::fs::metadata("/proc/self").ok().map(|m| std::os::unix::fs::MetadataExt::uid(&m));
        out.extend(drives(&parse_mountinfo(&text), &home, uid));
    }
    #[cfg(target_os = "macos")]
    if let Ok(rd) = std::fs::read_dir("/Volumes") {
        for e in rd.flatten() {
            let p = e.path();
            if std::fs::canonicalize(&p).is_ok_and(|c| c != Path::new("/")) {
                let mut pl = Place::folder(Section::Drives, &e.file_name().to_string_lossy(), p);
                pl.can_unmount = false;
                out.push(pl);
            }
        }
    }
    #[cfg(windows)]
    for letter in b'A'..=b'Z' {
        let p = PathBuf::from(format!("{}:\\", letter as char));
        if p.exists() {
            out.push(Place::folder(Section::Drives, &format!("{}:", letter as char), p));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let units = dirs::config_dir().map(|c| mount_units(&c.join("systemd/user"), &home)).unwrap_or_default();
        out.extend(down_again(&out, &units, remounts));
    }
    #[cfg(unix)]
    out.extend(network(&out, sftp_recent, &ssh_config_hosts()));
    out
}

/// FUSE mounts that are down but can come back: from systemd user services, and
/// ones Duopane unmounted. They go under "Not mounted".
fn down_again(places: &[Place], units: &[(String, PathBuf, String)], remounts: &[Remount]) -> Vec<Place> {
    let mounted: Vec<&Path> = places.iter().filter_map(|p| p.path.as_deref()).collect();
    let mut out: Vec<Place> = Vec::new();
    let mut listed: Vec<&Path> = Vec::new();
    for (unit, point, what) in units {
        if !mounted.contains(&point.as_path()) && !listed.contains(&point.as_path()) {
            listed.push(point);
            out.push(Place {
                section: Section::NotMounted,
                name: point.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| tilde(point)),
                detail: format!("{}  ·  {what}  ·  {unit}", tilde(point)),
                path: None,
                device: None,
                fuse: true,
                can_unmount: false,
                space: None,
                sftp: None,
                again: Some(Again::Unit(unit.clone())),
            });
        }
    }
    for r in remounts {
        if !mounted.contains(&r.point.as_path()) && !listed.contains(&r.point.as_path()) {
            listed.push(&r.point);
            let program = r.command.first().and_then(|c| Path::new(c).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            out.push(Place {
                section: Section::NotMounted,
                name: r.name.clone(),
                detail: format!("{}  ·  {program}  ·  mounted before", tilde(&r.point)),
                path: None,
                device: None,
                fuse: true,
                // Delete forgets it.
                can_unmount: true,
                space: None,
                sftp: None,
                again: Some(Again::Command { args: r.command.clone(), point: r.point.clone() }),
            });
        }
    }
    out
}

/// systemd user services that mount something with rclone or sshfs:
/// (service, mount point, "rclone (gdrive:)").
fn mount_units(dir: &Path, home: &Path) -> Vec<(String, PathBuf, String)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "service")).collect();
    files.sort();
    files
        .iter()
        .filter_map(|f| {
            let unit = f.file_name()?.to_string_lossy().into_owned();
            // Templates (name@.service) need an instance name; leave them be.
            if unit.contains('@') {
                return None;
            }
            let (point, what) = unit_mount(&std::fs::read_to_string(f).ok()?, home)?;
            Some((unit, point, what))
        })
        .collect()
}

/// The mount point (and a description) in a service's `ExecStart`, if it runs
/// `rclone mount` or `sshfs`.
fn unit_mount(text: &str, home: &Path) -> Option<(PathBuf, String)> {
    // Join continued lines ("\" at the end).
    let joined = text.replace("\\\n", " ");
    let exec = joined.lines().map(str::trim).find_map(|l| l.strip_prefix("ExecStart="))?;
    let words: Vec<String> = exec
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
        .map(|w| w.replace("%h", &home.to_string_lossy()))
        .collect();
    let program = words.first()?.trim_start_matches(['-', '@', '+', '!', ':']);
    let program = Path::new(program).file_name()?.to_string_lossy().into_owned();
    let rest: Vec<&String> = match program.as_str() {
        "rclone" if words.get(1).is_some_and(|w| w == "mount" || w == "nfsmount") => words[2..].iter().collect(),
        "sshfs" => words[1..].iter().collect(),
        _ => return None,
    };
    // The remote has a colon ("gdrive:", "me@host:/x"); the mount point is a path.
    let remote = rest.iter().find(|w| !w.starts_with('-') && w.contains(':'))?;
    let point = rest.iter().find(|w| w.starts_with('/') && !w.contains(':'))?;
    Some((PathBuf::from(point.as_str()), format!("{program} ({remote})")))
}

/// The command line of the program serving a FUSE mount (rclone, sshfs, …), so it
/// can be run again; the mount point in it made absolute. Only our own processes
/// count, and the program is named by where it really is: another user's process
/// (or a "sshfs" in /tmp) must never become a command we run later.
#[cfg(unix)]
fn fuse_command(point: &Path) -> Option<Vec<String>> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let me = std::process::id().to_string();
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let pid = e.file_name().to_string_lossy().into_owned();
        if pid == me || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if e.metadata().ok().map(|m| m.uid()) != Some(uid) {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else { continue };
        let mut args: Vec<String> = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        let cwd = std::fs::read_link(e.path().join("cwd")).unwrap_or_default();
        let Some(i) = args.iter().position(|a| cwd.join(a) == point) else { continue };
        // Only the programs that serve mounts: not an editor that has the folder open.
        let program = args.first().and_then(|a| Path::new(a).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !["rclone", "sshfs", "s3fs", "gocryptfs", "encfs", "davfs2", "bindfs", "mergerfs", "ntfs-3g", "fuse-zip", "archivemount"].contains(&program.as_str()) {
            continue;
        }
        // The program itself, and only from a folder others can't write to.
        let Ok(exe) = std::fs::read_link(e.path().join("exe")) else { continue };
        let writable_by_others = |p: &Path| std::fs::metadata(p).map_or(true, |m| m.permissions().mode() & 0o022 != 0 || (m.uid() != 0 && m.uid() != uid));
        let same_program = exe.file_name().is_some_and(|n| n.to_string_lossy() == program);
        if !same_program || !exe.is_absolute() || writable_by_others(&exe) || exe.parent().is_none_or(writable_by_others) {
            continue;
        }
        args[0] = exe.to_string_lossy().into_owned();
        args[i] = point.to_string_lossy().into_owned();
        return Some(args);
    }
    None
}

#[cfg(not(unix))]
fn fuse_command(_: &Path) -> Option<Vec<String>> {
    None
}

/// The systemd user service that mounts `point`, if one does.
fn unit_for(point: &Path) -> Option<String> {
    let home = dirs::home_dir()?;
    mount_units(&dirs::config_dir()?.join("systemd/user"), &home).into_iter().find(|(_, p, _)| p == point).map(|(u, _, _)| u)
}

/// Servers to connect to: recent ones first, then the hosts in `~/.ssh/config`,
/// leaving out those already mounted (they're under Drives), and "Connect…".
fn network(places: &[Place], recent: &[String], config_hosts: &[String]) -> Vec<Place> {
    let mounted: Vec<&Path> = places.iter().filter_map(|p| p.path.as_deref()).collect();
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for (t, from_config) in recent.iter().map(|t| (t, false)).chain(config_hosts.iter().map(|t| (t, true))) {
        let Ok(target) = Target::parse(t) else { continue };
        if seen.contains(&target.label) || mounted.iter().any(|m| *m == target.mount_dir()) {
            continue;
        }
        seen.push(target.label.clone());
        out.push(Place {
            section: Section::Network,
            name: target.label.clone(),
            detail: if from_config { "SFTP  ·  from ~/.ssh/config".into() } else { format!("SFTP  ·  {}", target.remote) },
            path: None,
            device: None,
            fuse: false,
            // Delete forgets a recent server; the ssh config isn't ours to change.
            can_unmount: !from_config,
            space: None,
            sftp: Some(t.clone()),
            again: None,
        });
    }
    out.push(Place {
        section: Section::Network,
        name: "Connect to SFTP…".into(),
        detail: "A server you can reach with ssh, opened as a folder (needs sshfs)".into(),
        path: None,
        device: None,
        fuse: false,
        can_unmount: false,
        space: None,
        sftp: Some(String::new()),
        again: None,
    });
    out
}

/// The `Host` names in `~/.ssh/config`, without patterns like `*` or `!x`.
fn ssh_config_hosts() -> Vec<String> {
    let Some(text) = dirs::home_dir().and_then(|h| std::fs::read_to_string(h.join(".ssh/config")).ok()) else { return vec![] };
    parse_ssh_hosts(&text)
}

fn parse_ssh_hosts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else { continue };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for h in rest.split_whitespace() {
            if !h.contains(['*', '?', '!']) && !out.iter().any(|o| o == h) {
                out.push(h.to_string());
            }
        }
    }
    out
}

/// An SFTP server as typed: `user@host:/folder`, `host` (a name from
/// `~/.ssh/config` works too) or `sftp://user@host:2222/folder`.
#[derive(Debug, PartialEq)]
pub struct Target {
    /// What sshfs is given: `user@host:folder`.
    pub remote: String,
    pub port: Option<u16>,
    /// A short name, also used for the folder it's mounted on.
    pub label: String,
}

impl Target {
    pub fn parse(text: &str) -> Result<Target, String> {
        let t = text.trim();
        let (host_part, path, port) = if let Some(url) = t.strip_prefix("sftp://").or_else(|| t.strip_prefix("ssh://")) {
            let (authority, path) = match url.find('/') {
                Some(i) => (&url[..i], &url[i..]),
                None => (url, ""),
            };
            let (host, port) = match authority.rsplit_once(':') {
                Some((h, p)) => (h, Some(p.parse::<u16>().map_err(|_| format!("\"{p}\" isn't a port number"))?)),
                None => (authority, None),
            };
            (host.to_string(), path.to_string(), port)
        } else {
            match t.split_once(':') {
                Some((h, p)) => (h.to_string(), p.to_string(), None),
                None => (t.to_string(), String::new(), None),
            }
        };
        let host_name = host_part.rsplit('@').next().unwrap_or("");
        if host_name.is_empty() || host_part.contains(char::is_whitespace) || host_part.starts_with('-') {
            return Err("Type the server as user@host:/folder (or just host)".into());
        }
        let path = path.trim_end_matches('/').to_string();
        let path = if path.is_empty() && t.contains(":/") && !t.contains("://") { "/".to_string() } else { path };
        let mut label = host_part.clone();
        if !path.is_empty() {
            label.push_str(&path.replace('/', "_"));
        }
        if let Some(p) = port {
            label.push_str(&format!("_{p}"));
        }
        // It names a folder: no slashes, and nothing like "..".
        let mut label = label.replace('/', "_");
        if label.starts_with('.') {
            label.insert(0, '_');
        }
        Ok(Target { remote: format!("{host_part}:{path}"), port, label })
    }

    /// Where it's mounted: `~/.cache/duopane/mounts/<label>`.
    pub fn mount_dir(&self) -> PathBuf {
        mounts_dir().join(&self.label)
    }
}

pub fn mounts_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("duopane/mounts")
}

/// Starts the slow lookups (free space, `lsblk`); results come through the channel.
fn look_up(places: &[Place], ctx: &egui::Context) -> mpsc::Receiver<Update> {
    let (tx, rx) = mpsc::channel();
    for p in places {
        if p.section == Section::Bookmarks || p.name == "Trash" {
            continue;
        }
        let Some(path) = p.path.clone() else { continue };
        let (tx, ctx) = (tx.clone(), ctx.clone());
        // One thread each: a hung network mount mustn't hold up the others.
        std::thread::spawn(move || {
            if let Some(s) = space(&path) {
                let _ = tx.send(Update::Space(path, s));
                ctx.request_repaint();
            }
        });
    }
    #[cfg(target_os = "linux")]
    {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let out = std::process::Command::new("lsblk")
                .args(["-J", "-b", "-o", "PATH,LABEL,SIZE,FSTYPE,MOUNTPOINTS,HOTPLUG,RM,TYPE,MODEL,VENDOR"])
                .stderr(std::process::Stdio::null())
                .output();
            if let Ok(o) = out {
                let _ = tx.send(Update::Devices(parse_lsblk(&String::from_utf8_lossy(&o.stdout))));
                ctx.request_repaint();
            }
        });
    }
    rx
}

pub enum PlacesOutcome {
    Open(usize, PathBuf),
    Mount(usize, Place),
    Unmount(Place),
    /// Connect to this server (as typed before).
    Sftp(usize, String),
    /// "Connect to SFTP…": ask for a server.
    AskSftp(usize),
    /// Forget a recent server.
    Forget(String),
    /// Forget a mount Duopane would mount again.
    ForgetMount(PathBuf),
    Close,
}

pub struct PlacesMenu {
    /// Which pane it's for (0 = left).
    pub pane: usize,
    pub places: Vec<Place>,
    selected: usize,
    updates: Option<mpsc::Receiver<Update>>,
    /// The pane's folders when the menu opened, to mark where each pane is.
    here: [PathBuf; 2],
}

impl PlacesMenu {
    pub fn new(pane: usize, places: Vec<Place>, here: [PathBuf; 2], ctx: &egui::Context) -> PlacesMenu {
        let updates = Some(look_up(&places, ctx));
        let mut m = PlacesMenu { pane, places, selected: 0, updates, here };
        m.select_current();
        m
    }

    /// The place the pane is in (the deepest one that holds its folder).
    fn current(&self, pane: usize) -> Option<usize> {
        let here = &self.here[pane];
        self.places
            .iter()
            .enumerate()
            .filter(|(_, p)| p.path.as_ref().is_some_and(|d| here.starts_with(d)))
            .max_by_key(|(_, p)| (p.path.as_ref().map_or(0, |d| d.components().count()), p.section == Section::Bookmarks))
            .map(|(i, _)| i)
    }

    fn select_current(&mut self) {
        self.selected = self.current(self.pane).unwrap_or(0);
    }

    fn poll(&mut self) {
        let Some(rx) = &self.updates else { return };
        while let Ok(u) = rx.try_recv() {
            match u {
                Update::Space(path, s) => {
                    for p in self.places.iter_mut().filter(|p| p.path.as_ref() == Some(&path)) {
                        p.space = Some(s);
                    }
                }
                Update::Devices(d) => {
                    for p in &mut self.places {
                        let Some(dev) = p.device.clone() else { continue };
                        if let Some(l) = d.labels.get(&dev) {
                            p.name = l.clone();
                        }
                        if d.removable.contains(&dev) {
                            p.can_unmount = true;
                        }
                    }
                    let keep = self.places.get(self.selected).cloned();
                    self.places.extend(d.unmounted);
                    if let Some(k) = keep {
                        self.selected = self.places.iter().position(|p| *p == k).unwrap_or(self.selected);
                    }
                }
            }
        }
    }

    fn pick(&self) -> Option<PlacesOutcome> {
        let p = self.places.get(self.selected)?;
        Some(match (&p.path, &p.sftp) {
            (Some(path), _) => PlacesOutcome::Open(self.pane, path.clone()),
            (None, Some(t)) if t.is_empty() => PlacesOutcome::AskSftp(self.pane),
            (None, Some(t)) => PlacesOutcome::Sftp(self.pane, t.clone()),
            (None, None) => PlacesOutcome::Mount(self.pane, p.clone()),
        })
    }

    /// Typing a letter moves to the next place whose name starts with it.
    fn jump_letter(&mut self, c: char) {
        let n = self.places.len();
        let c = c.to_lowercase().next().unwrap_or(c);
        if let Some(i) = (1..=n).map(|k| (self.selected + k) % n).find(|&i| self.places[i].name.to_lowercase().starts_with(c)) {
            self.selected = i;
        }
    }

    /// Draws the menu with its top-left corner at `at[pane]`; returns what to do
    /// once the user picks something.
    pub fn ui(&mut self, ctx: &egui::Context, at: [Pos2; 2]) -> Option<PlacesOutcome> {
        self.poll();
        let n = self.places.len();
        let (esc, enter, up, down, home, end, left, right, del, typed) = ctx.input_mut(|i| {
            let mut k = |key| i.consume_key(Modifiers::NONE, key);
            let keys = (
                k(Key::Escape),
                k(Key::Enter),
                k(Key::ArrowUp),
                k(Key::ArrowDown),
                k(Key::Home) || k(Key::PageUp),
                k(Key::End) || k(Key::PageDown),
                k(Key::ArrowLeft),
                k(Key::ArrowRight),
                k(Key::Delete),
            );
            let typed = i.events.iter().find_map(|e| match e {
                egui::Event::Text(t) => t.chars().next(),
                _ => None,
            });
            (keys.0, keys.1, keys.2, keys.3, keys.4, keys.5, keys.6, keys.7, keys.8, typed)
        });
        if esc {
            return Some(PlacesOutcome::Close);
        }
        if enter {
            return self.pick();
        }
        if n > 0 {
            if down {
                self.selected = (self.selected + 1) % n;
            }
            if up {
                self.selected = (self.selected + n - 1) % n;
            }
        }
        if home {
            self.selected = 0;
        }
        if end {
            self.selected = n.saturating_sub(1);
        }
        // Left / Right move the menu to the other pane, as Alt+F1 / Alt+F2 would.
        if (left && self.pane == 1) || (right && self.pane == 0) {
            self.pane = 1 - self.pane;
            self.select_current();
        }
        if let Some(c) = typed.filter(|c| c.is_alphanumeric()) {
            self.jump_letter(c);
        }
        if del {
            if let Some(p) = self.places.get(self.selected).filter(|p| p.can_unmount) {
                return Some(match (&p.sftp, &p.again) {
                    (Some(t), _) => PlacesOutcome::Forget(t.clone()),
                    (_, Some(Again::Command { point, .. })) => PlacesOutcome::ForgetMount(point.clone()),
                    _ => PlacesOutcome::Unmount(p.clone()),
                });
            }
        }

        let mut outcome = None;
        let th = theme::c();
        let area = egui::Area::new(egui::Id::new("places_menu")).order(egui::Order::Foreground).fixed_pos(at[self.pane]);
        let r = area.show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(460.0);
                let font = FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
                let small = egui::TextStyle::Small.resolve(ui.style());
                let row_h = theme::row_height(font.size) + small.size + 2.0;
                ui.spacing_mut().item_spacing.y = 0.0;
                let here = [self.current(0), self.current(1)];
                let mut last = None;
                for i in 0..n {
                    let p = &self.places[i];
                    if last != Some(p.section) {
                        last = Some(p.section);
                        // A line between sections, then the section's name.
                        if i > 0 {
                            ui.add_space(6.0);
                            let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                            ui.painter().hline(line.x_range(), line.center().y, egui::Stroke::new(1.0_f32, th.dim.gamma_multiply(0.5)));
                            ui.add_space(6.0);
                        } else {
                            ui.add_space(2.0);
                        }
                        ui.label(egui::RichText::new(p.section.title().to_uppercase()).small().strong().color(th.accent));
                        ui.add_space(3.0);
                    }
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    let paint = ui.painter_at(rect);
                    if i == self.selected {
                        paint.rect_filled(rect, 3.0, th.cursor);
                    } else if resp.hovered() {
                        paint.rect_filled(rect, 3.0, th.text.gamma_multiply(0.05));
                    }
                    let top = rect.top() + row_h * 0.08;
                    let color = if p.path.is_some() || p.sftp.is_some() { th.folder } else { th.dim };
                    let icon = match p.section {
                        Section::Places | Section::Bookmarks => "🗀",
                        Section::Network => "🖧",
                        _ if p.fuse => "🖧",
                        _ => "🖴",
                    };
                    paint.text(pos2(rect.left() + 8.0, top), Align2::LEFT_TOP, icon, font.clone(), color);
                    let name_r = paint.text(pos2(rect.left() + 32.0, top), Align2::LEFT_TOP, &p.name, font.clone(), color);
                    // Which pane is in it now.
                    let mark = match (here[0] == Some(i), here[1] == Some(i)) {
                        (true, true) => "◂ both",
                        (true, false) => "◂ left",
                        (false, true) => "◂ right",
                        _ => "",
                    };
                    paint.text(pos2(name_r.right() + 10.0, name_r.center().y), Align2::LEFT_CENTER, mark, small.clone(), th.selected);
                    if let Some((free, total)) = p.space {
                        let t = format!("{} free of {}", human_size(free), human_size(total));
                        paint.text(pos2(rect.right() - 8.0, name_r.center().y), Align2::RIGHT_CENTER, t, small.clone(), th.dim);
                    }
                    let detail = egui::Rect::from_min_max(pos2(rect.left() + 32.0, name_r.bottom()), pos2(rect.right() - 8.0, rect.bottom()));
                    paint.with_clip_rect(detail).text(detail.left_top(), Align2::LEFT_TOP, &p.detail, small.clone(), th.dim);
                    if resp.clicked() {
                        self.selected = i;
                        outcome = self.pick();
                    }
                }
                ui.add_space(8.0);
                let unmount = match self.places.get(self.selected) {
                    Some(p) if p.can_unmount && (p.sftp.is_some() || p.again.is_some()) => "   Del forgets it",
                    Some(p) if p.can_unmount => "   Del unmounts",
                    _ => "",
                };
                ui.label(egui::RichText::new(format!("Enter opens   ←→ other pane   letters jump{unmount}   Esc closes")).small().color(th.dim));
            });
        });
        // A click outside closes the menu.
        if outcome.is_none() && r.response.clicked_elsewhere() {
            outcome = Some(PlacesOutcome::Close);
        }
        outcome
    }
}

/// Why mounting didn't work.
#[derive(Debug, PartialEq)]
pub enum MountError {
    /// The server wants a password (no ssh key let us in).
    NeedsPassword,
    Failed(String),
}

impl From<String> for MountError {
    fn from(s: String) -> Self {
        MountError::Failed(s)
    }
}

impl From<&str> for MountError {
    fn from(s: &str) -> Self {
        MountError::Failed(s.into())
    }
}

/// The answer from mounting (the folder it was mounted at) or unmounting.
pub type MountAnswer = mpsc::Receiver<Result<Done, MountError>>;

/// Runs `udisksctl` (or `fusermount`) in the background. The answer is the
/// folder a drive was mounted at, or a message for the status bar.
pub fn mount(place: &Place) -> MountAnswer {
    let (tx, rx) = mpsc::channel();
    let p = place.clone();
    std::thread::spawn(move || {
        let _ = tx.send(run_mount(&p));
    });
    rx
}

pub fn unmount(place: &Place) -> MountAnswer {
    let (tx, rx) = mpsc::channel();
    let p = place.clone();
    std::thread::spawn(move || {
        let _ = tx.send(run_unmount(&p));
    });
    rx
}

fn output(cmd: &str, args: &[&str]) -> Result<String, String> {
    match std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::null()).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            // udisksctl puts the useful part after the D-Bus error name.
            let msg = err.lines().last().unwrap_or("").rsplit(": ").next().unwrap_or("").trim().to_string();
            Err(if msg.is_empty() { format!("{cmd} failed") } else { msg })
        }
        Err(_) => Err(format!("{cmd} isn't installed")),
    }
}

fn run_mount(p: &Place) -> Result<Done, MountError> {
    if let Some(again) = &p.again {
        return mount_again(again);
    }
    let dev = p.device.as_deref().ok_or("Nothing to mount")?;
    let out = output("udisksctl", &["mount", "-b", dev])?;
    // "Mounted /dev/sdb1 at /run/media/me/STICK" (older versions end with a full stop).
    if let Some(at) = out.split(" at ").nth(1) {
        return Ok(Done::Mounted(PathBuf::from(at.trim().trim_end_matches('.'))));
    }
    // Worded differently: find it in the mount table instead.
    std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|t| parse_mountinfo(&t).into_iter().rev().find(|m| m.source == dev).map(|m| Done::Mounted(m.point)))
        .ok_or_else(|| "it was mounted, but Duopane couldn't tell where".into())
}

fn run_unmount(p: &Place) -> Result<Done, MountError> {
    if !p.fuse {
        let dev = p.device.as_deref().ok_or("Nothing to unmount")?;
        output("udisksctl", &["unmount", "-b", dev])?;
        return Ok(Done::Unmounted(None));
    }
    let dir = p.path.as_ref().ok_or("Nothing to unmount")?;
    // A systemd service's mount is stopped through systemd, which then knows it's down.
    if let Some(unit) = unit_for(dir) {
        output("systemctl", &["--user", "stop", &unit])?;
        return Ok(Done::Unmounted(None));
    }
    // Our own SFTP mounts come back from the Network list; others are remembered.
    let ours = dir.starts_with(mounts_dir());
    let command = if ours { None } else { fuse_command(dir) };
    let point = dir.to_string_lossy().into_owned();
    output("fusermount3", &["-u", &point])
        .or_else(|_| output("fusermount", &["-u", &point]))
        // A connection that stopped answering keeps the mount busy; let go of it anyway.
        .or_else(|_| output("fusermount3", &["-uz", &point]))
        .or_else(|_| output("fusermount", &["-uz", &point]))?;
    if ours {
        // The mount folder goes too (only if it's empty).
        let _ = std::fs::remove_dir(dir);
    }
    Ok(Done::Unmounted(command.map(|command| Remount { name: p.name.clone(), point: dir.clone(), command })))
}

/// Mounts a FUSE mount again and waits (up to 20 s) for it to appear.
fn mount_again(again: &Again) -> Result<Done, MountError> {
    let point = match again {
        Again::Unit(unit) => {
            // A slow start can time out while systemd's own retry still brings it
            // up, so a failed start only counts if the mount doesn't appear soon after.
            let started = output("systemctl", &["--user", "start", unit]);
            let point = unit_for_name(unit, if started.is_ok() { 50 } else { 300 });
            if started.is_err() && !point.as_deref().is_some_and(is_mounted) {
                return Err(format!("{unit} didn't start; \"journalctl --user -u {unit}\" says why").into());
            }
            point
        }
        Again::Command { args, point } => {
            use std::os::unix::process::CommandExt;
            let (program, rest) = args.split_first().ok_or("Nothing to run")?;
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
            // Its own process group, so it outlives Duopane (and its terminal).
            let mut child = std::process::Command::new(program)
                .args(rest)
                .current_dir(home)
                .process_group(0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("couldn't run {program}: {e}"))?;
            let point = Some(point.clone());
            let started = std::time::Instant::now();
            loop {
                if point.as_ref().is_some_and(|p| is_mounted(p)) {
                    break;
                }
                // Programs that go to the background exit 0 once mounted; a
                // failure exits early with a reason.
                if let Ok(Some(status)) = child.try_wait() {
                    if !status.success() {
                        let mut err = String::new();
                        let _ = std::io::Read::read_to_string(&mut child.stderr.take().ok_or("")?, &mut err);
                        return Err(err.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("it stopped with an error").to_string().into());
                    }
                }
                if started.elapsed() > std::time::Duration::from_secs(20) {
                    return Err("it didn't come up within 20 seconds".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            // A program that stays in the foreground keeps running; reap it when it ends.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            point
        }
    };
    point.map(Done::Mounted).ok_or_else(|| "it was mounted, but Duopane couldn't tell where".into())
}

/// The mount point of a service, once it's mounted (waiting up to `tries` × 200 ms).
fn unit_for_name(unit: &str, tries: u32) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let point = mount_units(&dirs::config_dir()?.join("systemd/user"), &home).into_iter().find(|(u, _, _)| u == unit).map(|(_, p, _)| p)?;
    // A notify service is up once `start` returns; others may need a moment.
    for _ in 0..tries {
        if is_mounted(&point) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Some(point)
}

fn is_mounted(p: &Path) -> bool {
    std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|t| parse_mountinfo(&t).iter().any(|m| m.point == p))
}

/// Connects to an SFTP server with sshfs, in the background. Without a password
/// only ssh keys (and the ssh agent) are tried, so nothing waits on a prompt.
pub fn sftp_mount(target: &str, password: Option<String>) -> MountAnswer {
    let (tx, rx) = mpsc::channel();
    let target = target.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(run_sftp(&target, password));
    });
    rx
}

fn run_sftp(text: &str, password: Option<String>) -> Result<Done, MountError> {
    use std::io::Write;
    let t = Target::parse(text)?;
    let dir = t.mount_dir();
    // Already connected: just open it.
    if let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") {
        if parse_mountinfo(&info).iter().any(|m| m.point == dir) {
            return Ok(Done::Mounted(dir));
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't make {}: {e}", tilde(&dir)))?;
    // New servers are remembered on first use, as ssh asks you to; a server whose
    // key has changed since is refused.
    let mut opts = "reconnect,ServerAliveInterval=15,ServerAliveCountMax=3,ConnectTimeout=10,StrictHostKeyChecking=accept-new".to_string();
    opts.push_str(if password.is_some() { ",password_stdin,NumberOfPasswordPrompts=1" } else { ",BatchMode=yes" });
    let mut cmd = std::process::Command::new("sshfs");
    cmd.arg(&t.remote).arg(&dir).args(["-o", &opts]);
    if let Some(p) = t.port {
        cmd.args(["-p", &p.to_string()]);
    }
    cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            let _ = std::fs::remove_dir(&dir);
            return Err("sshfs isn't installed (it's in most distributions' packages)".into());
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(pw) = &password {
            let _ = writeln!(stdin, "{pw}");
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(Done::Mounted(dir));
    }
    let _ = std::fs::remove_dir(&dir);
    // sshfs only says "Connection reset by peer" whatever went wrong; ssh itself
    // tells why (keys only, so it can't stop at a prompt).
    let host = t.remote.split_once(':').map_or(t.remote.as_str(), |(h, _)| h);
    let mut ssh = std::process::Command::new("ssh");
    ssh.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=accept-new"]);
    if let Some(p) = t.port {
        ssh.args(["-p", &p.to_string()]);
    }
    let why = ssh.args(["--", host, "exit"]).stdin(std::process::Stdio::null()).output();
    let stderr = match why {
        // ssh got in with a key, so it's sshfs or the folder that failed.
        Ok(o) if o.status.success() => String::from_utf8_lossy(&out.stderr).into_owned(),
        Ok(o) => String::from_utf8_lossy(&o.stderr).into_owned(),
        Err(_) => String::from_utf8_lossy(&out.stderr).into_owned(),
    };
    Err(sftp_error(&stderr, password.is_some()))
}

/// Turns ssh's complaints into something friendlier.
fn sftp_error(stderr: &str, had_password: bool) -> MountError {
    let has = |s: &str| stderr.contains(s);
    if has("Permission denied") || has("Too many authentication failures") {
        return if had_password { MountError::Failed("the password wasn't accepted".into()) } else { MountError::NeedsPassword };
    }
    MountError::Failed(if has("REMOTE HOST IDENTIFICATION HAS CHANGED") || has("Host key verification failed") {
        "the server's identity has changed since last time; check it with ssh in a terminal".into()
    } else if has("Could not resolve hostname") {
        "couldn't find that server".into()
    } else if has("Connection refused") {
        "the server refused the connection (is ssh running there?)".into()
    } else if has("timed out") || has("No route to host") {
        "couldn't reach the server".into()
    } else if has("No such file") {
        "that folder isn't on the server".into()
    } else {
        stderr.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("sshfs failed").to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
26 1 0:24 /@ / rw,noatime shared:1 - btrfs /dev/nvme0n1p2 rw,subvol=/@
27 26 0:25 /@home /home rw,noatime shared:2 - btrfs /dev/nvme0n1p2 rw,subvol=/@home
28 26 0:26 /@log /var/log rw,noatime shared:3 - btrfs /dev/nvme0n1p2 rw
220 26 259:1 / /boot rw,relatime shared:183 - vfat /dev/nvme0n1p1 rw
2 26 8:2 / /mnt/BIG_BERTHA rw,nosuid shared:648 - fuseblk /dev/sda2 rw,user_id=0,group_id=0
1758 27 0:191 / /home/me/gdrive rw,nosuid,nodev shared:1543 - fuse.rclone gdrive: rw,user_id=1000,group_id=1000
1800 26 0:192 / /run/user/1000/doc rw - fuse.portal portal rw,user_id=1000
1801 26 0:193 / /run/docker/netns/abc rw - nsfs nsfs rw
1802 26 8:17 / /run/media/me/My\\040Stick rw,nosuid - exfat /dev/sdb1 rw
1803 26 0:30 / /proc rw - proc proc rw
1804 26 7:0 / /snap/core/1 ro - squashfs /dev/loop0 ro
";

    #[test]
    fn mounts_without_the_noise() {
        let d = drives(&parse_mountinfo(MOUNTINFO), Path::new("/home/me"), Some(1000));
        let points: Vec<_> = d.iter().map(|p| p.path.clone().unwrap()).collect();
        assert_eq!(points, [PathBuf::from("/mnt/BIG_BERTHA"), "/home/me/gdrive".into(), "/run/media/me/My Stick".into()]);
        assert_eq!(d[0].device.as_deref(), Some("/dev/sda2"));
        assert!(!d[0].can_unmount, "mounted by the system");
        assert!(d[1].fuse && d[1].can_unmount, "the user's own rclone mount");
        assert!(d[1].detail.contains("rclone (gdrive)"));
        assert!(d[2].can_unmount, "a USB stick");
        assert_eq!(d[2].name, "My Stick");
    }

    #[test]
    fn lsblk_finds_unmounted_drives() {
        let json = r#"{"blockdevices":[
            {"path":"/dev/sda","label":null,"size":18000207937536,"fstype":null,"mountpoints":[],"hotplug":false,"rm":false,"type":"disk","model":"WDC WUH721818","vendor":"ATA     "},
            {"path":"/dev/sda2","label":"BIG BERTHA","size":18000190111744,"fstype":"ntfs","mountpoints":["/mnt/BIG_BERTHA"],"hotplug":false,"rm":false,"type":"part","model":null,"vendor":null},
            {"path":"/dev/zram0","label":"zram0","size":33518780416,"fstype":"swap","mountpoints":["[SWAP]"],"hotplug":false,"rm":false,"type":"disk","model":null,"vendor":null},
            {"path":"/dev/sdb","label":null,"size":32000000000,"fstype":null,"mountpoints":[null],"hotplug":true,"rm":true,"type":"disk","model":"Cruzer","vendor":"SanDisk"},
            {"path":"/dev/sdb1","label":"STICK","size":32000000000,"fstype":"exfat","mountpoints":[null],"hotplug":true,"rm":true,"type":"part","model":null,"vendor":null},
            {"path":"/dev/nvme0n1p5","label":"Recovery","size":700000000,"fstype":"ntfs","mountpoints":[],"hotplug":false,"rm":false,"type":"part","model":null,"vendor":null}
        ]}"#;
        let d = parse_lsblk(json);
        assert_eq!(d.labels.get("/dev/sda2").map(String::as_str), Some("BIG BERTHA"));
        assert_eq!(d.unmounted.len(), 1, "only the stick: the rest is mounted, swap or a small system partition");
        assert_eq!(d.unmounted[0].name, "STICK");
        assert_eq!(d.unmounted[0].device.as_deref(), Some("/dev/sdb1"));
        assert!(d.unmounted[0].detail.starts_with("SanDisk Cruzer"));
        assert!(d.removable.contains(&"/dev/sdb1".to_string()));
    }

    /// One frame of the menu with these keys (one per frame, as egui takes them).
    fn press(ctx: &egui::Context, m: &mut PlacesMenu, key: Option<Key>, text: Option<&str>) -> Option<PlacesOutcome> {
        let mut events = vec![];
        if let Some(key) = key {
            events.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        }
        if let Some(t) = text {
            events.push(egui::Event::Text(t.into()));
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
            events,
            ..Default::default()
        };
        let mut out = None;
        let _ = ctx.run(input, |ctx| out = m.ui(ctx, [pos2(10.0, 40.0), pos2(610.0, 40.0)]));
        out
    }

    #[test]
    fn keyboard_picks_a_place() {
        let ctx = egui::Context::default();
        let places = vec![
            Place::folder(Section::Places, "Home", "/home/me".into()),
            Place::folder(Section::Places, "Root", "/".into()),
            Place { device: Some("/dev/sdb1".into()), path: None, ..Place::folder(Section::NotMounted, "STICK", "/".into()) },
            Place { can_unmount: true, ..Place::folder(Section::Drives, "gdrive", "/home/me/gdrive".into()) },
        ];
        // No lookups in the test: just the given places.
        let mut m = PlacesMenu { pane: 0, places, selected: 0, updates: None, here: ["/home/me/Music".into(), "/etc".into()] };
        m.select_current();
        assert_eq!(m.selected, 0, "starts on where the pane is");
        // Right moves to the right pane's menu, which starts on Root (it's in /etc).
        press(&ctx, &mut m, Some(Key::ArrowRight), None);
        assert_eq!((m.pane, m.selected), (1, 1));
        // A letter jumps; Enter on a drive that isn't mounted asks to mount it.
        press(&ctx, &mut m, None, Some("s"));
        assert!(matches!(press(&ctx, &mut m, Some(Key::Enter), None), Some(PlacesOutcome::Mount(1, p)) if p.name == "STICK"));
        // Delete only unmounts what can be.
        m.selected = 0;
        assert!(press(&ctx, &mut m, Some(Key::Delete), None).is_none());
        m.selected = 3;
        assert!(matches!(press(&ctx, &mut m, Some(Key::Delete), None), Some(PlacesOutcome::Unmount(p)) if p.name == "gdrive"));
        m.selected = 1;
        assert!(matches!(press(&ctx, &mut m, Some(Key::Enter), None), Some(PlacesOutcome::Open(1, p)) if p == Path::new("/")));
        assert!(matches!(press(&ctx, &mut m, Some(Key::Escape), None), Some(PlacesOutcome::Close)));
    }

    #[test]
    fn sftp_targets() {
        let t = Target::parse("per@nas:/srv/photos/").unwrap();
        assert_eq!((t.remote.as_str(), t.port, t.label.as_str()), ("per@nas:/srv/photos", None, "per@nas_srv_photos"));
        let t = Target::parse("nas").unwrap();
        assert_eq!((t.remote.as_str(), t.label.as_str()), ("nas:", "nas"));
        let t = Target::parse("nas:/").unwrap();
        assert_eq!(t.remote, "nas:/");
        let t = Target::parse("sftp://me@example.org:2222/home/me").unwrap();
        assert_eq!((t.remote.as_str(), t.port, t.label.as_str()), ("me@example.org:/home/me", Some(2222), "me@example.org_home_me_2222"));
        assert!(Target::parse("").is_err());
        assert!(Target::parse("-oProxyCommand=x:/").is_err(), "never something sshfs reads as an option");
        assert!(Target::parse("sftp://host:abc/").is_err());
    }

    #[test]
    fn ssh_config_hosts_without_patterns() {
        let cfg = "Host *\n  ServerAliveInterval 30\nHost nas pi\n  User per\nhost=router\nHost !bad *.lan box\n";
        assert_eq!(parse_ssh_hosts(cfg), ["nas", "pi", "router", "box"]);
    }

    #[test]
    fn network_section() {
        let mounted = Place::folder(Section::Drives, "nas", Target::parse("nas").unwrap().mount_dir());
        let n = network(&[mounted], &["per@pi:/data".to_string()], &["nas".to_string(), "box".to_string()]);
        let names: Vec<_> = n.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["per@pi_data", "box", "Connect to SFTP…"], "nas is already mounted");
        assert!(n[0].can_unmount && !n[1].can_unmount, "only recent servers can be forgotten");
    }

    #[test]
    fn ssh_errors_explained() {
        assert_eq!(sftp_error("per@nas: Permission denied (publickey,password).", false), MountError::NeedsPassword);
        assert_eq!(sftp_error("Permission denied, please try again.", true), MountError::Failed("the password wasn't accepted".into()));
        assert!(matches!(sftp_error("ssh: Could not resolve hostname nope: Name or service not known\nread: Connection reset by peer", false), MountError::Failed(m) if m.contains("find")));
    }

    /// A "sshfs" that another user could have put there, or that isn't the program
    /// it claims to be, is never remembered as the command to mount again.
    #[cfg(target_os = "linux")]
    #[test]
    fn mount_commands_only_come_from_safe_programs() {
        use std::os::unix::fs::PermissionsExt;
        let Ok(tail) = which("tail") else { return };
        let base = std::env::temp_dir().join(format!("duopane-fusecmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("sshfs");
        std::fs::copy(&tail, &fake).unwrap();
        let point = base.join("point");
        std::fs::write(&point, "").unwrap();
        let run = || {
            let mut c = std::process::Command::new(&fake).args(["-f"]).arg(&point).stdout(std::process::Stdio::null()).spawn().unwrap();
            // Wait until it really is running as the fake (a busy machine can take a while).
            let exe = PathBuf::from(format!("/proc/{}/exe", c.id()));
            let t = std::time::Instant::now();
            while std::fs::read_link(&exe).ok().as_ref() != Some(&fake) && t.elapsed() < std::time::Duration::from_secs(10) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let found = fuse_command(&point);
            let _ = c.kill();
            let _ = c.wait();
            found
        };
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(run(), None, "a folder anyone can write to");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(run(), Some(vec![fake.to_string_lossy().into_owned(), "-f".into(), point.to_string_lossy().into_owned()]));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn which(name: &str) -> Result<PathBuf, ()> {
        std::env::split_paths(&std::env::var_os("PATH").ok_or(())?).map(|d| d.join(name)).find(|p| p.is_file()).ok_or(())
    }

    #[test]
    fn systemd_mount_services() {
        let unit = "[Service]\nType=notify\nExecStart=rclone mount gdrive: %h/gdrive \\\n  --vfs-cache-mode writes \\\n  --log-level INFO\nExecStop=/bin/fusermount -uz %h/gdrive\n";
        assert_eq!(unit_mount(unit, Path::new("/home/me")), Some((PathBuf::from("/home/me/gdrive"), "rclone (gdrive:)".into())));
        let unit = "ExecStart=/usr/bin/sshfs -f -o reconnect me@nas:/srv /home/me/nas\n";
        assert_eq!(unit_mount(unit, Path::new("/home/me")), Some((PathBuf::from("/home/me/nas"), "sshfs (me@nas:/srv)".into())));
        assert_eq!(unit_mount("ExecStart=/usr/bin/syncthing serve\n", Path::new("/home/me")), None);
    }

    #[test]
    fn mounts_that_can_come_back() {
        let units = [("rclone-gdrive.service".to_string(), PathBuf::from("/home/me/gdrive"), "rclone (gdrive:)".to_string())];
        let remounts = [
            Remount { name: "photos".into(), point: "/home/me/photos".into(), command: vec!["rclone".into(), "mount".into(), "b2:photos".into(), "/home/me/photos".into(), "--daemon".into()] },
            Remount { name: "gdrive".into(), point: "/home/me/gdrive".into(), command: vec!["rclone".into()] },
        ];
        let d = down_again(&[], &units, &remounts);
        assert_eq!(d.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["gdrive", "photos"], "the service wins over a remembered command");
        assert_eq!(d[0].again, Some(Again::Unit("rclone-gdrive.service".into())));
        assert!(!d[0].can_unmount && d[1].can_unmount, "only remembered ones can be forgotten");
        let mounted = Place::folder(Section::Drives, "gdrive", "/home/me/gdrive".into());
        assert_eq!(down_again(&[mounted], &units, &[]).len(), 0, "it's up: under Drives instead");
    }

    #[test]
    fn octal_escapes() {
        assert_eq!(unescape(r"/run/media/me/My\040Stick"), "/run/media/me/My Stick");
        assert_eq!(unescape(r"a\134b"), r"a\b");
        assert_eq!(unescape(r"end\04"), r"end\04");
    }
}
