//! Core types for bogyman: the `Session` record (the product's contract),
//! the `Event` stream that feeds it, and the reducer that turns one into the other.

pub mod event;
pub mod liveness;
pub mod model;
pub mod reduce;

pub use event::{Event, EventKind};
pub use model::{
    Host, Kind, SCHEMA, Session, Source, State, Tasks, Terminal, Tokens, Vendor, WaitingFor,
};
pub use reduce::{Reducer, ReducerOptions, poll_state};

/// Privacy budget for any free text kept from a vendor payload.
pub const DETAIL_MAX_CHARS: usize = 200;

/// Replace control characters (C0, C1, DEL, line and paragraph separators) with
/// spaces, so vendor-sourced text can never carry terminal escapes.
pub fn strip_control(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Truncate a free-text detail to the privacy budget on a char boundary, with
/// control characters removed.
pub fn clip_detail(s: &str) -> String {
    let s = strip_control(s);
    if s.chars().count() <= DETAIL_MAX_CHARS {
        return s.trim().to_string();
    }
    let mut out: String = s.chars().take(DETAIL_MAX_CHARS - 1).collect();
    out.push('…');
    out.trim().to_string()
}
