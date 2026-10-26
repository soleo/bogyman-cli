//! Collectors turn vendor payloads into `bogyman_core::Event`s.
//!
//! Push collectors (`claude_hooks`, `codex_hooks`) parse the JSON a vendor hook
//! hands `bogyman hook <vendor>` on stdin. Pull collectors (`claude_sessions`)
//! read what the vendor leaves on disk. Every parser keeps the raw payload and
//! fails soft: an unrecognized event becomes `EventKind::Unknown`, never an error,
//! as long as a session id can be found.

pub mod claude_hooks;
pub mod claude_sessions;
pub mod codex_hooks;
mod util;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("payload is not a JSON object")]
    NotAnObject,
    #[error("payload has no session id (looked for {0})")]
    MissingSessionId(&'static str),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}
