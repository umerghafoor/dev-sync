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

**See what's on the remote first:**

```bash
devsync ls --changed
devsync ls src/api
devsync ls --all
```

| Marker | Meaning |
| --- | --- |
| `+` | only on the remote |
| `~` | changed on the remote (you haven't touched your copy) |
| `!` | changed on both sides |
| `=` | identical |

`--changed` hides identical files; `--all` also lists paths excluded by
`sync.ignore` or `.gitignore`.

**Only some files or folders:**

```bash
devsync pull src/api
devsync pull src/api/models.py tests/
devsync pull --dry-run src/
devsync pull --force config/settings.py
```

The same safety rules apply to just those paths. `--force` overwrites your
edits with the remote version instead of saving `*.devsync-remote`. Named
paths are pulled even if `sync.ignore` or `.gitignore` excludes them; the
built-in excludes (`.git`, `node_modules`, `.venv`, caches) and
`.devsyncignore` always apply.

**Copy somewhere else instead:**

```bash
devsync pull results/ --to ~/Downloads
```

`--to` copies the remote files exactly into another directory (here
`~/Downloads/results/…`) without touching your project.

**In VS Code:**

- right-click files or folders in the Explorer, or an editor tab, then
  choose **devsync: Pull from Remote**;
- **Pull Files…** (devsync sidebar, or the command palette) opens a
  searchable list of remote changes, including files you don't have yet,
  with folders at the top. Select several with Space and press Enter. The
  list button switches to all remote files.
