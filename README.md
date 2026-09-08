# UP-TAKE

**UP-TAKE - a precisely engineered quality-of-life tool that streamlines your existing workflows.**
<!-- source: the headline, the founder's own sentence, ADR-0039 (accepted 2026-09-08). It is quoted
     here and never reworded: a rewording is an amendment to that record, not an edit to this file. -->

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

## What it is meant to be

The area is the product. You place one anywhere on your screen, it stays while UP-TAKE runs, and its
type decides what it does: keep a still, tint, read text, sharpen a video, and later record, describe
what is in it, or hold a running application. The types are what makes an area useful, and nothing in
UP-TAKE is meant to exist outside one.
<!-- source: ADR-0038 decisions 1 and 3 (the area-system is the product; everything UP-TAKE does is an
     area, a property of an area, or an action on one), from the founder's answers A4 and A10 in
     Projects/UP-TAKE/HEADLINE-INTERVIEW.md. "later record, describe, hold an application" are ROADMAP
     2.14, 2.15 and 1.35, none built; the sentence says "later" for that reason (P-6). -->

The bar it is held to is the founder's: for each thing UP-TAKE does, it has to be the best way to do
that thing, or it has not succeeded. Where it can beat the tool you use for that job today, it is
because the area stays, sits over live content and works with the other areas, not because it has
more buttons.
<!-- source: ADR-0038 decision 2, from answers A6 and A11. The comparison per type is
     Projects/UP-TAKE/ROADMAP-AUDIT.md section 1 and is owed the founder's confirmation (Q-31), so no
     tool is named here and no comparison is claimed as won. -->

It is for two people: the ordinary Windows user, who should never have to learn a keyboard chord to
get something out of it, and the enthusiast who wants the desktop to work differently and will learn
anything. The first is who it is written for. The second is who is building it.
<!-- source: ADR-0038 decision 6, from answers A2 and A7. -->

We believe in quality over quantity, while making sure no important functionality is missing.
<!-- source: PRODUCT-VISION.md section 1, the founder's own mindset sentence, carried verbatim. -->

## What works today

Five of seven area types are built.
<!-- source: STATUS.md Phase 1 (1A merged, 1B partial); ROADMAP.md tasks 1.9b (Screenshot),
     1.23 (Filter, merged 2026-08-12), 1.24 (Upscale) and 1.26 (OCR); PRODUCT-VISION.md
     section 3.1 (the seven types).
     This line said "Two of seven" until 2026-08-21. Filter shipped on 2026-08-12 and the
     count was never updated, so the front page of a public repository under-reported what
     was built for ten days. Update it in the same change that ships a type. -->

