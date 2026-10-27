//! Human output: tables on a TTY, TSV when piped, ages like `9m12s`.

use bogyman_core::{Session, State, strip_control};
use comfy_table::{Cell, ContentArrangement, Table, presets};
use owo_colors::OwoColorize;
use time::OffsetDateTime;

use crate::Ctx;
use crate::paths::tilde;

pub fn fmt_age(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d{:02}h", s / 86_400, (s % 86_400) / 3600)
    }
}

pub fn ago(t: OffsetDateTime, now: OffsetDateTime) -> String {
    format!("{} ago", fmt_age((now - t).whole_seconds()))
}

pub fn state_cell(ctx: &Ctx, state: State, stale: bool) -> String {
    let label = if stale {
        format!("{} (stale)", state.label())
    } else {
        state.label().to_string()
    };
    if !ctx.color {
        return label;
    }
    match state {
        State::Blocked => label.yellow().bold().to_string(),
        State::Working => label.blue().to_string(),
        State::Done => label.green().to_string(),
        State::Failed => label.red().to_string(),
        State::Stopped | State::Unknown => label.dimmed().to_string(),
    }
}

pub fn waiting_cell(s: &Session) -> String {
    match (s.waiting_for, &s.waiting_detail) {
        (Some(w), Some(d)) => format!("{} · {}", w.label(), d),
        (Some(w), None) => w.label().to_string(),
        (None, _) => String::new(),
    }
}

/// Model names without the vendor prefix, so the column stays narrow.
pub fn model_cell(s: &Session) -> String {
    match s.model.as_deref() {
        Some(m) => m.strip_prefix("claude-").unwrap_or(m).to_string(),
        None => String::new(),
    }
}

pub fn cwd_cell(s: &Session) -> String {
    s.cwd
        .as_deref()
        .map(|c| strip_control(&tilde(std::path::Path::new(c))))
        .unwrap_or_default()
}

/// Print rows as a table on a TTY, or as header-less TSV when piped.
pub fn print_rows(ctx: &Ctx, headers: &[&str], rows: Vec<Vec<String>>) {
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|r| r.into_iter().map(|c| clean_cell(&c)).collect())
        .collect();
    if !ctx.tty {
        for r in rows {
            println!("{}", r.join("\t"));
        }
        return;
    }
    let mut t = Table::new();
    t.load_style(presets::NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic);
    t.set_header(headers.iter().map(|h| {
        if ctx.color {
            Cell::new(h.bold().to_string())
        } else {
            Cell::new(h)
        }
    }));
    for r in rows {
        t.add_row(r);
    }
    println!("{t}");
}

/// Belt and braces: every vendor-sourced cell is already stripped by `truncate`
/// or `cwd_cell`; the only cells allowed to start with ESC are the colored state
/// labels this module builds itself from constant text.
fn clean_cell(c: &str) -> String {
    if c.starts_with('\u{1b}') {
        c.to_string()
    } else {
        strip_control(c)
    }
}

/// Blocked first, oldest wait first; then working, done and the rest, most recent first.
pub fn sort_for_listing(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then_with(|| match a.state {
                State::Blocked => a.since.cmp(&b.since),
                _ => b.observed_at.cmp(&a.observed_at),
            })
    });
}

/// Clip to `max` chars with control characters removed. Every vendor-sourced
/// string goes through here or `strip_control` before it reaches the terminal.
pub fn truncate(s: &str, max: usize) -> String {
    let s = strip_control(s);
    if s.chars().count() <= max {
        s
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
