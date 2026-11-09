# Releasing

Releases are cut from `main` by pushing a version tag. CI does the rest.

## Steps

1. Make sure `main` is green.
2. Bump the version everywhere and roll the changelog:

   ```bash
   scripts/bump-version.sh 0.2.0
   ```

   This updates the workspace `Cargo.toml`, `Cargo.lock`,
   `extensions/vscode/package.json`, and turns `## [Unreleased]` in
   `CHANGELOG.md` into `## [0.2.0] - <today>`.
3. Review and commit:

   ```bash
   git diff
   git commit -am "chore: release 0.2.0"
   ```

4. Tag and push:

   ```bash
   git tag -a v0.2.0 -m "devsync 0.2.0"
   git push origin main v0.2.0
   ```

## What the release workflow does

`.github/workflows/release.yml`, triggered by `v*` tags:

1. **Verify**: the tag matches the versions in `Cargo.toml` and
   `package.json`, and the full test suite passes.
2. **Build** release archives, each containing the client plus the static
   Linux agents:
   - `aarch64-apple-darwin`, `x86_64-apple-darwin`
   - `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`
3. **Package** the VS Code extension (`.vsix`).
4. **Publish** a GitHub Release with the archives, the `.vsix`,
   `SHA256SUMS`, and release notes taken from the matching `CHANGELOG.md`
   section.
5. **Marketplaces** (optional): if the repository secrets `VSCE_PAT`
   (Visual Studio Marketplace) and/or `OVSX_PAT` (Open VSX) are set, the
   extension is published there too. Without them, those steps are skipped.

The docs site is redeployed from `main` by `.github/workflows/docs.yml`.

## Versioning

devsync follows [Semantic Versioning](https://semver.org). Until 1.0, minor
versions may break the config format or the CLI; such changes are called out
in the changelog. The wire protocol has its own `PROTOCOL_VERSION`, and
agents are content-addressed, so mismatched client/agent pairs never talk to
each other.

## Repository settings checklist

- **Settings → Pages**: source "GitHub Actions" (for the docs site).
- **Settings → Code security**: enable private vulnerability reporting
  (referenced by `SECURITY.md`) and Dependabot alerts.
- **Branch protection on `main`**: require the CI checks.
- **Secrets** (optional): `VSCE_PAT`, `OVSX_PAT`.
