//! Claude Code hook payloads (`bogyman hook claude` reads one from stdin).
//!
//! Registered on SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest,
//! Notification, Stop, StopFailure, TaskCreated, TaskCompleted and SessionEnd.
//! Anything else that arrives is kept as `Unknown`.

use bogyman_core::{Event, EventKind, Host, Kind, Source, Vendor};
use serde_json::Value;
use time::OffsetDateTime;

use crate::ParseError;
use crate::util::{str_field, tool_line};

/// Hook events `bogyman install` registers, in the order they appear in settings.
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "TaskCreated",
    "TaskCompleted",
    "SessionEnd",
];

pub fn parse(raw: Value, at: OffsetDateTime) -> Result<Event, ParseError> {
    if !raw.is_object() {
        return Err(ParseError::NotAnObject);
    }
    let session_id = str_field(&raw, &["session_id", "sessionId"])
        .ok_or(ParseError::MissingSessionId("session_id"))?;
    let name = str_field(&raw, &["hook_event_name", "hookEventName"]).unwrap_or_default();
    let cwd = str_field(&raw, &["cwd"]);
    let transcript_path = str_field(&raw, &["transcript_path", "transcriptPath"]);
    let kind = map_event(&name, &raw);
    Ok(Event {
        id: None,
        at,
        vendor: Vendor::ClaudeCode,
        vendor_session_id: session_id,
        source: Source::Hook,
        kind,
        cwd,
        pid: None,
        transcript_path,
        raw,
    })
}

pub(crate) fn map_event(name: &str, raw: &Value) -> EventKind {
    let tool = || str_field(raw, &["tool_name", "toolName"]).unwrap_or_else(|| "tool".into());
    match name {
        "SessionStart" => EventKind::SessionStart {
            kind: str_field(raw, &["kind"]).and_then(|k| match k.as_str() {
                "interactive" => Some(Kind::Interactive),
                "background" => Some(Kind::Background),
                _ => None,
            }),
            host: str_field(raw, &["entrypoint", "host"]).and_then(|h| host_from_entrypoint(&h)),
            name: str_field(raw, &["session_name", "name"]),
            model: str_field(raw, &["model"]),
        },
        "UserPromptSubmit" => EventKind::PromptSubmitted,
        "PreToolUse" => {
            let t = tool();
            let detail = tool_line(&t, raw.get("tool_input"));
            EventKind::ToolUse { tool: t, detail }
        }
        "PermissionRequest" => {
            let t = tool();
            let detail = tool_line(&t, raw.get("tool_input"));
            EventKind::PermissionRequest { tool: t, detail }
        }
        "Notification" => EventKind::Notification {
            notification_type: str_field(raw, &["notification_type", "notificationType"])
                .unwrap_or_else(|| "unknown".into()),
            message: str_field(raw, &["message", "title"]),
        },
        "Stop" => EventKind::Stop {
            message: str_field(raw, &["last_assistant_message", "message"]),
        },
        "StopFailure" => EventKind::StopFailure {
            error: str_field(raw, &["error", "error_details", "message"]),
        },
        "TaskCreated" => EventKind::TaskCreated {
            subject: str_field(raw, &["task_subject", "task_description"]),
        },
        "TaskCompleted" => EventKind::TaskCompleted {
            subject: str_field(raw, &["task_subject", "task_description"]),
        },
        "SessionEnd" => EventKind::SessionEnd {
            reason: str_field(raw, &["reason"]),
        },
        other => EventKind::Unknown {
            name: if other.is_empty() {
                "unnamed".into()
            } else {
                other.into()
            },
        },
    }
}

/// Map the `entrypoint` word Claude Code writes to its session file onto a host.
pub fn host_from_entrypoint(entry: &str) -> Option<Host> {
    let e = entry.to_ascii_lowercase();
    if e.contains("cowork") {
        Some(Host::DesktopCowork)
    } else if e.contains("desktop") {
        Some(Host::Desktop)
    } else if e.contains("vscode") || e.contains("jetbrains") || e.contains("ide") {
        Some(Host::Vscode)
    } else if e == "cli" || e.contains("terminal") {
        Some(Host::Terminal)
    } else {
        None
    }
}
