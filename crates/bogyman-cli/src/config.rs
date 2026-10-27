//! `~/.config/bogyman/config.toml`. Every key has a default; a missing file is fine.
//! Keys for the daemon, pollers, notifications and focus are accepted now so a
//! config written for a later version still parses, even though v0.1 reads only
//! `store` and `collectors`.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub poll: Poll,
    pub notify: Notify,
    pub focus: Focus,
    pub store: StoreCfg,
    pub collectors: Collectors,
    pub send: SendCfg,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Poll {
    pub claude_agents_secs: u64,
    pub codex_rollout_secs: u64,
}

impl Default for Poll {
    fn default() -> Self {
        Poll {
            claude_agents_secs: 5,
            codex_rollout_secs: 2,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Notify {
    pub enabled: bool,
    pub on: Vec<String>,
    pub debounce_secs: u64,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            enabled: false,
            on: vec!["blocked".into(), "failed".into()],
            debounce_secs: 30,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Focus {
    pub terminal: String,
}

impl Default for Focus {
    fn default() -> Self {
        Focus {
            terminal: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StoreCfg {
    pub retention_days: u32,
    /// Drop free-text detail (what a session is waiting on) entirely.
    pub redact: bool,
}

impl Default for StoreCfg {
    fn default() -> Self {
        StoreCfg {
            retention_days: 7,
            redact: false,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Collectors {
    /// Poll `~/.claude/sessions/*.json` on every query. Cheap; on by default.
    pub claude_sessions: bool,
    /// Read the tail of each live session's transcript for the model in use.
    pub claude_transcript: bool,
    /// Tail local Cowork transcripts. Not implemented in v0.1.
    pub cowork: bool,
}

impl Default for Collectors {
    fn default() -> Self {
        Collectors {
            claude_sessions: true,
            claude_transcript: true,
            cowork: false,
        }
    }
}

impl Config {
    /// Load the file, falling back to defaults. A malformed file is reported on
    /// stderr once and otherwise ignored, so a typo never takes the CLI down.
    pub fn load(path: &Path) -> Config {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("bogyman: ignoring {}: {e}", path.display());
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SendCfg {
    /// Command that queues a message for a Codex session. `{id}` is the thread
    /// id and `{message}` the text. Codex's own interface is treated as unstable,
    /// so this is a template rather than a hard-coded call.
    pub codex: Vec<String>,
}

impl Default for SendCfg {
    fn default() -> Self {
        SendCfg {
            codex: ["codex", "queue", "--session", "{id}", "{message}"]
                .into_iter()
                .map(String::from)
                .collect(),
        }
    }
}
