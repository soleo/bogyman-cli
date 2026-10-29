use anyhow::Result;
use bogyman_core::State;
use time::OffsetDateTime;

use crate::render::{cwd_cell, fmt_age, print_rows, truncate, waiting_cell};
use crate::{Ctx, EXIT_EMPTY, EXIT_OK, json, refresh};

pub fn run(ctx: &Ctx) -> Result<u8> {
    let now = OffsetDateTime::now_utc();
    let mut sessions = refresh::load_sessions(ctx, true)?;
    sessions.retain(|s| s.state == State::Blocked);
    sessions.sort_by_key(|s| s.since);
    if ctx.json {
        // An empty result is a success, not an error; the exit code carries the
        // "nothing is waiting" signal so `waiting && …` chains still work.
        json::ok(ctx, "waiting", serde_json::json!({"sessions": sessions}))?;
        return Ok(if sessions.is_empty() {
            EXIT_EMPTY
        } else {
            EXIT_OK
        });
    }
    if sessions.is_empty() {
        if ctx.tty {
            println!("Nothing is waiting on you.");
        }
        return Ok(EXIT_EMPTY);
    }
    let rows = sessions
        .iter()
        .map(|s| {
            vec![
                s.id.clone(),
                s.vendor.label().into(),
                s.host.label().into(),
                truncate(&s.display_name(), 28),
                truncate(&waiting_cell(s), 48),
                fmt_age(s.age_secs(now)),
                cwd_cell(s),
            ]
        })
        .collect();
    print_rows(
        ctx,
        &["ID", "VENDOR", "HOST", "NAME", "WAITING FOR", "AGE", "CWD"],
        rows,
    );
    Ok(EXIT_OK)
}
