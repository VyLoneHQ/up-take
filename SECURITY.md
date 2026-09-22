# Security Policy

UP-TAKE reads your screen and runs a global hotkey listener in the background. For a tool that does
that, security is the whole trust story, so this page is meant to be usable rather than decorative.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report it privately, either way round:

- Email **security@vylone.com**
- Or use GitHub
  [private vulnerability reporting](https://github.com/VyLoneHQ/up-take/security/advisories/new) on
  this repository

Include what you can:

- What the vulnerability is and what an attacker gets from it
- Steps to reproduce, or a proof of concept
- The affected version or commit

## What happens next

- **Acknowledgement** within 5 business days.
- **Initial assessment**, meaning severity and whether it is accepted, within 14 days.
- **A fix or mitigation timeline** once it is assessed. That is driven by severity and not by a fixed
  service level, because this is a solo project and a promise I cannot keep is worth less than none.

Reporters get credited in the release notes unless you would rather stay anonymous.

## Supported versions

There is no release yet, so there is nothing to support. Once something ships, only the latest
release is supported until 1.0, and this section will list the supported lines after that.
<!-- source: STATUS.md (no release; Phase 1 in progress). The previous version of this file carried a
     supported-versions table implying releases existed. -->

## Scope

**In scope:** the UP-TAKE application, its build and release pipeline, and this repository's GitHub
Actions workflows.

**Out of scope:** third-party dependencies, which go upstream, though I would still like to hear
about it. Also vylone.com, which has no security contact of its own yet.

## Design notes relevant to security

- **UP-TAKE has no telemetry and does not phone home.** No analytics, no crash reporting.
  <!-- source: PRODUCT-VISION.md; ADR-0010 (VyLone operates no endpoint). This is a design
       commitment, and its dependency half is now measured rather than assumed.
       CORRECTED 2026-08-02: this note previously said `reqwest` and `hyper` are "present
       transitively in the dependency tree via the app framework". On the target UP-TAKE ships they
       are not. `cargo tree -p up-take --target x86_64-pc-windows-msvc --edges normal` is 774
       entries and contains no `reqwest`, `hyper`, `h2`, `ureq` or `sentry`, with `tauri` itself
       present 6 times. Those crates reach the graph only through `tauri` on OTHER targets, which is
       why a cross-platform `cargo tree` lists them and the Windows one does not.
       What is still NOT verified is runtime behaviour: no probe has watched the built binary for
       outbound connections, and a dependency check says nothing about what the WebView does. Write
       that probe before strengthening the prose to a measured claim. -->
- **Captured screen content is never written to a log file.**
- **The overlay is excluded from screen capture at the window level, and that is the default.**
  Other capture tools cannot see UP-TAKE's own rendering, and UP-TAKE does not capture it either.
  **One setting changes it, and only you can turn it on:** *Show UP-TAKE in screen recordings*, in
  Settings under Capture. With it on, the exclusion is lifted and UP-TAKE appears in recordings
  like any other window. A frozen screen is meant to leave UP-TAKE out even then: for the moment
  it takes its own picture, UP-TAKE removes its window from the screen image and puts it back
  afterwards, and if it cannot remove it, it does not freeze. That is built and has not yet been
  confirmed on real hardware. Nothing else lifts the exclusion, and nothing lifts it on its own.
  <!-- source: ADR-0019 (overlay excluded from capture), decision 1 for the affinity and decision 3
       for the setting, which that ADR named from the start and roadmap 1.14 builds. The freeze
       sentence is decision 6 (backlog I-411), built in src-tauri/src/freeze.rs as a DWM cloak
       around the capture. "Not yet confirmed on real hardware" is literal: the ADR owes a rig
       pass showing a clean still, and this sentence is strengthened only after that pass, not
       before. Until 2026-09-22 this said a frozen screen "contains the overlay", which was true
       of the code before decision 6.

       This said "permanently and by design" and "UP-TAKE never captures it either" until
       2026-09-17. Both were true until the setting existed. Corrected rather than softened,
       because this file is the one a reader is entitled to take at face value: somebody who read
       "permanently", turned the setting on for a demo and was then captured by Teams would have
       been misled by the document that exists to stop exactly that.

       Found by the independent review of up-take PR #105. It was missed because the sweep that
       found the same sentence in overlay.rs, freeze.rs and lib.rs was scoped to
       `-- src-tauri/src README.md` and never looked here. -->
- **Early builds are unsigned.** An unsigned installer raises a SmartScreen warning on Windows. When
  a release exists, a SHA-256 checksum and a VirusTotal link go out with it.
  <!-- source: STATUS.md B-4 (code signing approach) -->
