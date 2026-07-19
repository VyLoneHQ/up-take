# UP-TAKE

**Press a hotkey, drag a box over anything on your screen, get the text, a spoken reading, or an AI
explanation of it — in under two seconds, with nothing leaving your machine.**

UP-TAKE replaces the 5–6 tools power users juggle to pull information off their screen (ShareX,
PowerToys Text Extractor, ScreenToGif/OBS, Google Lens, and whatever runs TTS) with one overlay:
capture, OCR, read-aloud, and AI analysis, triggered only when you ask for it.

> 📸 *Screenshot / demo GIF coming with the first working overlay build.*

## Why this exists

- **Intentional capture, not surveillance.** UP-TAKE records only when you press the hotkey. It never
  runs continuous background capture — that's a deliberate design choice, not a missing feature.
- **Local-first.** OCR and inference run entirely on your machine. Nothing you capture is uploaded
  anywhere by the core application.
- **Zero telemetry.** No analytics, no crash reporting, no phone-home beyond an explicit, opt-in update
  check. A tool that reads your screen has to be inspectable to be trusted — that's also why this is
  open source.
- **Open source, GPL-3.0.** Full source, always. See [License](#license) for what that does and doesn't
  mean for forks.

## Status

**Pre-alpha.** The overlay engine, capture pipeline, and OCR integration are under active development.
Nothing here is installable yet — see [ROADMAP](https://github.com/VyLoneHQ/up-take/issues) for progress.

## Install

Not yet available. Once a v0.1.0 beta ships, it will be an unsigned Windows installer for that first
release (see [SECURITY.md](SECURITY.md) and the note on SmartScreen below), with SHA-256 checksums and
a VirusTotal link published alongside it.

## Build from source

Requires Rust (stable, pinned via `rust-toolchain.toml`), Node.js, and `pnpm`.

```powershell
git clone https://github.com/VyLoneHQ/up-take.git
cd up-take
pnpm install
pnpm tauri dev
```

## A note on the first release

Early builds will be **unsigned**. An unsigned installer on Windows triggers a SmartScreen warning —
we'd rather tell you that up front than have you discover it. We're applying to the SignPath Foundation
free code-signing program for open-source projects as soon as there's a public release to apply with.
Until then: check the published SHA-256 checksum and VirusTotal scan before you run anything.

## Support

This is currently a solo-maintained project, best-effort. Please use
[GitHub Issues](https://github.com/VyLoneHQ/up-take/issues) for bugs and
[Discussions](https://github.com/VyLoneHQ/up-take/discussions) for questions — response times will vary.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). External contributions require signing the
[Contributor License Agreement](CLA.md).

## License

UP-TAKE is licensed under **GPL-3.0-or-later** — see [LICENSE](LICENSE).

The GPL covers the *code*. It does not cover the **UP-TAKE** or **VyLone** names/branding, which remain
all rights reserved. You're free to fork and modify the code under the GPL, but a fork may not call
itself UP-TAKE (the same arrangement Firefox and Chromium use, for the same reason).

Copyright (C) 2026 VyLone.
