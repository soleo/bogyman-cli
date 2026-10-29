//! `bogyman focus <id>`: bring a session in front of the user.
//!
//! - A live terminal session: walk its process's parents to the terminal app
//!   and select the tab that owns its tty. Ghostty, iTerm2 and Terminal through
//!   AppleScript; WezTerm and kitty through their own CLIs; a tmux pane is
//!   selected first and then the terminal running the tmux client is raised.
//!   On Linux the terminal's window is raised with xdotool or wmctrl.
//! - A Claude Code background session: `claude attach <agent id>`.
//! - A Desktop session: bring the Claude app forward.
//! - A session that has ended: resume it in its directory (`claude --resume`,
//!   `codex resume`).
//!
//! Attach and resume replace this process, so they only run on a terminal.
//! Under `--json` or in a pipe, focus reports the command instead.

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use bogyman_core::{Host, Kind, Session, Vendor};

use crate::{Ctx, EXIT_OK, EXIT_VENDOR_UNAVAILABLE, json, refresh, resolve};

/// Terminal apps focus knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Ghostty,
    ITerm2,
    Terminal,
    WezTerm,
    Kitty,
    /// The session runs inside tmux; its pane is selected, then the client's terminal.
    Tmux,
}

impl App {
    pub fn label(self) -> &'static str {
        match self {
            App::Ghostty => "Ghostty",
            App::ITerm2 => "iTerm2",
            App::Terminal => "Terminal",
            App::WezTerm => "WezTerm",
            App::Kitty => "kitty",
            App::Tmux => "tmux",
        }
    }

    /// `focus.terminal` in config.toml, when it names an app instead of `auto`.
    pub fn from_config(s: &str) -> Option<App> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ghostty" => Some(App::Ghostty),
            "iterm" | "iterm2" => Some(App::ITerm2),
            "terminal" | "terminal.app" | "apple_terminal" => Some(App::Terminal),
            "wezterm" => Some(App::WezTerm),
            "kitty" => Some(App::Kitty),
            "tmux" => Some(App::Tmux),
            _ => None,
        }
    }

    /// Recognize an app from a process's command path (`ps -o comm=`).
    pub fn from_command(comm: &str) -> Option<App> {
        let base = comm.rsplit('/').next().unwrap_or(comm);
        let lower = base.to_ascii_lowercase();
        if comm.contains("/iTerm.app/") || lower == "iterm2" {
            Some(App::ITerm2)
        } else if comm.contains("/Terminal.app/") {
            Some(App::Terminal)
        } else if lower == "ghostty" {
            Some(App::Ghostty)
        } else if lower == "wezterm-gui" || lower == "wezterm" {
            Some(App::WezTerm)
        } else if lower == "kitty" {
            Some(App::Kitty)
        } else if lower.starts_with("tmux") {
            Some(App::Tmux)
        } else {
            None
        }
    }
}

/// One row of `ps -A -o pid=,ppid=,tty=,comm=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// `/dev/ttys002`, or `None` for `??` / `?`.
    pub tty: Option<String>,
    pub comm: String,
}

pub fn parse_ps(text: &str) -> HashMap<u32, Proc> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(tty)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
            continue;
        };
        // The command path is the rest of the line and may contain spaces.
        let comm = it.collect::<Vec<_>>().join(" ");
        let tty = match tty {
            "??" | "?" | "-" => None,
            t if t.starts_with("/dev/") => Some(t.to_string()),
            t => Some(format!("/dev/{t}")),
        };
        out.insert(
            pid,
            Proc {
                pid,
                ppid,
                tty,
                comm,
            },
        );
    }
    out
}

fn process_table() -> Result<HashMap<u32, Proc>> {
    let out = crate::vendor_cmd::output(
        "ps",
        &["-A", "-o", "pid=,ppid=,tty=,comm="],
        std::time::Duration::from_secs(5),
    )?;
    Ok(parse_ps(&out))
}

