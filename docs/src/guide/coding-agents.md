# Coding agents

devsync lets a coding agent work on your machine while everything it
*runs* happens on the remote. The agent doesn't need to know anything about
it.

## Claude Code

```bash
devsync setup claude
```

This does two things:

1. Adds a `PreToolUse` hook to `.claude/settings.json` (existing settings
   are kept). Before Claude Code runs a shell command, the hook decides where
   it should run:
   - **local**: `git`, `ls`, `cat`, `grep`/`rg`, `find`, `sed`, file moves,
     `curl`, and similar.
   - **remote**: anything that runs project code, such as `pytest`,
     `npm test`, `npm install`, `python app.py`, `make`, `cargo run`, `ps`
     and `kill`.

   A remote command is rewritten to `devsync exec -- '<command>'`, so Claude
   Code sees the output and exit code as if it had run locally. Your
   permission settings still apply: the hook rewrites the command but never
   approves it.
2. Adds a short section to `CLAUDE.md` explaining the setup (results folder,
   background jobs, what to do about retry messages).

Restart Claude Code sessions afterwards, or open `/hooks` once, so the hook
is loaded.

### Steering the routing

- End a command with `# local` or `# remote` to force it.
- Per project, in `.devsync.toml`:

```toml
[exec]
local_commands = ["make"]        # e.g. if make only runs formatters here
remote_commands = ["git"]        # e.g. if git must run where the credentials are
```

How it decides: every pipeline segment's first word is checked, after
skipping `VAR=value` and wrappers such as `sudo`, `env` and `time`. Quotes,
`&&`, `|`, subshells and heredocs are understood (so `cat > f <<EOF … EOF`
stays local). If the command can't be parsed, it runs remotely.

## Other agents (Codex, Cursor, Aider, …)

```bash
devsync setup agents-md
```

This writes the same instructions to `AGENTS.md`. Agents without hooks then
follow them and call `devsync exec -- …` themselves.

## Long-running processes

Dev servers and training runs should not block an agent. The instructions
tell agents to use `devsync submit`, read `.devsync/jobs/<id>/output.log`
(or `devsync logs <id> --no-follow`), and stop jobs with `devsync kill`.
Combine with `devsync forward` so `curl localhost:3000` reaches a server on
the remote.
