# Contributing to UP-TAKE

Thanks for considering it. This is a solo project in an early phase, so read this before you open a
large pull request. It will save us both some time.

## Before you start

**Open an issue or a discussion first for anything non-trivial.** The overlay and capture internals
are still being designed, decisions get recorded and superseded regularly, and a pull request built
on an assumption that is about to change is work nobody gets to keep.

**Small pull requests get read faster.** Several focused ones beat one large one.

## What state the project is in

Five of seven area types are built and there is no release yet. The
[README](README.md) says what runs today. Text extraction has code now, and the README says what
is still missing from it. If you are looking for the read aloud or AI description features, those
are designed and not written, so there is no code there to improve.
<!-- source: STATUS.md Phase 1; ROADMAP.md task 1.26 and Phase 2; README.md. This count is a
     RESTATEMENT of the README's, so it has to move whenever that one does. It said "Two" until
     2026-08-22, when the README was corrected to four and this file was missed in the same change
     that wrote "update it in the same change that ships a type" into the README. The sentence
     below it named text extraction as unwritten until 2026-09-02, which roadmap 1.26 falsified in
     the same way: one file corrected, its restatement missed. -->

## Contributor License Agreement

**External contributions need a signed [CLA](CLA.md).** You keep your copyright. You grant VyLone
the right to also use the contribution under other licence terms, including commercial ones. The CLA
explains it in plain language and is short.

The reason is stated in the open rather than buried: UP-TAKE core is GPL-3.0-or-later, and a paid
tier built on the same codebase is planned for a later phase. That tier does not exist today. The
CLA is what keeps it possible without a relicensing scramble later.

**To sign, comment on your first pull request** saying you have read the CLA and accept it. There is
no bot. That comment is the record and it covers everything you send afterwards.
<!-- CORRECTED 2026-08-02: this file, CLA.md and the pull request template all said a CLA-assistant
     bot would prompt automatically. None is installed, checked against the repository's workflows,
     configuration and webhooks. -->

<!-- source: ROADMAP.md Phase 3 (Commercial, unstarted); ADR-0003 (the licence). The previous
     wording said VyLone "also ships" a Pro tier, present tense. Nothing ships. -->

## Development setup

Requires Rust (stable, pinned by `rust-toolchain.toml`), Node.js and `pnpm`. Windows 10 build 19041
or newer.

```powershell
git clone https://github.com/VyLoneHQ/up-take.git
cd up-take
pnpm install
pnpm tauri dev
```

## Before opening a pull request

All of these have to pass clean:

- `cargo fmt`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test` and `pnpm test`
- `biome ci .`

CI runs clippy in release as well as debug, so a release-only warning fails the build even when your
local debug run is green.

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/). The changelog is
generated from them.

```text
feat: add drag-to-select region overlay
fix: correct DPI scaling on secondary monitor
docs: clarify build-from-source steps
```

## Writing style, and what CI does and does not catch

**No em-dashes (`U+2014`) and no en-dashes (`U+2013`) anywhere in this repository.** Not in prose,
not in comments, not in doc comments, not in strings a user or an operator sees. Use a comma, a
colon, a full stop, or parentheses. Ranges take a plain hyphen (`1084-1099`) or the word "to".

`scripts/dash-ratchet.py` runs in CI and fails if the repository-wide count goes up. It is a
**ratchet**: the count may go down and never up, because the rule arrived against about twelve
hundred pre-existing characters, and a check that fails the build on the day it lands is a check
somebody disables. **If your change lowers the count it will also fail**, telling you to run
`python scripts/dash-ratchet.py --write-baseline` and commit the result. That is not pedantry: an
unbanked improvement is headroom, and the next change can spend it while the check reports no
regression.

**What the ratchet does NOT do, because the first version of this section claimed it did.** It does
not refuse a new dash on its own. It compares one repository-wide total against one ceiling, so a
change that adds a dash to a new comment and removes one somewhere else passes green. What it
guarantees is that the repository never gets worse in total and that every tranche of cleanup is
permanent. Refusing a newly added dash needs a diff-aware check, which is a different program,
because it has to tell an added line from a moved one. **So the rule above binds you and the script
is not what enforces it.**

**Picking the replacement is the whole job, and there is no single right answer.** It depends on what
the clause after the dash is doing:

| The clause after it is | Use |
| --- | --- |
| a consequence, or a joined clause | a comma |
| an explanation or a definition | a colon |
| an independent thought | a full stop, and start a new sentence |
| an aside | parentheses, or cut it |

Never a hyphen **as the replacement for a dash**. It reads as a typo, and it is what a find and
replace reaches for. A hyphen in a *range* is a different thing and is the correct form: `1084-1099`,
as the rule at the top of this section says. The two sentences used to sit eighteen lines apart with
nothing reconciling them, which an independent review read as a contradiction.

**If the character is data rather than punctuation**, inside a parsed string, a path, an identifier,
or a fixture a test compares byte for byte, do not change it. Add the file to `EXEMPT` in the script
with the reason it is data. An entry with a blank reason is refused, and so is one naming a path git
does not track. Untracked is the test, not "does not exist": a file sitting in your working tree that
you have not added is refused too, and the message says so.

**An exemption does not lower the count.** Exempt files are still counted, so the total cannot be cut
by declaring a file out of scope. What an exemption sets is the **floor**: the number the count can
never fall below, which the script prints beside the total. That is deliberate. The first version
skipped exempt files before counting, so a single entry could take a hundred and seventy characters
off the total and the script would then congratulate you and offer to bank it.

Why the rule exists: the people who read this repository are mostly engineers deciding whether the
work is serious, and the comments here carry most of the reasoning. The rest of the house style is
ordinary technical writing. Be specific, prefer a real number to an adjective, and say what you
actually ran.

## One thing worth knowing about this codebase

Automated checks here have a track record of passing over real defects, and most of the interesting
bugs have been found by running the app on real multi-monitor hardware. If your change touches the
capture path, the coordinate conversions or anything DPI related, say in the pull request whether you
drove it on hardware and what you saw. "Tests pass" is necessary and has repeatedly not been
sufficient.

## Pull request checklist

[.github/PULL_REQUEST_TEMPLATE.md](.github/PULL_REQUEST_TEMPLATE.md) is applied automatically when
you open one.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Be kind.
