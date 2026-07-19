# Contributing to UP-TAKE

Thanks for considering a contribution. This is currently a solo-maintained, early-stage project, so
please read this before opening a large PR — it'll save both of us time.

## Before you start

- **Check open issues and discussions first.** For anything non-trivial (new feature, architecture
  change), open an issue or discussion before writing code — the overlay/capture/OCR internals are
  still being actively designed, and a PR built on an assumption that's about to change is wasted work.
- **Small PRs get reviewed faster.** Prefer several focused PRs over one large one.

## Contributor License Agreement

**All external contributions require signing the [CLA](CLA.md).** A CLA-assistant bot will prompt you
automatically the first time you open a pull request. You keep your copyright; you're granting VyLone
the right to also use the contribution under other license terms (including commercial). See the CLA
for the full, plain-language explanation.

## Development setup

Requires Rust (stable, version pinned via `rust-toolchain.toml`), Node.js, and `pnpm`.

```powershell
git clone https://github.com/VyLoneHQ/up-take.git
cd up-take
pnpm install
pnpm tauri dev
```

## Before opening a PR

- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must pass clean
- `biome ci .` must pass clean
- `cargo test` and `pnpm test` must pass
- Follow [Conventional Commits](https://www.conventionalcommits.org/) for commit messages — this drives
  the changelog

## Commit style

```text
feat: add drag-to-select region overlay
fix: correct DPI scaling on secondary monitor
docs: clarify build-from-source steps
```

## Licensing model, so there are no surprises

UP-TAKE core is GPL-3.0-or-later. VyLone also ships a proprietary "Pro" tier built on the same codebase
under the CLA above. See [LEGAL-AND-COMMERCE](https://github.com/VyLoneHQ/up-take) references in the
README for the full picture.

## Pull request checklist

See [.github/PULL_REQUEST_TEMPLATE.md](.github/PULL_REQUEST_TEMPLATE.md) — it's applied automatically
when you open a PR.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Be kind.