/// The nearest ancestor of `pid` (itself excluded) that is a known terminal app,
/// with that ancestor's pid. Stops at pid 1 and guards against cycles.
pub fn terminal_ancestor(procs: &HashMap<u32, Proc>, pid: u32) -> Option<(App, u32)> {
    let mut cur = procs.get(&pid)?.ppid;
    for _ in 0..64 {
        if cur <= 1 {
            return None;
        }
        let p = procs.get(&cur)?;
        if let Some(app) = App::from_command(&p.comm) {
            return Some((app, p.pid));
        }
        cur = p.ppid;
    }
    None
}

/// The macOS app bundle an ancestor runs from (`/Applications/Cursor.app/…` →
/// `Cursor`), for hosts focus has no tab-level support for, like an editor.
pub fn app_bundle_ancestor(procs: &HashMap<u32, Proc>, pid: u32) -> Option<String> {
    let mut cur = procs.get(&pid)?.ppid;
    for _ in 0..64 {
        if cur <= 1 {
            return None;
        }
        let p = procs.get(&cur)?;
        if let Some(i) = p.comm.find(".app/Contents/MacOS/") {
            let name = p.comm[..i].rsplit('/').next()?.to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
        cur = p.ppid;
    }
    None
}

/// What focus will do for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Select the tab or pane that owns `tty` in `app`. `app_pid` is the
    /// terminal's own pid (used by the Linux window tools).
    Raise { app: App, tty: String, app_pid: u32 },
    /// Bring an app forward without picking a tab (Desktop, an editor).
    Activate { app: String },
    /// Replace this process with a vendor command (attach or resume).
    Exec {
        argv: Vec<String>,
        cwd: Option<String>,
        why: &'static str,
    },
}

/// Decide what to do. `procs` is the process table; `forced` is `focus.terminal`.
pub fn plan(s: &Session, procs: &HashMap<u32, Proc>, forced: Option<App>) -> Result<Plan> {
    // Ids go on a vendor's command line; one that starts with `-` would be
    // parsed as a flag (think `--dangerously-skip-permissions`).
    for id in std::iter::once(&s.vendor_session_id).chain(s.agent_id.iter()) {
        if id.starts_with('-') {
            return Err(anyhow!(
                "refusing to pass the id {id:?} to a vendor command"
            ));
        }
    }
    if matches!(s.host, Host::Desktop | Host::DesktopCowork) {
        return Ok(Plan::Activate {
            app: "Claude".into(),
        });
    }
    // `claude attach` restarts a stopped background session too, so it is the
    // right move whatever the state.
    if s.vendor == Vendor::ClaudeCode
        && s.kind == Kind::Background
        && let Some(agent) = &s.agent_id
    {
        return Ok(Plan::Exec {
            argv: vec!["claude".into(), "attach".into(), agent.clone()],
            cwd: None,
            why: "attach",
        });
    }
    let alive = s.state.is_live() && s.pid.is_some_and(|p| procs.contains_key(&p));
    if !alive {
        if s.state.is_live() && s.pid.is_none() {
            return Err(anyhow!(
                "{} is {} but no process id was recorded for it, so its window can't be found",
                s.id,
                s.state.label()
            ));
        }
        let argv = match s.vendor {
            Vendor::ClaudeCode => vec![
                "claude".into(),
                "--resume".into(),
                s.vendor_session_id.clone(),
            ],
            Vendor::Codex => vec!["codex".into(), "resume".into(), s.vendor_session_id.clone()],
        };
        return Ok(Plan::Exec {
            argv,
            cwd: s.cwd.clone(),
            why: "resume",
        });
    }
    let pid = s.pid.expect("checked above");
    let tty = procs.get(&pid).and_then(|p| p.tty.clone());
    if let Some((found, app_pid)) = terminal_ancestor(procs, pid)
        && let Some(tty) = tty.clone()
    {
        // A forced app overrides detection, but tmux is structural: a session in
        // tmux needs its pane selected whatever terminal shows it.
        let app = match forced {
            Some(f) if found != App::Tmux => f,
            _ => found,
        };
        return Ok(Plan::Raise { app, tty, app_pid });
    }
    if let Some(app) = app_bundle_ancestor(procs, pid) {
        return Ok(Plan::Activate { app });
    }
    Err(anyhow!(
        "couldn't find the terminal running {} (pid {pid}); set focus.terminal in config.toml",
        s.id
    ))
}

