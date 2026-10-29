//! Desktop notifications, sent by the daemon when a session enters a state the
//! user asked about (`[notify] on`, default blocked and failed).
//!
//! A notification fires once per entry into that state: the key is the
//! session's `since`, which changes only when the state does. Sessions already
//! in that state when the daemon started stay quiet, so a restart never
//! replays old waits, and `debounce_secs` caps how often one session can ping.
//!
//! Delivery: a `[notify] command` template if set, otherwise `osascript`
//! (macOS) or `notify-send` (Linux). Text goes to osascript as arguments, never
//! spliced into the script, so nothing in a session name can escape it.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, anyhow};
use bogyman_core::{Session, State};
use time::OffsetDateTime;

use crate::config::Notify as NotifyCfg;
use crate::render::{truncate, waiting_cell};

const BODY_MAX: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub session_id: String,
    pub title: String,
    pub body: String,
}

/// Remembers what has been announced since the daemon started.
#[derive(Debug)]
pub struct Notifier {
    started_at: OffsetDateTime,
    /// Session key → (`since` already announced, when the last note went out).
    seen: HashMap<String, (OffsetDateTime, OffsetDateTime)>,
}

fn state_wanted(cfg: &NotifyCfg, state: State) -> bool {
    cfg.on.iter().any(|w| w.eq_ignore_ascii_case(state.label()))
}

impl Notifier {
    pub fn new(started_at: OffsetDateTime) -> Notifier {
        Notifier {
            started_at,
            seen: HashMap::new(),
        }
    }

    /// The notes due for this batch of sessions.
    pub fn due(&mut self, cfg: &NotifyCfg, sessions: &[Session], now: OffsetDateTime) -> Vec<Note> {
        let debounce = time::Duration::seconds(cfg.debounce_secs as i64);
        let mut out = Vec::new();
        for s in sessions {
            if !state_wanted(cfg, s.state) || s.since <= self.started_at {
                continue;
            }
            let key = s.store_key();
            if let Some((since, last)) = self.seen.get(&key)
                && (*since == s.since || now - *last < debounce)
            {
                continue;
            }
            self.seen.insert(key, (s.since, now));
            out.push(note_for(s));
        }
        out
    }
}

pub fn note_for(s: &Session) -> Note {
    let name = truncate(&s.display_name(), 60);
    let (title, body) = match s.state {
        State::Blocked => (
            format!("{name} needs you"),
            match waiting_cell(s) {
                w if w.is_empty() => "waiting".to_string(),
                w => w,
            },
        ),
        State::Failed => (
            format!("{name} failed"),
            s.waiting_detail.clone().unwrap_or_default(),
        ),
        State::Done => (
            format!("{name} is done"),
            s.summary.clone().unwrap_or_default(),
        ),
        other => (format!("{name} is {}", other.label()), String::new()),
    };
    let body = if body.is_empty() {
        format!("{} · {}", s.vendor.label(), s.id)
    } else {
        truncate(&format!("{body} ({})", s.id), BODY_MAX)
    };
    Note {
        session_id: s.id.clone(),
        title,
        body,
    }
}

/// Expand `{title}`, `{body}` and `{id}` in a command template.
pub fn render_command(template: &[String], n: &Note) -> Vec<String> {
    template
        .iter()
        .map(|p| {
            p.replace("{title}", &n.title)
                .replace("{body}", &n.body)
                .replace("{id}", &n.session_id)
        })
        .collect()
}

/// The command that delivers `n`, or `None` where there is no notifier.
pub fn delivery_command(cfg: &NotifyCfg, n: &Note) -> Option<Vec<String>> {
    if !cfg.command.is_empty() {
        return Some(render_command(&cfg.command, n));
    }
    if cfg!(target_os = "macos") {
        let mut script = "display notification (item 2 of argv) with title \"bogyman\" subtitle (item 1 of argv)".to_string();
        if cfg.sound {
            script.push_str(" sound name \"Glass\"");
        }
        Some(
            [
                "osascript",
                "-e",
                "on run argv",
                "-e",
                &script,
                "-e",
                "end run",
                &n.title,
                &n.body,
            ]
            .map(String::from)
            .to_vec(),
        )
    } else if cfg!(target_os = "linux") {
        Some(
            ["notify-send", "--app-name=bogyman", &n.title, &n.body]
                .map(String::from)
                .to_vec(),
        )
    } else {
        None
    }
}

