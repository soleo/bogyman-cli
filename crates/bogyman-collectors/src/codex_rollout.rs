//! Tail Codex rollout files (`~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`)
//! for what Codex hooks don't carry: the model, token totals, the last reply and
//! whether a turn is running. The format is internal to Codex, so this reads
//! only the line types it knows and fails soft on everything else.
//!
//! Lines are `{"timestamp", "type", "payload"}`. The first is `session_meta`
//! (id, cwd, source). Later ones that matter: `turn_context` (model),
//! `event_msg` with `payload.type` `task_started` / `task_complete` /
//! `turn_aborted` (turn boundaries; `task_complete` has `last_agent_message`)
//! and `token_count` (`info.total_token_usage`).
//!
//! Approval prompts are not written to the rollout, so a blocked Codex session
//! still comes from the `PermissionRequest` hook.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bogyman_core::{Event, EventKind, Host, Source, Tokens, Vendor};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::util::{str_field, u64_field};

/// How far back from the end of a rollout to look for the latest turn.
const TAIL_BYTES: u64 = 512 * 1024;
/// Day directories to scan, newest first. Covers a session started late
/// yesterday that is still running.
const DAY_DIRS: usize = 3;

/// What one rollout says about its session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rollout {
    pub session_id: String,
    pub cwd: Option<String>,
    pub host: Option<Host>,
    pub model: Option<String>,
    pub tokens: Option<Tokens>,
    /// `working` while a turn runs, `idle` once it completed or was aborted.
    pub status: Option<String>,
    /// When the latest turn boundary happened.
    pub status_at: Option<OffsetDateTime>,
    pub last_message: Option<String>,
    pub started_at: Option<OffsetDateTime>,
}

/// The thread id in a rollout's file name (`rollout-2026-10-26T19-22-51-<uuid>.jsonl`).
pub fn id_from_file_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let rest = stem.strip_prefix("rollout-")?;
    // The uuid is the last 36 characters; the timestamp before it has dashes too.
    (rest.len() > 36).then(|| rest[rest.len() - 36..].to_string())
}

/// Rollout files modified since `since`, newest day directories first.
pub fn recent_files(sessions_dir: &Path, since: SystemTime) -> Vec<(PathBuf, SystemTime)> {
    let mut days = Vec::new();
    for year in sorted_dirs(sessions_dir) {
        for month in sorted_dirs(&year) {
            days.extend(sorted_dirs(&month));
        }
    }
    days.sort();
    days.reverse();
    let mut out = Vec::new();
    for day in days.into_iter().take(DAY_DIRS) {
        let Ok(rd) = std::fs::read_dir(&day) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(mtime) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
                continue;
            };
            if mtime >= since {
                out.push((path, mtime));
            }
        }
    }
    out
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

/// Read one rollout: its first line and the tail.
pub fn read(path: &Path) -> Option<Rollout> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(&mut f).read_line(&mut first).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let tail = String::from_utf8_lossy(&buf);
    let mut r = parse(&first, &tail)?;
    if r.session_id.is_empty() {
        r.session_id = id_from_file_name(path)?;
    }
    Some(r)
}

