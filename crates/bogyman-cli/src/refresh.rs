//! Bring the store up to date and hand back the sessions a command should see.

use anyhow::{Context, Result};
use bogyman_collectors::claude_sessions;
use bogyman_core::{Event, EventKind, ReducerOptions, Session, poll_state};
use bogyman_store::Store;
use time::OffsetDateTime;

use crate::Ctx;

pub fn open_store(ctx: &Ctx) -> Result<Store> {
    Store::open(&ctx.paths.db)
        .with_context(|| format!("opening store at {}", ctx.paths.db.display()))
}

/// Materialize the snapshot. With `poll`, first ingest what Claude Code's session
/// files say (deduplicated, so a quiet session adds no events).
pub fn load_sessions(ctx: &Ctx, poll: bool) -> Result<Vec<Session>> {
    let mut store = open_store(ctx)?;
    let now = OffsetDateTime::now_utc();
    if poll && ctx.config.collectors.claude_sessions {
        let snapshot = store.load_snapshot()?;
        for ev in claude_sessions::poll(&ctx.paths.claude_sessions_dir, now) {
            let key = Session::key(ev.vendor, &ev.vendor_session_id);
            let fresh = match snapshot.get(&key) {
                None => true,
                Some(s) => poll_adds_something(s, &ev),
            };
            if fresh {
                store.append(&ev)?;
            }
        }
    }
    let sessions = store.materialize(
        ReducerOptions {
            redact: ctx.config.store.redact,
        },
        now,
        bogyman_core::liveness::pid_alive,
    )?;
    Ok(apply_filters(ctx, sessions))
}

fn poll_adds_something(s: &Session, ev: &Event) -> bool {
    let EventKind::StatusPoll { status, name, .. } = &ev.kind else {
        return true;
    };
    if s.pid.is_none() && ev.pid.is_some() {
        return true;
    }
    if name.is_some() && s.name.as_deref() != name.as_deref() {
        return true;
    }
    if ev.at <= s.observed_at {
        return false;
    }
    match poll_state(status) {
        Some((state, _)) => state != s.state,
        None => false,
    }
}

pub fn apply_filters(ctx: &Ctx, sessions: Vec<Session>) -> Vec<Session> {
    sessions
        .into_iter()
        .filter(|s| ctx.vendor.is_none_or(|v| s.vendor == v))
        .filter(|s| match (&ctx.cwd, &s.cwd) {
            (None, _) => true,
            (Some(prefix), Some(cwd)) => cwd.starts_with(prefix.as_str()),
            (Some(_), None) => false,
        })
        .collect()
}
