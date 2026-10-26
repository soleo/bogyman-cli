//! `bogyman version`: which build is this? Also backs `--version`.

use anyhow::Result;

use crate::{Ctx, EXIT_OK};

pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_COMMIT: &str = env!("BOGYMAN_GIT_COMMIT");
pub const GIT_DIRTY: &str = env!("BOGYMAN_GIT_DIRTY");
pub const GIT_DATE: &str = env!("BOGYMAN_GIT_DATE");
pub const BUILT_AT: &str = env!("BOGYMAN_BUILT_AT");
pub const RUSTC: &str = env!("BOGYMAN_RUSTC");
pub const TARGET: &str = env!("BOGYMAN_TARGET");
pub const PROFILE: &str = env!("BOGYMAN_PROFILE");

/// One line, used by `--version` and as the first line of `bogyman version`.
pub const LINE: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("BOGYMAN_GIT_COMMIT"),
    ", ",
    env!("BOGYMAN_BUILT_AT"),
    ", ",
    env!("BOGYMAN_TARGET"),
    ")"
);

pub fn dirty() -> bool {
    GIT_DIRTY == "true"
}

pub fn run(ctx: &Ctx) -> Result<u8> {
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": bogyman_core::SCHEMA,
                "version": PKG_VERSION,
                "commit": GIT_COMMIT,
                "dirty": dirty(),
                "commit_date": GIT_DATE,
                "built_at": BUILT_AT,
                "target": TARGET,
                "profile": PROFILE,
                "rustc": RUSTC,
                "binary": std::env::current_exe().ok(),
            }))?
        );
        return Ok(EXIT_OK);
    }
    println!("bogyman {LINE}");
    println!(
        "commit     {}{}{}",
        GIT_COMMIT,
        if dirty() { " (dirty)" } else { "" },
        if GIT_DATE.is_empty() {
            String::new()
        } else {
            format!(" from {GIT_DATE}")
        }
    );
    println!("built      {BUILT_AT} · {PROFILE} · {RUSTC}");
    println!("target     {TARGET}");
    if let Ok(p) = std::env::current_exe() {
        println!("binary     {}", crate::paths::tilde(&p));
    }
    Ok(EXIT_OK)
}