/// Parse a rollout from its first line and a tail (newest line last). The tail
/// may begin mid-line; unparseable lines are skipped.
pub fn parse(first_line: &str, tail: &str) -> Option<Rollout> {
    let meta: Value = serde_json::from_str(first_line.trim()).ok()?;
    if meta.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let p = &meta["payload"];
    let mut r = Rollout {
        session_id: str_field(p, &["id", "session_id"]).unwrap_or_default(),
        cwd: str_field(p, &["cwd"]),
        host: str_field(p, &["source"]).and_then(|s| host_from_source(&s)),
        started_at: str_field(p, &["timestamp"])
            .or_else(|| str_field(&meta, &["timestamp"]))
            .and_then(|t| OffsetDateTime::parse(&t, &Rfc3339).ok()),
        ..Rollout::default()
    };
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at =
            str_field(&v, &["timestamp"]).and_then(|t| OffsetDateTime::parse(&t, &Rfc3339).ok());
        let p = &v["payload"];
        match v.get("type").and_then(Value::as_str) {
            Some("turn_context") if r.model.is_none() => r.model = str_field(p, &["model"]),
            Some("event_msg") => match p.get("type").and_then(Value::as_str) {
                Some("task_started") if r.status.is_none() => {
                    r.status = Some("working".into());
                    r.status_at = at;
                }
                Some("task_complete") if r.status.is_none() => {
                    r.status = Some("idle".into());
                    r.status_at = at;
                    r.last_message = str_field(p, &["last_agent_message"]);
                }
                Some("turn_aborted") if r.status.is_none() => {
                    r.status = Some("idle".into());
                    r.status_at = at;
                }
                Some("token_count") if r.tokens.is_none() => {
                    let total = &p["info"]["total_token_usage"];
                    if let (Some(input), Some(output)) = (
                        u64_field(total, &["input_tokens"]),
                        u64_field(total, &["output_tokens"]),
                    ) {
                        r.tokens = Some(Tokens { input, output });
                    }
                }
                _ => {}
            },
            _ => {}
        }
        if r.model.is_some() && r.status.is_some() && r.tokens.is_some() {
            break;
        }
    }
    Some(r)
}

/// `session_meta.source` says which front end started the thread.
fn host_from_source(s: &str) -> Option<Host> {
    match s {
        "vscode" => Some(Host::Vscode),
        "cli" | "exec" | "tui" => Some(Host::Terminal),
        _ => None,
    }
}

/// Thread names from `~/.codex/session_index.jsonl` (`{"id","thread_name"}` per
/// line; a later line wins).
pub fn thread_names(index: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(index) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let (Some(id), Some(name)) = (str_field(&v, &["id"]), str_field(&v, &["thread_name"])) {
            out.insert(id, name);
        }
    }
    out
}

/// The event a rollout becomes. `fallback_at` (the file's mtime) dates it when
/// no turn boundary has been written yet.
pub fn event_for(r: &Rollout, name: Option<String>, fallback_at: OffsetDateTime) -> Event {
    Event {
        id: None,
        at: r.status_at.unwrap_or(fallback_at),
        vendor: Vendor::Codex,
        vendor_session_id: r.session_id.clone(),
        source: Source::Poll,
        kind: EventKind::RolloutPoll {
            status: r.status.clone(),
            name,
            host: r.host,
            model: r.model.clone(),
            tokens: r.tokens.clone(),
            summary: r.last_message.clone(),
            started_at: r.started_at,
        },
        cwd: r.cwd.clone(),
        pid: None,
        transcript_path: None,
        // The rollout holds conversation content; none of it is kept raw.
        raw: Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_the_last_36_characters_of_the_file_stem() {
        let p = Path::new(
            "/x/2026/10/26/rollout-2026-10-26T19-22-51-01a17abd-085b-7d12-b8ba-48afcdfd4f9c.jsonl",
        );
        assert_eq!(
            id_from_file_name(p).as_deref(),
            Some("01a17abd-085b-7d12-b8ba-48afcdfd4f9c")
        );
        assert_eq!(id_from_file_name(Path::new("/x/notes.jsonl")), None);
    }

    #[test]
    fn a_file_that_does_not_start_with_session_meta_is_not_a_rollout() {
        assert!(parse(r#"{"type":"event_msg","payload":{}}"#, "").is_none());
        assert!(parse("not json", "").is_none());
    }

    #[test]
    fn a_started_turn_after_a_completed_one_means_working() {
        let first = r#"{"timestamp":"2026-10-27T00:22:51Z","type":"session_meta","payload":{"id":"t1","cwd":"/w","source":"cli"}}"#;
        let tail = [
            r#"{"timestamp":"2026-10-27T00:23:00Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":"first"}}"#,
            r#"{"timestamp":"2026-10-27T00:24:00Z","type":"event_msg","payload":{"type":"task_started"}}"#,
        ]
        .join("\n");
        let r = parse(first, &tail).unwrap();
        assert_eq!(r.status.as_deref(), Some("working"));
        assert_eq!(r.status_at.unwrap().unix_timestamp(), 1_793_060_640);
        assert_eq!(r.last_message, None, "the running turn has no reply yet");
        assert_eq!(r.host, Some(Host::Terminal));
    }
}
