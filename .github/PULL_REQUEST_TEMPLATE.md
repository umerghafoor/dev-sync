## What and why

<!-- What does this change, and what problem does it solve? Link issues: "Fixes #123". -->

## How it was tested

<!-- Unit/e2e tests added? Manual checks against a real remote? -->

## Checklist

- [ ] `make check` passes (fmt, clippy, tests)
- [ ] Tests cover the change (e2e for user-visible behaviour)
- [ ] `make cli-docs` run if commands or flags changed
- [ ] Docs updated (`docs/src/…`, READMEs) if behaviour changed
- [ ] `CHANGELOG.md` updated under `[Unreleased]` for user-facing changes
- [ ] No operation needs a long-lived connection; writes stay atomic; network errors stay retryable
