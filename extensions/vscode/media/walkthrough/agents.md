# Coding agents

Run **devsync: Set Up Claude Code Integration**, then restart Claude Code.

From then on, when Claude Code runs commands:

| Runs on your machine | Runs on the remote |
| --- | --- |
| `git`, `ls`, `cat`, `grep`, file edits | `pytest`, `npm test`, `npm install`, `python app.py`, `make`, … |

Claude Code sees the output and exit code as usual. Every remote run also
appears in the **Remote jobs** list.

Force a command one way by ending it with `# local` or `# remote`.

For other agents (Codex, Cursor, …) run `devsync setup agents-md` in a
terminal. It writes the same instructions to `AGENTS.md`.
