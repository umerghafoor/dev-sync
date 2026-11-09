# Connect your project

Open your project folder, then run **devsync: Connect Project to Remote…**
and enter where the code should run:

```text
you@server:~/myproject
```

- `you@server` is your ssh login. An alias from `~/.ssh/config` works too.
- `~/myproject` is the folder on the remote. It is created if missing.

This creates `.devsync.toml` in your project. If the remote folder already
has code, the first sync downloads it into your folder before sending
anything back, so you can connect an empty folder to an existing project. Then run **Doctor** once: it
checks ssh, uploads the helper, and runs a test command on the remote.

Files that are synced: everything, including `.env` and other gitignored
files, **except** `.git`, `node_modules`, `.venv` and caches. Install
dependencies on the remote by running e.g. `npm install` through devsync.
