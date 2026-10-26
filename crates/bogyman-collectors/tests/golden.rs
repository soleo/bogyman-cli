//! Golden-file tests: one fixture per vendor payload shape. A vendor format change
//! fails here rather than on a user's machine.

use std::path::PathBuf;

use bogyman_collectors::{claude_hooks, claude_sessions, codex_hooks};
use bogyman_core::{EventKind, Host, Kind, Source, Vendor};
use serde_json::Value;
use time::OffsetDateTime;

fn fixture(rel: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

type Check = Box<dyn Fn(&EventKind) -> bool>;

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_793_054_000).unwrap()
}

#[test]
fn claude_payloads_map_to_the_expected_events() {
    let cases: Vec<(&str, Check)> = vec![
        (
            "session_start.json",
            Box::new(|k| matches!(k, EventKind::SessionStart { .. })),
        ),
        (
            "user_prompt_submit.json",
            Box::new(|k| matches!(k, EventKind::PromptSubmitted)),
        ),
        (
            "pre_tool_use.json",
            Box::new(
                |k| matches!(k, EventKind::ToolUse { tool, detail } if tool == "Bash" && detail.as_deref() == Some("Bash: npm test")),
            ),
        ),
        (
            "pre_tool_use_ask_user_question.json",
            Box::new(
                |k| matches!(k, EventKind::ToolUse { tool, detail } if tool == "AskUserQuestion" && detail.as_deref() == Some("AskUserQuestion: Which auth library should we use?")),
            ),
        ),
        (
            "permission_request.json",
            Box::new(
                |k| matches!(k, EventKind::PermissionRequest { tool, detail } if tool == "Bash" && detail.as_deref() == Some("Bash: npm test")),
            ),
        ),
        (
            "notification_permission_prompt.json",
            Box::new(
                |k| matches!(k, EventKind::Notification { notification_type, .. } if notification_type == "permission_prompt"),
            ),
        ),
        (
            "notification_idle_prompt.json",
            Box::new(
                |k| matches!(k, EventKind::Notification { notification_type, .. } if notification_type == "idle_prompt"),
            ),
        ),
        (
            "stop.json",
            Box::new(
                |k| matches!(k, EventKind::Stop { message: Some(m) } if m.starts_with("Fixed the flaky")),
            ),
        ),
        (
            "stop_failure.json",
            Box::new(
                |k| matches!(k, EventKind::StopFailure { error: Some(e) } if e == "rate_limit"),
            ),
        ),
        (
            "task_created.json",
            Box::new(
                |k| matches!(k, EventKind::TaskCreated { subject: Some(s) } if s == "write integration tests"),
            ),
        ),
        (
            "task_completed.json",
            Box::new(|k| matches!(k, EventKind::TaskCompleted { subject: Some(_) })),
        ),
        (
            "session_end.json",
            Box::new(|k| matches!(k, EventKind::SessionEnd { reason: Some(r) } if r == "other")),
        ),
        (
            "subagent_stop.json",
            Box::new(|k| matches!(k, EventKind::Unknown { name } if name == "SubagentStop")),
        ),
    ];
    for (file, check) in cases {
        let ev = claude_hooks::parse(fixture(&format!("claude/2.1/{file}")), now())
            .unwrap_or_else(|e| panic!("{file}: {e}"));
        assert_eq!(ev.vendor, Vendor::ClaudeCode, "{file}");
        assert_eq!(ev.source, Source::Hook, "{file}");
        assert_eq!(
            ev.vendor_session_id, "1f0e2c9a-6d0b-4c11-9f39-2a77c1d4e8b5",
            "{file}"
        );
        assert_eq!(ev.cwd.as_deref(), Some("/Users/x/dev/tag-web"), "{file}");
        assert!(ev.transcript_path.is_some(), "{file}");
        assert!(check(&ev.kind), "{file}: unexpected {:?}", ev.kind);
        assert!(ev.raw.is_object(), "{file}: raw payload kept");
    }
}

#[test]
fn claude_payload_without_session_id_is_rejected() {
    let err =
        claude_hooks::parse(serde_json::json!({"hook_event_name": "Stop"}), now()).unwrap_err();
    assert!(err.to_string().contains("session id"));
    assert!(claude_hooks::parse(serde_json::json!([1, 2]), now()).is_err());
}

#[test]
fn claude_sessions_file_becomes_a_status_poll() {
    let raw = fixture("claude/2.1/sessions_file.json");
    let ev = claude_sessions::parse_file(&raw, now()).unwrap();
    assert_eq!(ev.source, Source::Poll);
    assert_eq!(ev.pid, Some(48122));
    assert_eq!(ev.at.unix_timestamp(), 1_793_054_047);
    match ev.kind {
        EventKind::StatusPoll {
            status,
            name,
            kind,
            host,
            started_at,
            ..
        } => {
            assert_eq!(status, "waiting");
            assert_eq!(name.as_deref(), Some("flaky-test-fix"));
            assert_eq!(kind, Some(Kind::Interactive));
            assert_eq!(host, Some(Host::Terminal));
            assert_eq!(started_at.unwrap().unix_timestamp(), 1_793_052_350);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn claude_sessions_poll_reads_a_directory() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/claude/2.1");
    let events = claude_sessions::poll(&dir, now());
    // Only the sessions file carries a sessionId; hook fixtures use session_id and are skipped.
    assert_eq!(events.len(), 1);
}

#[test]
fn codex_payloads_map_to_the_expected_events() {
    let ev = codex_hooks::parse_notify(
        fixture("codex/0.130/notify_agent_turn_complete.json"),
        now(),
    )
    .unwrap();
    assert_eq!(ev.vendor, Vendor::Codex);
    assert_eq!(ev.vendor_session_id, "b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e");
    assert_eq!(ev.cwd.as_deref(), Some("/Users/x/dev/hapio"));
    assert!(
        matches!(ev.kind, EventKind::Stop { message: Some(ref m) } if m.starts_with("Found 3"))
    );

    let ev = codex_hooks::parse_hook(fixture("codex/0.130/hook_permission_request.json"), now())
        .unwrap();
    assert!(
        matches!(ev.kind, EventKind::PermissionRequest { ref tool, ref detail } if tool == "shell" && detail.as_deref() == Some("shell: pnpm install --frozen-lockfile"))
    );

    let ev =
        codex_hooks::parse_hook(fixture("codex/0.130/hook_session_start.json"), now()).unwrap();
    assert!(
        matches!(ev.kind, EventKind::SessionStart { model: Some(ref m), .. } if m == "gpt-5.4-codex")
    );

    let ev = codex_hooks::parse_hook(fixture("codex/0.130/hook_stop.json"), now()).unwrap();
    assert!(matches!(ev.kind, EventKind::Stop { .. }));

    // An unknown notify type degrades to Unknown, never an error.
    let ev =
        codex_hooks::parse_notify(fixture("codex/0.130/notify_unknown_type.json"), now()).unwrap();
    assert!(matches!(ev.kind, EventKind::Unknown { ref name } if name == "something-new"));
}
