//! devsync command-line interface.

mod commands;
mod ui;
mod watch;

use clap::{Args, CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "devsync",
    version,
    about = "Edit locally, run remotely. Sync and remote execution built for flaky connections.",
    after_help = "Start with: devsync init user@host:~/project   then: devsync exec -- <command>"
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Args, Clone)]
pub struct Global {
    /// Run as if started in this directory.
    #[arg(short = 'C', long = "dir", global = true, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Machine-readable output (NDJSON events / JSON results).
    #[arg(long, global = true)]
    pub json: bool,
    /// Show every file transferred and connection details.
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Only errors and warnings.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Create .devsync.toml in the current directory.
    Init {
        /// user@host:/remote/path (or local:/path for a local agent).
        target: String,
        /// Overwrite an existing config.
        #[arg(long)]
        force: bool,
    },
    /// Make the remote tree match the local one.
    Push {
        /// Show what would change without transferring.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Allow deleting more than sync.max_delete remote files.
        #[arg(long)]
        force: bool,
    },
    /// Push on every save; collect results of running jobs. Keeps working offline.
    Watch,
    /// Sync, run a command remotely, show its output, pull back changed files.
    #[command(visible_alias = "run")]
    Exec {
        /// Skip the sync before running.
        #[arg(long)]
        no_push: bool,
        /// Do not bring back files the command changed.
        #[arg(long)]
        no_pull_back: bool,
        /// Command (use `--` before it if it has flags).
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Sync and start a command remotely without waiting; prints the job id.
    #[command(visible_alias = "bg")]
    Submit {
        #[arg(long)]
        no_push: bool,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Show a job's output (default: the latest job), following it while it runs.
    Logs {
        id: Option<String>,
        /// Print what has been collected so far and exit.
        #[arg(long)]
        no_follow: bool,
    },
    /// List jobs.
    Jobs {
        /// Ask the remote instead of the local results folder.
        #[arg(long)]
        remote: bool,
    },
    /// Stop a running job (default: the latest).
    Kill { id: Option<String> },
    /// Fetch output of running jobs into .devsync/jobs/ once, then exit.
    Collect,
    /// Get code from the remote: the whole project, or specific files/folders.
    ///
    /// Files that are new on the remote, or that you haven't edited since the
    /// last sync, are updated. Files you edited keep your version and the
    /// remote one is saved as <file>.devsync-remote (use --force to overwrite
    /// instead). Without paths the whole project is pulled and sync excludes
    /// apply; named paths are pulled even if excluded by `sync.ignore` or
    /// .gitignore. Built-in excludes (.git, node_modules, .venv, caches) and
    /// .devsyncignore always apply.
    ///
    /// Examples: `devsync pull`, `devsync pull src/api tests/test_api.py`,
    /// `devsync pull results/ --to ~/Downloads`.
    Pull {
        /// Remote files or directories, relative to the project (omit for everything).
        paths: Vec<String>,
        /// Overwrite local edits with the remote version.
        #[arg(long)]
        force: bool,
        /// Copy the paths into another local directory instead (exact copy).
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
        /// Show what would change without downloading.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// List files on the remote and how they compare with yours.
    ///
    /// Markers: + only on the remote, ~ changed on the remote, ! changed on
    /// both sides, = identical.
    Ls {
        /// Only under these remote paths.
        paths: Vec<String>,
        /// Only files that differ from your copy (+, ~, !).
        #[arg(short, long)]
        changed: bool,
        /// Include paths excluded by `sync.ignore` / .gitignore (built-in
        /// excludes and .devsyncignore always apply).
        #[arg(short, long)]
        all: bool,
    },
    /// Print the remote version of a file (for diffs). Exits 1 if the remote
    /// doesn't have it.
    #[command(visible_alias = "show")]
    Cat {
        /// File path relative to the project.
        path: String,
    },
    /// Run git in the remote project, as if you were there.
    ///
    /// Local changes are pushed first so git sees them (skip with
    /// --no-push). Commands that change files (checkout, pull, reset,
    /// stash, merge, …) bring the result back here; your unsent local edits
    /// are never overwritten.
    Git {
        /// Don't push local changes first.
        #[arg(long)]
        no_push: bool,
        /// Don't bring back files changed by the git command.
        #[arg(long)]
        no_pull: bool,
        /// Arguments for git (use `--` before them if they have flags).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Fetch git objects (by id) from the remote repository into the local
    /// cache .devsync/git-objects/, in one round trip. Used for fast diffs.
    #[command(hide = true)]
    GitObjects {
        /// Skip objects larger than this many bytes.
        #[arg(long, default_value_t = 4 * 1024 * 1024)]
        max_size: u64,
        /// Object ids (40 hex characters).
        ids: Vec<String>,
    },
    /// Browse the remote project one folder at a time: sizes, file counts and
    /// what is synced. Nothing is hashed; huge folders show lower bounds.
    Tree {
        /// Remote folder, relative to the project (default: the project root).
        path: Option<String>,
    },
    /// Choose what to sync: add files or folders to the selection.
    ///
    /// Without arguments, shows the current selection. Unselected paths are
    /// neither scanned nor synced, and stay untouched on the remote.
    Select {
        paths: Vec<String>,
        /// Download the selected paths right away.
        #[arg(long)]
        pull: bool,
        /// Go back to syncing everything.
        #[arg(long, conflicts_with = "paths")]
        all: bool,
    },
    /// Stop syncing files or folders (they stay untouched on the remote).
    Unselect {
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Configuration, connectivity and pending changes.
    Status,
    /// Local changes not yet on the remote, like `git status` (no network).
    Changes,
    /// Check the whole setup end to end and explain problems.
    Doctor,
    /// Forward remote ports to localhost (reconnects automatically).
    Forward {
        /// Ports (default: exec.ports from the config).
        ports: Vec<u16>,
    },
    /// Interactive shell in the matching remote directory.
    Shell,
    /// Install coding-agent integration.
    Setup {
        #[arg(value_enum)]
        what: SetupTarget,
    },
    /// Hook entry points for coding agents (reads the event on stdin).
    #[command(hide = true)]
    Hook {
        #[arg(value_enum)]
        agent: HookAgent,
    },
    /// Remote agent (started by devsync over ssh).
    #[command(hide = true)]
    Agent(AgentArgs),
    /// Print shell completions.
    Completions { shell: clap_complete::Shell },
}

#[derive(clap::ValueEnum, Clone, Copy)]
pub enum SetupTarget {
    /// Claude Code hook + CLAUDE.md instructions.
    Claude,
    /// AGENTS.md instructions (Codex, Cursor, others).
    AgentsMd,
    /// Everything above.
    All,
}

#[derive(clap::ValueEnum, Clone, Copy)]
pub enum HookAgent {
    Claude,
}

#[derive(Args)]
pub struct AgentArgs {
    /// Serve the protocol on stdin/stdout.
    #[arg(long)]
    pub stdio: bool,
    /// Project root on this machine.
    #[arg(long, default_value = "~")]
    pub root: String,
    #[command(subcommand)]
    pub sub: Option<AgentSub>,
}

#[derive(Subcommand)]
pub enum AgentSub {
    /// Supervise one job (internal).
    RunJob { dir: PathBuf },
}

fn init_logging(g: &Global) {
    let default = if g.verbose { "info" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_env("DEVSYNC_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).without_time().try_init();
}

fn main() {
    let cli = Cli::parse();
    // The agent side must stay silent on stderr unless asked.
    if !matches!(cli.cmd, Cmd::Agent(_) | Cmd::Hook { .. }) {
        init_logging(&cli.global);
        let _ = ctrlc::set_handler(|| {
            if devsync_client::interrupted() {
                std::process::exit(130);
            }
            devsync_client::INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
    if let Cmd::Completions { shell } = cli.cmd {
        clap_complete::generate(shell, &mut Cli::command(), "devsync", &mut std::io::stdout());
        return;
    }
    let code = commands::run(cli);
    ui::finish();
    std::process::exit(code);
}
