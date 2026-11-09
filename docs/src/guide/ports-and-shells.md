# Ports, shells and pulling files

## Port forwarding

Reach a server running on the remote from your browser or `curl`:

```bash
devsync forward 3000 8000
```

`localhost:3000` and `localhost:8000` now connect to the same ports on the
remote. If the tunnel drops, it is re-established with backoff; press Ctrl-C
to stop. With no arguments it forwards `exec.ports` from the config:

```toml
[exec]
ports = [3000]
```

A typical dev-server setup:

```bash
devsync submit -- npm run dev
devsync forward 3000
```

## Interactive shell

```bash
devsync shell
```

Opens a login shell on the remote in the directory matching your current
one. This is a normal interactive ssh session, so unlike `exec` it ends if
the connection drops. Use it for quick poking around, not for long-running
work.

## Pulling code from the remote

**The whole project, safely:**

```bash
devsync pull
devsync pull --dry-run
```

`devsync pull` brings the remote's changes into your folder:

| On the remote | Locally | Result |
| --- | --- | --- |
| new file | missing | downloaded |
| changed | unchanged since your last sync | updated to the remote version |
| changed | **you edited it too** | your version kept; remote saved as `<file>.devsync-remote` |

Sync excludes apply (`node_modules`, `.venv`, … are not downloaded), and
nothing you edited is ever overwritten. Run it, review any
`.devsync-remote` files, then push as usual. In VS Code: **⬇ Pull from
Remote** in the devsync sidebar or the Source Control toolbar.

**A specific path, exactly:**

```bash
devsync pull results/
devsync pull logs/train.log /tmp/
```

With a path, `pull` copies those remote files exactly, **overwriting** local
copies and ignoring sync excludes. The first command writes into `./results/`;
the second into `/tmp/logs/train.log`. Transfers are verified and atomic.
