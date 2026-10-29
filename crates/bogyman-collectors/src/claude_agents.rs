//! Parse `claude agents --json --all`: Claude Code's own list of sessions,
//! interactive and background, including finished background sessions.
//!
//! Every entry has `cwd`, `kind` and `startedAt`. Background entries add `id`
//! (the short handle `claude attach` takes) and `state` (`working`, `blocked`,
//! `done`, `failed`, `stopped`). Live entries add `pid` and `status` (`busy`,
//! `waiting`, `idle`); a waiting one adds `waitingFor` ("permission prompt",
//! "input needed", …). `sessionId` and `name` appear when set.
//!
//! Spawning the vendor CLI costs about 100 ms, so this runs in the daemon and in
//! `ls --watch`, never on a plain query.

use bogyman_core::{Event, EventKind, Host, Kind, Source, Vendor, WaitingFor};
use serde_json::Value;
use time::OffsetDateTime;

use crate::claude_sessions::ms_to_time;
use crate::util::{str_field, u64_field};

/// Parse the whole output. Entries that can't be tied to a session are skipped;
/// anything that is not a JSON array yields nothing.
pub fn parse(text: &str, now: OffsetDateTime) -> Vec<Event> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    items.iter().filter_map(|v| parse_entry(v, now)).collect()
}

/// One entry. The status word prefers `state` (what agent view shows) over the
/// live process `status`, since `state` distinguishes blocked from done.
pub fn parse_entry(raw: &Value, now: OffsetDateTime) -> Option<Event> {
    let agent_id = str_field(raw, &["id"]);
    // Hooks and session files key sessions by the full UUID. Without one, the
    // short id keeps a finished background session visible on its own.
    let session_id = str_field(raw, &["sessionId"]).or_else(|| agent_id.clone())?;
    let kind = str_field(raw, &["kind"]).map(|k| match k.as_str() {
        "interactive" => Kind::Interactive,
        "background" => Kind::Background,
        _ => Kind::Unknown,
    });
    let host = match kind {
        Some(Kind::Background) => Some(Host::AgentView),
        _ => None,
    };
    let status = str_field(raw, &["state"])
        .or_else(|| str_field(raw, &["status"]))
        .unwrap_or_else(|| "unknown".into());
    let waiting_for = str_field(raw, &["waitingFor"]).map(|w| WaitingFor::from_agents_phrase(&w));
    Some(Event {
        id: None,
        at: now,
        vendor: Vendor::ClaudeCode,
        vendor_session_id: session_id,
        source: Source::Poll,
        kind: EventKind::StatusPoll {
            status,
            name: str_field(raw, &["name"]),
            kind,
            host,
            model: None,
            started_at: u64_field(raw, &["startedAt"]).and_then(ms_to_time),
            waiting_for,
            agent_id,
        },
        cwd: str_field(raw, &["cwd"]),
        pid: u64_field(raw, &["pid"]).map(|p| p as u32),
        transcript_path: None,
        raw: raw.clone(),
    })
}
