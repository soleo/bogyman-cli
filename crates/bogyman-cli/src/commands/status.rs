//! One line for prompts and status bars. Reads the cache only: no vendor
//! processes, no session-directory poll, so it stays well under 20 ms.

use anyhow::Result;
use bogyman_core::State;

use crate::{Ctx, EXIT_OK, json, refresh};

pub fn run(ctx: &Ctx, format: Option<&str>) -> Result<u8> {
    let sessions = refresh::load_sessions(ctx, false)?;
    let count = |st: State| sessions.iter().filter(|s| s.state == st).count();
    let blocked = count(State::Blocked);
    let working = count(State::Working);
    let done = count(State::Done);
    let failed = count(State::Failed);
    let stale = sessions.iter().filter(|s| s.stale).count();
    let total = sessions.iter().filter(|s| s.state.is_live()).count();

    let default_line = {
        let mut parts = Vec::new();
        if blocked > 0 {
            parts.push(format!("⏳{blocked}"));
        }
        if working > 0 {
            parts.push(format!("▶{working}"));
        }
        if done > 0 {
            parts.push(format!("✓{done}"));
        }
        if failed > 0 {
            parts.push(format!("✗{failed}"));
        }
        parts.join(" ")
    };

    let line = match format {
        Some(f) => f
            .replace("{blocked}", &blocked.to_string())
            .replace("{working}", &working.to_string())
            .replace("{done}", &done.to_string())
            .replace("{failed}", &failed.to_string())
            .replace("{stale}", &stale.to_string())
            .replace("{total}", &total.to_string()),
        None => default_line,
    };

    if ctx.json {
        json::ok(
            ctx,
            "status",
            serde_json::json!({
                "blocked": blocked, "working": working, "done": done,
                "failed": failed, "stale": stale, "total": total, "line": line,
            }),
        )?;
    } else if format.is_some() || total > 0 || failed > 0 {
        println!("{line}");
    }
    Ok(EXIT_OK)
}
