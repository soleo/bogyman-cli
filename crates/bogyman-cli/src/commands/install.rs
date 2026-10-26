//! Register and remove bogyman's hooks.
//!
//! Claude Code: `~/.claude/settings.json`, one handler group per event under
//! `hooks.<Event>`. Codex: `~/.codex/hooks.json` in the same shape, plus `notify`
//! in `~/.codex/config.toml` for versions without hooks. Ours are recognised by
//! their command (`<exe> hook <vendor>`), so no foreign keys are added to the
//! vendor's schema. Writes are atomic (temp file + rename) after a `.bogyman-bak`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use bogyman_collectors::{claude_hooks, codex_hooks};
use serde_json::{Value, json};

use crate::commands::doctor::{VendorProbe, probe_vendor};
use crate::paths::tilde;
use crate::{Ctx, EXIT_OK, EXIT_VENDOR_UNAVAILABLE};

pub const MARKER: &str = "bogyman hook";

#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    pub changed: bool,
}

impl Report {
    fn say(&mut self, s: impl Into<String>) {
        self.lines.push(s.into());
    }
}

fn exe() -> Result<PathBuf> {
    let p = std::env::current_exe().context("locating the bogyman binary")?;
    Ok(p.canonicalize().unwrap_or(p))
}

/// The command string we register. Quoted if the path has spaces.
pub fn hook_command(exe: &Path, vendor: &str, extra: &str) -> String {
    let p = exe.to_string_lossy();
    let p = if p.contains(' ') {
        format!("\"{p}\"")
    } else {
        p.into_owned()
    };
    let mut s = format!("{p} hook {vendor}");
    if !extra.is_empty() {
        s.push(' ');
        s.push_str(extra);
    }
    s
}

/// Is a `notify` TOML item ours? Codex stores argv as an array, so join it first.
pub fn notify_is_ours(item: &toml_edit::Item) -> bool {
    let joined = match item.as_array() {
        Some(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        None => item.to_string(),
    };
    joined.contains(MARKER)
}

fn is_ours(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(MARKER))
}

