# devsync for VS Code

Write code on this machine; run it on a remote one. The extension is a thin
UI over the `devsync` CLI (see the main project README), so the editor,
coding agents and the terminal all share one sync engine.

## Features

- **Auto sync** — every save is pushed to the remote (`devsync watch`). The
  status bar shows `synced`, `syncing`, `changes`, or `offline`; when offline,
  changes are queued and sent when the link returns.
- **Run remotely** — `devsync: Run Command Remotely…` syncs, then runs the
  command on the remote in the directory matching the active file, in a
  terminal. Output streams back, the exit code is kept, and files the command
  created (lockfiles, generated code) are copied back.
- **Background jobs** — `Run Command Remotely in Background…` starts a job
  without keeping any connection open. The **Remote Jobs** view lists jobs;
  clicking one opens its output from the local results folder
  `.devsync/jobs/<id>/output.log`, which fills in as output is collected.
- **Remote shell** — terminal profile *devsync remote shell* and the
  `Open Remote Shell` command.
- **Conflicts** — if a remote command changes a file you also edited, the
  remote version is saved next to it and you can open a diff.
- **Coding agents** — `Set Up Claude Code Integration` installs a hook so
  Claude Code's test/build/install commands run on the remote automatically.

## Requirements

- The `devsync` binary on `PATH` (or set `devsync.binaryPath`).
- Key-based ssh access to the remote (`ssh user@host` works without a password prompt).

## Settings

| Setting | Default | |
| --- | --- | --- |
| `devsync.binaryPath` | `""` | Path to `devsync`; empty = search `PATH`, `~/.local/bin` and `~/.cargo/bin` |
| `devsync.autoSync` | `true` | Start auto sync when a devsync project opens |
| `devsync.openOutputOnBackgroundRun` | `true` | Open a background job's output file when it starts |

## Development

```bash
npm install
npm run compile      # or: npm run watch
npm run package      # produces devsync-<version>.vsix
```

Press F5 in VS Code with this folder open to launch an Extension Development Host.
