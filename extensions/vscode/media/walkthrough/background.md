# Long jobs and results

For training runs, servers or anything long, use **Run in Background**
(under ⋯ in the panel, or the command palette).

- The job runs on the remote **without a connection being held open**. You
  can close your laptop, switch networks, or lose the connection.
- Its output file opens right away and fills in as the job runs.
- You get a notification when it finishes.

Every job appears under **Remote jobs** in the panel:

| Action | How |
| --- | --- |
| See output | Click the job |
| Stop it | ■ button next to a running job |
| Run it again | ↻ button |

Output is saved in your project in `.devsync/jobs/<id>/output.log`.

To reach a server running on the remote, use **Forward Ports** (set
`ports = [8000]` under `[exec]` in `.devsync.toml`), then open
`http://localhost:8000`.
