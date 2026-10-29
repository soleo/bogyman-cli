use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::model::{Host, Kind, Source, Tokens, Vendor, WaitingFor};
use crate::{clip_detail, strip_control};

/// Payload keys that carry content rather than metadata. They never reach the store.
const CONTENT_KEYS: &[&str] = &[
    "prompt",
    "tool_input",
    "tool_response",
    "last_assistant_message",
    "last-assistant-message",
    "input-messages",
    "input_messages",
    "message",
    "title",
    "task_description",
    "permission_suggestions",
    "error_details",
    "mcp_server",
];

/// What a collector observed. Vendor-specific payloads are normalized into
/// `EventKind`; the untouched payload rides along in `raw` for forward compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Store row id, once persisted.
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub vendor: Vendor,
    pub vendor_session_id: String,
    pub source: Source,
    pub kind: EventKind,
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    SessionStart {
        kind: Option<Kind>,
        host: Option<Host>,
        name: Option<String>,
        model: Option<String>,
    },
    PromptSubmitted,
    ToolUse {
        tool: String,
        detail: Option<String>,
    },
    PermissionRequest {
        tool: String,
        detail: Option<String>,
    },
    Notification {
        notification_type: String,
        message: Option<String>,
    },
    Stop {
        message: Option<String>,
    },
    StopFailure {
        error: Option<String>,
    },
    TaskCreated {
        subject: Option<String>,
    },
    TaskCompleted {
        subject: Option<String>,
    },
    SessionEnd {
        reason: Option<String>,
    },
    /// Metadata read from the session's transcript tail: which model answered
    /// last, and where the session runs. Never changes the state.
    TranscriptPoll {
        model: Option<String>,
        host: Option<Host>,
    },
    /// A poller's view of the session (for example `~/.claude/sessions/<pid>.json`).
    StatusPoll {
        status: String,
        name: Option<String>,
        kind: Option<Kind>,
        host: Option<Host>,
        model: Option<String>,
        #[serde(default, with = "time::serde::rfc3339::option")]
        started_at: Option<OffsetDateTime>,
        /// Why the session waits, when the poller says so precisely.
        #[serde(default)]
        waiting_for: Option<WaitingFor>,
        /// The vendor's short handle for a background session.
        #[serde(default)]
        agent_id: Option<String>,
    },
    /// What a Codex rollout file says: turn status, model, token totals and the
    /// last reply.
    RolloutPoll {
        status: Option<String>,
        name: Option<String>,
        host: Option<Host>,
        model: Option<String>,
        tokens: Option<Tokens>,
        summary: Option<String>,
        #[serde(default, with = "time::serde::rfc3339::option")]
        started_at: Option<OffsetDateTime>,
    },
    Unknown {
        name: String,
    },
}

impl Event {
    /// Enforce the privacy rules before persisting: content keys leave the raw
    /// payload, every free-text field is clipped and stripped of control
    /// characters, and with `redact` the free text is dropped altogether.
    pub fn sanitize(&mut self, redact: bool) {
        if let Value::Object(map) = &mut self.raw {
            for k in CONTENT_KEYS {
                map.remove(*k);
            }
            for v in map.values_mut() {
                if let Value::String(s) = v {
                    *s = clip_detail(s);
                }
            }
        } else {
            self.raw = Value::Null;
        }
        if redact {
            self.raw = Value::Null;
        }
        let fix = |o: &mut Option<String>| {
            *o = if redact {
                None
            } else {
                o.as_deref().map(clip_detail)
            };
        };
        if let Some(c) = &mut self.cwd {
            *c = strip_control(c);
        }
        if let Some(t) = &mut self.transcript_path {
            *t = strip_control(t);
        }
        match &mut self.kind {
            EventKind::SessionStart { name, model, .. } => {
                fix(name);
                fix(model);
            }
            EventKind::ToolUse { tool, detail } | EventKind::PermissionRequest { tool, detail } => {
                *tool = clip_detail(tool);
                fix(detail);
            }
            EventKind::Notification {
                notification_type,
                message,
            } => {
                *notification_type = clip_detail(notification_type);
                fix(message);
            }
            EventKind::Stop { message } => fix(message),
            EventKind::StopFailure { error } => fix(error),
            EventKind::TaskCreated { subject } | EventKind::TaskCompleted { subject } => {
                fix(subject)
            }
            EventKind::SessionEnd { reason } => fix(reason),
            EventKind::StatusPoll {
                status,
                name,
                model,
                agent_id,
                ..
            } => {
                *status = clip_detail(status);
                *agent_id = agent_id.as_deref().map(clip_detail);
                // A session's name is shown in listings even under redact; it is
                // the vendor's label, not content.
                *name = name.as_deref().map(clip_detail);
                fix(model);
            }
            EventKind::TranscriptPoll { model, .. } => fix(model),
            EventKind::RolloutPoll {
                status,
                name,
                model,
                summary,
                ..
            } => {
                *status = status.as_deref().map(clip_detail);
                *name = name.as_deref().map(clip_detail);
                fix(model);
                fix(summary);
            }
            EventKind::Unknown { name } => *name = clip_detail(name),
            EventKind::PromptSubmitted => {}
        }
    }
}

