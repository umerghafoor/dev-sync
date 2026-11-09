# Quick start

This walks through a first project in about five minutes. Replace
`you@server` with your ssh destination (an alias from `~/.ssh/config`
works too).

## 1. Connect a project

```bash
cd ~/code/myproject
devsync init you@server:~/myproject
```

This writes `.devsync.toml` (commit it if your team shares the remote) and
adds `.devsync/` to `.gitignore`.

## 2. Check the setup

```bash
devsync doctor
```

`doctor` checks the config and the ssh login, uploads the agent if needed,
plans a sync, and runs a test command on the remote. It explains every
failure.

## 3. Run something

```bash
devsync exec -- python3 --version
devsync exec -- pytest -x
```

Each `exec`:

1. sends files that changed since the last sync,
2. runs the command on the remote in the directory matching your current one,
3. streams the output and returns the remote exit code,
4. copies back files the command created or changed.

Run it from a subdirectory and the command runs in the same subdirectory on
the remote.

## 4. Keep the remote up to date while you edit

```bash
devsync watch
```

Every save is pushed within a second. If the network drops, `watch` keeps
running and sends the queued changes once the link is back.

## 5. Long-running work

```bash
devsync submit -- python train.py --epochs 50
devsync logs
```

`submit` returns immediately with a job id. The job keeps running on the
remote even if your laptop sleeps or the network drops. `devsync logs`
follows it; press Ctrl-C to detach. Results are kept in
`.devsync/jobs/<id>/output.log`.

## 6. Let your coding agent use the remote

```bash
devsync setup claude
```

After restarting Claude Code, when it runs tests, builds or installs, those
commands run on the remote automatically. See
[Coding agents](guide/coding-agents.md).

## Next

- Change what gets synced: [Syncing files](guide/syncing.md)
- All options: [Configuration](reference/configuration.md)
- Something wrong? [Troubleshooting](troubleshooting.md)
