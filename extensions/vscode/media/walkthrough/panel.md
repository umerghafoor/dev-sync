# The devsync panel

**In the left bar:** click the **devsync icon** (two arrows), or press
⌘⌥D / Ctrl+Alt+D. The sidebar has a **Run** box at the top (type a command,
press Enter), your recent commands, and the lists below. The icon shows how
many changes haven't been sent yet.

**In Source Control** (⌃⇧G / Ctrl+Shift+G), next to Git, you'll find the
same thing as a **devsync** section:

| Part | What it shows |
| --- | --- |
| Command box | Type a command and press ⌘Enter / Ctrl+Enter to run it on the remote |
| **Not on remote yet** | Files you changed that haven't been sent yet: **A** added, **M** modified, **D** deleted |
| **Conflicts** | Files a remote command changed while you also edited them |
| **Remote jobs** | Recent commands: ✓ succeeded, ✗ failed, … running |

Toolbar buttons: **▶ Run**, **☁ Push**, **⏸ Pause / ⟳ Resume auto sync**,
**Terminal**, **Refresh**. More actions are under **⋯**.

With auto sync on (the default), "Not on remote yet" empties a moment after
each save. If the network is down, the list fills up and is sent once
the connection is back.

The status bar (bottom left) shows the same state: ✓ synced, ⟳ syncing,
☁ offline, ⏸ paused. Click it for all actions.