- **A plain pinned region.** A box you place and keep. Move it, resize it, dismiss it. Scroll over
  it to zoom what it shows; scrolling back stops at normal size, so zooming out is always a way
  back rather than a way to get lost.
  <!-- source: ROADMAP.md task 1.25 (merged 2026-08-13, PR #54); PRODUCT-VISION.md section 3.4 for
       the 1x floor. -->
- **A screenshot area.** Captures what is under it and keeps the still, pinned inside the area.
  Copy it or save it to a file.
- **A tint area.** A warm translucent wash you leave over part of the screen and go on working
  underneath. Clicks fall straight through it, which is the point, and it is also why dragging its
  middle would drag whatever is under it instead. It moves by a bar that appears when you hover
  just above its top edge, or by holding `Win+Shift` and dragging anywhere on it.
  <!-- source: ROADMAP.md task 1.23 (merged 2026-08-12, PR #53); the bar is 1.17(b2) (ADR-0028,
       PR #70, merged 2026-08-26) and the chord is 1.17(c) (PR #72, merged 2026-08-27). This bullet
       said the area could not be moved by its middle "yet" and that its border was its whole handle
       until 2026-09-08, twelve days after both fixes merged. -->
- **An OCR area.** Reads the text under it, shows you what it read in the area itself, and puts
  that text on your clipboard, so the next thing you do can be a paste. It takes the clipboard
  for the conversion you asked for most recently: convert two areas at once and the second one
  is what you paste, whichever finishes first.
  Two things worth knowing before you build this yourself. **The model files and the OCR
  runtime are not in this repository**, because they are 47.6 MB of binaries that belong in a
  release rather than in a git history. The installer carries them, and three scripts fetch
  and verify them against pinned checksums first: `scripts/acquire-onnxruntime.py`,
  `scripts/acquire-ppocr-detector.py` and `scripts/acquire-ppocr-recogniser.py`.
  Building the app itself needs none of them. Building an
  **installer** needs all three, plus the config that packages them:
  `pnpm tauri build --config src-tauri/tauri.release.conf.json`. And **there is no accuracy standard yet**:
  it misreads characters, and nothing in the project says what a good enough reading would be.
  Something does measure it now, which is new and is not the same as a standard:
  `cargo run --release -p uptake-ocr --example ocr_accuracy` scores the pipeline against text of
  known content rendered by `scripts/render-ocr-cards.py`. On 192 rendered cards at the shipping
  settings it reads with a **0.018 character error rate, 87 % of cards exactly right, and no
  card containing text read as containing none**. At 14 px and above every card is exact. Those are rendered cards, so they are an upper
  bound on real screen text. Check anything you take from it before you use it.
  <!-- source: ROADMAP.md task 1.26 (the area type), 1.31 (the pipeline that returns text) and
       1.13 (the clipboard sentence, including the rule about which conversion wins, which is
       stated and enforced in `src-tauri/src/ocr.rs` by `Request`, `record_request`,
       `claims_clipboard` and `forget`, each driven by tests beside them.
       ⚠️ THIS DELIBERATELY CARRIES NO TEST COUNT, AND THE THIRD WRONG COUNT IS WHY.
       It said "four tests" until round 1 of PR #83's review found the rule documented and
       not enforced (gesture order was not compared, so lock-acquisition order decided it),
       and the fix here changed it to "seven". Round 2 then added two more tests to the same
       mechanism and this line was not touched, so round 3 found it stale again at "nine" --
       a number corrected in one commit and broken by the next, which is the class the two
       corrections were themselves about. Naming the four items removes the rot instead of
       resetting it: a reader who wants the count can run the tests, and a count in prose is
       a fact with no control behind it);
       ⚠️ THE CAVEAT ABOVE LOST A CLAUSE IN THIS MERGE, DELIBERATELY. This branch's
       copy still read "and no test measures it", which was true when 1.13 was written and
       false the moment 1.32 merged. Resolved toward main rather than toward this branch: a
       conflict is where a stale claim gets re-asserted by whoever resolves it, and the older
       side is the one that had gone false.
       ADR-0035 (the runtime and the models ship in the installer) and ROADMAP.md 1.12 with
       BACKLOG.md I-337, whose packaging work is what the sentence above describes -- the first
       caveat is now about where the binaries live and how a build gets them, not about
       packaging being absent. ⚠️ Written in the branch that does that work, so I-337 is still
       marked open in BACKLOG.md as this is read; close it on the merge, not before. BACKLOG.md
       I-341 (no OCR accuracy bar exists, lane C). Delete the second caveat when I-341 is
       answered, and not before -- the harness measuring the pipeline does NOT answer it, and
       the sentence is written to keep those two apart. ROADMAP.md 1.32 with BACKLOG.md I-351
       are the source of the harness sentence, and BACKLOG.md I-363 of the three figures:
       `ocr_accuracy` on this workstation, 2026-09-04, drop_score 0.5, det_thresh 0.2,
       box_thresh 0.4 -- 192 cards, CER 0.018, exact 87.0 %, empty 0.0 %, measured
       2026-09-05 after ADR-0037 took BOTH models to the PP-OCRv6 small tier.
       ⚠️ These replace the FIRST run's figures (CER 0.330, exact 54.2 %, empty 25.5 %),
       which were measured at the upstream thresholds 0.3 / 0.6 that I-363 then changed.
       Both numbers are real; they describe different builds, and the old ones are what the
       founder was seeing at the rig. Quote either only with the upper-bound sentence
       attached, and only beside the thresholds it was measured at -- a CER without its
       detector configuration is not a fact about this product. -->
- **An upscale area.** Sharpens the piece of screen under it, in place. It covers the same region
  at the same size and does not magnify: what changes is how clean it looks. Three things it is
  not. It does not invent detail, and it cannot: the pixels under it are already the final ones
  your screen is showing. What it can undo is the softening a player or a browser introduced when
  it stretched a smaller image up, which is the case it is for, so it does most on a low-resolution
  video and very little on ordinary desktop text. It does not smooth motion; that is a separate
  thing and is not built. And it is a still rather than a live view, re-taken when you move or
  resize the area, so pointing it at a playing video sharpens one frame and not the video.
  <!-- source: ROADMAP.md task 1.29; ADR-0031 (upscale is enhancement, not magnification;
       accepted 2026-08-25 on the founder's verdict, option B's shape with option A's v1 scope).
       This bullet said "shows the piece of screen under it magnified" until 2026-08-26 and that
       was ROADMAP 1.24 / ADR-0030, which ADR-0031 supersedes in its product definition.
       All three negative sentences are ADR-0031's own scope and must not be softened away:
       frame generation and the live loop are ROADMAP 1.30 and a later row, the model-based
       enhancer is 2.12, and claiming any of them here is the P-6 failure this file has already
       had five of. The sharpening strength is deliberately not quoted, because the constant is
       left for a hardware sitting to settle. -->
  <!-- NOT YET DRIVEN ON HARDWARE at the time of writing, and this is the type where that matters
       most: ADR-0031 question 3 makes the founder's eye at the rig the ONLY v1 gate on whether
       the sharpening looks right, because there is no number to hold it to. If a rig pass finds
       it reads as over-sharpened, or as doing nothing, this bullet is what has to change. -->

Around those:

- **A three state model.** The hotkey toggles between placing areas and using your machine normally.
  Areas stay on screen either way.
  <!-- source: ADR-0012 (overlay interaction model) -->
- **Change an area's type from its own menu.** Right-click an area and pick a type from the submenu;
  it becomes that type in place.
  <!-- source: ROADMAP.md tasks 1.27 (PR #55, merged 2026-08-14) and 1.28 (PR #59, merged 2026-08-18). -->
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

Two more area types: record, and describe the image. Areas that work on each other are the
goal and are not a feature yet. And an area that holds a running application, so that a fullscreen
app fills the area instead of the monitor: decided for the first release on 2026-09-08, and gated on
three measurements before it may ship, none of which has been taken.
<!-- source: PRODUCT-VISION.md section 3.1; ROADMAP.md 2.14 (Record) and 2.15 (Analysis), rows since
     2026-09-08; 1.36 (composition, blocked on OPEN-QUESTIONS.md Q-22); 1.35 (hosting, ADR-0040,
     gates 1 to 3, and ADR-0029 for the spike that measured what it costs). This said Record and
     Analysis had "no roadmap row at all" until 2026-09-08, which was true until that day. -->

Reading aloud and AI description are in that list. If you came here for those, they are the
destination and not the current state.
<!-- This sentence named text extraction as unbuilt until 2026-09-08; the OCR area shipped on
     2026-09-02 and the paragraph above it was corrected then while this one was not. -->

## Why it exists

- **One system instead of a pile of tools.** The founder already had a tool for each of these jobs
  and built this so they would work as one system and stop costing him installs, licences, settings
  pages and privacy questions. That is the reason it exists. It is not a claim that UP-TAKE replaces
  anything you use today: it has to earn that one area at a time, and the section above says how far
  it has got.
  <!-- source: Projects/UP-TAKE/HEADLINE-INTERVIEW.md A8 (his words, verbatim there); ADR-0039
       decision 4 (consolidation is the reason and a consequence, never the pitch); ADR-0009, whose
       foreclosure of the "replaces N apps" pitch stands. Do not turn this bullet into that pitch. -->
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
