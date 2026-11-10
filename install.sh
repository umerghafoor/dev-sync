#!/bin/sh
# devsync installer.
#
#   curl -fsSL https://raw.githubusercontent.com/umerghafoor/dev-sync/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/umerghafoor/dev-sync/main/install.sh | sh -s -- --version 0.2.0
#   sh install.sh            (inside a release archive or a source checkout)
#   sh install.sh --uninstall
#
# Installs the `devsync` command to $PREFIX/bin (default ~/.local/bin), the
# static agents it uploads to remote machines to $PREFIX/share/devsync/agents,
# and, if VS Code's `code` command is available, the VS Code extension.
#
# Options:
#   --prefix DIR        install under DIR (default: ~/.local, or $PREFIX)
#   --version X.Y.Z     release to download (default: latest)
#   --from-source       build from this source checkout even if a release exists
#   --no-modify-path    don't add $PREFIX/bin to your shell's PATH
#   --no-vscode         don't install the VS Code extension
#   --uninstall         remove devsync (binary, agents, PATH line, extension)
#   -h, --help          show this help
#
# Environment: DEVSYNC_REPO (owner/name on GitHub), DEVSYNC_DOWNLOAD_BASE
# (mirror of the releases download URL), PREFIX.

set -eu

REPO=${DEVSYNC_REPO:-umerghafoor/dev-sync}
PREFIX=${PREFIX:-"$HOME/.local"}
VERSION=${DEVSYNC_VERSION:-latest}
MODIFY_PATH=1
VSCODE=1
FROM_SOURCE=0
UNINSTALL=0
EXTENSION_ID=devsync.devsync
RC_MARKER="# added by the devsync installer"

# ------------------------------------------------------------------ output

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); RED=$(printf '\033[31m')
    GREEN=$(printf '\033[32m'); YELLOW=$(printf '\033[33m'); RESET=$(printf '\033[0m')
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; RESET=""
fi

say()  { printf '%s\n' "${DIM}devsync:${RESET} $*"; }
ok()   { printf '%s\n' "${GREEN}✓${RESET} $*"; }
warn() { printf '%s\n' "${YELLOW}!${RESET} $*" >&2; }
die()  { printf '%s\n' "${RED}error:${RESET} $*" >&2; exit 1; }

usage() {
    sed -n '2,/^$/p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//' || true
    exit 0
}

# ------------------------------------------------------------------- args

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; PREFIX=$2; shift 2 ;;
        --prefix=*) PREFIX=${1#*=}; shift ;;
        --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION=${2#v}; shift 2 ;;
        --version=*) VERSION=${1#*=}; VERSION=${VERSION#v}; shift ;;
        --from-source) FROM_SOURCE=1; shift ;;
        --no-modify-path) MODIFY_PATH=0; shift ;;
        --no-vscode) VSCODE=0; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

