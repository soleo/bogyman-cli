//! Where things live. Honors BOGYMAN_HOME (everything under one directory, handy
//! for tests), XDG_DATA_HOME / XDG_CONFIG_HOME, CLAUDE_CONFIG_DIR and CODEX_HOME.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub db: PathBuf,
    pub log: PathBuf,
    pub config: PathBuf,
    pub claude_dir: PathBuf,
    pub claude_settings: PathBuf,
    pub claude_sessions_dir: PathBuf,
    pub codex_home: PathBuf,
    pub codex_hooks: PathBuf,
    pub codex_config: PathBuf,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl Paths {
    pub fn detect() -> Paths {
        let home = home();
        let (data_dir, config_dir) = match env_path("BOGYMAN_HOME") {
            Some(h) => (h.clone(), h),
            None => (
                env_path("XDG_DATA_HOME")
                    .unwrap_or_else(|| home.join(".local/share"))
                    .join("bogyman"),
                env_path("XDG_CONFIG_HOME")
                    .unwrap_or_else(|| home.join(".config"))
                    .join("bogyman"),
            ),
        };
        let claude_dir = env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"));
        let codex_home = env_path("CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
        Paths {
            db: data_dir.join("bogyman.db"),
            log: data_dir.join("bogyman.log"),
            config: config_dir.join("config.toml"),
            data_dir,
            claude_settings: claude_dir.join("settings.json"),
            claude_sessions_dir: claude_dir.join("sessions"),
            claude_dir,
            codex_hooks: codex_home.join("hooks.json"),
            codex_config: codex_home.join("config.toml"),
            codex_home,
        }
    }

    /// Append one line to the log. Errors are swallowed: logging must never fail a command.
    pub fn log(&self, msg: &str) {
        use std::io::Write;
        let _ = std::fs::create_dir_all(&self.data_dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
        {
            let now = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            let _ = writeln!(f, "{now} {msg}");
        }
    }
}

/// Render a path with the home directory as `~`.
pub fn tilde(p: &Path) -> String {
    let s = p.to_string_lossy();
    let h = home();
    let hs = h.to_string_lossy();
    if !hs.is_empty() && s.starts_with(hs.as_ref()) {
        format!("~{}", &s[hs.len()..])
    } else {
        s.into_owned()
    }
}
