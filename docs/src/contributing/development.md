# Development

## Layout

```text
crates/
  devsync-proto/      wire protocol (frames, Request/Response)
  devsync-fs/         ignore rules, hashing + cache, sync plan, atomic writes
  devsync-agent/      remote endpoint and detached job supervisor
  devsync-transport/  ssh/local sessions, agent deployment, retry policy
  devsync-client/     push, pull-back, job collection, agent hook, config
  devsync-cli/        the `devsync` binary (client and agent) + e2e tests
extensions/vscode/    VS Code extension (TypeScript)
docs/                 this book (mdBook)
scripts/              install, docs generation, version bump
legacy/               original bash prototype (reference only)
```

Dependencies only point down the list: `cli → client → transport → fs →
proto`, and `agent → fs → proto`.

## Prerequisites

- Rust 1.88+ (`rustup`)
- For agents: `rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl`
  (`make agents` does this). Linking uses the bundled `rust-lld`, so no cross
  C toolchain is needed. Keep dependencies pure Rust so this keeps working.
- Node.js 20+ for the VS Code extension
- `mdbook` for the docs (`cargo install mdbook`)

## Everyday commands

```bash
make check                      # rustfmt --check, clippy -D warnings, all tests
cargo test -p devsync-client    # one crate
cargo run -- exec -- echo hi    # run the CLI from source
make agents                     # static Linux agents
make docs                       # build the book into docs/book
make docs-serve                 # live preview at http://localhost:3000
make vscode                     # build the .vsix
```

## Tests

- Unit tests live next to the code (`#[cfg(test)]`).
- `crates/devsync-cli/tests/e2e.rs` drives the real binary:
  - against a `local` agent (no ssh), covering sync rules, exec, pull-back
    conflicts, detached jobs and kill;
  - through a fake `ssh` script that runs the remote side under a temporary
    `HOME`, covering agent deployment and network outages.

No test needs network access or a real ssh server.

## Trying it against a real server

```bash
cargo build && make agents
cd /some/project
/path/to/devsync/target/debug/devsync init you@server:~/scratch-project
/path/to/devsync/target/debug/devsync doctor
```

A debug client finds the release agents under `target/<triple>/release/`.

## VS Code extension

```bash
cd extensions/vscode
npm install
npm run watch
```

Open `extensions/vscode` in VS Code and press F5 to start an Extension
Development Host. The extension only talks to the CLI (`devsync --json …`),
so most behaviour can be tested from the terminal.

## Docs

The CLI reference page is generated from `--help`:

```bash
scripts/gen-cli-docs.sh > docs/src/reference/cli.md
```

CI fails if it's out of date. Run it after changing any command or flag.

## Coding guidelines

- Keep the session model intact: no operation may need a connection to
  outlive one request/response exchange batch. Long-running work belongs in
  jobs.
- Every write on either side goes through `devsync_fs::write` (staged, then
  atomic rename).
- Network errors must map to `TransportError::Connect`, `Disconnected` or
  `Timeout` so they're retried. Configuration and auth errors must not be.
- New protocol messages need a `PROTOCOL_VERSION` bump if they change
  existing encodings.
- User-facing output goes through `Event`s, so `--json` and the extension
  get it too.
