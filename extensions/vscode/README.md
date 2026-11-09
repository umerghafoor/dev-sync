# devsync for VS Code

**Edit locally, run remotely.** Write code on this machine and run it on a
remote one (GPU box, lab server, cloud VM) from a panel that works like Git's
Source Control view. Built for connections that drop.

## What you get

- **A devsync panel in Source Control**, next to Git:
  - a **command box**: type `pytest -x` and press **⌘Enter / Ctrl+Enter**
    to run it on the remote, the way you'd commit with Git;
  - **Not on remote yet**: files you changed that haven't been sent, marked
    **A** / **M** / **D**;
  - **Conflicts**: files a remote command changed while you edited them too,
    with **Compare**, **Keep yours** and **Accept remote** buttons;
  - **Remote jobs**: recent commands marked ✓ / ✗ / … (running). Click a job
    to see its output; use the buttons to stop it or run it again.
- **Auto sync.** Every save is sent to the remote. While offline, changes
  are queued and sent when the connection returns.
- **Status bar.** ✓ synced · ⟳ syncing · ☁ offline · ⏸ paused, plus the
  number of running jobs. Click it for every action.
- **Remote terminal**, port forwarding, and a one-click Claude Code setup.
- **A Getting Started walkthrough** (Help → Welcome → *Get started with devsync*).

## Setup

1. **Install the `devsync` command** (once). From the devsync repository:
   `make install`. This puts it in `~/.local/bin`, which the extension finds
   automatically.
2. **Make sure ssh works without a password:** `ssh you@server true`. If it
   prompts, run `ssh-keygen -t ed25519` and then `ssh-copy-id you@server`.
3. **Open your project folder** in VS Code.
4. Run **devsync: Connect Project to Remote…** (command palette, or the
   button in the Source Control view) and enter `you@server:~/myproject`.
5. Click **Run Doctor** when asked. It checks the connection, installs
   devsync's helper on the remote, and runs a test command.

Nothing needs to be installed on the remote machine.

## Everyday use

| I want to… | Do this |
| --- | --- |
| Run tests / a script on the remote | Source Control → devsync box → type the command → ⌘Enter |
| Run the same thing again | ⌘⌥R / Ctrl+Alt+R, or ▶ in the editor title bar |
| Start a long job (training, server) | devsync panel ⋯ → **Run on Remote in Background** |
| See a job's output | Click it under **Remote jobs** |
| Stop a job | ■ next to the running job |
| Get a shell on the remote | Terminal button in the devsync panel, or the *devsync remote terminal* profile |
| Send changes right now | ☁ **Push** button (auto sync usually does it for you) |
| Pause syncing | ⏸ button; ⟳ resumes |
| Open a remote web server locally | ⋯ → **Forward Ports** (set `ports = [8000]` under `[exec]` in `.devsync.toml`) |
| Resolve a conflict | Under **Conflicts**: ⇄ compare, ↩ keep yours, ✓ accept remote |
| Let Claude Code run on the remote | ⋯ → **Set Up Claude Code Integration** |

Commands run **in the remote folder matching the file you have open**, and
files they generate (lockfiles, build output) come back automatically.

## What gets synced

Everything in your project, **including** `.env` and other `.gitignore`'d
files, **except** `.git`, `node_modules`, `.venv`/`venv` and caches. Install
dependencies on the remote by running e.g. `npm install` from the command box.
To keep something local, list it in a `.devsyncignore` file (gitignore
syntax).

## Settings

| Setting | Default | |
| --- | --- | --- |
| `devsync.binaryPath` | `""` | Path to `devsync`. Empty: search `PATH`, `~/.local/bin` and `~/.cargo/bin` |
| `devsync.autoSync` | `true` | Send every save automatically when a devsync project opens |
| `devsync.openOutputOnBackgroundRun` | `true` | Open a background job's output when it starts |

Project options (remote host, excludes, environment, ports) live in
`.devsync.toml`. See the configuration reference in the devsync docs.

## Troubleshooting

- **"devsync command-line tool was not found"**: install it (`make install`),
  or set `devsync.binaryPath` to its full path.
- **Status bar shows ☁ offline**: changes are queued. Run **devsync: Run
  Doctor** to see why the remote can't be reached.
- **A tool works over ssh but not in devsync** (nvm, conda, pyenv): add
  `prelude = "source ~/.nvm/nvm.sh"` under `[exec]` in `.devsync.toml`.
- **Logs:** View → Output → **devsync**.

## Development

```bash
npm install
npm run watch
npm run package
```

Open this folder in VS Code and press F5 to start an Extension Development
Host. The extension only talks to the CLI (`devsync --json …`).
