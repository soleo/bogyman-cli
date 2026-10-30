//! Read Claude Desktop's session index: one `local_<id>.json` per Code-tab
//! session under `<Claude app dir>/claude-code-sessions/<account>/<org>/`
//! (`~/Library/Application Support/Claude` on macOS, `~/.config/Claude` on
//! Linux).
//!
//! What it adds is narrow. Hooks already fire for Desktop sessions and say
//! where they run, and the transcript names the model. The index adds Desktop's
//! own post-turn summary (`postTurnSummary.status_detail`, plus `needs_action`
//! when it asks something of you) and a project label (`title`). Each entry
//! links to the hook stream through `cliSessionId`.
//!
//! The index also lists CLI sessions that Desktop adopted, and keeps finished
//! sessions around, so an entry never sets host or state. The caller only
//! applies entries for sessions bogyman already tracks. The format is internal
//! to the app; anything unrecognized is skipped.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bogyman_core::{Event, EventKind, Source, Vendor};
use serde_json::Value;
use time::OffsetDateTime;

use crate::util::str_field;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The Claude Code session id hooks report.
    pub cli_session_id: String,
    pub title: Option<String>,
    pub summary: Option<String>,
}

/// Index files under `app_dir` changed since `since`.
pub fn recent_files(app_dir: &Path, since: SystemTime) -> Vec<PathBuf> {
    let root = app_dir.join("claude-code-sessions");
    let mut out = Vec::new();
    for account in subdirs(&root) {
        for org in subdirs(&account) {
            let Ok(rd) = std::fs::read_dir(&org) else {
                continue;
            };
            for entry in rd.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !(name.starts_with("local_") && name.ends_with(".json")) {
                    continue;
                }
                let fresh = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|m| m >= since);
                if fresh {
                    out.push(path);
                }
            }
        }
    }
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

pub fn read(path: &Path) -> Option<Entry> {
    let text = std::fs::read_to_string(path).ok()?;
    parse(&serde_json::from_str(&text).ok()?)
}

/// One index entry. `None` without a `cliSessionId`: nothing to link it to.
pub fn parse(raw: &Value) -> Option<Entry> {
    let cli_session_id = str_field(raw, &["cliSessionId"])?;
    let post = raw.get("postTurnSummary").unwrap_or(&Value::Null);
    let detail = str_field(post, &["status_detail"]);
    let needs = str_field(post, &["needs_action"]);
    let summary = match (detail, needs) {
        (Some(d), Some(n)) => Some(format!("{d} · needs: {n}")),
        (Some(d), None) => Some(d),
        (None, Some(n)) => Some(format!("needs: {n}")),
        (None, None) => None,
    };
    Some(Entry {
        cli_session_id,
        title: str_field(raw, &["title"]),
        summary,
    })
}

pub fn event_for(e: &Entry, at: OffsetDateTime) -> Event {
    Event {
        id: None,
        at,
        vendor: Vendor::ClaudeCode,
        vendor_session_id: e.cli_session_id.clone(),
        source: Source::Poll,
        kind: EventKind::IndexPoll {
            title: e.title.clone(),
            summary: e.summary.clone(),
        },
        cwd: None,
        pid: None,
        transcript_path: None,
        // The index carries MCP configs and paths; none of it is kept raw.
        raw: Value::Null,
    }
}