/// A tty path that is safe to put inside an AppleScript string.
pub fn safe_tty(tty: &str) -> Option<&str> {
    let ok = tty.starts_with("/dev/")
        && tty[5..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/');
    ok.then_some(tty)
}

/// AppleScript that selects the tab owning `tty` and brings its window forward.
/// Prints `ok` on success and `missing` when no tab owns the tty.
pub fn applescript(app: App, tty: &str) -> Option<String> {
    let tty = safe_tty(tty)?;
    Some(match app {
        App::Ghostty => format!(
            r#"tell application "Ghostty"
  set found to (every terminal whose tty is "{tty}")
  if (count of found) is 0 then return "missing"
  focus (item 1 of found)
  activate
end tell
return "ok""#
        ),
        App::ITerm2 => format!(
            r#"tell application "iTerm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if tty of s is "{tty}" then
          select w
          select t
          select s
          activate
          return "ok"
        end if
      end repeat
    end repeat
  end repeat
end tell
return "missing""#
        ),
        App::Terminal => format!(
            r#"tell application "Terminal"
  repeat with w in windows
    repeat with t in tabs of w
      if tty of t is "{tty}" then
        set selected of t to true
        set index of w to 1
        activate
        return "ok"
      end if
    end repeat
  end repeat
end tell
return "missing""#
        ),
        App::WezTerm | App::Kitty | App::Tmux => return None,
    })
}

/// An app name that is safe inside `tell application "…"`.
fn safe_app_name(name: &str) -> Option<&str> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'));
    ok.then_some(name)
}

fn osascript(script: &str) -> Result<String> {
    crate::vendor_cmd::output(
        "osascript",
        &["-e", script],
        std::time::Duration::from_secs(10),
    )
    .map(|s| s.trim().to_string())
}