pub fn send(cfg: &NotifyCfg, n: &Note) -> Result<()> {
    let argv = delivery_command(cfg, n).ok_or_else(|| anyhow!("no notifier on this platform"))?;
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| anyhow!("[notify] command is empty"))?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    crate::vendor_cmd::output(program, &args, Duration::from_secs(10))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bogyman_core::{Host, Kind, SCHEMA, Source, Vendor, WaitingFor};
    use time::macros::datetime;

    const START: OffsetDateTime = datetime!(2026-10-26 21:00:00 UTC);

    fn cfg() -> NotifyCfg {
        NotifyCfg {
            enabled: true,
            debounce_secs: 30,
            ..NotifyCfg::default()
        }
    }

    fn s(id: &str, state: State, since: OffsetDateTime) -> Session {
        Session {
            schema: SCHEMA,
            id: id.into(),
            vendor: Vendor::ClaudeCode,
            vendor_session_id: format!("{id}-uuid"),
            name: Some("flaky-test-fix".into()),
            cwd: None,
            repo: None,
            kind: Kind::Interactive,
            host: Host::Terminal,
            state,
            waiting_for: Some(WaitingFor::PermissionPrompt),
            waiting_detail: Some("Bash: npm test".into()),
            since,
            started_at: since,
            summary: None,
            tasks: None,
            turns: 0,
            tokens: None,
            model: None,
            pid: None,
            agent_id: None,
            terminal: None,
            transcript_path: None,
            source: Source::Hook,
            observed_at: since,
            stale: false,
        }
    }

    #[test]
    fn a_new_wait_notifies_once() {
        let mut n = Notifier::new(START);
        let t1 = START + time::Duration::seconds(10);
        let sessions = [s("cc-1", State::Blocked, t1)];
        let notes = n.due(&cfg(), &sessions, t1);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "flaky-test-fix needs you");
        assert_eq!(notes[0].body, "permission · Bash: npm test (cc-1)");
        // The same wait on the next tick, and long after, stays quiet.
        assert!(
            n.due(&cfg(), &sessions, t1 + time::Duration::seconds(1))
                .is_empty()
        );
        assert!(
            n.due(&cfg(), &sessions, t1 + time::Duration::hours(1))
                .is_empty()
        );
    }

    #[test]
    fn waits_that_predate_the_daemon_and_unwanted_states_are_quiet() {
        let mut n = Notifier::new(START);
        let now = START + time::Duration::seconds(5);
        let sessions = [
            s("cc-old", State::Blocked, START - time::Duration::minutes(3)),
            s("cc-working", State::Working, now),
            s("cc-done", State::Done, now),
        ];
        assert!(n.due(&cfg(), &sessions, now).is_empty());
        let mut with_done = cfg();
        with_done.on.push("done".into());
        assert_eq!(n.due(&with_done, &sessions, now).len(), 1);
    }

    #[test]
    fn a_new_wait_inside_the_debounce_window_waits_its_turn() {
        let mut n = Notifier::new(START);
        let t1 = START + time::Duration::seconds(10);
        assert_eq!(n.due(&cfg(), &[s("cc-1", State::Blocked, t1)], t1).len(), 1);
        // Unblocked and blocked again 5 s later: a new wait, but too soon.
        let t2 = t1 + time::Duration::seconds(5);
        assert!(
            n.due(&cfg(), &[s("cc-1", State::Blocked, t2)], t2)
                .is_empty()
        );
        let t3 = t1 + time::Duration::seconds(40);
        assert_eq!(n.due(&cfg(), &[s("cc-1", State::Blocked, t2)], t3).len(), 1);
    }

    #[test]
    fn delivery_passes_text_as_arguments_or_through_the_template() {
        let note = Note {
            session_id: "cc-1".into(),
            title: "a \"quoted\" name".into(),
            body: "end run\" & do shell script \"x".into(),
        };
        let argv = delivery_command(&cfg(), &note).unwrap();
        // The text is its own argv entries, never part of an -e script.
        assert_eq!(argv[argv.len() - 2], note.title);
        assert_eq!(argv[argv.len() - 1], note.body);
        let mut custom = cfg();
        custom.command = [
            "terminal-notifier",
            "-title",
            "{title}",
            "-message",
            "{body}",
            "-execute",
            "bogyman focus {id}",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            delivery_command(&custom, &note).unwrap(),
            vec![
                "terminal-notifier",
                "-title",
                "a \"quoted\" name",
                "-message",
                "end run\" & do shell script \"x",
                "-execute",
                "bogyman focus cc-1"
            ]
        );
    }
}
