# Contributing to devsync

Thanks for helping! This document covers how to propose changes. For a tour
of the code, see [Development](docs/src/contributing/development.md) and
[How it works](docs/src/architecture.md).

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to contribute

- **Report a bug.** Open an issue with the bug report form. Include the
  output of `devsync doctor` and the failing command run with `-v`.
- **Suggest a feature.** Open an issue with the feature request form and
  describe the problem before the solution.
- **Improve the docs.** Every page of the docs site has an edit link.
- **Fix something.** Issues labelled `good first issue` and `help wanted`
  are a good place to start. For larger changes, open an issue first so we
  can agree on the approach.

Security problems: please follow [SECURITY.md](SECURITY.md) instead of
opening a public issue.

## Development setup

```bash
git clone https://github.com/OWNER/devsync.git
cd devsync
make check
```

You need Rust 1.88+ (via [rustup](https://rustup.rs)); Node.js 20+ for the
VS Code extension; and `mdbook` for the docs (`cargo install mdbook`).

## Making a change

1. Fork the repository and create a branch from `main`:
   `git checkout -b fix/short-description`.
2. Make your change, with tests:
   - Unit tests next to the code (`#[cfg(test)]`).
   - Behaviour visible to users gets an end-to-end test in
     `crates/devsync-cli/tests/e2e.rs`. These need no network or ssh server.
3. Run everything CI runs:

   ```bash
   make check
   make cli-docs
   make docs
   ```

   `make cli-docs` regenerates the CLI reference; commit the result if you
   changed commands or flags.
4. Add a line under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md) for
   anything users will notice.
5. Open a pull request and fill in the template.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org):

```text
feat(client): pull back files deleted by remote commands
fix(transport): treat "connection reset" as transient
docs: explain prelude for conda users
```

Common types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `build`,
`ci`, `chore`. The scope is usually the crate name without the `devsync-`
prefix, or `vscode`.

## Pull request expectations

- CI is green: rustfmt, clippy with `-D warnings`, tests on Linux and macOS,
  the MSRV build, `cargo deny`, docs, and the extension build.
- Changes are focused: one topic per PR.
- New dependencies are pure Rust with permissive licenses (`deny.toml`
  enforces the list), so static agent builds keep working without a cross C
  toolchain.
- The [design rules](docs/src/contributing/development.md#coding-guidelines)
  hold: no operation may need a long-lived connection, all writes are atomic,
  and network errors stay retryable.

## Licensing

devsync is licensed under the [MIT License](LICENSE). Unless you state
otherwise, any contribution you submit is licensed under the same terms.
