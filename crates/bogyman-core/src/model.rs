use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// JSON schema version. Additive changes only within a major version.
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Vendor {
    ClaudeCode,
    Codex,
}

impl Vendor {
    /// Short prefix used in bogyman ids (`cc-1f0e2c9a`).
    pub fn prefix(self) -> &'static str {
        match self {
            Vendor::ClaudeCode => "cc",
            Vendor::Codex => "cx",
        }
    }

    /// The word used on the command line and in human tables.
    pub fn label(self) -> &'static str {
        match self {
            Vendor::ClaudeCode => "claude",
            Vendor::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Vendor> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude_code" | "cc" => Some(Vendor::ClaudeCode),
            "codex" | "cx" => Some(Vendor::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Working,
    Blocked,
    Done,
    Failed,
    Stopped,
    Unknown,
}

impl State {
    /// Live sessions are the ones `ls` shows by default.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            State::Working | State::Blocked | State::Done | State::Unknown
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Blocked => "blocked",
            State::Done => "done",
            State::Failed => "failed",
            State::Stopped => "stopped",
            State::Unknown => "unknown",
        }
    }

    /// Ordering used when grouping a listing: blocked first, then working, done, the rest.
    pub fn rank(self) -> u8 {
        match self {
            State::Blocked => 0,
            State::Working => 1,
            State::Done => 2,
            State::Unknown => 3,
            State::Failed => 4,
            State::Stopped => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Host {
    Terminal,
    AgentView,
    Desktop,
    DesktopCowork,
    Vscode,
    Unknown,
}

impl Host {
    pub fn label(self) -> &'static str {
        match self {
            Host::Terminal => "terminal",
            Host::AgentView => "agent-view",
            Host::Desktop => "desktop",
            Host::DesktopCowork => "cowork",
            Host::Vscode => "vscode",
            Host::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Interactive,
    Background,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingFor {
    PermissionPrompt,
    Question,
    SandboxRequest,
    Elicitation,
    Dialog,
    Auth,
    FirstPrompt,
    Unknown,
}

impl WaitingFor {
    pub fn label(self) -> &'static str {
        match self {
            WaitingFor::PermissionPrompt => "permission",
            WaitingFor::Question => "question",
            WaitingFor::SandboxRequest => "sandbox",
            WaitingFor::Elicitation => "elicitation",
            WaitingFor::Dialog => "dialog",
            WaitingFor::Auth => "auth",
            WaitingFor::FirstPrompt => "first prompt",
            WaitingFor::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Hook,
    Poll,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Tasks {
    pub done: u32,
    pub total: u32,
    pub current: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Terminal {
    pub app: Option<String>,
    pub tty: Option<String>,
}

/// One normalized record per session. Every collector writes into this; the CLI,
/// the JSON output and any future UI read only this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub schema: u32,
    pub id: String,
    pub vendor: Vendor,
    pub vendor_session_id: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub repo: Option<String>,
    pub kind: Kind,
    pub host: Host,
    pub state: State,
    pub waiting_for: Option<WaitingFor>,
    pub waiting_detail: Option<String>,
    /// When the current state began. Resets only on a state change, so wait age is accurate.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    pub summary: Option<String>,
    pub tasks: Option<Tasks>,
    pub turns: u32,
    pub tokens: Option<Tokens>,
    pub model: Option<String>,
    pub pid: Option<u32>,
    pub terminal: Option<Terminal>,
    pub transcript_path: Option<String>,
    /// Which kind of collector produced the most recent state change.
    pub source: Source,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    /// A working session with no event for 30 minutes.
    pub stale: bool,
}

impl Session {
    pub fn key(vendor: Vendor, vendor_session_id: &str) -> String {
        format!("{}:{}", vendor.label(), vendor_session_id)
    }

    /// Build the short id shown to users: vendor prefix + first 8 chars of the vendor id.
    pub fn short_id(vendor: Vendor, vendor_session_id: &str) -> String {
        let head: String = vendor_session_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .collect::<String>()
            .to_ascii_lowercase();
        format!("{}-{}", vendor.prefix(), head)
    }

    pub fn store_key(&self) -> String {
        Session::key(self.vendor, &self.vendor_session_id)
    }

    /// Seconds spent in the current state as of `now`.
    pub fn age_secs(&self, now: OffsetDateTime) -> i64 {
        (now - self.since).whole_seconds().max(0)
    }

    pub fn display_name(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        if let Some(r) = &self.repo {
            return r.clone();
        }
        self.id.clone()
    }
}

/// Last path component of a cwd, used as the repo name when nothing better is known.
pub fn repo_from_cwd(cwd: &str) -> Option<String> {
    std::path::Path::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}