fn read_json(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text).with_context(|| {
            format!(
                "{} is not valid JSON; fix it or restore the backup",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn write_atomic(path: &Path, contents: &str, backup: bool) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if backup && path.exists() {
        let bak = path.with_file_name(format!(
            "{}.bogyman-bak",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::copy(path, &bak).with_context(|| format!("writing backup {}", bak.display()))?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.bogyman-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    // Keep the original mode so a 0600 settings file never becomes 0644; a new
    // file is private, since vendor settings can hold tokens.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o7777)
            .unwrap_or(0o600);
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("setting permissions on {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Add our handler group for each event that lacks one. Returns the events added.
pub fn merge_hooks(root: &mut Value, events: &[&str], command: &str) -> Vec<String> {
    if !root.is_object() {
        *root = json!({});
    }
    let obj = root.as_object_mut().unwrap();
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().unwrap();
    let mut added = Vec::new();
    for ev in events {
        let groups = hooks.entry(*ev).or_insert_with(|| json!([]));
        if !groups.is_array() {
            *groups = json!([]);
        }
        let groups = groups.as_array_mut().unwrap();
        let present = groups.iter().any(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|hs| hs.iter().any(is_ours))
        });
        if present {
            continue;
        }
        groups.push(json!({
            "hooks": [{ "type": "command", "command": command, "timeout": 5 }]
        }));
        added.push(ev.to_string());
    }
    added
}

/// Remove our handlers everywhere. Returns the events touched.
pub fn strip_hooks(root: &mut Value) -> Vec<String> {
    let mut touched = Vec::new();
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return touched;
    };
    let mut empty_events = Vec::new();
    for (ev, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        let before = groups.len();
        for g in groups.iter_mut() {
            if let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                let n = hs.len();
                hs.retain(|h| !is_ours(h));
                if hs.len() != n {
                    touched.push(ev.clone());
                }
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|hs| !hs.is_empty())
        });
        if groups.is_empty() && (before > 0 || touched.contains(ev)) {
            empty_events.push(ev.clone());
        }
    }
    for ev in empty_events {
        hooks.remove(&ev);
    }
    if hooks.is_empty() {
        root.as_object_mut().map(|o| o.remove("hooks"));
    }
    touched.sort();
    touched.dedup();
    touched
}

fn install_claude(ctx: &Ctx, probe: &VendorProbe, dry_run: bool) -> Result<Report> {
    let mut rep = Report::default();
    let path = &ctx.paths.claude_settings;
    rep.say(format!("claude  {}", probe.describe()));
    let mut root = read_json(path)?;
    let cmd = hook_command(&exe()?, "claude", "");
    let added = merge_hooks(&mut root, claude_hooks::HOOK_EVENTS, &cmd);
    if added.is_empty() {
        rep.say(format!(
            "  {}  already registered, nothing to do",
            tilde(path)
        ));
        return Ok(rep);
    }
    rep.say(format!(
        "  {}  + hooks.{}",
        tilde(path),
        added.join(", hooks.")
    ));
    rep.changed = true;
    if !dry_run {
        write_atomic(
            path,
            &format!("{}\n", serde_json::to_string_pretty(&root)?),
            true,
        )?;
        rep.say(format!("  backup written to {}.bogyman-bak", tilde(path)));
    }
    Ok(rep)
}

fn install_codex(ctx: &Ctx, probe: &VendorProbe, dry_run: bool) -> Result<Report> {
    let mut rep = Report::default();
    rep.say(format!("codex   {}", probe.describe()));
    let exe = exe()?;

    // hooks.json
    let path = &ctx.paths.codex_hooks;
    let mut root = read_json(path)?;
    let cmd = hook_command(&exe, "codex", "");
    let added = merge_hooks(&mut root, codex_hooks::HOOK_EVENTS, &cmd);
    if added.is_empty() {
        rep.say(format!("  {}  already registered", tilde(path)));
    } else {
        rep.say(format!(
            "  {}  + hooks.{}",
            tilde(path),
            added.join(", hooks.")
        ));
        rep.changed = true;
        if !dry_run {
            write_atomic(
                path,
                &format!("{}\n", serde_json::to_string_pretty(&root)?),
                true,
            )?;
        }
    }

    // config.toml notify (older Codex without hooks; harmless alongside them)
    let cfg_path = &ctx.paths.codex_config;
    let text = std::fs::read_to_string(cfg_path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", cfg_path.display()))?;
    match doc.get("notify") {
        Some(v) if notify_is_ours(v) => {
            rep.say(format!("  {}  notify already set", tilde(cfg_path)));
        }
        Some(_) => {
            rep.say(format!(
                "  {}  notify is set to something else; left as is (bogyman relies on hooks.json)",
                tilde(cfg_path)
            ));
        }
        None => {
            let mut arr = toml_edit::Array::new();
            for part in [exe.to_string_lossy().as_ref(), "hook", "codex", "--notify"] {
                arr.push(part);
            }
            doc["notify"] = toml_edit::value(arr);
            rep.say(format!(
                "  {}  + notify = [bogyman hook codex --notify]",
                tilde(cfg_path)
            ));
            rep.changed = true;
            if !dry_run {
                write_atomic(cfg_path, &doc.to_string(), true)?;
            }
        }
    }
    rep.say("  if Codex does not fire hooks, check `codex features list` for a hooks flag, then run /hooks in Codex");
    Ok(rep)
}

fn uninstall_claude(ctx: &Ctx, dry_run: bool) -> Result<Report> {
    let mut rep = Report::default();
    let path = &ctx.paths.claude_settings;
    if !path.exists() {
        rep.say(format!("claude  {} not found", tilde(path)));
        return Ok(rep);
    }
    let mut root = read_json(path)?;
    let touched = strip_hooks(&mut root);
    if touched.is_empty() {
        rep.say(format!("claude  {}  no bogyman hooks", tilde(path)));
        return Ok(rep);
    }
    rep.say(format!(
        "claude  {}  - hooks.{}",
        tilde(path),
        touched.join(", hooks.")
    ));
    rep.changed = true;
    if !dry_run {
        write_atomic(
            path,
            &format!("{}\n", serde_json::to_string_pretty(&root)?),
            true,
        )?;
    }
    Ok(rep)
}

fn uninstall_codex(ctx: &Ctx, dry_run: bool) -> Result<Report> {
    let mut rep = Report::default();
    let path = &ctx.paths.codex_hooks;
    if path.exists() {
        let mut root = read_json(path)?;
        let touched = strip_hooks(&mut root);
        if touched.is_empty() {
            rep.say(format!("codex   {}  no bogyman hooks", tilde(path)));
        } else {
            rep.say(format!(
                "codex   {}  - hooks.{}",
                tilde(path),
                touched.join(", hooks.")
            ));
            rep.changed = true;
            if !dry_run {
                write_atomic(
                    path,
                    &format!("{}\n", serde_json::to_string_pretty(&root)?),
                    true,
                )?;
            }
        }
    } else {
        rep.say(format!("codex   {} not found", tilde(path)));
    }
    let cfg_path = &ctx.paths.codex_config;
    if let Ok(text) = std::fs::read_to_string(cfg_path)
        && let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>()
        && doc.get("notify").is_some_and(notify_is_ours)
    {
        doc.remove("notify");
        rep.say(format!("  {}  - notify", tilde(cfg_path)));
        rep.changed = true;
        if !dry_run {
            write_atomic(cfg_path, &doc.to_string(), true)?;
        }
    }
    Ok(rep)
}

fn targets(claude: bool, codex: bool) -> Result<(Option<VendorProbe>, Option<VendorProbe>)> {
    let c = probe_vendor("claude");
    let x = probe_vendor("codex");
    let want_claude = claude || (!codex && c.found);
    let want_codex = codex || (!claude && x.found);
    if !want_claude && !want_codex {
        return Err(anyhow!(
            "neither claude nor codex found on PATH; pass --claude or --codex to register anyway"
        ));
    }
    if claude && !c.found {
        eprintln!("bogyman: claude not found on PATH; registering hooks anyway");
    }
    if codex && !x.found {
        eprintln!("bogyman: codex not found on PATH; registering hooks anyway");
    }
    Ok((want_claude.then_some(c), want_codex.then_some(x)))
}

fn finish(ctx: &Ctx, reports: Vec<Report>, dry_run: bool, verb: &str) -> Result<u8> {
    let changed = reports.iter().any(|r| r.changed);
    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "schema": bogyman_core::SCHEMA,
                "changed": changed,
                "dry_run": dry_run,
                "lines": reports.iter().flat_map(|r| r.lines.clone()).collect::<Vec<_>>(),
            })
        );
        return Ok(EXIT_OK);
    }
    for r in &reports {
        for l in &r.lines {
            println!("{l}");
        }
    }
    if dry_run {
        println!("\nDry run: nothing written.");
    } else if changed {
        println!("\n{verb}. Run `bogyman doctor` to confirm events arrive.");
    } else {
        println!("\nNothing to change.");
    }
    Ok(EXIT_OK)
}

