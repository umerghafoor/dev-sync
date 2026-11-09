# Architecture

## Goals

1. Editing (by people or agents) happens on the local machine only.
2. Code runs on the remote machine and should feel like it ran locally:
   the same relative directory, live output, the real exit code, and generated
   files showing up locally.
3. An unreliable network must never lose work, corrupt files, or kill a
   running command. Nothing may depend on a connection staying up.

## Session model

There is no daemon and no persistent tunnel. Each operation opens one
**session**: a child process (`ssh -T host '~/.devsync/bin/devsync-<v>-<hash>
agent --stdio --root <path>'`) whose stdin/stdout carry protocol frames. The
session is closed as soon as the operation completes:

| Operation | Sessions |
| --- | --- |
| push | 1 per attempt (manifest, uploads, deletes) |
| exec | push + 1 to submit + 1 per output poll + 1 for pull-back |
| submit | push + 1 |
| collect / watch tick | 1 for all unfinished jobs |

If ssh multiplexing is enabled (`ssh.multiplex`, default), OpenSSH reuses one
TCP connection for consecutive sessions for `control_persist_secs`. This only
saves handshakes; correctness never depends on it.

Every session has a response timeout. ssh runs with `BatchMode=yes` (never
prompts), `ServerAliveInterval=5`, `ServerAliveCountMax=3` (dead links are
detected in about 15 s) and a connect timeout.

Failures are classified (`TransportError::is_transient`): network errors,
timeouts and dropped sessions are retried with exponential backoff and ±20%
jitter (`sync.retries`, 0 = until it works). Authentication and host-key
errors are fatal and reported at once.

## Protocol (`devsync-proto`)

Frames are a 4-byte big-endian length followed by a postcard payload (64 MiB
max). The client sends `Request`s and the agent answers each with one
`Response`, in order. Uploads are pipelined: up to 32 requests in flight
before waiting for acknowledgements, which is safe because acknowledgements
are tiny. The protocol version is checked in `Hello`. Agent binaries are
content-addressed, so client and agent always come from the same build.

## Sync (`devsync-fs`, `devsync-client::sync`)

- **Manifest.** Both sides walk their tree with the same `IgnoreSpec`:
  built-in excludes (VCS dirs, `node_modules`, virtualenvs, caches, devsync
  state), `.devsyncignore` files, config patterns, and `.gitignore` only when
  `use_gitignore = true`. Every entry carries a BLAKE3 hash. A per-side cache
  keyed on (size, mtime) avoids re-reading unchanged files, and entries
  modified within 2 s of the cache write are re-hashed to stay correct.
- **Plan.** Upload entries whose content, kind or executable bit differ. Delete
  remote-only entries only if `sync.delete` is on, and refuse more than
  `max_delete` without `--force`.
- **Transfer.** Files are sent as 1 MiB lz4 chunks into
  `.devsync-agent/partial/<hash>`. A new session first asks for
  `PartialOffsets` and resumes from there. `PutCommit` re-hashes the staged
  file and atomically renames it into place, with mode and mtime. A mismatch
  (e.g. the file changed mid-upload) is reported as retryable. Ancestors that
  are files are replaced by directories and vice versa. Symlinks are
  recreated, never followed.
- **Snapshot.** After a push, `.devsync/synced.v2.bin` records the hash of every
  file as the remote now has it. This is what makes pull-back safe.

## Jobs (`devsync-agent::jobs`, `devsync-client::jobs`)

```
client                              agent                       supervisor (setsid)
JobStart ─────────────────────────▶ write jobs/<id>/{meta,env,cmd.sh}
                                    spawn `devsync agent run-job` ──▶ bash -l cmd.sh
◀──────────────────────── JobStarted{id}                            > output.log 2>&1
(session closed)                                                     child.pid
JobPoll{id, offset} ──────────────▶ read output.log[offset..]       … exit → exit_code
◀───────── JobOutput{state, data, next_offset}
(session closed; repeat with adaptive interval)
```

- The supervisor calls `setsid()` and ignores SIGHUP, so neither the ssh
  session ending nor the network dropping affects the command. The command
  runs in its own process group so `JobKill` can signal the whole tree
  (SIGTERM, then SIGKILL after 3 s).
- A job is reported finished only once all of its output has been delivered.
  If the supervisor vanishes without an exit code (e.g. a reboot), the job is
  reported as `lost`.
- The client appends output to `.devsync/jobs/<id>/output.log` and records
  state in `status.json`. That folder is the source of truth for `logs`,
  `jobs`, the VS Code view, and coding agents.
- Submitting is not idempotent, so it is retried only when the connection
  could not be opened. If the connection drops after the request was sent,
  the user is told to check `devsync jobs`.
- Old jobs are garbage-collected on the agent (7 days, at most 200).

## Pull-back

When a job finishes, `JobChanges` lists remote files (with the sync excludes
applied) whose mtime is at or after the job's start. For each one:

| Local state | Action |
| --- | --- |
| same content as remote | nothing |
| equals the snapshot (unchanged since sync) or absent on both | download and replace atomically |
| edited locally since sync | download to `<file>.devsync-remote` and report a conflict |

Downloads are chunked, verified against the remote hash, and renamed into
place.

## Agent deployment (`devsync-transport::deploy`)

On first contact (or when the cached agent is missing), devsync:

1. Runs `uname -sm` to learn the remote OS and architecture.
2. Picks a matching build. Linux uses the static musl binaries, linked with
   the bundled `rust-lld`, so no cross C toolchain is needed. A remote with
   the same OS and architecture as the client gets the client binary itself.
3. Uploads it to `~/.devsync/bin/devsync-<version>-<hash12>` (temp file, then
   chmod, then rename) and records the name in `.devsync/remote.json`.

## Command routing for agents (`devsync-client::route`)

A small shell lexer understands quotes, escapes, operators (`&& || ; | &`,
subshells), comments and heredocs. It extracts the first word of every
pipeline segment, skipping `VAR=value` assignments and wrappers such as
`sudo`, `env` and `time`. If every word is a known local command (git,
coreutils, grep/rg, editors, …), the command stays local. Anything else is
rewritten by the Claude Code hook to `devsync exec -- '<original>'`. Commands
that cannot be parsed default to remote, and a trailing `# local` /
`# remote` overrides the decision.

## Security notes

- devsync only uses your ssh configuration and keys and opens no ports.
- The agent rejects absolute paths, `..`, and writes into its own state
  directory. Job ids are validated.
- Synced files keep their permission bits. Gitignored files such as `.env`
  are synced by default because they are usually needed to run the code. Set
  `use_gitignore = true` or add patterns if a secret must stay local.