fn activate_app(name: &str) -> Result<()> {
    if cfg!(target_os = "macos") {
        let name = safe_app_name(name).ok_or_else(|| anyhow!("unusable app name {name:?}"))?;
        osascript(&format!(r#"tell application "{name}" to activate"#))?;
        Ok(())
    } else {
        Err(anyhow!(
            "bringing {name} forward is only supported on macOS"
        ))
    }
}

/// Raise the window of the terminal whose pid is `app_pid` (Linux).
fn raise_linux_window(app_pid: u32) -> Result<()> {
    let t = std::time::Duration::from_secs(5);
    let pid = app_pid.to_string();
    if let Ok(out) = crate::vendor_cmd::output("xdotool", &["search", "--pid", &pid], t)
        && let Some(wid) = out.lines().next()
    {
        crate::vendor_cmd::output("xdotool", &["windowactivate", wid.trim()], t)?;
        return Ok(());
    }
    let list = crate::vendor_cmd::output("wmctrl", &["-lp"], t)
        .map_err(|_| anyhow!("neither xdotool nor wmctrl is available to raise the window"))?;
    let wid = list
        .lines()
        .find(|l| l.split_whitespace().nth(2) == Some(pid.as_str()))
        .and_then(|l| l.split_whitespace().next())
        .ok_or_else(|| anyhow!("no window belongs to terminal pid {pid}"))?;
    crate::vendor_cmd::output("wmctrl", &["-ia", wid], t)?;
    Ok(())
}

/// Carry out a `Raise`. Returns a line describing what happened.
fn raise(app: App, tty: &str, app_pid: u32, procs: &HashMap<u32, Proc>) -> Result<String> {
    let t = std::time::Duration::from_secs(5);
    match app {
        App::Tmux => {
            let panes = crate::vendor_cmd::output(
                "tmux",
                &["list-panes", "-a", "-F", "#{pane_tty} #{pane_id}"],
                t,
            )?;
            let pane = panes
                .lines()
                .find_map(|l| l.strip_prefix(tty).map(str::trim))
                .filter(|p| p.starts_with('%'))
                .ok_or_else(|| anyhow!("no tmux pane owns {tty}"))?
                .to_string();
            crate::vendor_cmd::output("tmux", &["select-window", "-t", &pane], t)?;
            crate::vendor_cmd::output("tmux", &["select-pane", "-t", &pane], t)?;
            let _ = crate::vendor_cmd::output("tmux", &["switch-client", "-t", &pane], t);
            // Then the terminal showing the most recently active tmux client.
            let clients = crate::vendor_cmd::output(
                "tmux",
                &[
                    "list-clients",
                    "-F",
                    "#{client_activity} #{client_pid} #{client_tty}",
                ],
                t,
            )?;
            let client = clients
                .lines()
                .filter_map(|l| {
                    let mut it = l.split_whitespace();
                    Some((
                        it.next()?.parse::<u64>().ok()?,
                        it.next()?.parse::<u32>().ok()?,
                        it.next()?.to_string(),
                    ))
                })
                .max_by_key(|(activity, _, _)| *activity);
            match client.and_then(|(_, cpid, ctty)| {
                terminal_ancestor(procs, cpid)
                    .filter(|(a, _)| *a != App::Tmux)
                    .map(|(a, apid)| (a, apid, ctty))
            }) {
                Some((outer, outer_pid, ctty)) => {
                    let line = raise(outer, &ctty, outer_pid, procs)?;
                    Ok(format!("selected tmux pane {pane}; {line}"))
                }
                None => Ok(format!(
                    "selected tmux pane {pane} (no attached client's terminal found)"
                )),
            }
        }
        App::WezTerm => {
            let list =
                crate::vendor_cmd::output("wezterm", &["cli", "list", "--format", "json"], t)?;
            let panes: serde_json::Value = serde_json::from_str(&list)?;
            let pane = panes
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .find(|p| p["tty_name"].as_str() == Some(tty))
                        .and_then(|p| p["pane_id"].as_u64())
                })
                .ok_or_else(|| anyhow!("no WezTerm pane owns {tty}"))?;
            crate::vendor_cmd::output(
                "wezterm",
                &["cli", "activate-pane", "--pane-id", &pane.to_string()],
                t,
            )?;
            bring_forward(App::WezTerm, app_pid)?;
            Ok(format!("raised WezTerm pane {pane}"))
        }
        App::Kitty => {
            // kitty matches a window by the pid of the process it started there:
            // the one on this tty whose parent is kitty itself.
            let pid = procs
                .values()
                .find(|p| p.tty.as_deref() == Some(tty) && p.ppid == app_pid)
                .map(|p| p.pid)
                .ok_or_else(|| anyhow!("no process found on {tty}"))?;
            crate::vendor_cmd::output(
                "kitty",
                &["@", "focus-window", "--match", &format!("pid:{pid}")],
                t,
            )
            .map_err(|e| anyhow!("{e:#} (kitty needs allow_remote_control)"))?;
            bring_forward(App::Kitty, app_pid)?;
            Ok("raised the kitty window".into())
        }
        App::Ghostty | App::ITerm2 | App::Terminal => {
            if !cfg!(target_os = "macos") {
                raise_linux_window(app_pid)?;
                return Ok(format!("raised the {} window", app.label()));
            }
            let script = applescript(app, tty).ok_or_else(|| anyhow!("unusable tty {tty:?}"))?;
            match osascript(&script)?.as_str() {
                "ok" => Ok(format!("raised the {} tab on {tty}", app.label())),
                _ => Err(anyhow!("no {} tab owns {tty}", app.label())),
            }
        }
    }
}

