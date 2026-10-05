## What does this change?

<!-- One or two sentences: what does this PR do, and why? -->

## Related issue

<!-- Closes #... , or "none" if this wasn't tracked in an issue -->

## Checklist

- [ ] I have accepted the [Contributor License Agreement](../CLA.md) in a comment on this PR (see its
      "How to sign" section: one line, on your first PR, and again if the Agreement has changed since)
- [ ] I have said above which parts, if any, are not my own original work or were written with an AI
      tool (the CLA's section 4)
- [ ] `cargo fmt` and `cargo clippy --all-targets -- -D warnings` pass clean
- [ ] `cargo clippy --release --all-targets -- -D warnings` passes clean (CI runs release as well as
      debug, and a release-only warning fails the build)
- [ ] `biome ci .` passes clean
- [ ] `cargo test` and `pnpm test` pass
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] I've updated relevant docs (README, CHANGELOG) if this changes user-facing behavior
- [ ] Tested manually on at least one real monitor/DPI configuration, if this touches the overlay
