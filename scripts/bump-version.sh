#!/bin/sh
# Bumps the project version everywhere and rolls the changelog.
#
#   scripts/bump-version.sh 0.2.0
set -eu

new=${1:-}
case "$new" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) echo "usage: $0 X.Y.Z" >&2; exit 1 ;;
esac

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ "$old" != "$new" ] || { echo "already at $new" >&2; exit 1; }
grep -q '^## \[Unreleased\]' CHANGELOG.md || { echo "CHANGELOG.md has no '## [Unreleased]' section" >&2; exit 1; }

today=$(date +%Y-%m-%d)

# Workspace version (the first `version = ` line belongs to [workspace.package]).
awk -v new="$new" '!done && /^version = "/ { print "version = \"" new "\""; done = 1; next } { print }' \
    Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml
cargo update --workspace --quiet

# VS Code extension.
(cd extensions/vscode && npm version "$new" --no-git-tag-version --allow-same-version >/dev/null)

# Changelog: keep an empty Unreleased section above the new release.
awk -v new="$new" -v today="$today" '
    /^## \[Unreleased\]/ { print; print ""; print "## [" new "] - " today; next }
    { print }' CHANGELOG.md > CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md

echo "bumped $old -> $new"
echo "next: review \`git diff\`, commit, then: git tag -a v$new -m \"devsync $new\" && git push origin main v$new"
