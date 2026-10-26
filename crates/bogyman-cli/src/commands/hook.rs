//! The hook entry point. Invoked by the vendor as the user; reads one JSON payload,
//! appends one row, exits 0. Never prints to stdout, never returns a decision.
//! Problems go to the log, because a failing hook must not break anyone's agent.

use std::io::Read;

use bogyman_collectors::{claude_hooks, codex_hooks};
use bogyman_core::Vendor;
use bogyman_store::Store;
use time::OffsetDateTime;

use crate::config::Config;
use crate::paths::Paths;

const MAX_PAYLOAD: u64 = 4 * 1024 * 1024;

pub fn run(paths: &Paths, vendor: Vendor, notify: Option<&str>) {
    let text = match notify {
        Some(arg) => arg.to_string(),
        None => {
            let mut buf = String::new();
            if let Err(e) = std::io::stdin().take(MAX_PAYLOAD).read_to_string(&mut buf) {
                paths.log(&format!("hook {}: reading stdin: {e}", vendor.label()));
                return;
            }
            buf
        }
    };
    if text.trim().is_empty() {
        paths.log(&format!("hook {}: empty payload", vendor.label()));
        return;
    }
    let raw: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            paths.log(&format!(
                "hook {}: payload is not JSON: {e}",
                vendor.label()
            ));
            return;
        }
    };
    let now = OffsetDateTime::now_utc();
    let parsed = match (vendor, notify.is_some()) {
        (Vendor::ClaudeCode, _) => claude_hooks::parse(raw, now),
        (Vendor::Codex, true) => codex_hooks::parse_notify(raw, now),
        (Vendor::Codex, false) => codex_hooks::parse_hook(raw, now),
    };
    let mut ev = match parsed {
        Ok(ev) => ev,
        Err(e) => {
            paths.log(&format!("hook {}: {e}", vendor.label()));
            return;
        }
    };
    // Privacy rules apply before anything touches disk. Config is one small file.
    ev.sanitize(Config::load(&paths.config).store.redact);
    match Store::open(&paths.db).and_then(|s| s.append(&ev)) {
        Ok(_) => {}
        Err(e) => paths.log(&format!("hook {}: store: {e}", vendor.label())),
    }
}
