# FAQ

### Does anything need to be installed on the remote?

No. You need an ssh server and a POSIX shell. devsync uploads a static agent
binary (about 3 MB) to `~/.devsync/bin/` the first time, and again only when
you upgrade devsync.

### Is it two-way sync?

No, on purpose. Your machine is the source of truth. Two-way sync means
conflicts on every save; devsync only brings back what commands generate,
and never overwrites your edits.

### Why are my `.gitignore`'d files synced?

Because they're usually what the code needs to run (`.env`, local configs,
data). Set `sync.use_gitignore = true` or list exclusions in
`.devsyncignore`. See [Syncing files](guide/syncing.md).

### Is my `.env` safe?

devsync copies it over your own ssh connection to a machine you already log
into, and keeps its permissions. If a secret must never leave your machine,
exclude it.

### Does it keep a connection open?

No. Every operation is a short ssh session. With `ssh.multiplex = true`,
OpenSSH may keep one idle TCP connection for `control_persist_secs` so the
next session starts faster, but nothing depends on it.

### Can several machines use the same remote project?

Yes. Each pushes its own tree. Jobs started from one machine are visible to
the others through `devsync jobs --remote` and `devsync logs <id>`.

### Can I use it with Docker or a VM on my machine?

Yes. With `remote.host = "local"`, the agent runs on this machine against
`remote.path`, which is useful for a shared folder mounted into a VM or
container. Any host reachable over ssh (including containers running sshd)
works as a normal remote.

### What about Windows?

The client and agent are Unix-only for now. WSL2 works as a client.

### How big a project can it handle?

Scanning uses a hash cache, so after the first sync only changed files are
read. Uploads are pipelined and compressed. Repositories with hundreds of
thousands of files work; exclude build outputs and data you don't need on
the remote to keep scans fast.

### How do I stop a runaway remote command?

`devsync kill <id>` (or Ctrl-C during `devsync exec`). It sends SIGTERM to the
command's whole process group, then SIGKILL after 3 seconds.
