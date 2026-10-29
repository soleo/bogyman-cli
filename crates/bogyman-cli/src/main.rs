//! bogyman: which of your agent sessions needs you, on what, and for how long.

mod commands;
mod config;
mod json;
mod notify;
mod paths;
mod refresh;
mod render;
mod resolve;
mod vendor_cmd;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use bogyman_core::Vendor;
use clap::{Parser, Subcommand, ValueEnum};

use config::Config;
use paths::Paths;

pub const EXIT_OK: u8 = 0;
pub const EXIT_ERROR: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_EMPTY: u8 = 3;
pub const EXIT_VENDOR_UNAVAILABLE: u8 = 4;

#[derive(Parser, Debug)]
#[command(
    name = "bogyman",
    version = commands::version::LINE,
    about = "Which of your agent sessions needs you, on what, and for how long.",
    long_about = "bogyman watches the Claude Code and Codex sessions you already started and \
keeps one list: who is blocked, on what, and since when.\n\n\
Exit codes: 0 ok, 1 error, 2 bad usage, 3 empty result (waiting), 4 vendor command unavailable."
)]
struct Cli {
    /// Machine-readable output (JSON, schema 1).
    #[arg(long, global = true)]
    json: bool,
    /// Only sessions from one vendor.
    #[arg(long, global = true, value_enum)]
    vendor: Option<VendorArg>,
    /// Only sessions whose working directory starts with PATH.
    #[arg(long, global = true, value_name = "PATH")]
    cwd: Option<PathBuf>,
    /// Never emit ANSI colors.
    #[arg(long, global = true)]
    no_color: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum VendorArg {
    Claude,
    Codex,
}

impl From<VendorArg> for Vendor {
    fn from(v: VendorArg) -> Vendor {
        match v {
            VendorArg::Claude => Vendor::ClaudeCode,
            VendorArg::Codex => Vendor::Codex,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// All live sessions, grouped blocked / working / done.
    Ls {
        /// Include failed and stopped sessions from the last 24 hours.
        #[arg(long)]
        all: bool,
        /// Redraw every second until interrupted.
        #[arg(long)]
        watch: bool,
    },
    /// Blocked sessions only, oldest wait first. Exit 3 when there are none.
    Waiting,
    /// One line for prompts and status bars, e.g. "⏳2 ▶3 ✓1". Reads the cache only.
    Status {
        /// Template with {blocked} {working} {done} {failed} {stale} {total} placeholders.
        #[arg(long)]
        format: Option<String>,
    },
    /// Full record for one session, plus its last 20 events.
    Show {
        /// bogyman id, a unique prefix, the vendor's own id, or the session name.
        id: String,
        /// Open the transcript file.
        #[arg(long)]
        open: bool,
    },
    /// Bring a session in front of you: raise its terminal tab, attach to a
    /// background session, or resume one that has ended.
    Focus {
        /// bogyman id, a unique prefix, the vendor's own id, or the session name.
        id: String,
    },
    /// Deliver a message to a session without attaching to it.
    Send {
        /// bogyman id, a unique prefix, the vendor's own id, or the session name.
        id: String,
        /// The text to send. Pass `-` to read it from stdin.
        message: String,
    },
    /// Hook entry point (internal). Reads one JSON payload from stdin, appends it, exits 0.
    #[command(hide = true)]
    Hook {
        vendor: VendorArg,
        /// Codex `notify` program mode: the payload arrives as this argument.
        #[arg(long, value_name = "JSON")]
        notify: Option<String>,
    },
    /// Register bogyman's hooks with Claude Code and Codex.
    Install {
        /// Only Claude Code.
        #[arg(long)]
        claude: bool,
        /// Only Codex.
        #[arg(long)]
        codex: bool,
        /// Show what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove bogyman's hook entries. Other hooks are left as they are.
    Uninstall {
        #[arg(long)]
        claude: bool,
        #[arg(long)]
        codex: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Optional background poller: `claude agents`, steady refresh, notifications.
    Daemon {
        #[command(subcommand)]
        action: DaemonCmd,
    },
    /// Diagnose the setup: vendor versions, hooks, store, last events.
    Doctor {
        /// Also dump the last 50 events.
        #[arg(long)]
        verbose: bool,
    },
    /// Prune events and dead sessions older than the retention window.
    Gc,
    /// Show which build this is: version, commit, build date, target.
    Version,
}

#[derive(Subcommand, Debug, Clone, Copy)]
enum DaemonCmd {
    /// Start the daemon in the background.
    Start,
    /// Stop the running daemon.
    Stop,
    /// Is it running, when did it last tick, how are its polls doing.
    Status,
    /// Run in the foreground (what start, launchd and systemd invoke).
    Run,
    /// Run the daemon at login: a launchd agent on macOS, a systemd user unit on Linux.
    Install {
        /// Print the service file instead of installing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Send a sample notification, to check delivery and permissions.
    NotifyTest,
    /// Remove the login service.
    Uninstall {
        #[arg(long)]
        dry_run: bool,
    },
}

impl From<DaemonCmd> for commands::daemon::Action {
    fn from(c: DaemonCmd) -> Self {
        use commands::daemon::Action;
        match c {
            DaemonCmd::Start => Action::Start,
            DaemonCmd::Stop => Action::Stop,
            DaemonCmd::Status => Action::Status,
            DaemonCmd::Run => Action::Run,
            DaemonCmd::NotifyTest => Action::NotifyTest,
            DaemonCmd::Install { dry_run } => Action::Install { dry_run },
            DaemonCmd::Uninstall { dry_run } => Action::Uninstall { dry_run },
        }
    }
}

impl Cmd {
    /// The name this command reports in its `--json` envelope.
    fn name(&self) -> &'static str {
        match self {
            Cmd::Ls { .. } => "ls",
            Cmd::Waiting => "waiting",
            Cmd::Status { .. } => "status",
            Cmd::Show { .. } => "show",
            Cmd::Focus { .. } => "focus",
            Cmd::Send { .. } => "send",
            Cmd::Hook { .. } => "hook",
            Cmd::Install { .. } => "install",
            Cmd::Uninstall { .. } => "uninstall",
            Cmd::Daemon { .. } => "daemon",
            Cmd::Doctor { .. } => "doctor",
            Cmd::Gc => "gc",
            Cmd::Version => "version",
        }
    }
}

/// Everything a command needs besides its own arguments.
pub struct Ctx {
    pub json: bool,
    pub vendor: Option<Vendor>,
    pub cwd: Option<String>,
    pub color: bool,
    pub tty: bool,
    pub paths: Paths,
    pub config: Config,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let paths = Paths::detect();

    // The hook path must never fail loudly: whatever happens, exit 0 and stay off stdout.
    if let Cmd::Hook { vendor, notify } = &cli.cmd {
        commands::hook::run(&paths, (*vendor).into(), notify.as_deref());
        return ExitCode::from(EXIT_OK);
    }

    let tty = std::io::stdout().is_terminal();
    let color = tty && !cli.no_color && std::env::var_os("NO_COLOR").is_none();
    let config = Config::load(&paths.config);
    let ctx = Ctx {
        json: cli.json,
        vendor: cli.vendor.map(Into::into),
        cwd: cli.cwd.map(|p| p.to_string_lossy().into_owned()),
        color,
        tty,
        paths,
        config,
    };

    let command = cli.cmd.name();
    let result: Result<u8> = match cli.cmd {
        Cmd::Ls { all, watch } => commands::ls::run(&ctx, all, watch),
        Cmd::Waiting => commands::waiting::run(&ctx),
        Cmd::Status { format } => commands::status::run(&ctx, format.as_deref()),
        Cmd::Show { id, open } => commands::show::run(&ctx, &id, open),
        Cmd::Focus { id } => commands::focus::run(&ctx, &id),
        Cmd::Send { id, message } => commands::send::run(&ctx, &id, &message),
        Cmd::Install {
            claude,
            codex,
            dry_run,
        } => commands::install::install(&ctx, claude, codex, dry_run),
        Cmd::Uninstall {
            claude,
            codex,
            dry_run,
        } => commands::install::uninstall(&ctx, claude, codex, dry_run),
        Cmd::Daemon { action } => commands::daemon::run(&ctx, action.into()),
        Cmd::Doctor { verbose } => commands::doctor::run(&ctx, verbose),
        Cmd::Gc => commands::gc::run(&ctx),
        Cmd::Version => commands::version::run(&ctx),
        Cmd::Hook { .. } => unreachable!(),
    };

    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            if ctx.json {
                json::err(&ctx, command, EXIT_ERROR, &format!("{e:#}"));
            } else {
                eprintln!("bogyman: {e:#}");
            }
            ExitCode::from(EXIT_ERROR)
        }
    }
}
