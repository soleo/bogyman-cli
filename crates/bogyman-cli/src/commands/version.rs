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

/// The ghost from the logo, rasterized from `bogyman.svg` into braille cells
/// (2x4 pixels each) by `scripts/logo-art.py`. Regenerate rather than edit.
pub const ART: [&str; 14] = [
    "⠀⠀⠀⠀⠀⠀⠀⠀⣠⣄⣤⡶⣿⡯⣶⣶⠆⠀⠀⠀⠀⠀⠀⠀",
    "⠀⠀⠀⠀⠀⠀⣠⡾⠛⠋⠁⡀⠀⠀⠉⠉⢿⣷⡤⠀⠀⠀⠀⠀",
    "⠀⠀⠀⢀⣀⠛⡛⢀⣐⣂⣀⡀⠂⠀⠀⢀⠀⠙⢷⡀⠀⠀⠀⠀",
    "⠀⠀⠀⣸⡷⠈⠁⢿⣍⣉⣉⣻⡿⢢⡶⠶⢶⣶⣾⣿⠦⠀⠀⠀",
    "⠀⠀⣼⣿⠁⠀⠀⠀⠈⠉⠉⠁⠀⠈⠛⠲⠶⠚⠋⢻⡇⠀⠀⠀",
    "⠀⠀⣾⠷⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⡇⠀⠀⠀",
    "⠀⣼⡏⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢹⣧⠀⠀",
    "⢰⡿⠀⠁⠀⢠⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠐⠸⠘⣧⡀",
    "⣿⠋⠀⠀⠀⣾⣭⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠰⠆⢹⡗",
    "⣿⢀⡀⡀⢀⣿⠀⠐⠂⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢸⡇⣤⣿",
    "⢿⣿⣿⢿⣼⠏⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠁⢀⣿⣷⡟⠛",
    "⠀⠀⠁⠐⠹⣷⡀⠀⠀⢠⠀⠀⠀⠀⢀⠀⠀⠃⢘⣾⠃⠀⠀⠀",
    "⠀⠀⠀⢀⣵⠿⠃⠇⠀⠀⠀⢰⣯⡌⠈⠀⠰⢀⠜⠷⣦⡄⠀⠀",
    "⠀⠀⠀⠿⠷⠶⠶⠶⣾⣤⣤⠿⢙⣷⣄⣒⣤⣀⣴⣮⣭⡷⠃⠀",
];

/// Lay the info lines out to the right of the art, row by row.
pub fn with_art(lines: &[String]) -> Vec<String> {
    let width = ART.iter().map(|a| a.chars().count()).max().unwrap_or(0);
    let rows = ART.len().max(lines.len());
    (0..rows)
        .map(|i| {
            let art = ART.get(i).copied().unwrap_or("");
            let pad = width.saturating_sub(art.chars().count());
            match lines.get(i) {
                Some(l) => format!("{art}{}  {l}", " ".repeat(pad)),
                None => art.trim_end().to_string(),
            }
        })
        .collect()
}

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
    let mut lines = vec![format!("bogyman {LINE}")];
    lines.push(format!(
        "commit     {}{}{}",
        GIT_COMMIT,
        if dirty() { " (dirty)" } else { "" },
        if GIT_DATE.is_empty() {
            String::new()
        } else {
            format!(" from {GIT_DATE}")
        }
    ));
    lines.push(format!("built      {BUILT_AT} · {PROFILE} · {RUSTC}"));
    lines.push(format!("target     {TARGET}"));
    if let Ok(p) = std::env::current_exe() {
        lines.push(format!("binary     {}", crate::paths::tilde(&p)));
    }
    if ctx.tty {
        for row in with_art(&lines) {
            println!("{row}");
        }
    } else {
        for l in lines {
            println!("{l}");
        }
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_rows_align_and_carry_every_line() {
        let lines: Vec<String> = (0..3).map(|i| format!("line {i}")).collect();
        let rows = with_art(&lines);
        assert_eq!(rows.len(), ART.len());
        for (i, l) in lines.iter().enumerate() {
            assert!(rows[i].ends_with(l), "{}", rows[i]);
        }
        // Every info line starts at the same column.
        let cols: Vec<usize> = rows[..3].iter().map(|r| r.find("line").unwrap()).collect();
        assert!(cols.iter().all(|c| *c == cols[0]));
        // More lines than art rows still all print.
        let n = ART.len() + 2;
        let many: Vec<String> = (0..n).map(|i| i.to_string()).collect();
        assert_eq!(with_art(&many).len(), n);
    }
}
