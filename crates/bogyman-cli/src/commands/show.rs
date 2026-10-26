use anyhow::{Context, Result};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::render::{ago, fmt_age, print_rows, waiting_cell};
use crate::{Ctx, EXIT_OK, refresh, resolve};

pub fn run(ctx: &Ctx, id: &str, open: bool) -> Result<u8> {
    let now = OffsetDateTime::now_utc();
    let sessions = refresh::load_sessions(ctx, true)?;
    let s = resolve::resolve(&sessions, id)?.clone();
    let store = refresh::open_store(ctx)?;
    let events = store.events_for(s.vendor, &s.vendor_session_id, 20)?;

    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": bogyman_core::SCHEMA,
                "session": s,
                "events": events,
            }))?
        );
    } else {
        let line = |k: &str, v: String| {
            if !v.is_empty() {
                println!("{k:<14}{v}");
            }
        };
        line("id", s.id.clone());
        line("vendor id", s.vendor_session_id.clone());
        line("vendor", s.vendor.label().into());
        line(
            "host",
            format!("{} · {:?}", s.host.label(), s.kind).to_lowercase(),
        );
        line("name", s.display_name());
        line("cwd", s.cwd.clone().unwrap_or_default());
        line(
            "state",
            format!(
                "{}{} for {}",
                s.state.label(),
                if s.stale { " (stale)" } else { "" },
                fmt_age(s.age_secs(now))
            ),
        );
        line("waiting for", waiting_cell(&s));
        line("since", s.since.format(&Rfc3339)?);
        line(
            "started",
            format!(
                "{} ({})",
                s.started_at.format(&Rfc3339)?,
                ago(s.started_at, now)
            ),
        );
        line(
            "turns",
            if s.turns > 0 {
                s.turns.to_string()
            } else {
                String::new()
            },
        );
        if let Some(t) = &s.tasks {
            line(
                "tasks",
                format!(
                    "{}/{}{}",
                    t.done,
                    t.total,
                    t.current
                        .as_deref()
                        .map(|c| format!(" · {c}"))
                        .unwrap_or_default()
                ),
            );
        }
        line("model", s.model.clone().unwrap_or_default());
        line("summary", s.summary.clone().unwrap_or_default());
        line("pid", s.pid.map(|p| p.to_string()).unwrap_or_default());
        line("transcript", s.transcript_path.clone().unwrap_or_default());
        line(
            "observed",
            format!("{} via {:?}", ago(s.observed_at, now), s.source).to_lowercase(),
        );
        if !events.is_empty() {
            println!();
            let rows = events
                .iter()
                .map(|e| {
                    vec![
                        e.at.format(&Rfc3339).unwrap_or_default(),
                        format!("{:?}", e.source).to_lowercase(),
                        e.kind.label(),
                        e.kind.detail().unwrap_or("").chars().take(60).collect(),
                    ]
                })
                .collect();
            print_rows(ctx, &["AT", "SRC", "EVENT", "DETAIL"], rows);
        }
    }

    if open {
        let path = s
            .transcript_path
            .as_deref()
            .context("this session has no known transcript path")?;
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let status = std::process::Command::new(opener).arg(path).status();
        if !matches!(status, Ok(st) if st.success()) {
            println!("{path}");
        }
    }
    Ok(EXIT_OK)
}
