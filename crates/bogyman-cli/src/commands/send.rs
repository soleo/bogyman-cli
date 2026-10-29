//! `bogyman send <id> "msg"`: deliver a message to a session without attaching.
//!
//! Claude Code: every session with cross-session messaging binds an inbox socket
//! (`messagingSocketPath` in `~/.claude/sessions/<pid>.json`). A script posts
//! newline-delimited JSON to it; the frame Claude Code documents is
//! `{"type":"user","message":{"role":"user","content":"…"}}`, with an optional
//! auth line first (required only on native Windows). The receiving session reads
//! the message between tool calls, or starts a turn with it when idle, and its own
//! inbound controls decide whether to deliver, hold or refuse it.
//!
//! Codex: wraps the vendor's queue command, templated in `[send] codex`.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use bogyman_collectors::claude_sessions;
use bogyman_core::{Session, State, Vendor};

use crate::{Ctx, EXIT_ERROR, EXIT_OK, EXIT_VENDOR_UNAVAILABLE, json, refresh, resolve};

const MAX_MESSAGE_BYTES: usize = 1_000_000;

/// The JSON line Claude Code's inbox accepts.
pub fn claude_frame(message: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": message },
    })
    .to_string()
}

/// Expand `{id}` and `{message}` in a command template.
pub fn render_template(template: &[String], id: &str, message: &str) -> Vec<String> {
    template
        .iter()
        .map(|part| part.replace("{id}", id).replace("{message}", message))
        .collect()
}

fn read_message(arg: &str) -> Result<String> {
    let text = if arg == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .take(MAX_MESSAGE_BYTES as u64 + 1)
            .read_to_string(&mut buf)
            .context("reading the message from stdin")?;
        buf
    } else {
        arg.to_string()
    };
    let text = text.trim_end_matches(['\n', '\r']).to_string();
    if text.trim().is_empty() {
        return Err(anyhow!("the message is empty"));
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(anyhow!(
            "the message is {} bytes; Claude Code refuses cross-session messages over about 1 MB",
            text.len()
        ));
    }
    Ok(text)
}

#[cfg(unix)]
pub fn post_to_socket(path: &Path, frame: &str) -> Result<()> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path).with_context(|| {
        format!(
            "connecting to {} (if this runs inside a Claude Code sandbox, Unix sockets are blocked there)",
            path.display()
        )
    })?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(frame.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    // Drain anything the inbox says back; it is informational only.
    let mut sink = Vec::new();
    let _ = stream.take(4096).read_to_end(&mut sink);
    Ok(())
}

#[cfg(not(unix))]
pub fn post_to_socket(_path: &Path, _frame: &str) -> Result<()> {
    Err(anyhow!(
        "bogyman send to Claude Code is not implemented on this platform yet"
    ))
}

fn send_claude(ctx: &Ctx, s: &Session, message: &str) -> Result<(u8, String)> {
    let Some(socket) =
        claude_sessions::socket_for(&ctx.paths.claude_sessions_dir, &s.vendor_session_id)
    else {
        return Ok((
            EXIT_VENDOR_UNAVAILABLE,
            format!(
                "{} has no inbox socket: the session has ended, runs in bare mode, or its Claude Code is older than 2.1.224",
                s.id
            ),
        ));
    };
    if !socket.exists() {
        return Ok((
            EXIT_VENDOR_UNAVAILABLE,
            format!(
                "{}: inbox socket {} is gone; the session has probably exited",
                s.id,
                socket.display()
            ),
        ));
    }
    post_to_socket(&socket, &claude_frame(message))?;
    let how = match s.state {
        State::Working => "it reads the message between tool calls",
        State::Blocked => "it is waiting on you; the message queues until that is answered",
        _ => "it starts a new turn with the message",
    };
    Ok((
        EXIT_OK,
        format!("sent to {} ({}); {how}", s.id, s.display_name()),
    ))
}

fn send_codex(ctx: &Ctx, s: &Session, message: &str) -> Result<(u8, String)> {
    let argv = render_template(&ctx.config.send.codex, &s.vendor_session_id, message);
    let Some((program, args)) = argv.split_first() else {
        return Err(anyhow!("[send] codex in config.toml is empty"));
    };
    let out = match std::process::Command::new(program).args(args).output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                EXIT_VENDOR_UNAVAILABLE,
                format!("{program} not found on PATH"),
            ));
        }
        Err(e) => return Err(e).with_context(|| format!("running {program}")),
    };
    if out.status.success() {
        Ok((
            EXIT_OK,
            format!(
                "sent to {} ({}) via {}",
                s.id,
                s.display_name(),
                argv.join(" ")
            ),
        ))
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Ok((
            EXIT_ERROR,
            format!("{} failed ({}): {err}", argv.join(" "), out.status),
        ))
    }
}

pub fn run(ctx: &Ctx, id: &str, message_arg: &str) -> Result<u8> {
    let message = read_message(message_arg)?;
    let sessions = refresh::load_sessions(ctx, true)?;
    let s = resolve::resolve(&sessions, id)?.clone();
    if !s.state.is_live() {
        let line = format!("{} is {}; nothing is listening", s.id, s.state.label());
        return finish(ctx, &s, EXIT_VENDOR_UNAVAILABLE, line);
    }
    let (code, line) = match s.vendor {
        Vendor::ClaudeCode => send_claude(ctx, &s, &message)?,
        Vendor::Codex => send_codex(ctx, &s, &message)?,
    };
    finish(ctx, &s, code, line)
}

fn finish(ctx: &Ctx, s: &Session, code: u8, line: String) -> Result<u8> {
    if ctx.json {
        if code == EXIT_OK {
            json::ok(
                ctx,
                "send",
                serde_json::json!({"id": s.id, "vendor": s.vendor, "detail": line}),
            )?;
        } else {
            json::err(ctx, "send", code, &line);
        }
    } else if code == EXIT_OK {
        println!("{line}");
    } else {
        eprintln!("bogyman: {line}");
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_matches_claude_code_documented_shape() {
        let f = claude_frame("hello \"there\"\nline two");
        let v: serde_json::Value = serde_json::from_str(&f).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "hello \"there\"\nline two");
        assert!(!f.contains('\n'), "one frame per line");
    }

    #[test]
    fn template_expands_both_placeholders() {
        let t: Vec<String> = ["codex", "queue", "--session", "{id}", "{message}"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            render_template(&t, "thread-1", "use pnpm"),
            vec!["codex", "queue", "--session", "thread-1", "use pnpm"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn posts_one_newline_terminated_frame_to_a_unix_socket() {
        use std::io::BufRead;
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(stream)
                .read_line(&mut line)
                .unwrap();
            line
        });
        post_to_socket(&path, &claude_frame("ping")).unwrap();
        let got = server.join().unwrap();
        assert_eq!(got, format!("{}\n", claude_frame("ping")));
    }
}