case "$PREFIX" in
    "~"/*) PREFIX="$HOME/${PREFIX#"~/"}" ;;
esac
BIN_DIR="$PREFIX/bin"
AGENT_DIR="$PREFIX/share/devsync/agents"

# ---------------------------------------------------------------- helpers

has() { command -v "$1" >/dev/null 2>&1; }

download() { # url dest
    if has curl; then
        curl -fsSL --retry 3 --connect-timeout 15 -o "$2" "$1"
    elif has wget; then
        wget -q -O "$2" "$1"
    else
        die "need curl or wget to download devsync"
    fi
}

sha256_of() {
    if has sha256sum; then sha256sum "$1" | cut -d' ' -f1
    elif has shasum; then shasum -a 256 "$1" | cut -d' ' -f1
    else die "need sha256sum or shasum to verify the download"
    fi
}

detect_target() {
    os=$(uname -s); arch=$(uname -m)
    case "$arch" in
        x86_64|amd64) arch=x86_64 ;;
        arm64|aarch64) arch=aarch64 ;;
        *) die "unsupported CPU architecture: $arch" ;;
    esac
    case "$os" in
        Darwin) echo "$arch-apple-darwin" ;;
        Linux) echo "$arch-unknown-linux-musl" ;;
        *) die "unsupported OS: $os (devsync supports macOS and Linux; use WSL2 on Windows)" ;;
    esac
}

find_code() {
    if has code; then command -v code; return; fi
    for c in \
        "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code" \
        "$HOME/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code" \
        /usr/share/code/bin/code /snap/bin/code; do
        [ -x "$c" ] && { echo "$c"; return; }
    done
    return 1
}

shell_rc() {
    case "$(basename "${SHELL:-sh}")" in
        zsh) echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
        bash) if [ "$(uname -s)" = Darwin ]; then echo "$HOME/.bash_profile"; else echo "$HOME/.bashrc"; fi ;;
        fish) echo "$HOME/.config/fish/conf.d/devsync.fish" ;;
        *) echo "$HOME/.profile" ;;
    esac
}

# ------------------------------------------------------------- uninstall

if [ "$UNINSTALL" = 1 ]; then
    say "removing devsync from $PREFIX"
    rm -f "$BIN_DIR/devsync"
    rm -rf "$PREFIX/share/devsync"
    for rc in "$HOME/.zshrc" "${ZDOTDIR:-$HOME}/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.profile"; do
        if [ -f "$rc" ] && grep -q "$RC_MARKER" "$rc"; then
            grep -v "$RC_MARKER" "$rc" > "$rc.devsync-tmp" && mv "$rc.devsync-tmp" "$rc"
            say "removed PATH line from $rc"
        fi
    done
    rm -f "$HOME/.config/fish/conf.d/devsync.fish"
    if code_bin=$(find_code); then
        "$code_bin" --uninstall-extension "$EXTENSION_ID" >/dev/null 2>&1 && say "removed the VS Code extension" || true
    fi
    ok "devsync uninstalled"
    say "remote machines keep ~/.devsync/ and <project>/.devsync-agent/; delete them there if you like"
    exit 0
fi

# ---------------------------------------------------------------- sources

TMP=$(mktemp -d 2>/dev/null || mktemp -d -t devsync)
trap 'rm -rf "$TMP"' EXIT INT TERM

# Where this script lives, when run as a file (not piped into sh).
HERE=""
if [ -f "$0" ] && [ "$(basename "$0")" = install.sh ]; then
    HERE=$(cd "$(dirname "$0")" && pwd)
fi

SRC_BIN=""; SRC_AGENTS=""; SRC_VSIX=""

install_from_release_dir() { # dir
    SRC_BIN="$1/bin/devsync"
    SRC_AGENTS="$1/share/devsync/agents"
    for v in "$1"/devsync-*.vsix; do [ -f "$v" ] && SRC_VSIX=$v; done
    return 0
}

install_from_source() { # repo root
    has cargo || die "building from source needs Rust: https://rustup.rs"
    has make || die "building from source needs make"
    say "building from source in $1 (first build takes a few minutes)…"
    (cd "$1" && make build agents)
    SRC_BIN="$1/target/release/devsync"
    SRC_AGENTS="$TMP/agents"
    mkdir -p "$SRC_AGENTS"
    for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
        f="$1/target/$t/release/devsync"
        [ -f "$f" ] && cp "$f" "$SRC_AGENTS/devsync-$t"
    done
    for v in "$1"/extensions/vscode/devsync-*.vsix; do [ -f "$v" ] && SRC_VSIX=$v; done
    if [ -z "$SRC_VSIX" ] && [ "$VSCODE" = 1 ] && has npm && find_code >/dev/null; then
        say "building the VS Code extension…"
        if (cd "$1" && make vscode >/dev/null 2>&1); then
            for v in "$1"/extensions/vscode/devsync-*.vsix; do [ -f "$v" ] && SRC_VSIX=$v; done
        else
            warn "could not build the VS Code extension (skipping it)"
        fi
    fi
}

install_from_download() {
    case "$REPO" in
        OWNER/*) die "this installer has no GitHub repository configured yet.
  Set DEVSYNC_REPO=owner/devsync, or clone the repository and run: sh install.sh" ;;
    esac
    target=$(detect_target)
    base=${DEVSYNC_DOWNLOAD_BASE:-"https://github.com/$REPO/releases/download"}
    if [ "$VERSION" = latest ]; then
        [ -z "${DEVSYNC_DOWNLOAD_BASE:-}" ] || die "--version is required with DEVSYNC_DOWNLOAD_BASE"
        say "looking up the latest release of ${REPO}…"
        if ! download "https://api.github.com/repos/$REPO/releases/latest" "$TMP/latest.json"; then
            # 404 here means "no release yet" if the repository itself answers.
            if download "https://api.github.com/repos/$REPO/releases?per_page=1" "$TMP/all.json" \
                && ! grep -q '"tag_name"' "$TMP/all.json"; then
                die "$REPO has no published release yet. Build from source instead (needs Rust, https://rustup.rs):
  git clone https://github.com/$REPO && cd $(basename "$REPO") && sh install.sh"
            fi
            die "could not query GitHub releases (offline? rate-limited?). Try --version X.Y.Z"
        fi
        VERSION=$(sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' "$TMP/latest.json" | head -1)
        [ -n "$VERSION" ] || die "could not determine the latest version"
    fi
    name="devsync-$VERSION-$target"
    url="$base/v$VERSION"
    say "downloading devsync $VERSION for ${target}…"
    download "$url/$name.tar.gz" "$TMP/$name.tar.gz" || die "download failed: $url/$name.tar.gz"
    download "$url/SHA256SUMS" "$TMP/SHA256SUMS" || die "download failed: $url/SHA256SUMS"
    want=$(grep " $name.tar.gz\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)
    [ -n "$want" ] || die "$name.tar.gz is not listed in SHA256SUMS"
    got=$(sha256_of "$TMP/$name.tar.gz")
    [ "$want" = "$got" ] || die "checksum mismatch for $name.tar.gz (expected $want, got $got)"
    ok "checksum verified"
    tar -xzf "$TMP/$name.tar.gz" -C "$TMP"
    install_from_release_dir "$TMP/$name"
    if [ "$VSCODE" = 1 ] && find_code >/dev/null; then
        vsix="devsync-$VERSION.vsix"
        if download "$url/$vsix" "$TMP/$vsix" 2>/dev/null; then
            want=$(grep " $vsix\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)
            if [ -n "$want" ] && [ "$want" = "$(sha256_of "$TMP/$vsix")" ]; then
                SRC_VSIX="$TMP/$vsix"
            else
                warn "VS Code extension checksum did not match (skipping it)"
            fi
        fi
    fi
}

if [ -n "$HERE" ] && [ -x "$HERE/bin/devsync" ] && [ "$FROM_SOURCE" = 0 ]; then
    say "installing from the release archive in $HERE"
    install_from_release_dir "$HERE"
elif [ -n "$HERE" ] && [ -f "$HERE/Cargo.toml" ] && [ -d "$HERE/crates/devsync-cli" ]; then
    install_from_source "$HERE"
elif [ "$FROM_SOURCE" = 1 ]; then
    die "--from-source must be run from a devsync source checkout: git clone … && sh install.sh"
else
    install_from_download
fi

[ -x "$SRC_BIN" ] || die "devsync binary not found at $SRC_BIN"

# ---------------------------------------------------------------- install

mkdir -p "$BIN_DIR" "$AGENT_DIR"
cp "$SRC_BIN" "$BIN_DIR/.devsync.tmp.$$"
chmod 755 "$BIN_DIR/.devsync.tmp.$$"
mv -f "$BIN_DIR/.devsync.tmp.$$" "$BIN_DIR/devsync"
ok "installed $BIN_DIR/devsync"

n=0
if [ -d "$SRC_AGENTS" ]; then
    for a in "$SRC_AGENTS"/devsync-*; do
        [ -f "$a" ] || continue
        cp "$a" "$AGENT_DIR/" && chmod 755 "$AGENT_DIR/$(basename "$a")" && n=$((n + 1))
    done
fi
if [ "$n" -gt 0 ]; then
    ok "installed $n remote agent build(s) to $AGENT_DIR"
else
    warn "no Linux agent builds installed; remotes that differ from this machine won't work"
fi

# macOS marks downloaded files as quarantined; the binary is not notarized.
if [ "$(uname -s)" = Darwin ] && has xattr; then
    xattr -d com.apple.quarantine "$BIN_DIR/devsync" 2>/dev/null || true
fi

# ------------------------------------------------------------------- PATH

on_path=0
case ":$PATH:" in *":$BIN_DIR:"*) on_path=1 ;; esac
if [ "$on_path" = 0 ]; then
    rc=$(shell_rc)
    if [ "$MODIFY_PATH" = 1 ]; then
        mkdir -p "$(dirname "$rc")"
        if [ -f "$rc" ] && grep -q "$RC_MARKER" "$rc"; then
            :
        elif [ "$(basename "$rc")" = devsync.fish ]; then
            printf 'fish_add_path "%s" %s\n' "$BIN_DIR" "$RC_MARKER" > "$rc"
        else
            # $PATH must stay literal: it is expanded when the rc file is sourced.
            # shellcheck disable=SC2016
            printf '\nexport PATH="%s:$PATH" %s\n' "$BIN_DIR" "$RC_MARKER" >> "$rc"
        fi
        ok "added $BIN_DIR to PATH in $rc"
        PATH_HINT="open a new terminal (or run: source $rc)"
    else
        PATH_HINT="add $BIN_DIR to your PATH"
    fi
fi

# ---------------------------------------------------------------- VS Code

if [ "$VSCODE" = 1 ]; then
    if code_bin=$(find_code); then
        if [ -n "$SRC_VSIX" ]; then
            if "$code_bin" --install-extension "$SRC_VSIX" --force >/dev/null 2>&1; then
                ok "installed the VS Code extension (reload VS Code windows to use it)"
            else
                warn "could not install the VS Code extension; install $SRC_VSIX via Extensions → ⋯ → Install from VSIX…"
            fi
        else
            say "VS Code extension not included in this install (see the docs to add it)"
        fi
    fi
fi

# ------------------------------------------------------------------- done

version=$("$BIN_DIR/devsync" --version 2>/dev/null || echo devsync)
printf '\n%s\n' "${BOLD}${version} is ready.${RESET}"
if [ -n "${PATH_HINT:-}" ]; then
    printf '  %s\n' "First $PATH_HINT."
fi
cat <<EOF

  Next, in your project:
    cd ~/path/to/project
    devsync init you@server:~/project
    devsync doctor
    devsync exec -- pytest

  Docs: https://github.com/$REPO#readme
EOF
