#!/bin/sh
# Installs devsync from an unpacked release tarball (or a source checkout).
#
#   ./install.sh                 install to ~/.local
#   PREFIX=/usr/local ./install.sh
set -eu

PREFIX=${PREFIX:-"$HOME/.local"}
here=$(cd "$(dirname "$0")" && pwd)

if [ -x "$here/bin/devsync" ]; then
    src_bin="$here/bin/devsync"
    src_agents="$here/share/devsync/agents"
elif [ -f "$here/../Cargo.toml" ]; then
    echo "source checkout detected; building (needs Rust)…"
    (cd "$here/.." && make build agents)
    src_bin="$here/../target/release/devsync"
    src_agents=""
else
    echo "error: run this from an unpacked devsync release or a source checkout" >&2
    exit 1
fi

mkdir -p "$PREFIX/bin" "$PREFIX/share/devsync/agents"
cp "$src_bin" "$PREFIX/bin/devsync.tmp" && chmod 755 "$PREFIX/bin/devsync.tmp" && mv -f "$PREFIX/bin/devsync.tmp" "$PREFIX/bin/devsync"

if [ -n "$src_agents" ] && [ -d "$src_agents" ]; then
    cp "$src_agents"/devsync-* "$PREFIX/share/devsync/agents/"
else
    for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
        f="$here/../target/$t/release/devsync"
        [ -f "$f" ] && cp "$f" "$PREFIX/share/devsync/agents/devsync-$t"
    done
fi
chmod 755 "$PREFIX/share/devsync/agents/"* 2>/dev/null || true

echo "installed: $PREFIX/bin/devsync"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "note: add $PREFIX/bin to your PATH" ;;
esac
"$PREFIX/bin/devsync" --version