pub fn install(ctx: &Ctx, claude: bool, codex: bool, dry_run: bool) -> Result<u8> {
    let (c, x) = match targets(claude, codex) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bogyman: {e}");
            return Ok(EXIT_VENDOR_UNAVAILABLE);
        }
    };
    let mut reports = Vec::new();
    if let Some(p) = c {
        reports.push(install_claude(ctx, &p, dry_run)?);
    }
    if let Some(p) = x {
        reports.push(install_codex(ctx, &p, dry_run)?);
    }
    finish(ctx, reports, dry_run, "Hooks registered")
}

pub fn uninstall(ctx: &Ctx, claude: bool, codex: bool, dry_run: bool) -> Result<u8> {
    let both = !claude && !codex;
    let mut reports = Vec::new();
    if claude || both {
        reports.push(uninstall_claude(ctx, dry_run)?);
    }
    if codex || both {
        reports.push(uninstall_codex(ctx, dry_run)?);
    }
    finish(ctx, reports, dry_run, "Hooks removed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_is_idempotent_and_leaves_other_hooks_alone() {
        let mut root = json!({
            "permissions": {"allow": ["Bash(ls)"]},
            "hooks": {
                "Stop": [{"hooks": [{"type": "command", "command": "~/.claude/hooks/notify.sh"}]}]
            }
        });
        let added = merge_hooks(
            &mut root,
            &["Stop", "SessionEnd"],
            "/usr/local/bin/bogyman hook claude",
        );
        assert_eq!(added, vec!["Stop", "SessionEnd"]);
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(
            root["hooks"]["Stop"][0]["hooks"][0]["command"],
            "~/.claude/hooks/notify.sh"
        );
        assert_eq!(root["permissions"]["allow"][0], "Bash(ls)");

        let again = merge_hooks(
            &mut root,
            &["Stop", "SessionEnd"],
            "/usr/local/bin/bogyman hook claude",
        );
        assert!(again.is_empty());
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 2);

        let touched = strip_hooks(&mut root);
        assert_eq!(touched, vec!["SessionEnd", "Stop"]);
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert!(root["hooks"].get("SessionEnd").is_none());
        assert_eq!(
            root["hooks"]["Stop"][0]["hooks"][0]["command"],
            "~/.claude/hooks/notify.sh"
        );
    }

    #[test]
    fn strip_removes_empty_hooks_object() {
        let mut root = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/x/bogyman hook claude"}]}]}});
        strip_hooks(&mut root);
        assert!(root.get("hooks").is_none());
    }

    #[test]
    fn notify_array_is_recognised() {
        let doc: toml_edit::DocumentMut =
            "notify = [\"/usr/local/bin/bogyman\", \"hook\", \"codex\", \"--notify\"]\n"
                .parse()
                .unwrap();
        assert!(notify_is_ours(doc.get("notify").unwrap()));
        let other: toml_edit::DocumentMut = "notify = [\"terminal-notifier\"]\n".parse().unwrap();
        assert!(!notify_is_ours(other.get("notify").unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_mode_and_defaults_to_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("settings.json");
        std::fs::write(&existing, "{}").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&existing, "{\"a\":1}\n", true).unwrap();
        assert_eq!(
            std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            existing
                .with_file_name("settings.json.bogyman-bak")
                .exists()
        );

        let fresh = dir.path().join("new").join("hooks.json");
        write_atomic(&fresh, "{}\n", true).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn hook_command_quotes_spaces() {
        assert_eq!(
            hook_command(Path::new("/a b/bogyman"), "claude", ""),
            "\"/a b/bogyman\" hook claude"
        );
        assert_eq!(
            hook_command(Path::new("/a/bogyman"), "codex", "--notify"),
            "/a/bogyman hook codex --notify"
        );
    }
}
