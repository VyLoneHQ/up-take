#!/usr/bin/env python3
"""Counts em-dashes and en-dashes in tracked files and refuses an increase.

This repository is public, so its source is public writing: comments, doc
comments and operator-visible strings are all read by strangers deciding whether
this is serious work. `CONTRIBUTING.md` states the rule for contributors, which
is the copy an outside reader can actually open.

Why a ratchet and not a gate
----------------------------

On the day the rule landed this repository held 1,187 of these characters across
46 files. A check that failed the build on day one is a check somebody disables,
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
    counts: dict[str, int] = {}
    unreadable: list[str] = []
    for path in paths:
        if not is_text(path) or path in EXEMPT:
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
    if not isinstance(value, int) or value < 0:
        raise SystemExit(f"REFUSED: the baseline at {where} is not a count: {value!r}")
    return value


def baseline_on(root: Path, ref: str) -> int | None:
    """The recorded ceiling on `ref`, or None when it is unreachable."""
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

    if args.list:
        for path, n in sorted(counts.items(), key=lambda kv: -kv[1]):
            print(f"{n:6d}  {path}")
        print(f"\n{total} in {len(counts)} file(s)")

    baseline_file = root / BASELINE_PATH
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
            json.dumps({"total": total, "files": len(counts)}, indent=2) + "\n",
            encoding="utf-8",
        )
        print(f"baseline written: {total} in {len(counts)} file(s)")
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
        print(
            f"WARNING: {args.against} carries no readable baseline, so this run "
            f"cannot tell whether the ceiling was raised. In CI that means the "
            f"ref was not fetched. Not failing on it, because an unreachable ref "
            f"is also what a first-ever run looks like.",
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
        for path, n in sorted(counts.items(), key=lambda kv: -kv[1])[:10]:
            print(f"  {n:6d}  {path}", file=sys.stderr)
        return 1

    if total < ceiling:
        print(
            f"{total} against a ceiling of {ceiling}. "
            f"{ceiling - total} fewer than the baseline: run "
            f"`python scripts/dash-ratchet.py --write-baseline` and commit it, "
            f"so the ground you gained is held."
        )
        return 0

    print(f"{total} em/en dashes, at the ceiling of {ceiling}. No regression.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