fn bring_forward(app: App, app_pid: u32) -> Result<()> {
    if cfg!(target_os = "macos") {
        activate_app(app.label())
    } else {
        raise_linux_window(app_pid)
    }
}

pub fn run(ctx: &Ctx, id: &str) -> Result<u8> {
    let sessions = refresh::load_sessions(ctx, true)?;
    let s = resolve::resolve(&sessions, id)?.clone();
    let procs = process_table()?;
    let forced = App::from_config(&ctx.config.focus.terminal);
    let plan = plan(&s, &procs, forced)?;
    match plan {
        Plan::Exec { argv, cwd, why } => exec_or_report(ctx, &s, argv, cwd, why),
        Plan::Activate { app } => {
            activate_app(&app)?;
            report(
                ctx,
                &s,
                "activated",
                Some(&app),
                None,
                format!("brought {app} forward"),
            )
        }
        Plan::Raise { app, tty, app_pid } => {
            let line = raise(app, &tty, app_pid, &procs)?;
            report(ctx, &s, "raised", Some(app.label()), Some(&tty), line)
        }
    }
}

fn report(
    ctx: &Ctx,
    s: &Session,
    action: &str,
    app: Option<&str>,
    tty: Option<&str>,
    line: String,
) -> Result<u8> {
    if ctx.json {
        json::ok(
            ctx,
            "focus",
            serde_json::json!({"id": s.id, "vendor": s.vendor, "action": action, "app": app, "tty": tty, "detail": line}),
        )?;
    } else {
        println!("{} ({}): {line}", s.id, s.display_name());
    }
    Ok(EXIT_OK)
}

/// Attach and resume take over the terminal, so they only run when there is
/// one. Otherwise the command is reported for the caller to run.
fn exec_or_report(
    ctx: &Ctx,
    s: &Session,
    argv: Vec<String>,
    cwd: Option<String>,
    why: &'static str,
) -> Result<u8> {
    use std::io::IsTerminal;
    let interactive = ctx.tty && !ctx.json && std::io::stdin().is_terminal();
    if !interactive {
        if ctx.json {
            json::ok(
                ctx,
                "focus",
                serde_json::json!({"id": s.id, "vendor": s.vendor, "action": why, "command": argv, "cwd": cwd,
                    "detail": format!("run the command to {why} the session")}),
            )?;
        } else {
            println!("{}", shell_line(&argv, cwd.as_deref()));
        }
        return Ok(EXIT_OK);
    }
    let (program, args) = argv.split_first().expect("non-empty argv");
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd.as_deref().filter(|d| std::path::Path::new(d).is_dir()) {
        cmd.current_dir(dir);
    }
    eprintln!(
        "bogyman: {why} {} ({}): {}",
        s.id,
        s.display_name(),
        argv.join(" ")
    );
    exec(cmd, program)
}

#[cfg(unix)]
fn exec(mut cmd: std::process::Command, program: &str) -> Result<u8> {
    use std::os::unix::process::CommandExt;
    let err = cmd.exec();
    if err.kind() == std::io::ErrorKind::NotFound {
        eprintln!("bogyman: {program} not found on PATH");
        return Ok(EXIT_VENDOR_UNAVAILABLE);
    }
    Err(anyhow!("running {program}: {err}"))
}

