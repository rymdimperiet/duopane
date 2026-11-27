//! Telling the user when a newer Duopane is out.
//!
//! At most once a day, a background thread asks GitHub for the latest release
//! (with `curl`, so no HTTP library is needed). Nothing is sent but the request
//! itself, and nothing is downloaded or installed: a newer version shows up as a
//! button in the key bar and a line in the About box. `check_for_updates` in the
//! settings file turns it off.

use std::sync::mpsc;
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/rymdimperiet/duopane/releases/latest";
pub const RELEASES: &str = "https://github.com/rymdimperiet/duopane/releases/latest";

/// How long between checks.
pub const EVERY: Duration = Duration::from_secs(20 * 60 * 60);

/// A release newer than the running version.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

/// Asks GitHub in the background; the answer is None when there's nothing newer
/// (or the check failed: no network is no reason to bother anyone).
pub fn check(ctx: eframe::egui::Context) -> mpsc::Receiver<Option<Release>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let found = latest().filter(|r| is_newer(&r.version, env!("CARGO_PKG_VERSION")));
        let _ = tx.send(found);
        ctx.request_repaint();
    });
    rx
}

fn latest() -> Option<Release> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "15", "-H", "Accept: application/vnd.github+json", LATEST])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse(&String::from_utf8_lossy(&out.stdout))
}

fn parse(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let version = v["tag_name"].as_str()?.trim_start_matches('v').to_string();
    // Only ever a page of this project's releases: the link is opened as it is.
    let url = v["html_url"].as_str().filter(|u| u.starts_with("https://github.com/rymdimperiet/duopane/releases/")).unwrap_or(RELEASES).to_string();
    Some(Release { version, url })
}

/// Whether `version` comes after `current` ("0.10.0" after "0.9.2").
pub fn is_newer(version: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(version), parts(current));
    let n = a.len().max(b.len());
    let pad = |v: &[u64]| (0..n).map(|i| v.get(i).copied().unwrap_or(0)).collect::<Vec<_>>();
    pad(&a) > pad(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("0.4.1", "0.4.0"));
        assert!(is_newer("v0.10.0", "0.9.2"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        assert!(!is_newer("0.4.0", "0.4"));
    }

    #[test]
    fn release_answer() {
        let ours = "https://github.com/rymdimperiet/duopane/releases/tag/v0.5.0";
        let r = parse(&format!(r#"{{"tag_name":"v0.5.0","html_url":"{ours}","draft":false,"prerelease":false}}"#)).unwrap();
        assert_eq!(r, Release { version: "0.5.0".into(), url: ours.into() });
        // A link anywhere else is never opened; the releases page is instead.
        let r = parse(r#"{"tag_name":"v0.5.0","html_url":"file:///etc/passwd","draft":false,"prerelease":false}"#).unwrap();
        assert_eq!(r.url, RELEASES);
        assert_eq!(parse(r#"{"tag_name":"v0.6.0-rc1","prerelease":true}"#), None);
        assert_eq!(parse("not json"), None);
    }
}
