//! Records build details for the About box (Shift+F1): the git commit, the date,
//! release or debug, the platform and the Rust version. Anything that can't be
//! found (no git, say) shows as "unknown" rather than failing the build.

use std::process::Command;

fn main() {
    let run = |cmd: &str, args: &[&str]| -> Option<String> {
        let out = Command::new(cmd).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
    };

    // The commit, marked when built from changes that aren't committed.
    let commit = run("git", &["rev-parse", "--short", "HEAD"]).map(|c| {
        let dirty = Command::new("git").args(["diff", "--quiet", "HEAD"]).status().is_ok_and(|s| !s.success());
        if dirty { format!("{c} (modified)") } else { c }
    });
    set("DUOPANE_COMMIT", commit.as_deref().unwrap_or("unknown"));

    // The date; SOURCE_DATE_EPOCH (if set) keeps reproducible builds reproducible.
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
    set("DUOPANE_BUILD_DATE", &date(secs));

    set("DUOPANE_PROFILE", &std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into()));
    set("DUOPANE_TARGET", &std::env::var("TARGET").unwrap_or_else(|_| "unknown".into()));
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    set("DUOPANE_RUSTC", run(&rustc, &["--version"]).as_deref().unwrap_or("unknown"));

    // Rebuild the details when the commit (or what's staged) changes.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}

fn set(key: &str, value: &str) {
    println!("cargo:rustc-env={key}={value}");
}

/// Seconds since 1970 -> "2026-11-19" (UTC), without a date library.
fn date(secs: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y}-{m:02}-{d:02}")
}
