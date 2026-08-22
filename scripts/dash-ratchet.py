#!/usr/bin/env python3
"""Counts em-dashes and en-dashes in tracked files and refuses an increase.

This repository is public, so its source is public writing: comments, doc
comments and operator-visible strings are all read by strangers deciding whether
this is serious work. `CONTRIBUTING.md` states the rule for contributors, which
is the copy an outside reader can actually open.

Why a ratchet and not a gate
----------------------------

This branch's own tree, at `914e920`, holds 1,187 of these characters across 46
files. `main` at `676ffde` holds 1,189 across 47: the two are this branch's,
removed from `.gitattributes`, and the pair is written out because the first
draft of this rule quoted the branch's number under `main`'s ref and a review
caught it. **Name the ref you counted at.**
A check that failed the build on day one is a check somebody disables,
and a disabled check reports nothing forever. So the baseline is recorded and may
only go **down**, the backlog is worked off in tranches, and every tranche
tightens the bound behind it.

What this does NOT do, said plainly because the first draft claimed otherwise
----------------------------------------------------------------------------

**It does not bind new writing immediately.** It compares one repository-wide
total against one ceiling, so a change adding five dashes to a new comment and
removing five elsewhere passes green, and so does one adding a dash to a file it
is sweeping in the same commit. An independent review found the claim and named
the case: `examples/testscreen/README.md` arrived carrying three, and this check
would have accepted or refused it purely on what else moved that day.

What it does guarantee is narrower and still worth having: **the repository never
gets worse in total, and every tranche is permanent.** Binding new writing
properly needs a diff-aware check, which is a different program, because it has to
tell an added line from a moved one. Recorded rather than pretended away.

**"The repository" means the file kinds in `TEXT_SUFFIXES` and `TEXT_NAMES`, and
saying so is a correction.** It is an allow list, so a tracked file with an
extension nobody has added is invisible: a review demonstrated it by committing
a `.rst` file holding forty em-dashes and getting an exit 0. The guarantee above
is real for the 24 kinds listed and silent about the rest, and the asymmetry
worth noticing is that `EXEMPT` refuses a stale path while this list can lapse by
**omission**, which nothing detects. Plausible near misses here: `.xml`, `.wxs`,
`.nsi` and `.nsh` (the MSI and NSIS bundles), `.rst`, `.mdx`, and any
extensionless file not in `TEXT_NAMES`. Add the suffix when the repository gains
the format.

**The second half of that guarantee is enforced rather than requested, and it was
not in the first version.** Being *below* the ceiling is a REFUSAL, so a sweep
cannot land without lowering the bound behind it. Two independent reviews found
the same hole the same day: the ceiling only moved when somebody remembered to run
`--write-baseline`, and a reviewer's probe swept seventeen characters, left the
baseline alone, and spent all seventeen again in a brand-new file while this check
printed *No regression*. A guarantee that rests on a person remembering a step
fifteen times is not one.

Where the one-way property actually lives, because the first version got this
wrong
-------------------------------------------------------------------------------

The ceiling is a committed JSON file, so "the baseline only goes down" cannot be
enforced by the code that writes it. The first version guarded only
`--write-baseline`, which left the ceiling itself editable by hand: setting it to
1500 and adding two dashes passed, and the script congratulated the author on the
313 it had apparently gained. An independent review found it by trying.

So the committed baseline is compared against the baseline on the reference ref
(`--against`, `origin/main` in CI). A pull request that raises it is refused
whatever it did to the file. On the reference ref itself the comparison is with
itself and passes, which is correct: the check exists for changes.

Why it does not rewrite anything
--------------------------------

A dash has no single correct replacement. It becomes a comma, a colon, a full
stop or a pair of parentheses depending on the clause after it, and substituting
commas everywhere produces run-on sentences joined by commas, which reads worse
than the dash did. This counts. A person edits.

Load-bearing characters
-----------------------

Some of these characters are data rather than punctuation: a parsed string, a
path, an identifier, a fixture another test compares against. Those go in EXEMPT
with a reason, and an entry whose reason is blank is REFUSED rather than
honoured, so the list cannot fill up with silent exemptions. An entry naming a
path that no longer exists is refused too, because a stale exemption is one
nobody notices has stopped applying.

**An exemption does not lower the count, and that is a correction.** The first
version skipped exempt paths before counting, so adding one entry removed its
file's dashes from the total and the script then congratulated the author on the
ground it had apparently gained and invited them to bank it with
`--write-baseline`. A second review found it: one entry for `placement.rs` was
worth 172. The ceiling is now measured over every text file, exempt ones
included, so an exemption cannot move the number by one. What it changes is the
**floor**: the total can never fall below the number of load-bearing characters,
and the exemption list is the record of where that floor comes from. Exempt
paths are also left out of the "worst offenders" list a failure prints, because
naming a file nobody may edit is noise.

Fail closed on an unreachable reference
---------------------------------------

`--against` needs a ref. The first version treated "no baseline there" and "no
ref there" as one case and warned rather than refusing, so a CI run whose fetch
had failed passed green with the anti-tamper guard switched off and nothing in
the log a reader would stop at. Those are now two cases. **A ref that cannot be
resolved is a refusal**, because that is a broken checkout and the guard is not
running. **A resolvable ref that carries no baseline is the first run**, which is
a real state exactly once, and it is allowed with a notice. Once this lands on
`main` the second case stops occurring, with no flag to remove and no ratchet
left switched off behind one.

**"Stops occurring" is not "cannot recur", and a review caught the difference.**
`reference is None` is evaluated on every run, so if `main` ever loses its
baseline the anti-tamper comparison silently switches off again and any ceiling
is accepted. What keeps that narrow is the refusal above it: a change that deletes
the baseline is refused by the `not baseline_file.exists()` branch before it can
merge, so `main` cannot reach that state through this check. It is a residual
rather than an open door, and it is written down rather than rounded up.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

# Written as escapes, not literals. `.py` is in the suffix list below, so a
# literal pair here would make this file the one place the rule is broken by the
# code enforcing it, and the only way out would be an EXEMPT entry for the
# checker itself.
DASHES = (chr(0x2014), chr(0x2013))  # em, en

# Extensions carrying prose a reader can reach. An allow list rather than a deny
# list, so a lockfile or a binary is excluded by being absent, and a new text
# format is opted in deliberately. `.py` is here so the checker checks itself,
# which the first version did not.
TEXT_SUFFIXES = {
    ".cjs",
    ".css",
    ".html",
    ".js",
    ".json",
    ".md",
    ".mjs",
    ".ps1",
    ".py",
    ".rs",
    ".scss",
    ".sh",
    ".svelte",
    ".svg",
    ".toml",
    ".ts",
    ".tsx",
    ".txt",
    ".yaml",
    ".yml",
}

# Text files with no suffix at all, which the set above cannot reach.
TEXT_NAMES = {".editorconfig", ".gitattributes", ".gitignore", "LICENSE"}

# Paths whose dashes are data, each with the reason it is data. A blank reason is
# refused; see `check_exemptions`.
EXEMPT: dict[str, str] = {}

BASELINE_PATH = "scripts/dash-baseline.json"


def repo_root() -> Path:
    """The repository root, so the answer does not depend on the caller's cwd.

    The first version ran `git ls-files` and opened the results relative to the
    process working directory. From `scripts/` that counted **zero** dashes and
    `--write-baseline` recorded a ceiling of zero without refusing, because zero
    is lower than the ceiling and the only guard there was pointed the other way.
    Every later run then failed permanently, and the fix an operator reaches for
    is to hand-edit the ceiling. Found by the independent review of this script.
    """
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        check=True,
        text=True,
    ).stdout.strip()
    return Path(out)


def is_text(path: str) -> bool:
    name = Path(path).name
    return Path(path).suffix in TEXT_SUFFIXES or name in TEXT_NAMES


def tracked_files(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        capture_output=True,
        check=True,
    ).stdout
    return [p for p in out.decode("utf-8").split("\0") if p]


def check_exemptions(root: Path, tracked: set[str]) -> list[str]:
    """Refusals for the exemption list. Empty list means it is well formed."""
    problems = []
    for path, reason in EXEMPT.items():
        if not reason.strip():
            problems.append(
                f"EXEMPT['{path}'] has no reason. An exemption without one is a "
                f"silent hole; say what the character is doing there."
            )
        if path not in tracked:
            problems.append(
                f"EXEMPT['{path}'] names a path that is not tracked. A stale "
                f"exemption is one nobody notices has stopped applying."
            )
    return problems


def count(root: Path, paths: list[str]) -> tuple[dict[str, int], list[str]]:
    """Per-file counts over every text file, exempt paths included.

    Exempt paths are counted deliberately. Skipping them here is what let an
    exemption lower the ceiling and be reported as ground gained; see the module
    docstring. `EXEMPT` decides what a failure lists, not what the total is.
    """
    counts: dict[str, int] = {}
    unreadable: list[str] = []
    for path in paths:
        if not is_text(path):
            continue
        try:
            text = (root / path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            # NOT skipped silently, and this is the fix rather than the message.
            # The first version printed a warning to stderr and returned 0, so a
            # tracked file that had gone missing or turned invalid lowered the
            # count and the run stayed green. A file this check cannot read is a
            # file it cannot vouch for, and the honest answer is a refusal.
            unreadable.append(f"{path}: {type(error).__name__}")
            continue
        n = sum(text.count(d) for d in DASHES)
        if n:
            counts[path] = n
    return counts, unreadable


def read_baseline(raw: str, where: str) -> int:
    """Parses a baseline, turning every malformed shape into one refusal."""
    try:
        value = json.loads(raw)["total"]
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise SystemExit(
            f"REFUSED: the baseline at {where} is not readable "
            f"({type(error).__name__}). It is the ceiling this check enforces, "
            f"so an unreadable one fails closed."
        ) from error
    # `bool` is a subclass of `int`, so a bare isinstance check accepts `true`
    # as a ceiling of 1 and the run then goes red naming the wrong problem.
    # Found by review.
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise SystemExit(f"REFUSED: the baseline at {where} is not a count: {value!r}")
    return value


def ref_exists(root: Path, ref: str) -> bool:
    """Whether `ref` resolves in this repository."""
    return (
        subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"],
            capture_output=True,
        ).returncode
        == 0
    )


def baseline_on(root: Path, ref: str) -> int | None:
    """The recorded ceiling on `ref`, or None when `ref` carries no baseline.

    Callers must ask `ref_exists` first. This returning None means the file is
    absent on a ref that does exist, which is the first-run case and nothing
    else; an unresolvable ref is a broken checkout and is refused by the caller.
    Collapsing the two is what made the anti-tamper guard fail open.
    """
    result = subprocess.run(
        ["git", "-C", str(root), "show", f"{ref}:{BASELINE_PATH}"],
        capture_output=True,
    )
    if result.returncode != 0:
        return None
    return read_baseline(result.stdout.decode("utf-8"), f"{ref}:{BASELINE_PATH}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="record the current count as the new ceiling; only ever lowers it",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="print the per-file counts, worst first",
    )
    parser.add_argument(
        "--against",
        default="origin/main",
        help=(
            "ref whose baseline this one may not exceed. This is what stops the "
            "ceiling being raised by hand. Unreachable refs are reported, not "
            "assumed away."
        ),
    )
    args = parser.parse_args()

    root = repo_root()
    tracked = tracked_files(root)

    problems = check_exemptions(root, set(tracked))
    if problems:
        for p in problems:
            print(f"REFUSED: {p}", file=sys.stderr)
        return 1

    counts, unreadable = count(root, tracked)
    if unreadable:
        print(
            "REFUSED: tracked files this check could not read. A file it cannot "
            "read is one it cannot vouch for, and skipping them silently lowers "
            "the count:",
            file=sys.stderr,
        )
        for u in unreadable:
            print(f"  {u}", file=sys.stderr)
        return 1

    total = sum(counts.values())
    exempt_total = sum(n for path, n in counts.items() if path in EXEMPT)
    actionable = {path: n for path, n in counts.items() if path not in EXEMPT}

    if args.list:
        for path, n in sorted(counts.items(), key=lambda kv: -kv[1]):
            mark = "  (exempt)" if path in EXEMPT else ""
            print(f"{n:6d}  {path}{mark}")
        print(f"\n{total} in {len(counts)} file(s)")
        if exempt_total:
            print(f"{exempt_total} of them exempt, which is the floor this can reach")

    baseline_file = root / BASELINE_PATH

    # Two different failures, and merging them is what switched the guard off.
    # A ref that does not resolve is a broken checkout: in CI that is a fetch
    # that failed, and continuing would run the ratchet with its anti-tamper
    # comparison silently disabled.
    #
    # `--write-baseline` is exempt, and that is a fix rather than a carve-out.
    # It reads only the committed file and never `reference`, so refusing it on
    # an unresolvable ref blocked a contributor whose clone calls the remote
    # something else from banking the sweep this very check had just ordered
    # them to bank, with a message about fixing their checkout. Friction in the
    # attended workflow that bought no safety, which is what `P-5` forbids.
    if not args.write_baseline and not ref_exists(root, args.against):
        print(
            f"REFUSED: {args.against} does not resolve, so the committed ceiling "
            f"cannot be compared against anything and the guard that stops it "
            f"being raised by hand is not running. In CI this is a failed fetch. "
            f"Fix the checkout rather than the number.",
            file=sys.stderr,
        )
        return 1

    reference = baseline_on(root, args.against)

    if args.write_baseline:
        if baseline_file.exists():
            current = read_baseline(baseline_file.read_text(encoding="utf-8"), BASELINE_PATH)
            if total > current:
                print(
                    f"REFUSED: {total} is above the recorded baseline of {current}. "
                    f"The ratchet only turns one way; fix the regression rather "
                    f"than raising the ceiling.",
                    file=sys.stderr,
                )
                return 1
        baseline_file.write_text(
            json.dumps(
                {
                    "total": total,
                    "files": len(counts),
                    # Recorded so the floor is visible in the diff. An exemption
                    # cannot change `total`, but it can change what the total is
                    # allowed to reach, and that belongs in review rather than in
                    # one line of a Python dict nobody opens.
                    "exempt_total": exempt_total,
                    "exempt_paths": sorted(EXEMPT),
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
            # KEEP THIS ARGUMENT. Without it, Python's text mode turns every
            # `\n` into `\r\n` on Windows, silently converting this whole file
            # from LF to CRLF on every `--write-baseline`. `.gitattributes` then
            # normalizes it back at staging, so `git diff` shows only the count
            # line and nothing in the commit path can see the damage. Observed
            # here on 2026-08-21: the file was 6 LF at `HEAD` and 6 CRLF in the
            # working tree straight after a write. UP-TAKE `I-68` is the
            # workspace-level record of the hazard, and
            # `test_the_written_baseline_keeps_LF_line_endings` is the guard, so
            # deleting this line now goes red instead of going unnoticed.
            #
            # The first version of this comment began "`newline="\n"` or Python's
            # text mode turns every `\n` into `\r\n`", which reads as blaming the
            # argument for the damage it prevents -- an instruction to delete it.
            newline="\n",
        )
        print(f"baseline written: {total} in {len(counts)} file(s)")
        if exempt_total:
            print(f"  {exempt_total} exempt, across {len(EXEMPT)} path(s): the floor")
        return 0

    if not baseline_file.exists():
        print(
            "REFUSED: no baseline recorded. Run --write-baseline once, and "
            "commit the result.",
            file=sys.stderr,
        )
        return 1

    ceiling = read_baseline(baseline_file.read_text(encoding="utf-8"), BASELINE_PATH)

    # The guard the first version did not have. Without it the ceiling is a
    # hand-editable number in a committed file and the ratchet turns both ways.
    if reference is None:
        # `args.against` resolved and carries no baseline, so this is the change
        # that introduces one. True exactly once per repository, and it stops
        # being true the moment this lands, with no flag left behind to remove.
        print(
            f"NOTICE: {args.against} carries no baseline, so this is the first "
            f"run and there is nothing to compare the ceiling against. The "
            f"anti-tamper comparison starts working on the next change.",
            file=sys.stderr,
        )
    elif ceiling > reference:
        print(
            f"REFUSED: the committed ceiling is {ceiling}, above {args.against}'s "
            f"{reference}. A ratchet turns one way. Lower the count, do not raise "
            f"the bound.",
            file=sys.stderr,
        )
        return 1

    if total > ceiling:
        print(
            f"FAILED: {total} em/en dashes against a ceiling of {ceiling}.\n"
            f"This repository is public and its source is public writing; see the "
            f"style section of CONTRIBUTING.md.\n"
            f"Use a comma, a colon, a full stop or parentheses.\n",
            file=sys.stderr,
        )
        # Exempt paths are left out: they are counted in the total and nobody may
        # edit them, so listing them as offenders sends a reader to the one place
        # the answer is "leave it alone".
        for path, n in sorted(actionable.items(), key=lambda kv: -kv[1])[:10]:
            print(f"  {n:6d}  {path}", file=sys.stderr)
        if exempt_total:
            print(
                f"  ({exempt_total} of the {total} are exempt and are not listed; "
                f"they are the floor, not work.)",
                file=sys.stderr,
            )
        return 1

    if total < ceiling:
        # REFUSED rather than a nudge, and this is the correction that makes
        # "every tranche is permanent" true. Two independent reviews found the
        # same hole on the same day: the ceiling only moved when somebody
        # remembered to run --write-baseline, so a swept tranche that was not
        # banked stayed as headroom, and the next change could spend it in a
        # brand-new file while the check printed "No regression". Seventeen
        # characters were handed back that way in a reviewer's probe.
        #
        # The obligation was prose in CONTRIBUTING.md and in the workspace
        # backlog row, which is the "a rule an agent has to remember" class.
        # Being below the ceiling is now the failure, so the ratchet is always
        # at its stop and the ground cannot be given back.
        print(
            f"REFUSED: {total} against a ceiling of {ceiling}, which is "
            f"{ceiling - total} fewer. Good, and it is not banked yet: the "
            f"ceiling has to come down with it or the next change can spend "
            f"the difference. Run this and commit the result:\n"
            f"    python scripts/dash-ratchet.py --write-baseline\n",
            file=sys.stderr,
        )
        return 1

    print(f"{total} em/en dashes, at the ceiling of {ceiling}. No regression.")
    if exempt_total:
        print(f"{exempt_total} of them are exempt: the floor this can reach.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
