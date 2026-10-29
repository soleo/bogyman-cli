//! Diagnose the setup: vendor versions, hooks registered, store health, last events.

use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use bogyman_collectors::{claude_hooks, codex_hooks};
use bogyman_core::Vendor;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::commands::install::{MARKER, notify_is_ours};
use crate::paths::tilde;
use crate::render::{ago, print_rows};
use crate::{Ctx, EXIT_OK, json, refresh};

pub const MIN_CLAUDE: (u64, u64, u64) = (2, 1, 139);
pub const MIN_CODEX: (u64, u64, u64) = (0, 130, 0);

#[derive(Debug, Clone)]
pub struct VendorProbe {
    pub name: &'static str,
    pub found: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub semver: Option<(u64, u64, u64)>,
}

impl VendorProbe {
    pub fn minimum(&self) -> (u64, u64, u64) {
        if self.name == "claude" {
            MIN_CLAUDE
        } else {
            MIN_CODEX
        }
    }

    pub fn meets_minimum(&self) -> Option<bool> {
        self.semver.map(|v| v >= self.minimum())
    }

    pub fn describe(&self) -> String {
        if !self.found {
            return "not found on PATH".into();
        }
        let v = self
            .version
            .clone()
            .unwrap_or_else(|| "version unknown".into());
        let p = self.path.as_deref().map(tilde).unwrap_or_default();
        let min = self.minimum();
        let ok = match self.meets_minimum() {
            Some(true) => format!("≥ {}.{}.{} ok", min.0, min.1, min.2),
            Some(false) => format!("BELOW minimum {}.{}.{}", min.0, min.1, min.2),
            None => String::new(),
        };
        format!("{v} at {p}  {ok}").trim_end().to_string()
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let num: String = s[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut it = num.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    Some((it.next()?, it.next().unwrap_or(0), it.next().unwrap_or(0)))
}

pub fn probe_vendor(name: &'static str) -> VendorProbe {
    let path = which(name);
    let version = path.as_ref().and_then(|p| {
        Command::new(p)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
            .filter(|s| !s.is_empty())
    });
    VendorProbe {
        name,
        found: path.is_some(),
        semver: version.as_deref().and_then(parse_semver),
        path,
        version,
    }
}

/// How many of `events` have one of our handlers registered in `file`.
fn hooks_registered(file: &std::path::Path, events: &[&str]) -> (usize, Vec<String>) {
    let root: Value = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let mut missing = Vec::new();
    let mut n = 0;
    for ev in events {
        let ok = root["hooks"][*ev].as_array().is_some_and(|groups| {
            groups.iter().any(|g| {
                g["hooks"].as_array().is_some_and(|hs| {
                    hs.iter()
                        .any(|h| h["command"].as_str().is_some_and(|c| c.contains(MARKER)))
                })
            })
        });
        if ok {
            n += 1;
        } else {
            missing.push(ev.to_string());
        }
    }
    (n, missing)
}

pub fn run(ctx: &Ctx, verbose: bool) -> Result<u8> {
    let now = OffsetDateTime::now_utc();
    let store = refresh::open_store(ctx)?;
    let sessions = refresh::load_sessions(ctx, true)?;
    let live = sessions.iter().filter(|s| s.state.is_live()).count();
    let events = store.event_count()?;
    let db_size = std::fs::metadata(&ctx.paths.db)
        .map(|m| m.len())
        .unwrap_or(0);
    let last = store.last_event_per_vendor()?;
    let last_for = |v: Vendor| {
        last.iter()
            .find(|(lv, _, _)| *lv == v)
            .map(|(_, at, kind)| format!("{} ({kind})", ago(*at, now)))
            .unwrap_or_else(|| "never".into())
    };

    let claude = probe_vendor("claude");
    let codex = probe_vendor("codex");
    let (c_hooks, c_missing) =
        hooks_registered(&ctx.paths.claude_settings, claude_hooks::HOOK_EVENTS);
    let (x_hooks, x_missing) = hooks_registered(&ctx.paths.codex_hooks, codex_hooks::HOOK_EVENTS);
    let sessions_files = std::fs::read_dir(&ctx.paths.claude_sessions_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .count()
        })
        .unwrap_or(0);
    let codex_notify = std::fs::read_to_string(&ctx.paths.codex_config)
        .ok()
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|d| d.get("notify").map(notify_is_ours));

    let mut warnings: Vec<String> = Vec::new();
    // The poller the daemon and `ls --watch` run. Running it here also proves it works.
    let agents_poll: Result<usize, String> = if !ctx.config.collectors.claude_agents {
        Err("disabled in config".into())
    } else if !claude.found {
        Err("claude not on PATH".into())
    } else {
        refresh::poll_claude_agents(ctx).map_err(|e| format!("{e:#}"))
    };
    if claude.found
        && ctx.config.collectors.claude_agents
        && let Err(e) = &agents_poll
    {
        warnings.push(format!(
            "claude: `claude agents --json` failed ({e}); background sessions won't appear"
        ));
    }
    if claude.found && c_hooks < claude_hooks::HOOK_EVENTS.len() {
        warnings.push(format!(
            "claude: {} hook(s) missing ({}); run `bogyman install`",
            c_missing.len(),
            c_missing.join(", ")
        ));
    }
    if claude.meets_minimum() == Some(false) {
        warnings.push("claude: below the minimum version; some signals will be missing".into());
    }
    if codex.found && x_hooks < codex_hooks::HOOK_EVENTS.len() {
        warnings.push(format!(
            "codex: {} hook(s) missing; run `bogyman install --codex`",
            x_missing.len()
        ));
    }
    if codex.meets_minimum() == Some(false) {
        warnings.push("codex: below the minimum version for hooks; notify fallback only".into());
    }
    if claude.found && c_hooks > 0 && !last.iter().any(|(v, _, _)| *v == Vendor::ClaudeCode) {
        warnings.push(
            "claude: hooks registered but no event received yet; start a session and run a prompt"
                .into(),
        );
    }

