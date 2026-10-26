//! Poll `~/.claude/sessions/<pid>.json`, the per-session status record Claude
//! Code writes (busy / waiting / idle). Cheap, needs no daemon, and is the only
//! signal for sessions whose hooks are disabled.

use std::path::Path;

use bogyman_core::{Event, EventKind, Host, Kind, Source, Vendor};
use serde_json::Value;
use time::OffsetDateTime;

use crate::claude_hooks::host_from_entrypoint;
use crate::util::{str_field, u64_field};

fn ms_to_time(ms: u64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000).ok()
}

/// Parse one session file. Returns `None` when it is not a session record.
pub fn parse_file(raw: &Value, fallback_at: OffsetDateTime) -> Option<Event> {
    // The real file uses camelCase and always carries a pid; hook payloads
    // (snake_case, no pid) must not be mistaken for it.
    let session_id = str_field(raw, &["sessionId"])?;
    let pid = Some(u64_field(raw, &["pid"])? as u32);
    let status = str_field(raw, &["status"]).unwrap_or_else(|| "unknown".into());
    let kind = str_field(raw, &["kind"]).map(|k| match k.as_str() {
        "interactive" => Kind::Interactive,
        "background" => Kind::Background,
        _ => Kind::Unknown,
    });
    let host = match kind {
        Some(Kind::Background) => Some(Host::AgentView),
        _ => str_field(raw, &["entrypoint"]).and_then(|e| host_from_entrypoint(&e)),
    };
    let at = u64_field(raw, &["statusUpdatedAt", "updatedAt"])
        .and_then(ms_to_time)
        .unwrap_or(fallback_at);
    let started_at = u64_field(raw, &["startedAt"]).and_then(ms_to_time);
    Some(Event {
        id: None,
        at,
        vendor: Vendor::ClaudeCode,
        vendor_session_id: session_id,
        source: Source::Poll,
        kind: EventKind::StatusPoll {
            status,
            name: str_field(raw, &["name"]),
            kind,
            host,
            model: str_field(raw, &["model"]),
            started_at,
        },
        cwd: str_field(raw, &["cwd"]),
        pid,
        transcript_path: str_field(raw, &["transcriptPath", "transcript_path"]),
        raw: raw.clone(),
    })
}

/// Read every `*.json` in `dir`. Unreadable files are skipped.
pub fn poll(dir: &Path, now: OffsetDateTime) -> Vec<Event> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(raw) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(ev) = parse_file(&raw, now) {
            out.push(ev);
        }
    }
    out.sort_by_key(|e| e.at);
    out
}
