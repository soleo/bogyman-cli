//! Pins the `--json` contract, schema 1. Every command answers with one
//! envelope: `{schema, command, ok, data|error}`, on one line when piped.
//!
//! Hermetic: BOGYMAN_HOME, CLAUDE_CONFIG_DIR and CODEX_HOME all point at temp
//! directories (`Paths::detect` honors each), so nothing here reads or writes the
//! real `~/.claude`, `~/.codex` or event store.

use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// A bogyman run in a throwaway home, with no vendor binaries on PATH so the
/// result does not depend on what the developer happens to have installed.
fn run(args: &[&str]) -> (Output, TempDir) {
    let home = TempDir::new().expect("tempdir");
    let empty_path = home.path().join("nobin");
    std::fs::create_dir_all(&empty_path).expect("mkdir");
    let out = Command::new(env!("CARGO_BIN_EXE_bogyman"))
        .args(args)
        .env("BOGYMAN_HOME", home.path())
        .env("CLAUDE_CONFIG_DIR", home.path().join("claude"))
        .env("CODEX_HOME", home.path().join("codex"))
        .env("PATH", &empty_path)
        .env("NO_COLOR", "1")
        .output()
        .expect("run bogyman");
    (out, home)
}

/// Parse stdout and assert the invariants every envelope shares.
fn envelope(out: &Output, command: &str) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim_end().lines().count(),
        1,
        "{command}: piped --json must be one line, got:\n{stdout}"
    );
    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("{command}: stdout is not JSON ({e}):\n{stdout}"));
    assert_eq!(v["schema"], 1, "{command}: schema");
    assert_eq!(v["command"], command, "{command}: command");
    assert!(v["ok"].is_boolean(), "{command}: ok must be a bool");
    v
}

fn ok_envelope(out: &Output, command: &str) -> Value {
    let v = envelope(out, command);
    assert_eq!(v["ok"], true, "{command}: expected ok");
    assert!(v["data"].is_object(), "{command}: data must be an object");
    assert!(
        v.get("error").is_none(),
        "{command}: ok must carry no error"
    );
    v
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("exit code")
}

#[test]
fn ls_reports_an_empty_session_list() {
    let (out, _h) = run(&["ls", "--json"]);
    let v = ok_envelope(&out, "ls");
    assert_eq!(v["data"]["sessions"], serde_json::json!([]));
    assert_eq!(code(&out), 0);
}

#[test]
fn waiting_is_a_success_with_an_empty_list_but_still_exits_3() {
    let (out, _h) = run(&["waiting", "--json"]);
    let v = ok_envelope(&out, "waiting");
    assert_eq!(v["data"]["sessions"], serde_json::json!([]));
    assert_eq!(code(&out), 3, "empty result keeps its own exit code");
}

#[test]
fn status_counts_are_numbers() {
    let (out, _h) = run(&["status", "--json"]);
    let v = ok_envelope(&out, "status");
    for k in ["blocked", "working", "done", "failed", "stale", "total"] {
        assert!(v["data"][k].is_u64(), "status: {k} must be a number");
    }
    assert!(v["data"]["line"].is_string());
}

#[test]
fn version_and_gc_and_doctor_answer_with_an_envelope() {
    for cmd in ["version", "gc", "doctor"] {
        let (out, _h) = run(&[cmd, "--json"]);
        ok_envelope(&out, cmd);
        assert_eq!(code(&out), 0, "{cmd}");
    }
}

#[test]
fn install_and_uninstall_report_dry_runs() {
    // --claude forces a target even with nothing on PATH, so this is deterministic.
    let (out, _h) = run(&["install", "--claude", "--dry-run", "--json"]);
    let v = ok_envelope(&out, "install");
    assert_eq!(v["data"]["dry_run"], true);
    assert!(v["data"]["lines"].is_array());
    assert_eq!(code(&out), 0);

    let (out, _h) = run(&["uninstall", "--dry-run", "--json"]);
    let v = ok_envelope(&out, "uninstall");
    assert_eq!(v["data"]["dry_run"], true);
    assert_eq!(code(&out), 0);
}

#[test]
fn install_without_a_vendor_reports_exit_4_as_json() {
    let (out, _h) = run(&["install", "--json"]);
    let v = envelope(&out, "install");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["exit"], 4);
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("neither claude nor codex")),
        "unexpected message: {}",
        v["error"]["message"]
    );
    assert_eq!(code(&out), 4);
}

#[test]
fn a_failure_is_json_on_stdout_not_bare_text_on_stderr() {
    let (out, _h) = run(&["show", "definitely-no-such-session", "--json"]);
    let v = envelope(&out, "show");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["exit"], 1);
    assert!(
        !v["error"]["message"].as_str().unwrap_or("").is_empty(),
        "error.message must say something"
    );
    assert!(v.get("data").is_none(), "a failure carries no data");
    assert_eq!(code(&out), 1);
    assert!(
        out.stderr.is_empty(),
        "under --json the reason belongs on stdout"
    );
}

#[test]
fn the_envelope_names_the_command_that_failed() {
    let (out, _h) = run(&["send", "definitely-no-such-session", "hi", "--json"]);
    let v = envelope(&out, "send");
    assert_eq!(v["ok"], false);
    assert_eq!(code(&out), 1);
}
