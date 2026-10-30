//! Bring the store up to date and hand back the sessions a command should see.

use anyhow::{Context, Result};
use bogyman_collectors::{
    claude_agents, claude_sessions, claude_transcript, codex_rollout, desktop_index,
};
use bogyman_core::{Event, EventKind, Host, ReducerOptions, Session, State, Vendor, poll_state};
use bogyman_store::Store;
use time::OffsetDateTime;

use crate::Ctx;

/// How long `claude agents --json` may take before a poll gives up on it.
const CLAUDE_AGENTS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
        append_fresh(
            ctx,
            &store,
            claude_sessions::poll(&ctx.paths.claude_sessions_dir, now),
        )?;
    }
    if poll && ctx.config.collectors.codex_rollout {
        let events = rollout_events(ctx, &store.load_snapshot()?, now);
        append_fresh(ctx, &store, events)?;
    }
    if poll && ctx.config.collectors.desktop_index {
        let events = desktop_events(ctx, &store.load_snapshot()?, now);
        for mut ev in events {
            ev.sanitize(ctx.config.store.redact);
            store.append(&ev)?;
        }
    }
    let opts = ReducerOptions {
        redact: ctx.config.store.redact,
    };
    let mut sessions = store.materialize(opts, now, bogyman_core::liveness::pid_alive)?;
    if poll && ctx.config.collectors.claude_transcript {
        let mut added = false;
        for s in sessions
            .iter()
            .filter(|s| s.vendor == Vendor::ClaudeCode && s.state.is_live())
        {
            let path = match (&s.transcript_path, &s.cwd) {
                (Some(p), _) => std::path::PathBuf::from(p),
                (None, Some(cwd)) => claude_transcript::transcript_path(
                    &ctx.paths.claude_dir,
                    cwd,
                    &s.vendor_session_id,
                ),
                (None, None) => continue,
            };
            let Some(tail) = claude_transcript::read_tail(&path) else {
                continue;
            };
            let model_new = tail.model.is_some() && tail.model != s.model;
            let host_new = tail.host.is_some() && s.host == Host::Unknown;
            if model_new || host_new {
                let mut ev = claude_transcript::event_for(
                    &s.vendor_session_id,
                    s.cwd.as_deref(),
                    &tail,
                    now,
                );
                ev.sanitize(ctx.config.store.redact);
                store.append(&ev)?;
                added = true;
            }
        }
        if added {
            sessions = store.materialize(opts, now, bogyman_core::liveness::pid_alive)?;
        }
    }
    Ok(apply_filters(ctx, sessions))
}

/// Ask Claude Code for its session list (`claude agents --json --all`) and store
/// what changed. Spawns the vendor CLI, so callers run it on a timer, not per
/// query. Returns how many events were added.
pub fn poll_claude_agents(ctx: &Ctx) -> Result<usize> {
    if !ctx.config.collectors.claude_agents {
        return Ok(0);
    }
    let text = crate::vendor_cmd::output(
        "claude",
        &["agents", "--json", "--all"],
        CLAUDE_AGENTS_TIMEOUT,
    )?;
    let now = OffsetDateTime::now_utc();
    let events = claude_agents::parse(&text, now);
    let store = open_store(ctx)?;
    append_fresh(ctx, &store, events)
}

/// Codex sessions whose rollout changed in the last day and since bogyman last
/// looked. An unchanged file is not even opened.
fn rollout_events(
    ctx: &Ctx,
    snapshot: &std::collections::HashMap<String, Session>,
    now: OffsetDateTime,
) -> Vec<Event> {
    let since = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
    let files = codex_rollout::recent_files(&ctx.paths.codex_sessions_dir, since);
    if files.is_empty() {
        return Vec::new();
    }
    let names = codex_rollout::thread_names(&ctx.paths.codex_session_index);
    let mut out = Vec::new();
    for (path, mtime) in files {
        let mtime = OffsetDateTime::from(mtime);
        let known = codex_rollout::id_from_file_name(&path)
            .and_then(|id| snapshot.get(&Session::key(Vendor::Codex, &id)));
        if known.is_some_and(|s| s.observed_at >= mtime && s.tokens.is_some()) {
            continue;
        }
        let Some(r) = codex_rollout::read(&path) else {
            continue;
        };
        let name = names.get(&r.session_id).cloned();
        out.push(codex_rollout::event_for(&r, name, mtime.min(now)));
    }
    out
}

/// Desktop index entries for Claude sessions bogyman already tracks, when they
/// would fill in a name or change the summary. The index also holds finished
/// and adopted sessions, so an unknown id is never a new session.
fn desktop_events(
    ctx: &Ctx,
    snapshot: &std::collections::HashMap<String, Session>,
    now: OffsetDateTime,
) -> Vec<Event> {
    let since = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
    let redact = ctx.config.store.redact;
    desktop_index::recent_files(&ctx.paths.claude_desktop_dir, since)
        .iter()
        .filter_map(|p| desktop_index::read(p))
        .filter(|e| {
            let Some(s) = snapshot.get(&Session::key(Vendor::ClaudeCode, &e.cli_session_id)) else {
                return false;
            };
            let name_gap = s.name.is_none() && e.title.is_some();
            let summary = e.summary.as_deref().map(bogyman_core::clip_detail);
            let summary_new = !redact && summary.is_some() && summary != s.summary;
            name_gap || summary_new
        })
        .map(|e| desktop_index::event_for(&e, now))
        .collect()
}

/// Append the poll events that tell the snapshot something new.
fn append_fresh(ctx: &Ctx, store: &Store, events: Vec<Event>) -> Result<usize> {
    let snapshot = store.load_snapshot()?;
    let mut added = 0;
    for mut ev in events {
        ev.sanitize(ctx.config.store.redact);
        let key = Session::key(ev.vendor, &ev.vendor_session_id);
        let fresh = match snapshot.get(&key) {
            None => true,
            Some(s) => poll_adds_something(s, &ev),
        };
        if fresh {
            store.append(&ev)?;
            added += 1;
        }
    }
    Ok(added)
}

fn poll_adds_something(s: &Session, ev: &Event) -> bool {
    if let EventKind::RolloutPoll {
        status,
        name,
        model,
        tokens,
        summary,
        ..
    } = &ev.kind
    {
        let state_new = status
            .as_deref()
            .and_then(poll_state)
            .is_some_and(|(state, _)| state != s.state && ev.at > s.observed_at);
        let differs = |new: &Option<String>, old: &Option<String>| new.is_some() && new != old;
        return state_new
            || differs(name, &s.name)
            || differs(model, &s.model)
            || (tokens.is_some() && *tokens != s.tokens)
            || differs(summary, &s.summary);
    }
    let EventKind::StatusPoll {
        status,
        name,
        waiting_for,
        agent_id,
        ..
    } = &ev.kind
    else {
        return true;
    };
    if agent_id.is_some() && s.agent_id != *agent_id {
        return true;
    }
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
        Some((state, _)) if state != s.state => true,
        Some((State::Blocked, _)) => waiting_for.is_some() && *waiting_for != s.waiting_for,
        _ => false,
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
