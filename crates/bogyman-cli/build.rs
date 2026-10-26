//! Capture build provenance so `bogyman version` can say which build is installed.
//! Everything degrades to "unknown" outside a git checkout (for example a
//! `cargo install` from a crates.io tarball).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    let commit = git(&["rev-parse", "--short=9", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
    let commit_date = git(&["log", "-1", "--format=%cd", "--date=short"]).unwrap_or_default();
    let built_at = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
        .map(|secs| {
            // Civil date from a unix timestamp; enough for a build stamp.
            let days = secs / 86_400;
            let (y, m, d) = civil_from_days(days as i64);
            format!("{y:04}-{m:02}-{d:02}")
        })
        .unwrap_or_default();
    let rustc = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());

    println!("cargo:rustc-env=BOGYMAN_GIT_COMMIT={commit}");
    println!(
        "cargo:rustc-env=BOGYMAN_GIT_DIRTY={}",
        if dirty { "true" } else { "false" }
    );
    println!("cargo:rustc-env=BOGYMAN_GIT_DATE={commit_date}");
    println!("cargo:rustc-env=BOGYMAN_BUILT_AT={built_at}");
    println!("cargo:rustc-env=BOGYMAN_RUSTC={rustc}");
    println!("cargo:rustc-env=BOGYMAN_TARGET={target}");
    println!("cargo:rustc-env=BOGYMAN_PROFILE={profile}");
    for p in [
        "../../.git/HEAD",
        "../../.git/refs/heads",
        "../../.git/index",
    ] {
        println!("cargo:rerun-if-changed={p}");
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}

// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
