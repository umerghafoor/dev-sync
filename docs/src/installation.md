# Installation

devsync is installed **only on your local machine**. The remote just needs
an ssh server; devsync uploads its agent there automatically the first time
it connects.

## Supported platforms

| Role | Platforms |
| --- | --- |
| Local machine (client) | macOS (Apple Silicon, Intel), Linux (x86_64, arm64) |
| Remote machine | Linux x86_64 and arm64 (any distribution: the agent is a static binary); macOS with the same architecture as the client |

## From a release

Download the archive for your machine from the project's GitHub Releases
page, then:

```bash
tar -xzf devsync-*.tar.gz
cd devsync-*/
./install.sh
```

This installs `devsync` to `~/.local/bin` and the remote agents to
`~/.local/share/devsync/agents`. Use another location with
`PREFIX=/usr/local ./install.sh`.

Check the download against `SHA256SUMS` from the same release:

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing
```

## From source

Requires [Rust](https://rustup.rs) 1.88 or newer.

```bash
git clone https://github.com/OWNER/devsync.git
cd devsync
make install
```

`make install` builds the client and the static Linux agents (the
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` targets are
added with `rustup` if missing; no cross compiler is needed).

## Put devsync on your PATH

If `devsync` reports `command not found` after installing, add the install
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
rm -f ~/.local/bin/devsync
rm -rf ~/.local/share/devsync
```

Or run `make uninstall` from a source checkout. On remotes, delete
`~/.devsync/` and the `.devsync-agent/` folder inside synced projects.
