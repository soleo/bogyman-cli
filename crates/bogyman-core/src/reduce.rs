//! Fold events into sessions.
//!
//! Resolution rules from the PRD: a hook event beats a poll of the same age;
//! `since` resets only on a state change; a working session with no event for
//! 30 minutes is marked stale but not changed.

use std::collections::HashMap;

use time::{Duration, OffsetDateTime};

use crate::clip_detail;
use crate::event::{Event, EventKind};
use crate::model::{
    Host, Kind, SCHEMA, Session, Source, State, Tasks, Vendor, WaitingFor, repo_from_cwd,
};

pub const STALE_AFTER: Duration = Duration::minutes(30);

/// Map a poller's status word onto a session state. `None` means "no opinion".
pub fn poll_state(status: &str) -> Option<(State, Option<WaitingFor>)> {
    match status {
        "busy" | "working" | "running" => Some((State::Working, None)),
        "waiting" | "blocked" => Some((State::Blocked, Some(WaitingFor::Unknown))),
        "idle" | "done" => Some((State::Done, None)),
        "stopped" | "exited" | "ended" => Some((State::Stopped, None)),
        "failed" | "error" => Some((State::Failed, None)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReducerOptions {
    /// Drop free-text detail (what a session is waiting on) entirely.
    pub redact: bool,
}

#[derive(Debug, Default)]
pub struct Reducer {
    pub sessions: HashMap<String, Session>,
    pub opts: ReducerOptions,
}

impl Reducer {
    pub fn new(sessions: HashMap<String, Session>, opts: ReducerOptions) -> Self {
        Reducer { sessions, opts }
    }

    pub fn apply_all<'a>(&mut self, events: impl IntoIterator<Item = &'a Event>) {
        for ev in events {
            self.apply(ev);
        }
    }

    pub fn apply(&mut self, ev: &Event) {
        let key = Session::key(ev.vendor, &ev.vendor_session_id);
        let redact = self.opts.redact;
        let session = self.sessions.entry(key).or_insert_with(|| new_session(ev));
        apply_event(session, ev, redact);
    }

    /// Finish a fold: stale flags and process liveness.
    pub fn finalize(&mut self, now: OffsetDateTime, pid_alive: impl Fn(u32) -> bool) {
        for s in self.sessions.values_mut() {
            s.stale = s.state == State::Working && now - s.observed_at > STALE_AFTER;
            if s.state.is_live()
                && let Some(pid) = s.pid
                && !pid_alive(pid)
            {
                set_state(s, State::Stopped, None, None, now, Source::Poll);
            }
        }
    }

    pub fn into_sessions(self) -> Vec<Session> {
        let mut v: Vec<Session> = self.sessions.into_values().collect();
        crate::model::assign_unique_ids(&mut v);
        v
    }
}

fn new_session(ev: &Event) -> Session {
    let repo = ev.cwd.as_deref().and_then(repo_from_cwd);
    Session {
        schema: SCHEMA,
        id: Session::short_id(ev.vendor, &ev.vendor_session_id),
        vendor: ev.vendor,
        vendor_session_id: ev.vendor_session_id.clone(),
        name: None,
        cwd: ev.cwd.clone(),
        repo,
        kind: Kind::Unknown,
        host: default_host(ev.vendor),
        state: State::Unknown,
        waiting_for: None,
        waiting_detail: None,
        since: ev.at,
        started_at: ev.at,
        summary: None,
        tasks: None,
        turns: 0,
        tokens: None,
        model: None,
        pid: ev.pid,
        agent_id: None,
        terminal: None,
        transcript_path: ev.transcript_path.clone(),
        source: ev.source,
        observed_at: ev.at,
        stale: false,
    }
}

fn default_host(vendor: Vendor) -> Host {
    match vendor {
        Vendor::ClaudeCode => Host::Unknown,
        Vendor::Codex => Host::Terminal,
    }
}

fn set_state(
    s: &mut Session,
    state: State,
    waiting_for: Option<WaitingFor>,
    detail: Option<String>,
    at: OffsetDateTime,
    source: Source,
) {
    if s.state != state || s.waiting_for != waiting_for {
        s.since = at;
    }
    s.state = state;
    s.waiting_for = waiting_for;
    s.waiting_detail = detail;
    s.source = source;
}

fn apply_event(s: &mut Session, ev: &Event, redact: bool) {
    // Metadata that every event may carry.
    if s.cwd.is_none() && ev.cwd.is_some() {
        s.cwd = ev.cwd.clone();
        s.repo = ev.cwd.as_deref().and_then(repo_from_cwd);
    }
    if s.pid.is_none() && ev.pid.is_some() {
        s.pid = ev.pid;
    }
    if s.transcript_path.is_none() && ev.transcript_path.is_some() {
        s.transcript_path = ev.transcript_path.clone();
    }
    if ev.at < s.started_at {
        s.started_at = ev.at;
    }

    let detail = |d: &Option<String>| -> Option<String> {
        if redact {
            None
        } else {
            d.as_deref().map(clip_detail)
        }
    };

    if let EventKind::StatusPoll {
        status,
        name,
        kind,
        host,
        model,
        started_at,
        waiting_for: poll_waiting,
        agent_id,
    } = &ev.kind
    {
        if agent_id.is_some() {
            s.agent_id = agent_id.clone();
        }
        if name.is_some() {
            s.name = name.clone();
        }
        if let Some(k) = kind {
            s.kind = *k;
        }
        if let Some(h) = host {
            s.host = *h;
        }
        if model.is_some() {
            s.model = model.clone();
        }
        if let Some(st) = started_at
            && *st < s.started_at
        {
            s.started_at = *st;
        }
        // A hook event beats a poll of the same age.
        let hook_is_fresher = s.source == Source::Hook && s.observed_at >= ev.at;
        if hook_is_fresher {
            return;
        }
        if let Some((state, waiting)) = poll_state(status) {
            let waiting = if state == State::Blocked {
                poll_waiting.or(waiting)
            } else {
                waiting
            };
            // Don't downgrade a precise blocked reason to "unknown" on a poll that agrees.
            let waiting = if state == State::Blocked
                && s.state == State::Blocked
                && waiting == Some(WaitingFor::Unknown)
            {
                s.waiting_for.or(waiting)
            } else {
                waiting
            };
            if s.state != state || s.waiting_for != waiting {
                let d = if state == State::Blocked {
                    s.waiting_detail.clone()
                } else {
                    None
                };
                let refined = s.state == state;
                let since = s.since;
                set_state(s, state, waiting, d, ev.at, Source::Poll);
                if refined {
                    // Only the reason got more precise; the wait itself is the same one.
                    s.since = since;
                }
            }
        }
        s.observed_at = s.observed_at.max(ev.at);
        return;
    }

    if let EventKind::TranscriptPoll { model, host } = &ev.kind {
        if model.is_some() {
            s.model = model.clone();
        }
        if let Some(h) = host
            && s.host == Host::Unknown
        {
            s.host = *h;
        }
        return;
    }

    // Everything below is a hook event.
    s.observed_at = ev.at;
    match &ev.kind {
        EventKind::SessionStart {
            kind,
            host,
            name,
            model,
        } => {
            if let Some(k) = kind {
                s.kind = *k;
            }
            if let Some(h) = host {
                s.host = *h;
            }
            if name.is_some() {
                s.name = name.clone();
            }
            if model.is_some() {
                s.model = model.clone();
            }
            // A fresh session sits at the prompt, ready for the user.
            if s.state == State::Unknown || s.state == State::Stopped {
                set_state(s, State::Done, None, None, ev.at, Source::Hook);
            }
        }
        EventKind::PromptSubmitted => {
            s.turns += 1;
            set_state(s, State::Working, None, None, ev.at, Source::Hook);
        }
        EventKind::ToolUse { tool, detail: d } => {
            if tool == "AskUserQuestion" {
                set_state(
                    s,
                    State::Blocked,
                    Some(WaitingFor::Question),
                    detail(d),
                    ev.at,
                    Source::Hook,
                );
            } else if s.state != State::Working {
                set_state(s, State::Working, None, None, ev.at, Source::Hook);
            }
        }
        EventKind::PermissionRequest { tool, detail: d } => {
            let waiting = if tool == "AskUserQuestion" {
                WaitingFor::Question
            } else {
                WaitingFor::PermissionPrompt
            };
            set_state(
                s,
                State::Blocked,
                Some(waiting),
                detail(d),
                ev.at,
                Source::Hook,
            );
        }
        EventKind::Notification {
            notification_type,
            message,
        } => match notification_type.as_str() {
            "permission_prompt" => {
                if s.state != State::Blocked {
                    set_state(
                        s,
                        State::Blocked,
                        Some(WaitingFor::PermissionPrompt),
                        detail(message),
                        ev.at,
                        Source::Hook,
                    );
                }
            }
            "idle_prompt" => {
                if s.state == State::Working || s.state == State::Unknown {
                    set_state(s, State::Done, None, None, ev.at, Source::Hook);
                }
            }
            "elicitation_dialog" | "elicitation" => set_state(
                s,
                State::Blocked,
                Some(WaitingFor::Elicitation),
                detail(message),
                ev.at,
                Source::Hook,
            ),
            "agent_needs_input" | "question" => set_state(
                s,
                State::Blocked,
                Some(WaitingFor::Question),
                detail(message),
                ev.at,
                Source::Hook,
            ),
            "auth_required" | "auth_expired" | "login_required" => set_state(
                s,
                State::Blocked,
                Some(WaitingFor::Auth),
                detail(message),
                ev.at,
                Source::Hook,
            ),
            "sandbox_request" | "sandbox" => set_state(
                s,
                State::Blocked,
                Some(WaitingFor::SandboxRequest),
                detail(message),
                ev.at,
                Source::Hook,
            ),
            // auth_success and anything we do not understand leave the state alone.
            _ => {}
        },
        EventKind::Stop { message } => {
            if message.is_some() && !redact {
                s.summary = message.as_deref().map(clip_detail);
            }
            set_state(s, State::Done, None, None, ev.at, Source::Hook);
        }
        EventKind::StopFailure { error } => {
            set_state(s, State::Failed, None, detail(error), ev.at, Source::Hook);
        }
        EventKind::TaskCreated { subject } => {
            let t = s.tasks.get_or_insert_with(Tasks::default);
            t.total += 1;
            if t.current.is_none() && !redact {
                t.current = subject.as_deref().map(clip_detail);
            }
        }
        EventKind::TaskCompleted { subject } => {
            let t = s.tasks.get_or_insert_with(Tasks::default);
            t.done += 1;
            if t.total < t.done {
                t.total = t.done;
            }
            if !redact {
                t.current = subject.as_deref().map(clip_detail);
            }
        }
        EventKind::SessionEnd { .. } => {
            set_state(s, State::Stopped, None, None, ev.at, Source::Hook);
        }
        EventKind::Unknown { .. } => {
            // Keep the state; an unrecognized vendor event only proves the session is alive.
            if s.state == State::Stopped {
                set_state(s, State::Unknown, None, None, ev.at, Source::Hook);
            }
        }
        EventKind::StatusPoll { .. } | EventKind::TranscriptPoll { .. } => {
            unreachable!("handled above")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn ev(at: OffsetDateTime, kind: EventKind) -> Event {
        Event {
            id: None,
            at,
            vendor: Vendor::ClaudeCode,
            vendor_session_id: "1f0e2c9a-6d0b-4c11-9f39-2a77c1d4e8b5".into(),
            source: Source::Hook,
            kind,
            cwd: Some("/Users/x/dev/tag-web".into()),
            pid: None,
            transcript_path: None,
            raw: serde_json::Value::Null,
        }
    }

    fn poll(at: OffsetDateTime, status: &str) -> Event {
        let mut e = ev(
            at,
            EventKind::StatusPoll {
                status: status.into(),
                name: Some("flaky-test-fix".into()),
                kind: Some(Kind::Interactive),
                host: Some(Host::Terminal),
                model: None,
                started_at: None,
                waiting_for: None,
                agent_id: None,
            },
        );
        e.source = Source::Poll;
        e.pid = Some(48122);
        e
    }

    const T0: OffsetDateTime = datetime!(2026-10-26 21:00:00 UTC);

    fn only(r: Reducer) -> Session {
        let mut v = r.into_sessions();
        assert_eq!(v.len(), 1);
        v.pop().unwrap()
    }

    #[test]
    fn permission_request_blocks_and_keeps_since() {
        let mut r = Reducer::default();
        r.apply(&ev(
            T0,
            EventKind::SessionStart {
                kind: None,
                host: None,
                name: None,
                model: None,
            },
        ));
        r.apply(&ev(T0 + Duration::seconds(5), EventKind::PromptSubmitted));
        r.apply(&ev(
            T0 + Duration::seconds(60),
            EventKind::PermissionRequest {
                tool: "Bash".into(),
                detail: Some("npm test".into()),
            },
        ));
        // The vendor's delayed notification must not reset the wait clock.
        r.apply(&ev(
            T0 + Duration::seconds(66),
            EventKind::Notification {
                notification_type: "permission_prompt".into(),
                message: Some("Claude needs your permission".into()),
            },
        ));
        let s = only(r);
        assert_eq!(s.id, "cc-1f0e2c9a");
        assert_eq!(s.state, State::Blocked);
        assert_eq!(s.waiting_for, Some(WaitingFor::PermissionPrompt));
        assert_eq!(s.waiting_detail.as_deref(), Some("npm test"));
        assert_eq!(s.since, T0 + Duration::seconds(60));
        assert_eq!(s.turns, 1);
        assert_eq!(s.repo.as_deref(), Some("tag-web"));
    }

    #[test]
    fn tool_use_after_permission_resumes_work_and_stop_finishes() {
        let mut r = Reducer::default();
        r.apply(&ev(T0, EventKind::PromptSubmitted));
        r.apply(&ev(
            T0 + Duration::seconds(1),
            EventKind::PermissionRequest {
                tool: "Bash".into(),
                detail: None,
            },
        ));
        r.apply(&ev(
            T0 + Duration::seconds(30),
            EventKind::ToolUse {
                tool: "Bash".into(),
                detail: None,
            },
        ));
        assert_eq!(r.sessions.values().next().unwrap().state, State::Working);
        r.apply(&ev(
            T0 + Duration::seconds(90),
            EventKind::Stop {
                message: Some("Done.".into()),
            },
        ));
        let s = only(r);
        assert_eq!(s.state, State::Done);
        assert_eq!(s.waiting_for, None);
        assert_eq!(s.since, T0 + Duration::seconds(90));
        assert_eq!(s.summary.as_deref(), Some("Done."));
    }

    #[test]
    fn ask_user_question_is_a_question_not_a_permission() {
        let mut r = Reducer::default();
        r.apply(&ev(T0, EventKind::PromptSubmitted));
        r.apply(&ev(
            T0 + Duration::seconds(1),
            EventKind::ToolUse {
                tool: "AskUserQuestion".into(),
                detail: Some("2 options".into()),
            },
        ));
        let s = only(r);
        assert_eq!(s.state, State::Blocked);
        assert_eq!(s.waiting_for, Some(WaitingFor::Question));
    }

    #[test]
    fn tasks_count_up() {
        let mut r = Reducer::default();
        for i in 0..7 {
            r.apply(&ev(
                T0 + Duration::seconds(i),
                EventKind::TaskCreated {
                    subject: Some(format!("t{i}")),
                },
            ));
        }
        for i in 0..3 {
            r.apply(&ev(
                T0 + Duration::seconds(10 + i),
                EventKind::TaskCompleted {
                    subject: Some(format!("t{i}")),
                },
            ));
        }
        let s = only(r);
        let t = s.tasks.unwrap();
        assert_eq!((t.done, t.total), (3, 7));
        assert_eq!(t.current.as_deref(), Some("t2"));
    }

    #[test]
    fn hook_beats_poll_of_same_age_but_newer_poll_wins() {
        let mut r = Reducer::default();
        r.apply(&ev(T0, EventKind::PromptSubmitted));
        // An older poll saying idle must not override the newer hook.
        r.apply(&poll(T0 - Duration::seconds(1), "idle"));
        assert_eq!(r.sessions.values().next().unwrap().state, State::Working);
        // A newer poll saying waiting is accepted, with reason unknown.
        r.apply(&poll(T0 + Duration::seconds(5), "waiting"));
        let s = r.sessions.values().next().unwrap().clone();
        assert_eq!(s.state, State::Blocked);
        assert_eq!(s.waiting_for, Some(WaitingFor::Unknown));
        assert_eq!(s.name.as_deref(), Some("flaky-test-fix"));
        assert_eq!(s.host, Host::Terminal);
        assert_eq!(s.pid, Some(48122));
        // A poll that agrees with a precise hook reason keeps the reason and the clock.
        let mut r2 = Reducer::default();
        r2.apply(&ev(
            T0,
            EventKind::PermissionRequest {
                tool: "Bash".into(),
                detail: Some("rm -rf".into()),
            },
        ));
        r2.apply(&poll(T0 + Duration::seconds(2), "waiting"));
        let s2 = only(r2);
        assert_eq!(s2.waiting_for, Some(WaitingFor::PermissionPrompt));
        assert_eq!(s2.since, T0);
    }

    #[test]
    fn session_end_stops_and_dead_pid_stops() {
        let mut r = Reducer::default();
        r.apply(&ev(T0, EventKind::PromptSubmitted));
        r.apply(&ev(
            T0 + Duration::seconds(1),
            EventKind::SessionEnd {
                reason: Some("other".into()),
            },
        ));
        assert_eq!(r.sessions.values().next().unwrap().state, State::Stopped);

        let mut r2 = Reducer::default();
        r2.apply(&poll(T0, "busy"));
        r2.finalize(T0 + Duration::seconds(10), |_| false);
        assert_eq!(r2.sessions.values().next().unwrap().state, State::Stopped);
    }

    #[test]
    fn stale_after_thirty_minutes_without_events() {
        let mut r = Reducer::default();
        r.apply(&ev(T0, EventKind::PromptSubmitted));
        r.finalize(T0 + Duration::minutes(31), |_| true);
        let s = only(r);
        assert_eq!(s.state, State::Working);
        assert!(s.stale);
    }

    #[test]
    fn transcript_poll_sets_model_without_touching_state() {
        let mut r = Reducer::default();
        r.apply(&ev(
            T0,
            EventKind::PermissionRequest {
                tool: "Bash".into(),
                detail: None,
            },
        ));
        let mut tp = ev(
            T0 + Duration::seconds(5),
            EventKind::TranscriptPoll {
                model: Some("claude-opus-5-5".into()),
                host: Some(Host::Terminal),
            },
        );
        tp.source = Source::Poll;
        r.apply(&tp);
        let s = only(r);
        assert_eq!(s.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(s.host, Host::Terminal);
        assert_eq!(s.state, State::Blocked);
        assert_eq!(s.since, T0);
        assert_eq!(
            s.observed_at, T0,
            "a transcript read is not a liveness signal"
        );
    }

    #[test]
    fn colliding_short_ids_get_longer() {
        let mut r = Reducer::default();
        for sid in [
            "01a17abd-085b-7d12-b8ba-48afcdfd4f9c",
            "01a17abd-8964-7882-92af-8d40a5238a93",
            "01a17abe-72c2-7551-9e03-b5ba2f1ee518",
        ] {
            let mut e = ev(T0, EventKind::PromptSubmitted);
            e.vendor = Vendor::Codex;
            e.vendor_session_id = sid.into();
            r.apply(&e);
        }
        let mut ids: Vec<String> = r.into_sessions().into_iter().map(|s| s.id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["cx-01a17abd085b", "cx-01a17abd8964", "cx-01a17abe"]
        );
    }

    #[test]
    fn redact_drops_detail() {
        let mut r = Reducer::new(HashMap::new(), ReducerOptions { redact: true });
        r.apply(&ev(
            T0,
            EventKind::PermissionRequest {
                tool: "Bash".into(),
                detail: Some("secret".into()),
            },
        ));
        let s = only(r);
        assert_eq!(s.waiting_detail, None);
        assert_eq!(s.state, State::Blocked);
    }

    #[test]
    fn control_characters_are_stripped() {
        assert_eq!(
            clip_detail("a\u{1b}[2Jb\nc\u{7f}d\u{2028}e"),
            "a [2Jb c d e"
        );
        assert_eq!(crate::strip_control("plain"), "plain");
    }

    #[test]
    fn detail_is_clipped_to_200_chars() {
        let long = "x".repeat(500);
        assert_eq!(clip_detail(&long).chars().count(), 200);
        assert_eq!(clip_detail("short"), "short");
    }

    #[test]
    fn a_precise_poll_reason_refines_a_wait_without_resetting_its_age() {
        let mut r = Reducer::default();
        r.apply(&poll(T0, "waiting"));
        let mut precise = poll(T0 + Duration::seconds(5), "blocked");
        if let EventKind::StatusPoll {
            waiting_for,
            agent_id,
            ..
        } = &mut precise.kind
        {
            *waiting_for = Some(WaitingFor::PermissionPrompt);
            *agent_id = Some("7c5dcf5d".into());
        }
        r.apply(&precise);
        let s = only(r);
        assert_eq!(s.state, State::Blocked);
        assert_eq!(s.waiting_for, Some(WaitingFor::PermissionPrompt));
        assert_eq!(s.since, T0, "the wait started at the first poll");
        assert_eq!(s.agent_id.as_deref(), Some("7c5dcf5d"));
    }
}
