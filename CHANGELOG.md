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
- A whole screen grab, `Win+Shift+G`, copying the monitor under the cursor to the clipboard without
  summoning the overlay. It goes through the same capture call the freeze uses when its fast path is
  off, so expect the same lateness.
- System tray icon and menu, and a single-instance guard.
- Areas: drag to create, then move, resize and dismiss them. Areas live until dismissed or until the
  app exits, and do not survive a restart.
- Three area types, chosen from the area's own right-click menu. A plain pinned region; a screenshot
  area that captures what is under it and keeps the still, which Copy and Save export; and a filter
  area that tints what is underneath and lets clicks pass through to it.
- Freeze the screen while placing an area, with `Ctrl+Space`.
- The overlay is excluded from screen capture, so other recording tools do not see it.

<!-- source: STATUS.md Phase 1 (1A merged, 1B partial, 1D partial); ROADMAP.md tasks 1.1 to 1.9f,
     plus 1.27 (area types chosen from the area's own menu) and 1.28 (those types in a submenu),
     which the "Three area types" entry above derives from and which sit outside the 1.1-1.9f range
     this line used to name on its own. `P-6` asks for the source of the claim rather than of the
     page, and that range stopped covering the entry when the entry was rewritten. Corrected after
     the independent review of 1.28.
     The previous entry read "Project scaffolding: license, community files, CI groundwork" and had
     not been touched since Phase 0. -->

