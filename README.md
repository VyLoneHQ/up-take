# UP-TAKE

UP-TAKE is a Windows overlay that pins parts of your screen as areas and leaves them there. You
summon it with a hotkey and drag a box over anything. That box stays where you put it for as long as
UP-TAKE is running.

<!-- warmth line (VOICE.md section 3): the sentence below is here for how it lands, not for what it
     informs. It is true: areas persist until dismissed or the app exits (ADR-0009). -->
The window you keep alt-tabbing back to can just sit there instead.

It is free and open source under GPL-3.0, so it stays that way.
<!-- source: MASTER-PLAN.md section 1 (free, GPL-3.0); ADR-0003 (the licence) -->

**Nothing is installable yet.** There is no release, no installer and no download. What follows is
what actually runs today if you build it yourself.

## What works today

Four of seven area types are built.
<!-- source: STATUS.md Phase 1 (1A merged, 1B partial); ROADMAP.md tasks 1.9b (Screenshot),
     1.23 (Filter, merged 2026-08-12) and 1.24 (Upscale); PRODUCT-VISION.md section 3.1 (the
     seven types).
     This line said "Two of seven" until 2026-08-21. Filter shipped on 2026-08-12 and the
     count was never updated, so the front page of a public repository under-reported what
     was built for ten days. Update it in the same change that ships a type. -->

- **A plain pinned region.** A box you place and keep. Move it, resize it, dismiss it.
- **A screenshot area.** Captures what is under it and keeps the still, pinned inside the area.
  Copy it or save it to a file.
