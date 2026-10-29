//! Process liveness and lookups: whether a session's process is alive, what a
//! process is called and who its parent is, and signalling the daemon.

#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // Signal 0 performs error checking only. EPERM means the process exists but
    // belongs to someone else, which still counts as alive.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    true
}

/// Ask a process to exit (SIGTERM). False if it doesn't exist or isn't ours.
#[cfg(unix)]
pub fn terminate(pid: u32) -> bool {
    pid > 1 && unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0
}

#[cfg(not(unix))]
pub fn terminate(_pid: u32) -> bool {
    false
}

/// The real user id, for addressing the user's launchd domain (`gui/<uid>`).
#[cfg(unix)]
pub fn uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
pub fn uid() -> u32 {
    0
}

/// What the hook path needs to know about a process: its name, its parent, and
/// whether it has a controlling terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub name: String,
    pub ppid: u32,
    pub has_tty: bool,
}

#[cfg(target_os = "macos")]
pub fn proc_info(pid: u32) -> Option<ProcInfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    let raw = if info.pbi_name[0] != 0 {
        &info.pbi_name[..]
    } else {
        &info.pbi_comm[..]
    };
    let bytes: Vec<u8> = raw
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    Some(ProcInfo {
        name: String::from_utf8_lossy(&bytes).into_owned(),
        ppid: info.pbi_ppid,
        // NODEV is all ones.
        has_tty: info.e_tdev != u32::MAX && info.e_tdev != 0,
    })
}

#[cfg(target_os = "linux")]
pub fn proc_info(pid: u32) -> Option<ProcInfo> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state ppid pgrp session tty_nr …`; comm may contain spaces or ')'.
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat[open + 1..close].to_string();
    let mut rest = stat[close + 1..].split_whitespace();
    let _state = rest.next()?;
    let ppid = rest.next()?.parse().ok()?;
    let tty_nr: i64 = rest.nth(2)?.parse().ok()?;
    Some(ProcInfo {
        name,
        ppid,
        has_tty: tty_nr != 0,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn proc_info(_pid: u32) -> Option<ProcInfo> {
    None
}

/// The nearest ancestor of this process, within `max_hops`, whose name is
/// `name` and which has a terminal. A hook uses it to find the agent that ran it.
pub fn terminal_ancestor_named(name: &str, max_hops: usize) -> Option<u32> {
    let mut pid = std::process::id();
    for _ in 0..max_hops {
        let ppid = proc_info(pid)?.ppid;
        if ppid <= 1 {
            return None;
        }
        let parent = proc_info(ppid)?;
        if parent.name == name && parent.has_tty {
            return Some(ppid);
        }
        pid = ppid;
    }
    None
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process_and_its_parent() {
        let me = proc_info(std::process::id()).expect("own process is visible");
        assert!(!me.name.is_empty());
        assert!(me.ppid > 0);
        assert!(proc_info(me.ppid).is_some());
    }

    #[test]
    fn a_missing_ancestor_name_is_none() {
        assert_eq!(terminal_ancestor_named("no-such-agent-binary", 8), None);
    }
}
