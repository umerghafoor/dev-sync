# Comparison with other tools

devsync covers one workflow: **edit here, run there, over an unreliable
link**. Many good tools overlap with parts of it.

| | Editing stays local | Survives disconnects | Runs commands | Output/exit code back | Generated files back | Remote install |
| --- | --- | --- | --- | --- | --- | --- |
| **devsync** | yes | yes (detached jobs, resumable transfers) | yes | yes | yes, conflict-safe | none (auto-deployed) |
| VS Code Remote-SSH | no (edits on the remote) | no (editor freezes) | in remote terminal | yes | n/a | VS Code server |
| sshfs | no (files live remotely) | no (I/O hangs) | no | n/a | n/a | none |
| `ssh host cmd` | n/a | no (command dies) | yes | yes | no | none |
| rsync (+ a watcher) | yes | retry by hand | no | no | by hand | rsync |
| Mutagen | yes | yes | no (sync and forwarding only) | no | two-way sync | auto-deployed agent |
| Unison | yes | partly | no | no | two-way sync | matching unison version |
| Syncthing | yes | yes | no | no | two-way sync | daemon on both sides |
| git push / pull | yes | retry by hand | via hooks | no | commit by hand | git |

## When to use something else

- **You want the editor running on the remote** (huge repos, remote-only
  language servers): VS Code Remote-SSH, but only on a stable connection.
- **You need true two-way sync** (people editing on both sides):
  Mutagen or Unison.
- **You only need to copy files once:** rsync.

devsync works fine alongside these. For example, you can use devsync for
running code and Mutagen for something else.