- **A tint area.** A warm translucent wash you leave over part of the screen and go on working
  underneath. Clicks fall straight through it, which is the point. That is also why you cannot
  move it by dragging its middle yet: its border and its close control are its handle.
  <!-- source: ROADMAP.md task 1.23 (merged 2026-08-12, PR #53, a59181a). The move limitation is
       that row's own "ships degraded and knowingly" note; the fix is task 1.17(b2), which is
       blocked on ADR-0028. Do not drop this sentence before b2 lands. -->
- **An upscale area.** Shows the piece of screen under it magnified, so something small is easier
  to read without moving closer. Two things it is not. It does not invent detail: the pixels are
  the ones already on your screen, stretched by the graphics card. And it is a still rather than a
  live view, re-taken when you move or resize the area, so pointing it at a playing video shows you
  a frame and not the video.
  <!-- source: ROADMAP.md task 1.24; ADR-0030 (upscale v1 is GPU resampling, no model, passive).
       Both negative sentences are ADR-0030's own scope and must not be softened away: the image
       enhancer over live video is the TARGET, is a Phase 2 row (2.12), and claiming it here is
       the P-6 failure this file has already had five of. The 2x factor is deliberately not
       quoted, because ROADMAP 1.24 leaves it for a hardware sitting to settle. -->
  <!-- NOT YET DRIVEN ON HARDWARE at the time of writing. If a rig pass finds that the passive
       re-take reads as broken rather than as a still, this bullet is what has to change. -->


Around those:

- **A three state model.** The hotkey toggles between placing areas and using your machine normally.
  Areas stay on screen either way.
  <!-- source: ADR-0012 (overlay interaction model) -->
- **Freeze while you place.** `Ctrl+Space` holds the screen still so something about to disappear
  does not have to be caught in time. It is late by roughly 350 ms by default, which matters for
  anything moving fast. There is a faster path behind a setting that is off by default because it
  costs measurable CPU on every monitor it holds.
  <!-- source: ADR-0026 (freeze on demand) and its second amendment; BACKLOG.md I-13 (the lateness,
       measured on the rig against a stopwatch); ROADMAP.md task 1.9f (the warm path, settings gated) -->
- **Grab a whole screen.** `Win+Shift+G` copies the monitor your cursor is on to the clipboard. No
  overlay, no selection, nothing to place. It goes through the same capture call the freeze uses when
  its fast path is off, so expect the same lateness: the pixels are the screen about a third of a
  second after you pressed the key. Fine for a page you are reading, wrong for something that is
  disappearing.
  <!-- source: ADR-0014 section 4 (a separate hotkey does an instant whole-monitor grab of the
       monitor under the cursor); ROADMAP.md task 1.9e. The lateness figure is UT-F-45, measured on
       the rig against a stopwatch for the FREEZE's cold path. This feature calls the same
       uptake_capture::capture_region and has not itself been measured on hardware, so the wording
       above says "expect" rather than quoting a number for it. Do not tighten that until a rig pass
       has run this path. -->
- **Multi-monitor and mixed DPI.** Verified on a four monitor rig with mixed scaling, a portrait
  display and negative coordinates. 4K at 150% and ultrawide are untested, because that hardware is
  not here.
  <!-- source: ROADMAP.md Phase 1A kill-criterion note; STATUS.md F-9 (M-2 and M-5 untestable) -->
- **UP-TAKE does not appear in screen recordings.** The overlay is excluded from capture at the
  window level, deliberately and permanently, so OBS, Teams, Discord and the Snipping Tool do not
  see it. That is a privacy property first and a limitation second, and it does mean "send me a
  screenshot of what you are seeing" does not work.
  <!-- source: ADR-0019 (overlay excluded from capture), decisions 2 and 5 -->

## What is designed and not built

Three more area types: record, read the text and describe the image. Areas that work
on each other are the goal and are not a feature yet.
<!-- source: PRODUCT-VISION.md section 3.1; ROADMAP.md section 1C (OCR unstarted) and Phase 2
     (TTS, analysis); BACKLOG.md I-3 (composition unscheduled) -->

Text extraction, reading aloud and AI description are all in that list. If you came here for those,
they are the destination and not the current state.

## Why it exists

- **It captures when you ask it to.** There is no continuous background recording and no
  plan for one.
  <!-- source: PRODUCT-VISION.md section 3.5; ADR-0026 (freeze is explicit, PLACEMENT only) -->
- **Processing is meant to stay on your machine.** VyLone runs no endpoint and does not intend to.
  Where a model is needed you point UP-TAKE at your own.
  <!-- source: ADR-0010 (no VyLone-run services); PRODUCT-VISION.md section 12 (custom endpoint,
       free and never gated) -->
- **No telemetry, no analytics, no crash reporting.** A tool that reads your screen has to be
  inspectable to be worth trusting, which is most of why it is open source.
  <!-- source: PRODUCT-VISION.md; SECURITY.md. NOTE: this is a design commitment. The dependency
       graph now supports it -- no HTTP client crate on the Windows target, checked 2026-08-02 and
       recorded in full in SECURITY.md -- but no probe has watched the built binary at runtime. Do
       not upgrade the wording to a measured claim without one. -->

## Build from source

Requires Rust (stable, pinned by `rust-toolchain.toml`), Node.js and `pnpm`. Windows 10 build 19041
or newer.
<!-- source: MASTER-PLAN.md section 4.1 (the 19041 floor and degrade-not-abort behaviour) -->

```powershell
git clone https://github.com/VyLoneHQ/up-take.git
cd up-take
pnpm install
pnpm tauri dev
```

`Win+Shift+U` summons the overlay once it is running. `Win+Shift+G` copies the monitor under your
cursor to the clipboard without summoning anything.

## When there is a release

The first builds will be **unsigned**, and an unsigned installer on Windows raises a SmartScreen
warning. Better you hear that here than discover it. I am applying to the SignPath Foundation free
signing program for open source projects as soon as there is a public release to apply with. Until
then a SHA-256 checksum and a VirusTotal link go out with anything downloadable.
<!-- source: STATUS.md B-4 (code signing approach); LEGAL-AND-COMMERCE.md section 5 -->

## Support

This is a solo project and support is best effort. Use
[Issues](https://github.com/VyLoneHQ/up-take/issues) for bugs and
[Discussions](https://github.com/VyLoneHQ/up-take/discussions) for questions. Response times vary.

Security problems go to [SECURITY.md](SECURITY.md) instead, never to a public issue.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). External contributions need a signed
[Contributor License Agreement](CLA.md).

## How this is built

Every change goes through the same sequence before it lands.

An agent writes it. Then a second model reviews the change in a fresh context, against the codebase
and not against the first agent's account of it, and that review has repeatedly found real defects.
The fixes go back through it. Where a test claims to guard something, the code it guards is broken
on purpose to confirm the test goes red.

Then it is driven on a real multi-monitor rig, across mixed DPI, a portrait display and negative
coordinates, because that is where this project's defects actually turn up.

Every claim in this file is traced to a source document before it is published.

I set the scope, decide what counts as done, and decide whether it ships. I do not read every line
that lands, and the review above is what carries that weight instead.
<!-- source: WORKFLOW/PREFERENCES.md P-2 (disclosure names both halves); AGENTIC-OS BACKLOG.md I-64
     (the founder's approved disclosure text, adapted for this surface and approved by him
     2026-08-02 in session 20260802T1916Z). It is ADAPTED rather than verbatim: I-64's text was
     written for audit reports and says "Finally I read the corrected version in full", which I-60
     and D-45 both record as untrue of this repository. P-0 ranks true above consistent, so the
     sentence was replaced rather than carried across. Do not "restore" the verbatim wording. -->
<!-- source for the rig sentence: STATUS.md F-9 (4K@150% and ultrawide untestable on this hardware),
     F-15/F-38 (defects found on hardware that CI passed). It deliberately does NOT claim every
     combination is covered -- the "What works today" section above says 4K and ultrawide are
     untested, and a disclosure contradicting the same page is the P-6 failure this file exists
     under. -->

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).

The GPL covers the code. It does not cover the **UP-TAKE** or **VyLone** names and branding, which
stay all rights reserved. Fork and modify the code freely under the GPL, but a fork cannot call
itself UP-TAKE. Firefox and Chromium use the same arrangement for the same reason.

Copyright (C) 2026 VyLone.
