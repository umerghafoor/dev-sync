# VS Code extension

The extension is a thin UI over the `devsync` CLI, so the editor, the
terminal and coding agents all share one sync engine and one results folder.
Install it as described in [Installation](../installation.md#vs-code-extension).

## Status bar

| Shows | Meaning |
| --- | --- |
| `✓ devsync` | Remote matches your files |
| `● devsync: changes` | Saved changes waiting to be sent |
| `⟳ devsync: syncing` | Sending |
| `☁ devsync: offline` | Can't reach the remote; changes are queued and retried |
| `⏸ devsync: paused` | Auto sync is off |
| `· 2 running` | Number of remote jobs still running |

Click it for the action menu.

## Commands

All commands are in the command palette under **devsync:**.

| Command | What it does |
| --- | --- |
| Run Command Remotely… | Syncs, then runs a command in a terminal on the remote, in the directory of the active file |
| Re-run Last Remote Command | |
| Run Command Remotely in Background… | Starts a job and opens its `output.log`, which fills in as the job runs |
| Open Remote Shell | Interactive shell on the remote (also available as the *devsync remote shell* terminal profile) |
| Push Now / Start/Stop Auto Sync | |
| Collect Job Results | Fetch output of running jobs now |
| Forward Ports | Runs `devsync forward` for `exec.ports` |
| Show Status / Run Doctor | |
| Initialize Project… | Creates `.devsync.toml` |
| Set Up Claude Code Integration | Runs `devsync setup claude` |

## Remote Jobs view

The devsync icon in the activity bar lists jobs, newest first, with state and
exit code. Click a job to open its output file. Inline actions: stop
(running jobs) and run again.

## Conflicts

If a remote command changes a file you also edited, a notification offers
**Compare**, which opens a diff of your version against the remote one
(`<file>.devsync-remote`).

## Settings

| Setting | Default | |
| --- | --- | --- |
| `devsync.binaryPath` | `""` | Path to `devsync`. Empty: searches `PATH`, `~/.local/bin`, `~/.cargo/bin`, `/usr/local/bin`, `/opt/homebrew/bin` |
| `devsync.autoSync` | `true` | Start auto sync when a devsync project is opened |
| `devsync.openOutputOnBackgroundRun` | `true` | Open a background job's output when it starts |

The extension activates in any workspace containing a `.devsync.toml`.
