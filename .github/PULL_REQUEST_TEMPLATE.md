## What does this change?

<!-- One or two sentences: what does this PR do, and why? -->

## Related issue

<!-- Closes #... , or "none" if this wasn't tracked in an issue -->

## Checklist

- [ ] I have accepted the [Contributor License Agreement](../CLA.md) in a comment on this PR (see its
      "How to sign" section, one line, first PR only)
- [ ] `cargo fmt` and `cargo clippy --all-targets -- -D warnings` pass clean
- [ ] `cargo clippy --release --all-targets -- -D warnings` passes clean (CI runs release as well as
      debug, and a release-only warning fails the build)
- [ ] `biome ci .` passes clean
- [ ] `cargo test` and `pnpm test` pass
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] I've updated relevant docs (README, CHANGELOG) if this changes user-facing behavior
- [ ] Tested manually on at least one real monitor/DPI configuration, if this touches the overlay
