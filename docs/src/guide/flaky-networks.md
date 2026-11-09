# Working over flaky networks

devsync assumes the network will fail. This page lists what happens in each
situation.

| Situation | What happens |
| --- | --- |
| Network down when you run `push`/`exec` | Retries with exponential backoff (0.5 s up to 30 s, with jitter) until it works, with a message on each retry. Set `sync.retries` to give up after N attempts |
| Connection drops during an upload | The partial file stays staged on the remote; the next attempt resumes from where it stopped. Nothing half-written ever appears in the project |
| Connection drops while a command runs | Nothing happens to the command. Output collection pauses and resumes; no output is lost or duplicated |
| Laptop sleeps during a long job | Same as above: run `devsync logs <id>` when you're back |
| Connection drops right as a command is being started | devsync can't know whether the request reached the remote, so it won't risk starting it twice. It tells you to check `devsync jobs --remote` |
| `devsync watch` while offline | Keeps watching, marks changes pending, shows `offline`, and sends everything once the link is back |
| ssh hangs (half-open TCP) | Keepalives detect a dead link in ~15 s; each response also has a timeout (`ssh.response_timeout_secs`) |
| Remote rebooted mid-job | The job shows as `lost` |
| Wrong password, key or host key | Not retried; reported immediately with a hint |

| The shared ssh connection breaks (network change, dead TCP, server session limit) | devsync closes it and opens a fresh one; if that fails too it connects directly for 5 minutes |
| An agent upload is cut off | The upload is checked before it's installed; a truncated agent already on the remote is detected by size and replaced. Uploads are gzip-compressed, and local devsync processes take turns uploading |
| devsync is updated while VS Code's auto sync runs | The extension notices the new binary and restarts auto sync on it |

On high-latency links (VPNs, Tailscale relays) each new ssh connection can
take 10–20 s, while sessions on the shared connection cost one round trip.
Keep `ssh.multiplex = true` and a long `ssh.control_persist_secs` (default
600) so the connection setup is paid rarely.

## Tuning

```toml
[ssh]
connect_timeout_secs = 20       # slow links / satellite
response_timeout_secs = 300     # huge trees on slow disks
multiplex = true                # reuse one TCP connection for short sessions
control_persist_secs = 600      # keep that connection this long when idle

[sync]
retries = 0                     # 0 = keep trying; e.g. 5 to fail fast in scripts
```

Multiplexing saves an ssh handshake per operation. If your network kills idle
connections aggressively, lower `control_persist_secs`; correctness never
depends on it.

## Mobile hotspots, VPNs, captive portals

- If the IP changes (hotspot to Wi-Fi), the next session simply connects
  again. Running jobs are unaffected.
- If ssh access needs a VPN, connect it first. While it's down, devsync
  waits and retries.
- Jump hosts work through `~/.ssh/config` (`ProxyJump`) or
  `ssh.options = ["ProxyJump=bastion"]`.
