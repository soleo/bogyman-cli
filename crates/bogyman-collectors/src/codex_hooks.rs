//! Codex CLI payloads: `~/.codex/hooks.json` hooks (same shape as Claude Code's,
//! read from stdin) and the older `notify` program payload (passed as an argument,
//! `{"type":"agent-turn-complete","thread-id":…}`).

use bogyman_core::{Event, EventKind, Source, Vendor};
use serde_json::Value;
use time::OffsetDateTime;

use crate::ParseError;
use crate::claude_hooks::map_event;
use crate::util::str_field;

/// Hook events `bogyman install --codex` registers.
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "Stop",
    "SessionEnd",
];

const ID_KEYS: &[&str] = &[
    "session_id",
    "thread_id",
    "thread-id",
    "conversation_id",
    "conversation-id",
    "sessionId",
    "threadId",
];

fn session_id(raw: &Value) -> Result<String, ParseError> {
    str_field(raw, ID_KEYS).ok_or(ParseError::MissingSessionId("session_id or thread-id"))
}

/// A hook payload from `~/.codex/hooks.json`.
pub fn parse_hook(raw: Value, at: OffsetDateTime) -> Result<Event, ParseError> {
    if !raw.is_object() {
        return Err(ParseError::NotAnObject);
    }
    let sid = session_id(&raw)?;
    let name =
        str_field(&raw, &["hook_event_name", "hookEventName", "event", "type"]).unwrap_or_default();
    let kind = match map_event(&name, &raw) {
        EventKind::Unknown { name } => map_codex_specific(&name, &raw),
        k => k,
    };
    Ok(Event {
        id: None,
        at,
        vendor: Vendor::Codex,
        vendor_session_id: sid,
        source: Source::Hook,
        kind,
        cwd: str_field(&raw, &["cwd", "working_directory"]),
        pid: None,
        transcript_path: str_field(&raw, &["transcript_path", "rollout_path"]),
        raw,
    })
}

/// The `notify` payload Codex passes as a JSON argument.
pub fn parse_notify(raw: Value, at: OffsetDateTime) -> Result<Event, ParseError> {
    if !raw.is_object() {
        return Err(ParseError::NotAnObject);
    }
    let sid = session_id(&raw)?;
    let ty = str_field(&raw, &["type"]).unwrap_or_default();
    let kind = map_codex_specific(&ty, &raw);
    Ok(Event {
        id: None,
        at,
        vendor: Vendor::Codex,
        vendor_session_id: sid,
        source: Source::Hook,
        kind,
        cwd: str_field(&raw, &["cwd"]),
        pid: None,
        transcript_path: None,
        raw,
    })
}

/// Names Codex uses that have no Claude equivalent. Matched loosely because the
/// format is treated as unstable.
fn map_codex_specific(name: &str, raw: &Value) -> EventKind {
    let n = name.to_ascii_lowercase().replace('_', "-");
    if n.contains("turn") && (n.contains("complete") || n.contains("end")) {
        EventKind::Stop {
            message: str_field(raw, &["last-assistant-message", "last_assistant_message"]),
        }
    } else if n.contains("turn") && n.contains("start") {
        EventKind::PromptSubmitted
    } else if n.contains("approval") || n.contains("permission") {
        let tool =
            str_field(raw, &["tool_name", "tool", "command"]).unwrap_or_else(|| "command".into());
        EventKind::PermissionRequest {
            tool: tool.clone(),
            detail: str_field(raw, &["command", "reason", "message"])
                .map(|d| format!("{tool}: {d}")),
        }
    } else if n.contains("error") || n.contains("fail") {
        EventKind::StopFailure {
            error: str_field(raw, &["message", "error"]),
        }
    } else if n.contains("session") && (n.contains("end") || n.contains("exit")) {
        EventKind::SessionEnd {
            reason: str_field(raw, &["reason"]),
        }
    } else {
        EventKind::Unknown {
            name: if name.is_empty() {
                "unnamed".into()
            } else {
                name.into()
            },
        }
    }
}