#[cfg(not(unix))]
fn exec(mut cmd: std::process::Command, program: &str) -> Result<u8> {
    match cmd.status() {
        Ok(st) => Ok(if st.success() {
            EXIT_OK
        } else {
            crate::EXIT_ERROR
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(EXIT_VENDOR_UNAVAILABLE),
        Err(e) => Err(anyhow!("running {program}: {e}")),
    }
}

/// A copy-pasteable shell line, quoting anything that needs it.
pub fn shell_line(argv: &[String], cwd: Option<&str>) -> String {
    let quote = |a: &str| {
        if !a.is_empty()
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+".contains(c))
        {
            a.to_string()
        } else {
            format!("'{}'", a.replace('\'', r"'\''"))
        }
    };
    let cmd: Vec<String> = argv.iter().map(|a| quote(a)).collect();
    match cwd {
        Some(d) => format!("cd {} && {}", quote(d), cmd.join(" ")),
        None => cmd.join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bogyman_core::{SCHEMA, Source, State};
    use time::macros::datetime;

    const PS: &str = "    1     0 ??       /sbin/launchd
13726     1 ??       /Applications/Ghostty.app/Contents/MacOS/ghostty
26600 13726 ttys002  /usr/bin/login
26663 26600 ttys002  -zsh
66550 26663 ttys002  claude
 7001     1 ??       /Applications/Cursor.app/Contents/Frameworks/Cursor Helper.app/Contents/MacOS/Cursor Helper
 7002  7001 ??       /Applications/Cursor.app/Contents/MacOS/Cursor
 7003  7002 ttys009  /bin/zsh
 7004  7003 ttys009  claude
 8000     1 ??       tmux
 8001  8000 ttys010  -zsh
 8002  8001 ttys010  codex
";

    fn session(vendor: Vendor, state: State, pid: Option<u32>) -> Session {
        let t = datetime!(2026-10-26 21:00:00 UTC);
        Session {
            schema: SCHEMA,
            id: "cc-1f0e2c9a".into(),
            vendor,
            vendor_session_id: "1f0e2c9a-6d0b-4c11-9f39-2a77c1d4e8b5".into(),
            name: Some("flaky-test-fix".into()),
            cwd: Some("/Users/dev/tag web".into()),
            repo: None,
            kind: Kind::Interactive,
            host: Host::Terminal,
            state,
            waiting_for: None,
            waiting_detail: None,
            since: t,
            started_at: t,
            summary: None,
            tasks: None,
            turns: 0,
            tokens: None,
            model: None,
            pid,
            agent_id: None,
            terminal: None,
            transcript_path: None,
            source: Source::Hook,
            observed_at: t,
            stale: false,
        }
    }

    #[test]
    fn ps_rows_keep_command_paths_with_spaces() {
        let t = parse_ps(PS);
        assert_eq!(t[&66550].tty.as_deref(), Some("/dev/ttys002"));
        assert_eq!(t[&13726].tty, None);
        assert!(t[&7001].comm.ends_with("Cursor Helper"));
    }

    #[test]
    fn a_terminal_session_raises_its_tab_by_tty() {
        let procs = parse_ps(PS);
        let s = session(Vendor::ClaudeCode, State::Blocked, Some(66550));
        assert_eq!(
            plan(&s, &procs, None).unwrap(),
            Plan::Raise {
                app: App::Ghostty,
                tty: "/dev/ttys002".into(),
                app_pid: 13726
            }
        );
        // focus.terminal overrides what detection found.
        assert!(matches!(
            plan(&s, &procs, Some(App::ITerm2)).unwrap(),
            Plan::Raise {
                app: App::ITerm2,
                ..
            }
        ));
    }

    #[test]
    fn a_session_in_tmux_selects_its_pane_whatever_the_override() {
        let procs = parse_ps(PS);
        let s = session(Vendor::Codex, State::Working, Some(8002));
        assert!(matches!(
            plan(&s, &procs, Some(App::ITerm2)).unwrap(),
            Plan::Raise { app: App::Tmux, ref tty, .. } if tty == "/dev/ttys010"
        ));
    }

    #[test]
    fn an_editor_terminal_brings_the_editor_forward() {
        let procs = parse_ps(PS);
        let s = session(Vendor::ClaudeCode, State::Done, Some(7004));
        assert_eq!(
            plan(&s, &procs, None).unwrap(),
            Plan::Activate {
                app: "Cursor".into()
            }
        );
    }

    #[test]
    fn a_background_session_attaches_and_desktop_activates_claude() {
        let procs = parse_ps(PS);
        let mut s = session(Vendor::ClaudeCode, State::Stopped, None);
        s.kind = Kind::Background;
        s.agent_id = Some("7c5dcf5d".into());
        assert!(matches!(
            plan(&s, &procs, None).unwrap(),
            Plan::Exec { ref argv, why: "attach", .. } if argv == &["claude", "attach", "7c5dcf5d"]
        ));
        let mut d = session(Vendor::ClaudeCode, State::Working, Some(61906));
        d.host = Host::Desktop;
        assert_eq!(
            plan(&d, &procs, None).unwrap(),
            Plan::Activate {
                app: "Claude".into()
            }
        );
    }

    #[test]
    fn an_ended_session_resumes_in_its_directory() {
        let procs = parse_ps(PS);
        let s = session(Vendor::Codex, State::Stopped, Some(424242));
        match plan(&s, &procs, None).unwrap() {
            Plan::Exec { argv, cwd, why } => {
                assert_eq!(why, "resume");
                assert_eq!(argv[..2], ["codex".to_string(), "resume".to_string()]);
                assert_eq!(
                    shell_line(&argv, cwd.as_deref()),
                    "cd '/Users/dev/tag web' && codex resume 1f0e2c9a-6d0b-4c11-9f39-2a77c1d4e8b5"
                );
            }
            other => panic!("{other:?}"),
        }
        // A live session whose process died before the next poll noticed resumes too.
        let gone = session(Vendor::ClaudeCode, State::Done, Some(424242));
        assert!(matches!(
            plan(&gone, &procs, None).unwrap(),
            Plan::Exec { why: "resume", .. }
        ));
    }

    #[test]
    fn a_live_session_without_a_pid_is_an_error_not_a_second_copy() {
        let procs = parse_ps(PS);
        let s = session(Vendor::Codex, State::Working, None);
        let err = plan(&s, &procs, None).unwrap_err().to_string();
        assert!(err.contains("no process id"), "{err}");
    }

    #[test]
    fn an_id_that_looks_like_a_flag_is_refused() {
        let procs = parse_ps(PS);
        let mut s = session(Vendor::ClaudeCode, State::Stopped, None);
        s.vendor_session_id = "--dangerously-skip-permissions".into();
        assert!(plan(&s, &procs, None).is_err());
        let mut b = session(Vendor::ClaudeCode, State::Working, None);
        b.kind = Kind::Background;
        b.agent_id = Some("-x".into());
        assert!(plan(&b, &procs, None).is_err());
    }

    #[test]
    fn scripts_only_accept_plain_tty_paths() {
        assert!(
            applescript(App::Ghostty, "/dev/ttys002")
                .unwrap()
                .contains("\"/dev/ttys002\"")
        );
        assert!(applescript(App::ITerm2, "/dev/ttys002\" & do shell script \"x").is_none());
        assert!(applescript(App::Terminal, "ttys002").is_none());
        assert!(safe_app_name("Claude").is_some());
        assert!(safe_app_name("x\" to quit").is_none());
    }

    #[test]
    fn apps_are_recognized_from_their_command_paths() {
        assert_eq!(
            App::from_command("/Applications/iTerm.app/Contents/MacOS/iTerm2"),
            Some(App::ITerm2)
        );
        assert_eq!(
            App::from_command(
                "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"
            ),
            Some(App::Terminal)
        );
        assert_eq!(
            App::from_command("/usr/bin/wezterm-gui"),
            Some(App::WezTerm)
        );
        assert_eq!(App::from_command("tmux: server"), Some(App::Tmux));
        assert_eq!(App::from_command("-zsh"), None);
    }
}
