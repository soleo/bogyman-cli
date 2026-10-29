//! The `--json` contract, schema 1. One envelope for every command:
//!
//! ```json
//! {"schema":1,"command":"ls","ok":true,"data":{"sessions":[]}}
//! {"schema":1,"command":"show","ok":false,"error":{"message":"…","exit":1}}
//! ```
//!
//! Pretty on a TTY, one line when piped, so a consumer reading a pipe can parse
//! line by line. Failures go to stdout too: a script reading one stream always
//! gets JSON back, and the exit code carries the same status.
//!
//! Key order is meaningful here and holds because serde_json is built with
//! `preserve_order`.

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::Ctx;

fn envelope(command: &str, ok: bool) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("schema".into(), json!(bogyman_core::SCHEMA));
    m.insert("command".into(), Value::String(command.to_owned()));
    m.insert("ok".into(), Value::Bool(ok));
    m
}

fn success(command: &str, data: Value) -> Value {
    let mut m = envelope(command, true);
    m.insert("data".into(), data);
    Value::Object(m)
}

fn failure(command: &str, exit: u8, message: &str) -> Value {
    let mut m = envelope(command, false);
    m.insert("error".into(), json!({"message": message, "exit": exit}));
    Value::Object(m)
}

fn emit(v: &Value, pretty: bool) -> Result<()> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(v)?);
    } else {
        println!("{v}");
    }
    Ok(())
}

/// Success. Pretty on a TTY, compact when piped.
pub fn ok(ctx: &Ctx, command: &str, data: Value) -> Result<()> {
    emit(&success(command, data), ctx.tty)
}

/// Success, always on one line. For `--watch`, where the output is a stream and
/// has to stay line-delimited whether or not a terminal is attached.
pub fn ok_line(command: &str, data: Value) -> Result<()> {
    emit(&success(command, data), false)
}

/// Failure. Infallible, so the error path can never itself fail.
pub fn err(ctx: &Ctx, command: &str, exit: u8, message: &str) {
    let _ = emit(&failure(command, exit, message), ctx.tty);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_carries_schema_command_and_data() {
        let v = success("ls", json!({"sessions": []}));
        assert_eq!(v["schema"], bogyman_core::SCHEMA);
        assert_eq!(v["command"], "ls");
        assert_eq!(v["ok"], true);
        assert!(v["data"]["sessions"].is_array());
        assert!(v.get("error").is_none());
    }

    #[test]
    fn failure_carries_message_and_exit_but_no_data() {
        let v = failure("show", 1, "no session matches 'zzz'");
        assert_eq!(v["command"], "show");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["message"], "no session matches 'zzz'");
        assert_eq!(v["error"]["exit"], 1);
        assert!(v.get("data").is_none());
    }

    #[test]
    fn keys_keep_their_order() {
        let v = success("gc", json!({}));
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["schema", "command", "ok", "data"]);
    }

    #[test]
    fn the_watch_stream_stays_line_delimited() {
        let v = success("ls", json!({"sessions": [{"id": "cc-1", "name": "a\nb"}]}));
        assert!(
            !v.to_string().contains('\n'),
            "newlines inside strings must be escaped, not emitted raw"
        );
    }
}
