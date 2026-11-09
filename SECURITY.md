# Security Policy

## Supported versions

devsync is pre-1.0. Security fixes are released for the latest minor version
only.

| Version | Supported |
| --- | --- |
| 0.1.x | yes |
| < 0.1 | no |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately through GitHub's
[private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability):
go to the repository's **Security** tab and choose **Report a
vulnerability**.

Please include:

- the devsync version (`devsync --version`) and the OS of both machines,
- what an attacker can do, and under which assumptions (for example "a user
  who controls the remote machine can …"),
- steps to reproduce, or a proof of concept.

What to expect:

- an acknowledgement within 5 working days,
- an assessment and a plan within 14 days,
- a fix released as soon as practical, with credit in the advisory if you
  want it.

## Threat model

devsync connects two machines **you already control** over your own ssh
setup. In scope:

- **Path traversal or writes outside the project.** The agent must reject
  anything that escapes the project root or touches its state directory.
  This applies to both directions, since pull-back writes on the client.
- **Command injection.** Commands should run only as given; nothing
  received over the wire should become a shell command unintentionally.
- **The coding-agent hook** changing commands in ways the user did not
  intend, or bypassing the agent's permission prompts.
- **Leaking files** that are excluded from sync.

Out of scope:

- An attacker who already has a shell on either machine as your user.
- The security of ssh itself and of your ssh configuration.
- Files you chose to sync (for example `.env`, which is synced by default by
  design; see the docs on excluding secrets).
