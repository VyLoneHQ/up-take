# Security Policy

UP-TAKE reads your screen and runs a global hotkey listener in the background. Taking security issues
seriously here isn't optional — it's the whole trust story the project depends on.

## Supported Versions

Pre-1.0, only the latest released version is supported. Once 1.0 ships, this section will list the
supported release lines.

| Version | Supported |
| --- | --- |
| Latest release | ✅ |
| Older releases | ❌ |

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Report privately to **security@vylone.com**, or via GitHub's
[private vulnerability reporting](https://github.com/VyLoneHQ/up-take/security/advisories/new) on this
repository.

Include, if you can:

- A description of the vulnerability and its potential impact
- Steps to reproduce, or a proof of concept
- The affected version/commit

## Response Commitment

- **Acknowledgement** within 5 business days.
- **Initial assessment** (severity, whether it's accepted) within 14 days.
- **Fix or mitigation timeline** communicated once assessed — driven by severity, not a fixed SLA, since
  this is currently a solo-maintained project.

We'll credit reporters in the release notes unless you ask to stay anonymous.

## Scope

In scope: the UP-TAKE application, its build/release pipeline, and this repository's GitHub Actions
workflows.

Out of scope: third-party dependencies (report those upstream — though we'd still like to hear about
it), and the vylone.com marketing site (report separately if it ever gains its own security contact).

## Design Notes Relevant to Security

- UP-TAKE has **zero telemetry** in the core — it does not phone home.
- No captured screen content is ever logged.
- Local log files never contain captured content — see `SPECS/ingest-api-v1.md` in the project's
  planning workspace for the data-handling contract with its companion app, ARC-HIVE.
