# Run your first command

1. Open a file in the folder you want to run in (e.g. `tests/test_api.py`).
2. In the devsync panel, type a command in the box, for example:

   ```text
   pytest -x
   ```

3. Press **⌘Enter** (Mac) or **Ctrl+Enter**.

A terminal opens and shows the output live. Before the command runs, devsync:

- sends your changes,
- runs the command **on the remote, in the same folder** as your open file,
- returns the real exit code,
- copies back files the command created (lockfiles, generated code).

**Run it again:** ⌘⌥R / Ctrl+Alt+R, or the ▶ button in the editor title bar.

Ctrl-C in the terminal stops the command on the remote.
