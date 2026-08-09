#!/usr/bin/env python3
"""Counts em-dashes and en-dashes in tracked files and refuses an increase.

This repository is public, so its source is public writing: comments, doc
comments and operator-visible strings are all read by strangers deciding whether
this is serious work. The workspace rule is `P-1` in
`WORKFLOW/PREFERENCES.md` of the private planning repository, extended to source
on 2026-08-09 by the founder.

Why a ratchet and not a gate
----------------------------

On the day the rule landed this repository held 1,187 of these characters across
46 files. A check that failed the build on day one is a check somebody disables,
and a disabled check reports nothing forever. So the baseline is recorded and may
only go **down**: new writing is bound immediately, the backlog is worked off in
tranches, and every tranche tightens the bound behind it.

Why it does not rewrite anything
--------------------------------

A dash has no single correct replacement. It becomes a comma, a colon, a full
stop or a pair of parentheses depending on the clause after it, and substituting
commas everywhere manufactures comma splices, which the same rule set bans in the
next breath. This counts. A person edits.

Load-bearing characters
-----------------------

Some of these characters are data rather than punctuation: a parsed string, a
path, an identifier, a fixture another test compares against. Those are exempt by
listing the file in EXEMPT below with the reason, not by weakening the count.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

DASHES = ("—", "–")  # em, en

# Extensions that carry prose a reader can reach. Lockfiles, binaries and
# vendored trees are excluded by being absent rather than by a deny list, so a
# new file type is opted in deliberately.
TEXT_SUFFIXES = {
    ".rs",
    ".ts",
    ".tsx",
    ".js",
    ".svelte",
    ".md",
    ".toml",
    ".yml",
    ".yaml",
    ".json",
    ".html",
    ".css",
}

# Paths whose dashes are data. Each needs a reason; an entry without one is a
# silent exemption and this script refuses to carry those.
EXEMPT: dict[str, str] = {}

BASELINE = Path(__file__).with_name("dash-baseline.json")


def tracked_files() -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z"],
        capture_output=True,
        check=True,
    ).stdout
    return [p for p in out.decode("utf-8").split("\0") if p]


def count(paths: list[str]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for path in paths:
        if Path(path).suffix not in TEXT_SUFFIXES or path in EXEMPT:
            continue
        try:
            text = Path(path).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            # A file we cannot read as UTF-8 carries no prose we can check. Said
            # out loud rather than skipped silently, because "0 findings" and
            # "0 files read" look identical in a CI log otherwise.
            print(f"  (unreadable as UTF-8, not counted: {path})", file=sys.stderr)
            continue
        n = sum(text.count(d) for d in DASHES)
        if n:
            counts[path] = n
    return counts


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
    args = parser.parse_args()

    counts = count(tracked_files())
    total = sum(counts.values())

    if args.list:
        for path, n in sorted(counts.items(), key=lambda kv: -kv[1]):
            print(f"{n:6d}  {path}")
        print(f"\n{total} in {len(counts)} file(s)")

    if args.write_baseline:
        previous = json.loads(BASELINE.read_text()) if BASELINE.exists() else None
        if previous is not None and total > previous["total"]:
            print(
                f"REFUSED: {total} is above the recorded baseline of "
                f"{previous['total']}. The ratchet only turns one way; fix the "
                f"regression rather than raising the ceiling.",
                file=sys.stderr,
            )
            return 1
        BASELINE.write_text(
            json.dumps({"total": total, "files": len(counts)}, indent=2) + "\n",
            encoding="utf-8",
        )
        print(f"baseline written: {total} in {len(counts)} file(s)")
        return 0

    if not BASELINE.exists():
        print(
            "REFUSED: no baseline recorded. Run --write-baseline once, and "
            "commit the result.",
            file=sys.stderr,
        )
        return 1

    ceiling = json.loads(BASELINE.read_text())["total"]
    if total > ceiling:
        print(
            f"FAILED: {total} em/en dashes against a ceiling of {ceiling}.\n"
            f"This repository is public and its source is public writing "
            f"(P-1, extended to source 2026-08-09).\n"
            f"Use a comma, a colon, a full stop or parentheses. Do NOT raise "
            f"the ceiling.\n",
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