impl EventKind {
    /// Short label for event listings.
    pub fn label(&self) -> String {
        match self {
            EventKind::SessionStart { .. } => "session_start".into(),
            EventKind::PromptSubmitted => "prompt".into(),
            EventKind::ToolUse { tool, .. } => format!("tool {tool}"),
            EventKind::PermissionRequest { tool, .. } => format!("permission {tool}"),
            EventKind::Notification {
                notification_type, ..
            } => format!("notify {notification_type}"),
            EventKind::Stop { .. } => "stop".into(),
            EventKind::StopFailure { .. } => "stop_failure".into(),
            EventKind::TaskCreated { .. } => "task_created".into(),
            EventKind::TaskCompleted { .. } => "task_completed".into(),
            EventKind::SessionEnd { .. } => "session_end".into(),
            EventKind::StatusPoll { status, .. } => format!("poll {status}"),
            EventKind::TranscriptPoll { model, .. } => {
                format!("transcript {}", model.as_deref().unwrap_or("?"))
            }
            EventKind::RolloutPoll { status, .. } => {
                format!("rollout {}", status.as_deref().unwrap_or("?"))
            }
            EventKind::Unknown { name } => format!("unknown {name}"),
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            EventKind::ToolUse { detail, .. } | EventKind::PermissionRequest { detail, .. } => {
                detail.as_deref()
            }
            EventKind::Notification { message, .. } => message.as_deref(),
            EventKind::Stop { message } => message.as_deref(),
            EventKind::StopFailure { error } => error.as_deref(),
            EventKind::TaskCreated { subject } | EventKind::TaskCompleted { subject } => {
                subject.as_deref()
            }
            EventKind::SessionEnd { reason } => reason.as_deref(),
            EventKind::SessionStart { name, .. } | EventKind::StatusPoll { name, .. } => {
                name.as_deref()
            }
            EventKind::TranscriptPoll { model, .. } => model.as_deref(),
            EventKind::RolloutPoll { summary, .. } => summary.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn ev(kind: EventKind, raw: Value) -> Event {
        Event {
            id: None,
            at: datetime!(2026-10-26 21:00:00 UTC),
            vendor: Vendor::ClaudeCode,
            vendor_session_id: "abc".into(),
            source: Source::Hook,
            kind,
            cwd: Some("/tmp/x\u{1b}[31m".into()),
            pid: None,
            transcript_path: None,
            raw,
        }
    }

    #[test]
    fn sanitize_prunes_content_and_clips_detail() {
        let long = "y".repeat(400);
        let mut e = ev(
            EventKind::PermissionRequest {
                tool: "Bash".into(),
                detail: Some(format!("Bash: {long}\u{1b}[0m")),
            },
            serde_json::json!({
                "session_id": "abc", "hook_event_name": "PermissionRequest",
                "prompt": "secret prompt", "tool_input": {"command": "rm -rf"},
                "cwd": "/tmp/x", "permission_mode": "default"
            }),
        );
        e.sanitize(false);
        assert!(e.raw.get("prompt").is_none());
        assert!(e.raw.get("tool_input").is_none());
        assert_eq!(e.raw["cwd"], "/tmp/x");
        assert_eq!(e.cwd.as_deref(), Some("/tmp/x [31m"));
        let EventKind::PermissionRequest {
            detail: Some(d), ..
        } = &e.kind
        else {
            panic!()
        };
        assert_eq!(d.chars().count(), 200);
        assert!(!d.contains('\u{1b}'));
    }

    #[test]
    fn sanitize_with_redact_drops_free_text() {
        let mut e = ev(
            EventKind::Stop {
                message: Some("summary".into()),
            },
            serde_json::json!({"session_id": "abc"}),
        );
        e.sanitize(true);
        assert!(e.raw.is_null());
        assert!(matches!(e.kind, EventKind::Stop { message: None }));
    }
}
