# Syncing files

devsync syncs **one way**, from your machine to the remote. Your local tree
is the source of truth; the remote is a mirror that commands run in. The only
data that flows back is command output and files that commands generate (see
[pull-back](running-commands.md#files-generated-by-commands)).

## When syncing happens

| Trigger | What happens |
| --- | --- |
| `devsync push` | One sync, then exit |
| `devsync watch` (or VS Code auto sync) | Sync on every save, about 300 ms after the last change |
| `devsync exec` / `devsync submit` | Sync right before starting the command (skip with `--no-push`) |

Only changed files are sent. Changes are detected by content hash (BLAKE3),
with a local cache so unchanged files are not read again.

```bash
devsync push --dry-run    # list what would be sent or deleted
devsync push -v           # list every file as it is sent
devsync status            # pending local changes, link state, running jobs
```

## Connecting to a remote that already has code

If the remote folder already contains the project (say you cloned it there,
or another machine has been syncing to it), the **first sync from a new
machine downloads the remote's code first**:

- files that exist only on the remote are copied to your folder;
- if a file exists on both sides with different content, your local version
  is kept and sent to the remote, and the remote's version is saved next to it
  as `<file>.devsync-remote` so nothing is lost;
- excluded paths (`node_modules`, `.venv`, …) are not downloaded.

After that first sync, syncing is one-way again, from your machine to the
remote. This also means connecting an empty folder never wipes an existing
remote project, even with `delete = true`. To start from your local files
only, set `sync.initial_pull = false` before the first sync.

```bash
devsync push --dry-run
```

This shows what the first sync would download. To bring remote changes
down at any later time, run `devsync pull` (see
[Pulling code from the remote](ports-and-shells.md#pulling-code-from-the-remote)).

## Choosing what to sync (large projects)

Remote projects often hold far more than code: datasets, model weights,
outputs. You choose which parts devsync handles. Unchosen paths are never
scanned or transferred, and stay untouched on the remote.

```bash
devsync tree
devsync tree src
devsync unselect data models
devsync select models/small --pull
devsync select
devsync select --all
```

- `devsync tree` lists one remote folder level at a time with sizes and file
  counts, and marks each entry `[x]` synced, `[~]` partly, `[ ]` not synced,
  or *always excluded*. Nothing is hashed; counting a huge folder stops after
  a few seconds and shows a lower bound (`≥ 141 MB`).
- `devsync unselect <path…>` stops syncing paths (stored as `/path` in
  `sync.ignore`).
- `devsync select <path…>` syncs them again; `--pull` downloads them right
  away. When `sync.include` lists paths, only those are synced.
- `devsync select` shows the current selection, and `--all` goes back to
  everything.

In VS Code, use the **Remote Files** view in the devsync sidebar: check or
uncheck folders, and checking one downloads it.

**Large-remote guard.** The first time a machine syncs, or when you
`devsync pull` everything, with nothing chosen yet, devsync first takes a
quick look at the remote. If it holds more than `sync.large_remote_limit_mb`
(default 1 GB), devsync stops, lists the biggest folders and asks you to
choose, instead of hashing and downloading everything. Auto sync shows this
once and waits until the selection changes; edits to `.devsync.toml` take
effect without restarting it.

## What is synced

Everything in the project directory except:

1. **Built-in excludes**, which are always skipped: `.git`, `.hg`, `.svn`,
   `node_modules`, `__pycache__`, `*.pyc`, `.venv`, `venv`, `.mypy_cache`,
   `.pytest_cache`, `.ruff_cache`, `.DS_Store`, `.idea`, and devsync's own
   `.devsync.toml`, `.devsync/`, `.devsync-agent/`, conflict copies (`*.devsync-remote`) and
   temp files. Dependencies and virtualenvs are
   platform-specific, so recreate them on the remote with
   `devsync exec -- npm ci` or `devsync exec -- pip install -r requirements.txt`.
2. **`.devsyncignore`** files, in gitignore syntax and in any directory.
3. **`sync.ignore`** patterns in `.devsync.toml`.
4. **`.gitignore`**, only if `sync.use_gitignore = true`.

> **Why `.gitignore` is not used by default:** files you keep out of git,
> such as `.env`, local configs and datasets, are usually exactly what the
> code needs in order to run. Exclude the ones that must stay local with
> `.devsyncignore`.

```gitignore
# .devsyncignore
*.log
dist/
secrets/production.env
```

Excluded files are never touched on the remote: not sent, not deleted.

## Deletions

By default, deleting a file locally does **not** delete it remotely. This is
safe, but stale files can pile up. To mirror deletions:

```toml
[sync]
delete = true
max_delete = 200   # refuse bigger deletions unless `devsync push --force`
```

## Symlinks, permissions, empty directories

- Symlinks are recreated as symlinks. They are never followed.
- The executable bit is preserved (scripts stay runnable).
- Empty directories are not synced, the same as git.

## How transfers survive interruptions

Files are sent in 1 MiB compressed chunks to a staging area on the remote,
checked against their hash, and then moved into place atomically, so a
half-written file is never visible. If the connection drops halfway through
a large file, the next attempt resumes where it stopped.
