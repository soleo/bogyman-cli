//! Run a vendor CLI with a deadline. A wedged vendor process must never hang a
//! poll loop, so the child is killed once the deadline passes.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

/// Run `program args…`, returning stdout when it exits 0 within `timeout`.
pub fn output(program: &str, args: &[&str], timeout: Duration) -> Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running {program}"))?;
    // Drain stdout on a thread: a child that fills the pipe would otherwise
    // block before it can exit.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!("{program} did not finish within {timeout:?}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(anyhow!("{program} exited with {status}"));
    }
    Ok(out)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn returns_stdout_of_a_quick_command() {
        let out = output("sh", &["-c", "echo hi"], Duration::from_secs(5)).unwrap();
        assert_eq!(out.trim(), "hi");
    }

    #[test]
    fn kills_a_command_that_overruns() {
        let started = Instant::now();
        let err = output("sh", &["-c", "sleep 5"], Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("did not finish"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_nonzero_exit_is_an_error() {
        assert!(output("sh", &["-c", "exit 3"], Duration::from_secs(5)).is_err());
    }
}
