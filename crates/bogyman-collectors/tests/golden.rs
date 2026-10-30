//! Golden-file tests: one fixture per vendor payload shape. A vendor format change
//! fails here rather than on a user's machine.

use std::path::PathBuf;

use bogyman_collectors::{
    claude_agents, claude_hooks, claude_sessions, claude_transcript, codex_hooks, codex_rollout,
    desktop_index,
};
use bogyman_core::{EventKind, Host, Kind, Source, Vendor, WaitingFor};
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

#[test]
fn transcript_tail_yields_the_last_models_name_and_the_entrypoint() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/claude/2.1/transcript_tail.jsonl");
    let tail = claude_transcript::read_tail(&path).unwrap();
    assert_eq!(tail.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(tail.host, Some(Host::Terminal));

    // A truncated first line (mid-file start) and trailing non-assistant lines are fine.
    let text = "ssion\":\"x\"}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-sonnet-5-5\"}}\n{\"type\":\"progress\"}\n";
    assert_eq!(
        claude_transcript::parse_tail(text)
            .unwrap()
            .model
            .as_deref(),
        Some("claude-sonnet-5-5")
    );
    assert!(claude_transcript::parse_tail("{\"type\":\"user\"}\n").is_none());

    assert_eq!(
        claude_transcript::project_dir_name("/Users/x/dev/tag-web"),
        "-Users-x-dev-tag-web"
    );
    let p = claude_transcript::transcript_path(
        std::path::Path::new("/home/u/.claude"),
        "/srv/app.v2",
        "abc",
    );
    assert_eq!(
        p,
        PathBuf::from("/home/u/.claude/projects/-srv-app-v2/abc.jsonl")
    );
}

#[test]
fn claude_agents_json_maps_each_entry_to_a_status_poll() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/claude/2.1/agents_json.json");
    let text = std::fs::read_to_string(path).unwrap();
    let events = claude_agents::parse(&text, now());
    // The entry with neither sessionId nor id can't be tied to a session.
    assert_eq!(events.len(), 3);

    let interactive = &events[0];
    assert_eq!(
        interactive.vendor_session_id,
        "1f0e2c9a-6d0b-4c11-9f39-2a77c1d4e8b5"
    );
    assert_eq!(interactive.pid, Some(48122));
    assert!(matches!(
        &interactive.kind,
        EventKind::StatusPoll { status, kind: Some(Kind::Interactive), host: None, agent_id: None, .. } if status == "busy"
    ));

    let blocked = &events[1];
    assert_eq!(blocked.source, Source::Poll);
    match &blocked.kind {
        EventKind::StatusPoll {
            status,
            host,
            waiting_for,
            agent_id,
            started_at,
            ..
        } => {
            assert_eq!(status, "blocked", "state wins over status");
            assert_eq!(*host, Some(Host::AgentView));
            assert_eq!(*waiting_for, Some(WaitingFor::PermissionPrompt));
            assert_eq!(agent_id.as_deref(), Some("7c5dcf5d"));
            assert_eq!(started_at.unwrap().unix_timestamp(), 1_793_052_900);
        }
        other => panic!("{other:?}"),
    }

    // A finished background session with no UUID falls back to its short id.
    let done = &events[2];
    assert_eq!(done.vendor_session_id, "a1b2c3d4");
    assert_eq!(done.pid, None);
    assert!(matches!(&done.kind, EventKind::StatusPoll { status, .. } if status == "done"));
}

#[test]
fn claude_agents_output_that_is_not_an_array_yields_nothing() {
    assert!(claude_agents::parse("", now()).is_empty());
    assert!(claude_agents::parse("{\"error\":\"not logged in\"}", now()).is_empty());
}

#[test]
fn codex_rollout_yields_model_tokens_last_reply_and_turn_status() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/codex/0.164");
    let files = codex_rollout::recent_files(&root.join("sessions"), std::time::UNIX_EPOCH);
    assert_eq!(files.len(), 1);
    let r = codex_rollout::read(&files[0].0).unwrap();
    assert_eq!(r.session_id, "01a17abd-085b-7d12-b8ba-48afcdfd4f9c");
    assert_eq!(r.cwd.as_deref(), Some("/Users/dev/deps-audit"));
    assert_eq!(r.host, Some(Host::Terminal));
    assert_eq!(r.model.as_deref(), Some("gpt-5.5-codex"));
    assert_eq!(
        r.tokens,
        Some(bogyman_core::Tokens {
            input: 5400,
            output: 410
        }),
        "the newest total wins"
    );
    assert_eq!(r.status.as_deref(), Some("idle"));
    assert_eq!(
        r.last_message.as_deref(),
        Some("Two packages are behind; pinned both.")
    );

    let names = codex_rollout::thread_names(&root.join("session_index.jsonl"));
    let ev = codex_rollout::event_for(&r, names.get(&r.session_id).cloned(), now());
    assert_eq!(ev.vendor, Vendor::Codex);
    assert_eq!(ev.source, Source::Poll);
    assert!(ev.raw.is_null(), "rollout content never rides along raw");
    assert_eq!(
        ev.at,
        r.status_at.unwrap(),
        "dated by the turn boundary, not the poll"
    );
    assert!(
        matches!(&ev.kind, EventKind::RolloutPoll { name: Some(n), .. } if n == "deps-audit"),
        "a later index line wins: {:?}",
        ev.kind
    );
}

#[test]
fn codex_rollout_scan_ignores_files_older_than_the_cutoff() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/codex/0.164");
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    assert!(codex_rollout::recent_files(&root.join("sessions"), future).is_empty());
}

#[test]
fn desktop_index_entries_link_by_cli_session_id_and_carry_the_summary() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/claude-desktop/4.4212");
    let mut files = desktop_index::recent_files(&root, std::time::UNIX_EPOCH);
    files.sort();
    assert_eq!(files.len(), 3);
    let entries: Vec<_> = files
        .iter()
        .filter_map(|f| desktop_index::read(f))
        .collect();
    // The entry without a cliSessionId can't be linked to anything.
    assert_eq!(entries.len(), 2);

    let done = &entries[0];
    assert_eq!(done.cli_session_id, "1f0e2c9a-6d0b-4c11-9f39-2a77c1d4e8b5");
    assert_eq!(done.title.as_deref(), Some("tag-web"));
    assert_eq!(
        done.summary.as_deref(),
        Some("Fixed the flaky retry test and pushed the branch"),
        "an empty needs_action adds nothing"
    );
    let asking = &entries[1];
    assert_eq!(
        asking.summary.as_deref(),
        Some("needs: Pick a migration name")
    );

    let ev = desktop_index::event_for(done, now());
    assert_eq!(ev.vendor, Vendor::ClaudeCode);
    assert_eq!(ev.source, Source::Poll);
    assert!(
        ev.raw.is_null(),
        "MCP configs and paths never ride along raw"
    );
    assert!(matches!(ev.kind, EventKind::IndexPoll { .. }));
}
