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

/// Truncate a free-text detail to the privacy budget (200 chars) on a char boundary.
pub fn clip_detail(s: &str) -> String {
    const MAX: usize = 200;
    if s.chars().count() <= MAX {
        return s.trim().to_string();
    }
    let mut out: String = s.chars().take(MAX - 1).collect();
    out.push('…');
    out.trim().to_string()
}
