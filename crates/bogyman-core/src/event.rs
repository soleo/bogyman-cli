use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::model::{Host, Kind, Source, Vendor};

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
    /// A poller's view of the session (for example `~/.claude/sessions/<pid>.json`).
    StatusPoll {
        status: String,
        name: Option<String>,
        kind: Option<Kind>,
        host: Option<Host>,
        model: Option<String>,
        #[serde(default, with = "time::serde::rfc3339::option")]
        started_at: Option<OffsetDateTime>,
    },
    Unknown {
        name: String,
    },
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
            _ => None,
        }
    }
}
