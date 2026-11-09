# devsync

**Edit locally, run remotely.** Write code (or let a coding agent write it) on
your own machine, and run it on a remote one: a GPU box, a lab server, a dev
VM, a Raspberry Pi. Built for connections that drop.

```text
 your machine                                    remote machine
 ─────────────                                   ──────────────
 editor / Claude Code                            
   │ save                                        
   ▼                                             
 devsync ── short ssh session: changed files ──▶ agent ──▶ project files
   │                                               │
   │ ── short session: "run pytest" ─────────────▶ │ ──▶ job (detached, keeps running
   │                                               │      if the link drops)
   │ ◀─ short sessions: new output, exit code ──── │      output.log, exit_code
   ▼                                             
 .devsync/jobs/<id>/output.log   ◀── pull-back of files the job created
```

## Why

| Approach | Problem on a flaky link |
| --- | --- |
| VS Code Remote-SSH, editing over sshfs | The editor (and your agent) freeze or die when the connection does |
| `ssh host pytest` | A dropped connection kills the command and its output |
| rsync | One-shot, no retries, no notion of running anything; macOS ships openrsync with a reduced flag set |
| Mutagen, Unison, Syncthing | Two-way sync daemons: powerful but heavy, and they don't run commands |
| git push + pull | You commit just to test; secrets and generated files need special handling |

devsync keeps **all editing local** and treats the remote as a place to *run*
things:

- **No long-lived connections.** Every operation is a short ssh session.
  Commands run detached on the remote; their output and exit code are written
  to files there and collected into `.devsync/jobs/<id>/` locally by later
  short sessions. A dropped connection only delays output.
- **Resumable, verified transfers.** Content-hashed (BLAKE3), lz4-compressed,
  resumed after interruptions, written atomically.
- **Feels local.** `devsync exec pytest` runs in the remote directory that
  matches your current one, streams output, keeps the exit code, and copies
  back files the command created (lockfiles, generated code). It never
  overwrites a local edit: conflicts are saved as `<file>.devsync-remote`.
- **Agent-ready.** A Claude Code hook routes code-executing shell commands to
  the remote automatically. Git, grep and file edits stay local.
- **Zero setup on the remote.** devsync uploads a small static agent binary
  over ssh the first time and reuses it after that. Supported remotes: Linux
  x86_64 and arm64, and macOS with the same architecture as your machine.
- **VS Code extension** with sync status, remote runs, and a jobs view.

## Quick start

Requirements: key-based ssh to the remote (`ssh you@box` works without a
password prompt). Nothing needs to be installed on the remote.

```bash
# build and install (needs Rust; installs to ~/.local/bin)
make install

cd ~/code/myproject
devsync init you@box:~/myproject   # writes .devsync.toml
devsync doctor                     # checks ssh, deploys the agent, runs a test command
devsync exec -- pytest -x          # sync, run remotely, stream output
```

Day to day:

```bash
devsync watch                      # push on every save (keeps working offline)
devsync exec -- npm test           # run and wait (Ctrl-C stops the remote command)
devsync submit -- python train.py  # start in background, prints a job id
devsync logs                       # follow the latest job (Ctrl-C detaches)
devsync jobs                       # list jobs; results live in .devsync/jobs/<id>/
devsync kill <id>
devsync shell                      # interactive shell in the matching remote dir
devsync forward 8000               # localhost:8000 → remote:8000, auto-reconnecting
devsync pull results/              # fetch remote files
devsync status                     # link, pending changes, running jobs
```

## Coding agents

```bash
devsync setup claude      # Claude Code: PreToolUse hook + CLAUDE.md instructions
devsync setup agents-md   # AGENTS.md instructions for other agents
```

With the hook installed, when Claude Code runs `pytest`, `npm install`,
`python app.py`, `make` and the like, devsync syncs first, runs the command on the
remote, and returns the output and exit code as if it had run locally. Reading,
searching, editing and `git` stay local. End a command with `# local` or
`# remote` to override. Lists of extra local or remote commands can be set in
`.devsync.toml`.

## VS Code

```bash
make vscode
code --install-extension extensions/vscode/devsync-*.vsix
```

The extension adds sync state to the status bar (auto sync), run-remotely
commands, a **Remote Jobs** view (output opens straight from the local results
folder), a remote-shell terminal profile, conflict diffs, and one-click setup.
See [extensions/vscode/README.md](extensions/vscode/README.md).

## Configuration

`.devsync.toml` at the project root. Excludes are set there and in
`.devsyncignore` files (gitignore syntax). By default `.gitignore`'d files
such as `.env` **are** synced; dependency folders (`node_modules`, `.venv`,
`.git`, …) never are. Full reference: [docs/CONFIG.md](docs/CONFIG.md).

## How it works

Five crates, each with a single job:

| Crate | Role |
| --- | --- |
| `devsync-proto` | Framed bincode protocol shared by both sides |
| `devsync-fs` | Ignore rules, hashing with cache, sync planning, atomic writes |
| `devsync-agent` | Remote endpoint: transfers and detached job supervisor |
| `devsync-transport` | Short ssh/local sessions, agent deployment, retry/backoff |
| `devsync-client` | Push, pull-back, job collection, agent hook |
| `devsync-cli` | The `devsync` binary (also the agent) |

Details: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Development

```bash
make check     # rustfmt, clippy -D warnings, all tests
make agents    # static Linux builds (rust-lld; no cross toolchain needed)
make dist      # release tarball with client + agents
make vscode    # extension .vsix
```

The end-to-end tests (`crates/devsync-cli/tests/e2e.rs`) drive the real
binary against a local agent and through a fake `ssh`. They cover agent
deployment, outages, conflicts and detached jobs.

## License

MIT
