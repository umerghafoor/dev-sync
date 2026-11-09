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

## Pulling files from the remote

```bash
devsync pull results/                # into ./results/
devsync pull logs/train.log /tmp/    # into /tmp/logs/train.log
devsync pull .                       # everything
```

`pull` copies files regardless of the sync excludes. Transfers are verified
and atomic.
