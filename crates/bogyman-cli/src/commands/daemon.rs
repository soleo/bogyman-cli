//! `bogyman daemon`: an optional background poller.
//!
//! Hooks keep working without it. The daemon adds what needs a timer: the
//! `claude agents --json` poll (too slow for every query), a steady refresh
//! of session files, transcripts and Codex rollouts so the snapshot stays
//! current between queries, a periodic gc, and notifications.
//!
//! One instance at a time: the running daemon holds an flock on
//! `daemon.lock` for its whole life, so a crash never leaves a stale claim.
//! Each tick it writes `daemon.json` (pid, start time, last tick, poll results)
//! for `daemon status` and `doctor`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bogyman_core::liveness;
use bogyman_core::lock::{self, Lock};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::paths::tilde;
use crate::{Ctx, EXIT_OK, json, notify, refresh};

const TICK: Duration = Duration::from_secs(1);
const GC_EVERY: Duration = Duration::from_secs(6 * 3600);
/// launchd label and systemd unit name.
const LABEL: &str = "dev.bogyman.daemon";
const UNIT: &str = "bogyman.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Status,
    Run,
    NotifyTest,
    Install { dry_run: bool },
    Uninstall { dry_run: bool },
}

/// How the last attempt at one poller went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollStatus {
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_ok: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub events_added: u64,
}

impl PollStatus {
    fn record(&mut self, now: OffsetDateTime, r: Result<usize>) {
        match r {
            Ok(n) => {
                self.last_ok = Some(now);
                self.last_error = None;
                self.events_added += n as u64;
            }
            Err(e) => self.last_error = Some(format!("{e:#}")),
        }
    }
}

/// What the running daemon writes to `daemon.json` every tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub pid: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_tick: OffsetDateTime,
    pub claude_agents: PollStatus,
    pub refresh: PollStatus,
    #[serde(default)]
    pub notifications_sent: u64,
}

pub fn lock_path(ctx: &Ctx) -> PathBuf {
    ctx.paths.data_dir.join("daemon.lock")
}

pub fn state_path(ctx: &Ctx) -> PathBuf {
    ctx.paths.data_dir.join("daemon.json")
}

/// The daemon's pid when one is running.
pub fn running_pid(ctx: &Ctx) -> Option<u32> {
    let path = lock_path(ctx);
    lock::is_held(&path).then(|| lock::holder_pid(&path).unwrap_or(0))
}

