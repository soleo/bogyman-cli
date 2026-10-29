use anyhow::Result;
use bogyman_core::Session;
use time::{Duration, OffsetDateTime};

use crate::render::{
    cwd_cell, fmt_age, model_cell, print_rows, sort_for_listing, state_cell, truncate, waiting_cell,
};
use crate::{Ctx, EXIT_OK, json, refresh};

pub fn run(ctx: &Ctx, all: bool, watch: bool) -> Result<u8> {
    if !watch {
        return once(ctx, all, false);
    }
    loop {
        // Under --json the output is an NDJSON stream, so it must not be
        // interrupted by a screen clear even on a terminal.
        if ctx.tty && !ctx.json {
            print!("\x1b[2J\x1b[H");
        }
        once(ctx, all, true)?;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn once(ctx: &Ctx, all: bool, watch: bool) -> Result<u8> {
    let now = OffsetDateTime::now_utc();
    let mut sessions = refresh::load_sessions(ctx, true)?;
    sessions.retain(|s| s.state.is_live() || (all && now - s.observed_at < Duration::hours(24)));
    sort_for_listing(&mut sessions);
    if ctx.json {
        let data = serde_json::json!({"sessions": sessions});
        if watch {
            json::ok_line("ls", data)?;
        } else {
            json::ok(ctx, "ls", data)?;
        }
        return Ok(EXIT_OK);
    }
    if sessions.is_empty() {
        if ctx.tty {
            println!("No live sessions. (Hooks wired? Try `bogyman doctor`.)");
        }
        return Ok(EXIT_OK);
    }
    let rows = sessions.iter().map(|s| row(ctx, s, now)).collect();
    print_rows(
        ctx,
        &[
            "ID",
            "VENDOR",
            "HOST",
            "NAME",
            "MODEL",
            "STATE",
            "WAITING FOR",
            "AGE",
            "CWD",
        ],
        rows,
    );
    Ok(EXIT_OK)
}

fn row(ctx: &Ctx, s: &Session, now: OffsetDateTime) -> Vec<String> {
    let mut name = truncate(&s.display_name(), 28);
    if let Some(t) = &s.tasks
        && t.total > 0
    {
        name = format!("{name} [{}/{}]", t.done, t.total);
    }
    vec![
        s.id.clone(),
        s.vendor.label().into(),
        s.host.label().into(),
        name,
        model_cell(s),
        state_cell(ctx, s.state, s.stale),
        truncate(&waiting_cell(s), 40),
        fmt_age(s.age_secs(now)),
        cwd_cell(s),
    ]
}
