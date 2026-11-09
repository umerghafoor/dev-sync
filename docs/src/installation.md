# Installation

devsync is installed **only on your local machine**. The remote just needs
an ssh server; devsync uploads its agent there automatically the first time
it connects.

## Supported platforms

| Role | Platforms |
| --- | --- |
| Local machine (client) | macOS (Apple Silicon, Intel), Linux (x86_64, arm64) |
| Remote machine | Linux x86_64 and arm64 (any distribution: the agent is a static binary); macOS with the same architecture as the client |

## Quick install

```bash
curl -fsSL https://raw.githubusercontent.com/umerghafoor/dev-sync/main/install.sh | sh
```

The installer:

1. detects your OS and CPU and downloads the matching release archive,
2. verifies it against the release's `SHA256SUMS`,
3. installs `devsync` to `~/.local/bin` and the remote agents to
   `~/.local/share/devsync/agents`,
4. adds `~/.local/bin` to your shell's `PATH` (zsh, bash or fish) if needed,
5. installs the VS Code extension when VS Code's `code` command is available.

Open a new terminal afterwards and check with `devsync --version`.

Options go after `sh -s --`:

```bash
curl -fsSL https://raw.githubusercontent.com/umerghafoor/dev-sync/main/install.sh | sh -s -- --version 0.1.0 --no-vscode
```

| Option | Effect |
| --- | --- |
| `--version X.Y.Z` | Install a specific release (default: latest) |
| `--prefix DIR` | Install under `DIR` instead of `~/.local` |
| `--no-modify-path` | Don't touch shell startup files |
| `--no-vscode` | Skip the VS Code extension |
| `--from-source` | Build from the source checkout the script is in |
| `--uninstall` | Remove devsync, its `PATH` line and the extension |

If you prefer not to pipe into `sh`, download `install.sh`, read it, then run
`sh install.sh`.

## From a release archive

Download the archive for your machine and `SHA256SUMS` from the GitHub
Releases page, then:

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing
tar -xzf devsync-*.tar.gz
cd devsync-*/
sh install.sh
```

## From source

Requires [Rust](https://rustup.rs) 1.88 or newer.

```bash
git clone https://github.com/umerghafoor/dev-sync.git
cd devsync
sh install.sh
```

Inside a source checkout, `install.sh` builds the client and the static
Linux agents (`make build agents`; the `*-linux-musl` targets are added with
`rustup` if missing, and no cross compiler is needed). If `npm` and VS Code
are available, it also builds and installs the extension. `make install` does
the same for the CLI, without the `PATH` and VS Code steps.

## Put devsync on your PATH

The installer does this for you. If you used `--no-modify-path` or
`make install` and `devsync` reports `command not found`, add the install
directory to your shell's `PATH`.

zsh (the macOS default):

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

bash:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

Then check:

```bash
devsync --version
```

## ssh access

devsync uses your normal ssh setup (`~/.ssh/config`, keys, jump hosts) and
never prompts for passwords. If this works without asking for a password,
devsync will too:

```bash
ssh you@your-server true
```

If it asks for a password, set up a key once:

```bash
ssh-keygen -t ed25519
ssh-copy-id you@your-server
```

## VS Code extension

Build the extension or download `devsync-<version>.vsix` from a release, then
either:

- in VS Code, open the Extensions view, click `⋯` and choose **Install from VSIX…**, or
- from a terminal, run `code --install-extension devsync-<version>.vsix`. The
  `code` command comes from **Shell Command: Install 'code' command in PATH**
  in VS Code's command palette.

## Shell completions

```bash
devsync completions zsh > ~/.zfunc/_devsync
devsync completions bash > ~/.local/share/bash-completion/completions/devsync
devsync completions fish > ~/.config/fish/completions/devsync.fish
```

For zsh, make sure `~/.zfunc` is in `fpath` before `compinit`.

## Uninstall

```bash
curl -fsSL https://raw.githubusercontent.com/umerghafoor/dev-sync/main/install.sh | sh -s -- --uninstall
```

This removes the binary, the agents, the `PATH` line the installer added, and
the VS Code extension. From a source checkout, `sh install.sh --uninstall` or
`make uninstall` (the latter removes only the binary and agents). On remotes, delete
`~/.devsync/` and the `.devsync-agent/` folder inside synced projects.
