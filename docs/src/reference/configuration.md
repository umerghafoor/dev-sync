# Configuration reference

`devsync init user@host:path` writes `.devsync.toml` at the project root.
Commands work from any subdirectory, and remote commands run in the matching
remote subdirectory. Unknown keys are rejected so typos surface immediately.

## `[remote]`

| Key | Default | Description |
| --- | --- | --- |
| `host` | (required) | `user@host`, an alias from `~/.ssh/config`, or `local` (agent on this machine: VMs with shared folders, containers, tests) |
| `path` | (required) | Project directory on the remote. `~` is the remote home; relative paths are relative to it. Created if missing |

## `[ssh]`

| Key | Default | Description |
| --- | --- | --- |
| `port` | ssh default | |
| `identity` | ssh default | Private key file |
| `options` | `[]` | Extra `-o` options, e.g. `["ProxyJump=bastion", "Compression=yes"]` |
| `multiplex` | `true` | Reuse one TCP connection across devsync's short sessions (ControlMaster). Sessions stay short either way |
| `control_persist_secs` | `60` | Idle time before that shared connection closes |
| `connect_timeout_secs` | `10` | |
| `response_timeout_secs` | `120` | Max wait for any single agent response before the session is abandoned and retried |

Everything in `~/.ssh/config` (users, keys, jump hosts) applies as usual.
Login must not prompt: devsync runs ssh with `BatchMode=yes`.

## `[sync]`

| Key | Default | Description |
| --- | --- | --- |
| `use_gitignore` | `false` | `false`: files matched by `.gitignore` (`.env`, local configs, data) are synced. `true`: they are skipped |
| `ignore` | `[]` | Extra exclude patterns (gitignore syntax) |
| `initial_pull` | `true` | On this machine's first sync, download files that exist only on the remote (differing files are kept locally and saved as `<file>.devsync-remote`) before pushing |
| `delete` | `false` | Delete remote files that were deleted locally |
| `max_delete` | `200` | Refuse larger deletions unless `devsync push --force` |
| `retries` | `0` | Attempts for network operations; `0` retries until the link is back |

Always excluded: `.git`, `.hg`, `.svn`, `.devsync`, `.devsync-agent`,
`node_modules`, `__pycache__`, `*.pyc`, `.venv`, `venv`, `.mypy_cache`,
`.pytest_cache`, `.ruff_cache`, `.DS_Store`, `.idea`, and devsync's own `.devsync.toml`, conflict copies (`*.devsync-remote`) and temp files.

`.devsyncignore` files (gitignore syntax, any directory) add more excludes.
Excluded files are left alone on the remote, even with `delete = true`.

## `[exec]`

| Key | Default | Description |
| --- | --- | --- |
| `pull_back` | `true` | Copy back files a remote command created or changed. Never overwrites local edits; conflicts go to `<file>.devsync-remote` |
| `prelude` | `""` | Shell snippet run before every command, e.g. `source ~/.nvm/nvm.sh` or `conda activate ml` |
| `ports` | `[]` | Ports `devsync forward` exposes on localhost |
| `local_commands` | `[]` | Extra commands the agent hook keeps local (e.g. `["make"]` if `make` only formats) |
| `remote_commands` | `[]` | Extra commands the agent hook always sends remote |
| `poll_interval_ms` | `400` | Output poll interval while a job runs (backs off to 5x when idle) |

### `[exec.env]`

Environment variables for remote commands:

```toml
[exec.env]
PYTHONUNBUFFERED = "1"
CUDA_VISIBLE_DEVICES = "0"
```

Commands run in a login shell (`bash -l`, or `sh -l` if bash is missing), so
`~/.profile` / `~/.bash_profile` apply. Use `prelude` for anything only set
in interactive shells.

## Environment variables

| Variable | Purpose |
| --- | --- |
| `DEVSYNC_LOG` | Log filter, e.g. `debug` or `devsync_transport=debug` |
| `DEVSYNC_SSH` | ssh binary to use |
| `DEVSYNC_AGENT_DIR` | Directory containing `devsync-<target-triple>` agent builds |
| `NO_COLOR` | Disable colored output |

## Files devsync creates

| Path | Side | Contents |
| --- | --- | --- |
| `.devsync/` | local (`init` adds it to `.gitignore` in git projects) | hash cache, sync snapshot, remote info, `jobs/<id>/{command,output.log,status.json}` |
| `.devsync-agent/` | remote project | hash cache, partial uploads, `jobs/<id>/` |
| `~/.devsync/bin/` | remote home | agent binaries |
