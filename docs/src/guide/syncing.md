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
