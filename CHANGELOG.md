# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Once there is a release to generate notes from, this file is produced from
[Conventional Commits](https://www.conventionalcommits.org/) via [git-cliff](https://git-cliff.org/).
Until then it is written by hand.

## [Unreleased]

**Nothing has been released.** Everything below is on `main` and has to be built from source.

### Added

- A transparent, always-on-top overlay that stays out of the way of the desktop underneath, with
  per-region click-through.
- Multi-monitor support including mixed DPI, portrait displays and negative coordinates.
- A global summon hotkey, `Win+Shift+U`, with conflict handling.
- System tray icon and menu, and a single-instance guard.
- Areas: drag to create, then move, resize and dismiss them. Areas live until dismissed or until the
  app exits, and do not survive a restart.
- Two area types. A plain pinned region, and a screenshot area that captures what is under it and
  keeps the still. Copy to clipboard or save to a file.
- Freeze the screen while placing an area, with `Ctrl+Space`.
- The overlay is excluded from screen capture, so other recording tools do not see it.

<!-- source: STATUS.md Phase 1 (1A merged, 1B partial, 1D partial); ROADMAP.md tasks 1.1 to 1.9f.
     The previous entry read "Project scaffolding: license, community files, CI groundwork" and had
     not been touched since Phase 0. -->

