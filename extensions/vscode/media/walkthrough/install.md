# Install the devsync command

The extension is a front end for the `devsync` command-line tool.

**One line** (macOS and Linux):

```bash
curl -fsSL https://raw.githubusercontent.com/umerghafoor/dev-sync/main/install.sh | sh
```

**Or from source** (needs Rust):

```bash
git clone <devsync repository>
cd devsync
sh install.sh
```

This puts `devsync` in `~/.local/bin`. The extension finds it there even if
that folder is not on your `PATH`.

**Check your ssh login** to the remote works without a password:

```bash
ssh you@server true
```

If it asks for a password, set up a key once:

```bash
ssh-keygen -t ed25519
ssh-copy-id you@server
```

Nothing has to be installed on the remote. devsync uploads a small helper
there by itself.
