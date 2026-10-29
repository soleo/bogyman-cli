//! A whole-file advisory lock (flock). The daemon holds one for its lifetime,
//! so "is it running" never depends on a pid file that a crash left behind.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// An exclusive lock, released when dropped or when the process exits.
#[derive(Debug)]
pub struct Lock {
    file: File,
}

impl Lock {
    /// Take the lock without waiting. `Ok(None)` means another process holds it.
    pub fn try_acquire(path: &Path) -> std::io::Result<Option<Lock>> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        if flock_exclusive_nonblocking(&file)? {
            Ok(Some(Lock { file }))
        } else {
            Ok(None)
        }
    }

    /// Record the holder's pid in the lock file, for `daemon status` and `stop`.
    pub fn write_pid(&mut self, pid: u32) -> std::io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        writeln!(self.file, "{pid}")?;
        self.file.flush()
    }
}

/// Whether some process holds the lock at `path` right now.
pub fn is_held(path: &Path) -> bool {
    match Lock::try_acquire(path) {
        Ok(Some(_probe)) => false, // dropped at once, releasing it
        Ok(None) => true,
        Err(_) => false,
    }
}

/// The pid the holder wrote, if any.
pub fn holder_pid(path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse().ok()
}

#[cfg(unix)]
fn flock_exclusive_nonblocking(file: &File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(err)
    }
}

#[cfg(not(unix))]
fn flock_exclusive_nonblocking(_file: &File) -> std::io::Result<bool> {
    Ok(true)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_until_the_first_lets_go() {
        let dir = tempfile_dir();
        let path = dir.join("daemon.lock");
        let mut first = Lock::try_acquire(&path).unwrap().expect("free at first");
        first.write_pid(4242).unwrap();
        // flock locks belong to the open file description, so a second open in
        // this same process is refused just as another process would be.
        assert!(Lock::try_acquire(&path).unwrap().is_none());
        assert!(is_held(&path));
        assert_eq!(holder_pid(&path), Some(4242));
        drop(first);
        assert!(!is_held(&path));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bogyman-lock-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