pub fn read_state(ctx: &Ctx) -> Option<State> {
    let text = std::fs::read_to_string(state_path(ctx)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_state(path: &Path, state: &State) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn run(ctx: &Ctx, action: Action) -> Result<u8> {
    match action {
        Action::Run => run_loop(ctx),
        Action::NotifyTest => notify_test(ctx),
        Action::Start => start(ctx),
        Action::Stop => stop(ctx),
        Action::Status => status(ctx),
        Action::Install { dry_run } => install(ctx, dry_run),
        Action::Uninstall { dry_run } => uninstall(ctx, dry_run),
    }
}

/// Something the loop runs every `every`.
struct Timer {
    every: Duration,
    last: Option<Instant>,
}

impl Timer {
    fn new(every: Duration) -> Timer {
        Timer {
            every: every.max(TICK),
            last: None,
        }
    }

    fn due(&mut self) -> bool {
        if self.last.is_none_or(|t| t.elapsed() >= self.every) {
            self.last = Some(Instant::now());
            true
        } else {
            false
        }
    }
}

fn run_loop(ctx: &Ctx) -> Result<u8> {
    let path = lock_path(ctx);
    let Some(mut held) =
        Lock::try_acquire(&path).with_context(|| format!("opening {}", path.display()))?
    else {
        let pid = lock::holder_pid(&path).unwrap_or(0);
        return Err(anyhow!("another daemon is already running (pid {pid})"));
    };
    let pid = std::process::id();
    held.write_pid(pid)?;
    ctx.paths.log(&format!("daemon: started (pid {pid})"));

    let started_at = OffsetDateTime::now_utc();
    let mut state = State {
        pid,
        started_at,
        last_tick: started_at,
        claude_agents: PollStatus::default(),
        refresh: PollStatus::default(),
        notifications_sent: 0,
    };
    let mut agents = Timer::new(Duration::from_secs(ctx.config.poll.claude_agents_secs));
    let mut refresh_timer = Timer::new(Duration::from_secs(ctx.config.poll.codex_rollout_secs));
    let mut gc = Timer::new(GC_EVERY);
    let mut notifier = notify::Notifier::new(started_at);
    let state_file = state_path(ctx);
    loop {
        let now = OffsetDateTime::now_utc();
        if ctx.config.collectors.claude_agents && agents.due() {
            state
                .claude_agents
                .record(now, refresh::poll_claude_agents(ctx));
        }
        if refresh_timer.due() {
            // A query's own refresh: session files, transcripts, rollouts, then
            // fold everything new into the snapshot.
            match refresh::load_sessions(ctx, true) {
                Ok(sessions) => {
                    state.refresh.record(now, Ok(0));
                    if ctx.config.notify.enabled {
                        for note in notifier.due(&ctx.config.notify, &sessions, now) {
                            match notify::send(&ctx.config.notify, &note) {
                                Ok(()) => state.notifications_sent += 1,
                                Err(e) => ctx.paths.log(&format!("daemon: notify: {e:#}")),
                            }
                        }
                    }
                }
                Err(e) => state.refresh.record(now, Err(e)),
            }
        }
        if gc.due() {
            let r = refresh::open_store(ctx)
                .and_then(|mut s| Ok(s.gc(ctx.config.store.retention_days, now)?));
            if let Err(e) = r {
                ctx.paths.log(&format!("daemon: gc: {e:#}"));
            }
        }
        for (name, p) in [
            ("claude agents", &state.claude_agents),
            ("refresh", &state.refresh),
        ] {
            if let Some(e) = &p.last_error {
                ctx.paths.log(&format!("daemon: {name}: {e}"));
            }
        }
        state.last_tick = now;
        if let Err(e) = write_state(&state_file, &state) {
            ctx.paths.log(&format!("daemon: writing state: {e:#}"));
        }
        std::thread::sleep(TICK);
    }
}

fn notify_test(ctx: &Ctx) -> Result<u8> {
    let note = notify::Note {
        session_id: "cc-test".into(),
        title: "bogyman test needs you".into(),
        body: "permission · this is what a blocked session looks like".into(),
    };
    notify::send(&ctx.config.notify, &note)?;
    let mut line = "sent a test notification".to_string();
    if !ctx.config.notify.enabled {
        line.push_str("; the daemon won't send real ones until notify.enabled = true");
    }
    report(
        ctx,
        "notify-test",
        serde_json::json!({"enabled": ctx.config.notify.enabled}),
        line,
    )
}

fn start(ctx: &Ctx) -> Result<u8> {
    if let Some(pid) = running_pid(ctx) {
        return report(
            ctx,
            "start",
            serde_json::json!({"running": true, "pid": pid, "started": false}),
            format!("daemon already running (pid {pid})"),
        );
    }
    let exe = std::env::current_exe().context("finding the bogyman binary")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["daemon", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("starting {} daemon run", exe.display()))?;
    // Wait for the child to take the lock, so a status right after is accurate.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(pid) = running_pid(ctx) {
            return report(
                ctx,
                "start",
                serde_json::json!({"running": true, "pid": pid, "started": true}),
                format!(
                    "daemon started (pid {pid}); log at {}",
                    tilde(&ctx.paths.log)
                ),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!(
        "the daemon did not come up within 3 s; see {}",
        tilde(&ctx.paths.log)
    ))
}

/// Put the child in its own process group, so closing the terminal that ran
/// `daemon start` doesn't take the daemon with it.
#[cfg(unix)]
fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn detach(_cmd: &mut std::process::Command) {}

fn stop(ctx: &Ctx) -> Result<u8> {
    let Some(pid) = running_pid(ctx) else {
        return report(
            ctx,
            "stop",
            serde_json::json!({"running": false, "stopped": false}),
            "daemon is not running".into(),
        );
    };
    if pid == 0 || !liveness::terminate(pid) {
        return Err(anyhow!("couldn't signal the daemon (pid {pid})"));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if running_pid(ctx).is_none() {
            let mut line = format!("daemon stopped (pid {pid})");
            if service_installed(ctx) {
                line.push_str(&format!(
                    "; the {} will start it again, `bogyman daemon uninstall` removes it",
                    service_kind()
                ));
            }
            return report(
                ctx,
                "stop",
                serde_json::json!({"running": false, "stopped": true, "pid": pid}),
                line,
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!("the daemon (pid {pid}) is still running after 5 s"))
}

fn status(ctx: &Ctx) -> Result<u8> {
    let pid = running_pid(ctx);
    let st = read_state(ctx).filter(|s| Some(s.pid) == pid);
    let now = OffsetDateTime::now_utc();
    if ctx.json {
        json::ok(
            ctx,
            "daemon",
            serde_json::json!({
                "running": pid.is_some(),
                "pid": pid,
                "state": st,
                "service": {"kind": service_kind(), "path": service_path(ctx), "installed": service_installed(ctx)},
            }),
        )?;
        return Ok(EXIT_OK);
    }
    match (pid, &st) {
        (Some(pid), Some(s)) => {
            println!(
                "daemon    running (pid {pid}), up {}, last tick {} ago",
                crate::render::fmt_age((now - s.started_at).whole_seconds()),
                crate::render::fmt_age((now - s.last_tick).whole_seconds())
            );
            for (name, p) in [("claude agents", &s.claude_agents), ("refresh", &s.refresh)] {
                let line = match (&p.last_error, p.last_ok) {
                    (Some(e), _) => format!("failing: {e}"),
                    (None, Some(t)) if name == "claude agents" => format!(
                        "ok {} ago, {} change(s) recorded",
                        crate::render::fmt_age((now - t).whole_seconds()),
                        p.events_added
                    ),
                    (None, Some(t)) => format!(
                        "ok {} ago",
                        crate::render::fmt_age((now - t).whole_seconds())
                    ),
                    (None, None) => "not run yet".into(),
                };
                println!("  {name:<14}{line}");
            }
        }
        (Some(pid), None) => println!("daemon    running (pid {pid}), starting up"),
        (None, _) => println!("daemon    not running (optional; `bogyman daemon start`)"),
    }
    println!(
        "service   {} {}",
        service_kind(),
        if service_installed(ctx) {
            format!("installed at {}", tilde(&service_path(ctx)))
        } else {
            "not installed (`bogyman daemon install`)".into()
        }
    );
    Ok(EXIT_OK)
}

fn report(ctx: &Ctx, action: &str, data: serde_json::Value, line: String) -> Result<u8> {
    if ctx.json {
        let mut data = data;
        data["action"] = action.into();
        data["detail"] = line.into();
        json::ok(ctx, "daemon", data)?;
    } else {
        println!("{line}");
    }
    Ok(EXIT_OK)
}

// --- Service install (launchd on macOS, systemd --user on Linux) -------------

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn service_kind() -> &'static str {
    if cfg!(target_os = "macos") {
        "launchd agent"
    } else {
        "systemd user unit"
    }
}

fn service_path(_ctx: &Ctx) -> PathBuf {
    if cfg!(target_os = "macos") {
        home()
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
            .join("systemd/user")
            .join(UNIT)
    }
}

fn service_installed(ctx: &Ctx) -> bool {
    service_path(ctx).exists()
}

/// Environment the service needs to see: PATH, so `claude`, `codex`,
/// `osascript` resolve as they do in your shell, and the overrides that move
/// bogyman's or the vendors' files.
fn service_env() -> Vec<(String, String)> {
    [
        "PATH",
        "BOGYMAN_HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
    ]
    .iter()
    .filter_map(|k| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| (k.to_string(), v))
    })
    .collect()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn launchd_plist(exe: &Path, log: &Path, env: &[(String, String)]) -> String {
    let env_xml: String = env
        .iter()
        .map(|(k, v)| {
            format!(
                "\t\t<key>{}</key>\n\t\t<string>{}</string>\n",
                xml_escape(k),
                xml_escape(v)
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>daemon</string>
		<string>run</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
{env_xml}	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<key>StandardErrorPath</key>
	<string>{log}</string>
	<key>ProcessType</key>
	<string>Background</string>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.to_string_lossy()),
        log = xml_escape(&log.to_string_lossy()),
    )
}

/// systemd quoting: wrap in double quotes, escaping `\`, `"` and `%`.
fn systemd_quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

pub fn systemd_unit(exe: &Path, env: &[(String, String)]) -> String {
    let env_lines: String = env
        .iter()
        .map(|(k, v)| format!("Environment={}\n", systemd_quote(&format!("{k}={v}"))))
        .collect();
    format!(
        "[Unit]\nDescription=bogyman daemon: polls agent sessions and sends notifications\n\n\
[Service]\nExecStart={} daemon run\n{env_lines}Restart=on-failure\nRestartSec=10\n\n\
[Install]\nWantedBy=default.target\n",
        systemd_quote(&exe.to_string_lossy())
    )
}

fn sh(program: &str, args: &[&str]) -> Result<()> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn install(ctx: &Ctx, dry_run: bool) -> Result<u8> {
    let exe = std::env::current_exe().context("finding the bogyman binary")?;
    let path = service_path(ctx);
    let env = service_env();
    let body = if cfg!(target_os = "macos") {
        launchd_plist(&exe, &ctx.paths.data_dir.join("daemon.err"), &env)
    } else {
        systemd_unit(&exe, &env)
    };
    if dry_run {
        if ctx.json {
            json::ok(
                ctx,
                "daemon",
                serde_json::json!({"action": "install", "dry_run": true, "path": path, "contents": body}),
            )?;
        } else {
            println!("would write {}:\n\n{body}", tilde(&path));
        }
        return Ok(EXIT_OK);
    }
    if let Some(pid) = running_pid(ctx) {
        // The service is about to start its own daemon; the lock would refuse it.
        if !liveness::terminate(pid) {
            return Err(anyhow!("stop the running daemon (pid {pid}) first"));
        }
    }
    std::fs::create_dir_all(path.parent().expect("service path has a parent"))?;
    std::fs::write(&path, &body).with_context(|| format!("writing {}", path.display()))?;
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", liveness::uid());
        let target = format!("{domain}/{LABEL}");
        // Re-installing replaces a loaded agent; bootout fails harmlessly if none.
        let _ = sh("launchctl", &["bootout", &target]);
        sh(
            "launchctl",
            &["bootstrap", &domain, &path.to_string_lossy()],
        )?;
    } else {
        sh("systemctl", &["--user", "daemon-reload"])?;
        sh("systemctl", &["--user", "enable", "--now", UNIT])?;
    }
    report(
        ctx,
        "install",
        serde_json::json!({"path": path, "dry_run": false}),
        format!(
            "installed {} at {}; the daemon starts now and at every login",
            service_kind(),
            tilde(&path)
        ),
    )
}

fn uninstall(ctx: &Ctx, dry_run: bool) -> Result<u8> {
    let path = service_path(ctx);
    if !path.exists() {
        return report(
            ctx,
            "uninstall",
            serde_json::json!({"path": path, "removed": false}),
            format!("no {} installed", service_kind()),
        );
    }
    if dry_run {
        return report(
            ctx,
            "uninstall",
            serde_json::json!({"path": path, "dry_run": true}),
            format!("would remove {}", tilde(&path)),
        );
    }
    if cfg!(target_os = "macos") {
        let _ = sh(
            "launchctl",
            &["bootout", &format!("gui/{}/{LABEL}", liveness::uid())],
        );
    } else {
        let _ = sh("systemctl", &["--user", "disable", "--now", UNIT]);
    }
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    report(
        ctx,
        "uninstall",
        serde_json::json!({"path": path, "removed": true}),
        format!("removed {}", tilde(&path)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_runs_the_daemon_with_the_callers_path() {
        let env = vec![("PATH".to_string(), "/a/b:/c&d".to_string())];
        let p = launchd_plist(
            Path::new("/opt/bogy man/bogyman"),
            Path::new("/tmp/d.err"),
            &env,
        );
        assert!(p.contains("<string>/opt/bogy man/bogyman</string>"));
        assert!(p.contains("<string>daemon</string>\n\t\t<string>run</string>"));
        assert!(p.contains("<key>PATH</key>\n\t\t<string>/a/b:/c&amp;d</string>"));
        assert!(p.contains("<key>KeepAlive</key>\n\t<true/>"));
    }

    #[test]
    fn the_unit_quotes_paths_and_environment() {
        let env = vec![("PATH".to_string(), "/a b:%x".to_string())];
        let u = systemd_unit(Path::new("/opt/bogy man/bogyman"), &env);
        assert!(u.contains("ExecStart=\"/opt/bogy man/bogyman\" daemon run"));
        assert!(u.contains("Environment=\"PATH=/a b:%%x\""));
        assert!(u.contains("Restart=on-failure"));
    }

    #[test]
    fn timers_fire_at_once_then_wait_their_interval() {
        let mut t = Timer::new(Duration::from_secs(60));
        assert!(t.due());
        assert!(!t.due());
    }
}
