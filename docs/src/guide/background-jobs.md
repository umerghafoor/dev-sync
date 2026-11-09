# Background jobs and results

Every remote command is a **job**. `exec` starts a job and waits for it;
`submit` starts one and returns immediately.

```bash
devsync submit -- python train.py      # prints the job id
devsync bg -- npm run dev              # alias
```

## Where results go

Results are collected into a folder in your project:

```text
.devsync/jobs/<id>/
├── command        the command line
├── output.log     stdout + stderr, as far as collected
└── status.json    state, exit code, bytes collected, pull-back done
```

These are plain files, so editors, scripts and coding agents can read them
directly. Output arrives in the folder whenever something collects it:

| Collector | When |
| --- | --- |
| `devsync logs [id]` | Continuously, while you follow the job |
| `devsync watch` / VS Code auto sync | Every ~2 s while a job runs |
| `devsync collect` | Once (handy from scripts or cron) |
| `devsync exec` | Continuously, for its own job |

When a job finishes, the collector also copies back the files it generated.

## Managing jobs

```bash
devsync jobs                  # local list: id, state, exit code, age, command
devsync jobs --remote         # what the agent knows (e.g. jobs started elsewhere)
devsync logs                  # follow the latest job; Ctrl-C detaches
devsync logs 1794236         # any unique prefix of an id works
devsync logs --no-follow <id> # print what has been collected and exit
devsync kill [id]             # stop a job (default: latest)
```

Job states:

| State | Meaning |
| --- | --- |
| `running` | Still running, or output not fully collected yet |
| `exited` | Finished; `exit_code` is set (143 = stopped by `kill`) |
| `lost` | The remote lost track of it (e.g. the machine rebooted) |

## Why no connection stays open

`submit` opens an ssh session, starts the job under a detached supervisor
(`setsid`, SIGHUP ignored) and closes the session. The supervisor writes the
output and exit code to files on the remote. Collecting is a series of short
sessions that each fetch the new bytes. You can close your laptop, switch
networks, or lose the connection for hours; the job keeps running and the
output is still there when you come back.

The remote keeps a job for 7 days, and at most 200 jobs.