    let daemon_pid = crate::commands::daemon::running_pid(ctx);
    let daemon_state =
        crate::commands::daemon::read_state(ctx).filter(|s| Some(s.pid) == daemon_pid);
    if let Some(st) = &daemon_state
        && (now - st.last_tick).whole_seconds() > 30
    {
        warnings.push(format!(
            "daemon: running but its last tick was {} ago; see {}",
            ago(st.last_tick, now),
            tilde(&ctx.paths.log)
        ));
    }

    if ctx.json {
        json::ok(
            ctx,
            "doctor",
            serde_json::json!({
                "bogyman": env!("CARGO_PKG_VERSION"),
                "build": {"commit": crate::commands::version::GIT_COMMIT, "built_at": crate::commands::version::BUILT_AT, "target": crate::commands::version::TARGET},
                "store": {"path": ctx.paths.db, "bytes": db_size, "events": events, "sessions": sessions.len(), "live": live},
                "config": {"path": ctx.paths.config, "present": ctx.paths.config.exists()},
                "claude": {"found": claude.found, "path": claude.path, "version": claude.version, "meets_minimum": claude.meets_minimum(),
                           "hooks_registered": c_hooks, "hooks_expected": claude_hooks::HOOK_EVENTS.len(), "hooks_missing": c_missing,
                           "sessions_dir": ctx.paths.claude_sessions_dir, "sessions_files": sessions_files,
                           "agents_poll": match &agents_poll { Ok(n) => serde_json::json!({"ok": true, "events_added": n}), Err(e) => serde_json::json!({"ok": false, "error": e}) },
                           "last_event": last_for(Vendor::ClaudeCode)},
                "codex": {"found": codex.found, "path": codex.path, "version": codex.version, "meets_minimum": codex.meets_minimum(),
                          "hooks_registered": x_hooks, "hooks_expected": codex_hooks::HOOK_EVENTS.len(), "hooks_missing": x_missing,
                          "notify_registered": codex_notify, "last_event": last_for(Vendor::Codex)},
                "daemon": {"running": daemon_pid.is_some(), "pid": daemon_pid, "state": daemon_state},
                "warnings": warnings,
            }),
        )?;
        return Ok(EXIT_OK);
    }

    println!("bogyman   {}", crate::commands::version::LINE);
    println!(
        "store     {}  ({} KB, {} events, {} sessions, {} live)",
        tilde(&ctx.paths.db),
        db_size / 1024,
        events,
        sessions.len(),
        live
    );
    println!(
        "config    {}  {}",
        tilde(&ctx.paths.config),
        if ctx.paths.config.exists() {
            ""
        } else {
            "(missing, using defaults)"
        }
    );
    println!("log       {}", tilde(&ctx.paths.log));
    println!(
        "daemon    {}",
        match (daemon_pid, &daemon_state) {
            (Some(pid), Some(st)) =>
                format!("running (pid {pid}), last tick {}", ago(st.last_tick, now)),
            (Some(pid), None) => format!("running (pid {pid})"),
            (None, _) =>
                "not running (optional: background sessions and notifications need it)".into(),
        }
    );
    println!();
    println!("claude    {}", claude.describe());
    println!(
        "  hooks        {}/{} registered in {}",
        c_hooks,
        claude_hooks::HOOK_EVENTS.len(),
        tilde(&ctx.paths.claude_settings)
    );
    println!(
        "  sessions dir {} file(s) in {}",
        sessions_files,
        tilde(&ctx.paths.claude_sessions_dir)
    );
    println!(
        "  agents poll  {}",
        match &agents_poll {
            Ok(n) => format!("ok, {n} change(s) recorded"),
            Err(e) => e.clone(),
        }
    );
    println!("  last event   {}", last_for(Vendor::ClaudeCode));
    println!();
    println!("codex     {}", codex.describe());
    println!(
        "  hooks        {}/{} registered in {}",
        x_hooks,
        codex_hooks::HOOK_EVENTS.len(),
        tilde(&ctx.paths.codex_hooks)
    );
    println!(
        "  notify       {}",
        match codex_notify {
            Some(true) => "bogyman".to_string(),
            Some(false) => "set to something else".to_string(),
            None => format!("not set in {}", tilde(&ctx.paths.codex_config)),
        }
    );
    println!("  last event   {}", last_for(Vendor::Codex));

    if !warnings.is_empty() {
        println!();
        for w in &warnings {
            println!("warning   {w}");
        }
    }

    if verbose {
        println!();
        let rows = store
            .recent_events(50)?
            .into_iter()
            .map(|e| {
                vec![
                    e.at.format(&Rfc3339).unwrap_or_default(),
                    e.vendor.label().into(),
                    bogyman_core::Session::short_id(e.vendor, &e.vendor_session_id),
                    e.kind.label(),
                    e.kind.detail().unwrap_or("").chars().take(50).collect(),
                ]
            })
            .collect();
        print_rows(ctx, &["AT", "VENDOR", "SESSION", "EVENT", "DETAIL"], rows);
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::parse_semver;

    #[test]
    fn parses_vendor_version_lines() {
        assert_eq!(parse_semver("2.1.308 (Claude Code)"), Some((2, 1, 308)));
        assert_eq!(parse_semver("codex-cli 0.130.0"), Some((0, 130, 0)));
        assert_eq!(parse_semver("v1.2"), Some((1, 2, 0)));
        assert_eq!(parse_semver("nope"), None);
    }
}
