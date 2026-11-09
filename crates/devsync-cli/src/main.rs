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
    /// Copy a file or directory from the remote project.
    Pull {
        /// Path relative to the remote project root.
        path: String,
        /// Local directory to place it under (default: project root).
        dest: Option<PathBuf>,
    },
    /// Configuration, connectivity and pending changes.
    Status,
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
    std::process::exit(code);
}
