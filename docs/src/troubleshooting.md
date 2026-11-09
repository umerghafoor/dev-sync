# Troubleshooting

Start with:

```bash
devsync doctor
```

It checks the config, ssh, agent deployment, sync and remote execution, and
explains each failure. For more detail, add `-v` to any command, or set
`DEVSYNC_LOG=debug`.

## `command not found: devsync`

The install directory isn't on your `PATH`. See
[Installation → PATH](installation.md#put-devsync-on-your-path).

## `make: *** No rule to make target '#'`

You pasted a command with a trailing `# comment` into zsh, which doesn't
treat `#` as a comment in interactive shells by default. Run the command
without the comment, or enable comments with `setopt interactivecomments` in
`~/.zshrc`.

## `ssh refused: Permission denied` / `Host key verification failed`

devsync runs ssh in batch mode and never prompts. Make this work without
any prompt first:

```bash
ssh you@server true
```

- Password prompt: set up a key (`ssh-copy-id you@server`) or load it into
  the agent (`ssh-add`).
- New host: connect once by hand to accept the host key.
- Key in a non-default location: set `ssh.identity` or use `~/.ssh/config`.

## `no devsync build for remote platform linux/x86_64`

The agent binaries for the remote aren't installed next to your devsync. From
a source checkout, run `sh install.sh` (it builds them), or set
`DEVSYNC_AGENT_DIR` to a folder containing `devsync-x86_64-unknown-linux-musl`.

## `agent was uploaded but cannot be executed`

The remote home is probably mounted `noexec`, or the remote uses an
unsupported architecture. Check with `ssh you@server 'uname -sm; mount | grep home'`.

## `frame too large`, or the remote "printed N bytes without starting the devsync agent"

Something in the remote shell's startup files (`~/.bashrc`, `~/.profile`,
`/etc/profile.d/…`) runs when ssh starts a non-interactive command.
devsync ignores anything those files *print* (banners, conda/nvm messages)
and sends nothing until its agent is running, so they can't swallow its data. Remaining cases:

- **A startup file waits for input** (a prompt, `read`, a menu). The
  connection then times out. Make it run only in interactive shells, e.g.
  put it below this line in `~/.bashrc`:

  ```bash
  case $- in *i*) ;; *) return;; esac
  ```

- **A startup file prints megabytes of output.** Same fix.

`ControlSocket … already exists, disabling multiplexing` is harmless: two
devsync sessions started at the same moment, and one didn't share the
connection.

## "the remote project is large; choose what to sync"

The first sync, or a whole-project `devsync pull`, stopped before scanning
a big remote. Run `devsync tree` to see where the size is, then
`devsync unselect data` (or check/uncheck in VS Code's **Remote Files**).
Auto sync continues once `.devsync.toml` changes. To sync everything anyway,
set `sync.large_remote_limit_mb = 0`.

## Every operation is slow (10–20 s) on a VPN or Tailscale link

Opening an ssh connection is slow there; devsync reuses one shared
connection. Check `ssh.multiplex = true`, and raise
`ssh.control_persist_secs` (default 600; older configs may say 60). Check the
raw cost with `ssh -v you@host true`: the time before
"Connection established" is network setup.

## A command works over `ssh` but not with devsync

devsync runs commands in a **non-interactive login shell**, which loads
`~/.profile` / `~/.bash_profile` but not `~/.bashrc`. Tools set up only in
`.bashrc` (nvm, conda, pyenv) need the `prelude`:

```toml
[exec]
prelude = "source ~/.nvm/nvm.sh"
```

## Files I need on the remote aren't there

```bash
devsync push --dry-run
```

If a file is missing from the list, it is excluded by a built-in rule,
a `.devsyncignore`, `sync.ignore`, or (with `use_gitignore = true`)
`.gitignore`. See [Syncing files](guide/syncing.md#what-is-synced).

## `refusing to delete N remote files`

With `sync.delete = true`, devsync stops before deleting more than
`sync.max_delete` files, which usually means the wrong directory or a mass
rename. Check with `devsync push --dry-run`, then use `devsync push --force`
if it's intended.

## A `<file>.devsync-remote` appeared

A remote command changed a file that you also changed locally, so devsync
kept your version and saved the remote one next to it. Compare the two, keep
what you want, and delete the `.devsync-remote` file.

## A job shows `lost`

The remote supervisor disappeared without recording an exit code, which
usually means the remote rebooted or the process was killed by the OOM
killer. Whatever output was written is in `.devsync/jobs/<id>/output.log`.

## The Claude Code hook doesn't route commands

- Restart the Claude Code session, or open `/hooks` once.
- Check `.claude/settings.json` contains a `PreToolUse` hook that calls
  `devsync hook claude`, and that its path still exists (re-run
  `devsync setup claude` after moving the binary).
- Test the hook directly:

```bash
echo '{"tool_name":"Bash","cwd":"'"$PWD"'","tool_input":{"command":"pytest"}}' | devsync hook claude
```

It should print JSON containing `devsync exec`. An empty result means the
command was classified as local, or `cwd` isn't inside a devsync project.

## Reset local state

All local state is disposable:

```bash
rm -rf .devsync
```

You lose the local job history; the next push re-hashes files and compares
them with the remote.
