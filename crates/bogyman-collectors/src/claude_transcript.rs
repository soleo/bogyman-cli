//! Read the tail of a Claude Code transcript (`~/.claude/projects/<project>/<id>.jsonl`)
//! for metadata the hooks do not carry: the model that answered last and the
//! entrypoint. The file format is internal to Claude Code and changes between
//! versions, so this reads only what it recognises and fails soft.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use bogyman_core::{Event, EventKind, Host, Source, Vendor};
use serde_json::Value;
use time::OffsetDateTime;

use crate::claude_hooks::host_from_entrypoint;
use crate::util::str_field;

/// Windows to scan from the end of the file, smallest first. A turn's assistant
/// line is usually close to the end, but attachments and tool results can push
/// it back by megabytes in a long session.
const TAIL_WINDOWS: [u64; 3] = [256 * 1024, 2 * 1024 * 1024, 16 * 1024 * 1024];

/// Claude Code names the project directory after the working directory with
/// every non-alphanumeric character replaced by `-`.
pub fn project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn transcript_path(claude_dir: &Path, cwd: &str, session_id: &str) -> PathBuf {
    claude_dir
        .join("projects")
        .join(project_dir_name(cwd))
        .join(format!("{session_id}.jsonl"))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranscriptTail {
    pub model: Option<String>,
    pub host: Option<Host>,
}

/// Scan the last lines of a transcript for the newest assistant message.
pub fn read_tail(path: &Path) -> Option<TranscriptTail> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut best: Option<TranscriptTail> = None;
    for window in TAIL_WINDOWS {
        let start = len.saturating_sub(window);
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity((len - start) as usize);
        f.read_to_end(&mut buf).ok()?;
        let text = String::from_utf8_lossy(&buf);
        match parse_tail(&text) {
            Some(t) if t.model.is_some() => return Some(t),
            Some(t) => best = Some(t),
            None => {}
        }
        if start == 0 {
            break;
        }
    }
    best
}

/// The same scan over text already in memory (newest line last).
pub fn parse_tail(text: &str) -> Option<TranscriptTail> {
    let mut out = TranscriptTail::default();
    for line in text.lines().rev() {
        // A partial first line (we started mid-file) will fail to parse; skip it.
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if out.host.is_none()
            && let Some(e) = str_field(&v, &["entrypoint"])
        {
            out.host = host_from_entrypoint(&e);
        }
        if out.model.is_none() && v.get("type").and_then(Value::as_str) == Some("assistant") {
            out.model = v
                .get("message")
                .and_then(|m| str_field(m, &["model"]))
                .or_else(|| str_field(&v, &["requestedModel"]));
        }
        if out.model.is_some() && out.host.is_some() {
            break;
        }
    }
    (out.model.is_some() || out.host.is_some()).then_some(out)
}

pub fn event_for(
    session_id: &str,
    cwd: Option<&str>,
    tail: &TranscriptTail,
    at: OffsetDateTime,
) -> Event {
    Event {
        id: None,
        at,
        vendor: Vendor::ClaudeCode,
        vendor_session_id: session_id.to_string(),
        source: Source::Poll,
        kind: EventKind::TranscriptPoll {
            model: tail.model.clone(),
            host: tail.host,
        },
        cwd: cwd.map(String::from),
        pid: None,
        transcript_path: None,
        raw: Value::Null,
    }
}
