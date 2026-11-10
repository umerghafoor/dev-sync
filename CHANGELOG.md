# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Progress for pushes and pulls: a live progress bar in the terminal (files,
  bytes, speed, time left, current file) and `stage`, `pull_planned` and
  `progress` events in `--json` output.
- VS Code: redesigned Run panel with a status card, a live progress card, a
  grid of sync actions and an **Activity** list of files recently pushed or
  pulled; progress in the status bar and in Push/Pull notifications.
- Retry now when offline: press Enter in `devsync watch`, or use **Retry now**
  in VS Code (sidebar, status-bar menu, command palette).

- VS Code: a Source Control provider that works like Git's view: a command
  box (⌘Enter runs on the remote), "Not on remote yet" with A/M/D badges,
  conflicts with compare / keep yours / accept remote, and remote jobs with
  ✓/✗ status, stop and run-again actions.
- VS Code: Getting Started walkthrough, run/re-run in the editor title bar,
  ⌘⌥R / Ctrl+Alt+R to re-run, extension icon.
- VS Code: devsync icon in the Activity Bar with a sidebar: a Run box
  (command field, Run / Run in Background / Terminal buttons, status, recent
  commands), "Not on Remote Yet", Conflicts and Remote Jobs, plus a count
  badge on the icon; ⌘⌥D / Ctrl+Alt+D focuses it; `devsync.sourceControl`
  setting.
- First sync from a new machine downloads files that exist only on the remote
  (differing files are kept locally; the remote copy is saved as
  `<file>.devsync-remote`), so an empty folder can be connected to an existing
  remote project. Disable with `sync.initial_pull = false`.
- `devsync pull` without a path pulls the whole project safely: remote-only
  and locally untouched files are updated, files you edited keep your version
  (the remote one is saved as `<file>.devsync-remote`), sync excludes apply;
  `--dry-run` previews. VS Code: **Pull from Remote** button.
- `devsync pull <paths…>` pulls specific files or folders with the same
  safety rules; `--force` overwrites local edits; `--to DIR` copies elsewhere.
- `devsync ls [paths…]` lists remote files with `+`/`~`/`!`/`=` markers
  (`--changed`, `--all`, `--json`).
- VS Code: **Pull from Remote** in the Explorer and editor-tab context menus,
  and **Pull Files…**, a searchable picker of remote files and folders.
- Choosing what to sync: `devsync tree` browses the remote one level at a
  time with time-limited sizes (no hashing); `devsync select` / `unselect`
  edit `sync.include` / `sync.ignore` (comments preserved); unselected paths
  are not even scanned. VS Code: **Remote Files** view with checkboxes.
- Large-remote guard: the first sync or a whole pull of a remote larger than
  `sync.large_remote_limit_mb` (1 GB) with nothing chosen stops and asks what
  to sync; auto sync waits for a selection instead of retrying.
- `devsync watch` reloads `.devsync.toml` when it changes.
- VS Code restarts auto sync when the devsync binary is updated.
- `devsync changes`: git-status-like list of local changes not yet on the
  remote (`--json` supported).
- Project logo.
- `install.sh`: one-line installer (`curl … | sh`) that downloads the right
  release, verifies its checksum, sets up `PATH`, installs the VS Code
  extension, and supports `--version`, `--prefix`, `--uninstall`; it also
  installs from release archives and builds source checkouts.

### Changed

- VS Code: the separate "Remote Jobs" sidebar is replaced by the Source
  Control panel.
- Wire protocol and local caches now use `postcard` instead of the
  unmaintained `bincode` (RUSTSEC-2025-0141). The protocol version is now 2;
  the matching agent is uploaded automatically. After upgrading, the first
  push re-hashes the project, and `devsync changes` lists every file until
  that push.

### Fixed

- Pulls appeared to start over every time on a flaky link. Now only one
  devsync process syncs a project at a time (auto sync and a manual pull no
  longer download the same files into the same temp file and break each
  other); interrupted downloads resume where they stopped; a file that
  changes on the remote mid-download is skipped (and never overwritten by an
  older local copy) instead of failing the whole pull; and pulls report how
  many files are already here.
- "Not on remote yet" (`devsync changes`) listed every file until a first sync
  had completed. It now compares with what the remote actually holds (recorded
  at every remote scan and kept even if a sync is interrupted), so only files
  that differ are listed.
- Truncated agent uploads (connection dropped mid-transfer) were installed as
  if complete, causing endless reconnect loops; uploads are now verified
  before installation, existing agents are checked by size, uploads are
  gzip-compressed and serialised across local devsync processes, and old
  agents are cleaned up after a week.
- A broken shared ssh connection is replaced with a fresh one before falling
  back to direct connections (temporarily), and `ssh.control_persist_secs`
  now defaults to 600 s for high-latency links.
- A broken shared (multiplexed) ssh connection no longer causes an endless
  "exit code 255" retry loop: devsync closes it and connects directly.
- Large remote projects: scans may take up to an hour instead of timing out
  after 120 s, and the hash cache is checkpointed every 10 s so an
  interrupted first scan resumes instead of starting over.
- Connections no longer break when the remote shell's startup files print
  text or binary bytes ("frame too large"): the agent now sends a greeting,
  and the client skips anything before it and sends nothing until it
  arrives (protocol version 3; the new agent is installed automatically).
- Remote platform detection and agent upload tolerate shell startup output
  (including invalid UTF-8), and a truncated upload is detected and retried.
- A cached agent from an older protocol is replaced instead of being tried.
- `.devsync.toml` is no longer synced to the remote, and `devsync init` no
  longer creates a `.gitignore` in folders that don't use git.
- Conflict copies (`*.devsync-remote`) and devsync temp files are never synced.
- VS Code: the CLI is found in `~/.local/bin` even when the editor does not
  inherit the shell `PATH`.

## [0.1.0] - 2026-11-09

First public release.

### Added

- `devsync` CLI: `init`, `push`, `watch`, `exec`/`run`, `submit`/`bg`, `logs`,
  `jobs`, `kill`, `collect`, `pull`, `status`, `doctor`, `forward`, `shell`,
  `setup`, `completions`; `--json` output for integrations.
- One-way, content-hashed (BLAKE3) sync with a persistent hash cache,
  lz4-compressed pipelined uploads, resume after interruption, atomic writes,
  symlink and executable-bit support, and guarded deletions.
- Ignore rules: built-in excludes, `.devsyncignore`, config patterns, and
  opt-in `.gitignore` (gitignored files such as `.env` are synced by default).
- Detached remote jobs: commands outlive ssh sessions; output and exit codes
  are collected into `.devsync/jobs/<id>/` through short poll sessions.
- Conflict-safe pull-back of files generated by remote commands
  (`<file>.devsync-remote` on conflict).
- Automatic agent deployment over ssh (static Linux x86_64/arm64 builds),
  content-addressed and cached per project.
- Network resilience: retries with exponential backoff and jitter, ssh
  keepalives, response timeouts, and an offline queue in `watch`.
- Claude Code `PreToolUse` hook that routes code-executing commands to the
  remote; `CLAUDE.md` and `AGENTS.md` instructions.
- VS Code extension: sync status, auto sync, remote runs, Remote Jobs view,
  remote shell profile, port forwarding, conflict diffs.
- Documentation site (mdBook), CI, and release automation.

[Unreleased]: https://github.com/umerghafoor/dev-sync/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/umerghafoor/dev-sync/releases/tag/v0.1.0
