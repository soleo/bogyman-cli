//! Turn what the user typed into one session: a bogyman id, a unique prefix,
//! the vendor's own id, or the session name.

use anyhow::{Result, anyhow};
use bogyman_core::Session;

pub fn resolve<'a>(sessions: &'a [Session], query: &str) -> Result<&'a Session> {
    let q = query.trim();
    let ql = q.to_ascii_lowercase();
    if let Some(s) = sessions
        .iter()
        .find(|s| s.id == ql || s.vendor_session_id == q)
    {
        return Ok(s);
    }
    if let Some(s) = sessions
        .iter()
        .find(|s| s.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(q)))
    {
        return Ok(s);
    }
    let candidates: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            s.id.starts_with(&ql)
                || s.vendor_session_id.starts_with(q)
                || s.name
                    .as_deref()
                    .is_some_and(|n| n.to_ascii_lowercase().starts_with(&ql))
        })
        .collect();
    match candidates.len() {
        1 => Ok(candidates[0]),
        0 => Err(anyhow!("no session matches '{q}' (try `bogyman ls --all`)")),
        _ => {
            let list: Vec<String> = candidates
                .iter()
                .map(|s| format!("{} ({})", s.id, s.display_name()))
                .collect();
            Err(anyhow!("'{q}' is ambiguous: {}", list.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bogyman_core::{Host, Kind, SCHEMA, Source, State, Vendor};
    use time::macros::datetime;

    fn s(id: &str, vid: &str, name: Option<&str>) -> Session {
        let t = datetime!(2026-10-26 21:00:00 UTC);
        Session {
            schema: SCHEMA,
            id: id.into(),
            vendor: Vendor::ClaudeCode,
            vendor_session_id: vid.into(),
            name: name.map(String::from),
            cwd: None,
            repo: None,
            kind: Kind::Unknown,
            host: Host::Unknown,
            state: State::Done,
            waiting_for: None,
            waiting_detail: None,
            since: t,
            started_at: t,
            summary: None,
            tasks: None,
            turns: 0,
            tokens: None,
            model: None,
            pid: None,
            terminal: None,
            transcript_path: None,
            source: Source::Hook,
            observed_at: t,
            stale: false,
        }
    }

    #[test]
    fn resolves_by_id_prefix_vendor_id_and_name() {
        let all = vec![
            s("cc-1f0e2c9a", "1f0e2c9a-6d0b", Some("flaky-test-fix")),
            s("cc-1f0e9999", "1f0e9999-aaaa", Some("deps-audit")),
        ];
        assert_eq!(resolve(&all, "cc-1f0e2c9a").unwrap().id, "cc-1f0e2c9a");
        assert_eq!(resolve(&all, "1f0e9999-aaaa").unwrap().id, "cc-1f0e9999");
        assert_eq!(resolve(&all, "flaky").unwrap().id, "cc-1f0e2c9a");
        assert_eq!(resolve(&all, "DEPS-AUDIT").unwrap().id, "cc-1f0e9999");
        assert_eq!(resolve(&all, "cc-1f0e9").unwrap().id, "cc-1f0e9999");
        assert!(
            resolve(&all, "cc-1f0e")
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(
            resolve(&all, "zzz")
                .unwrap_err()
                .to_string()
                .contains("no session")
        );
    }
}
