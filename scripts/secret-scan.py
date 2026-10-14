#!/usr/bin/env python3
"""Refuses a secret in the tracked files, and in every commit of a range.

This repository is public. A secret pushed here is public from the moment the
push lands, and deleting it in a later commit does not take it back: the commit
that added it stays readable on GitHub, in the pull request and by its sha. So
this check reads two things.

1. **The tree.** Every tracked file, as it stands in the checkout. On a pull
   request that is the merge result, which is what would land on `main`.
   Each file is read twice: as detect-secrets parses it, and line by line as
   written, because the parsed read of a YAML file never sees its comments
   (`scan_tree` has the details).
2. **The range** (`--range BASE..HEAD`). The lines each commit in the range
   added, one commit at a time. This is what catches a secret that a branch
   added and then removed: the tree is clean, and the commit is still public.
   Merge commits are skipped, because the lines they bring in were added by
   commits that are read on their own (or are already on `main`).

A finding is refused unless `.secrets.baseline` lists it. The baseline is
detect-secrets' own file, and it holds two things: the settings (which
detectors run, and which lines and files are passed over), and the findings
that a person has read and judged not to be secrets. A finding matches its
baseline entry by file, type and a hash of the value, never by line number, so
moving a line does not break the match.

What a finding prints is the file, the line, the detector and, for a range, the
commit. **Never the value.** The log of a public repository's CI run is public
too, and a check that printed what it found would publish it a second time.

Exit codes: `0` nothing new; `1` at least one finding the baseline does not
list; `2` the check could not run (no baseline, a git error, nothing to scan).
`2` is not a pass, and a CI step treats it as a failure like `1`.

CI can only find a secret that is already public. The same check runs before
that, on the machine that makes the commit, through `.pre-commit-config.yaml`:
`--staged` before each commit, and `--unpushed` before each push, which also
reads commits made without the first hook (`--no-verify`, a rebase, a merge).
`pre-commit install` once per clone sets up both.

Usage:
    python3 scripts/secret-scan.py                      # the tree
    python3 scripts/secret-scan.py --range A..B          # the tree and A..B
    python3 scripts/secret-scan.py --staged              # the commit being made
    python3 scripts/secret-scan.py --unpushed            # commits no remote has
    python3 scripts/secret-scan.py --write-baseline      # rewrite the findings

A false positive
----------------

Read the line first. If it is not a secret, either add
`# pragma: allowlist secret` (or the comment syntax of the file) at the end of
the line, or run `--write-baseline` and set `"is_secret": false` on the new
entry. The pragma is the only way to pass a line that is in a commit of the
range but no longer in the tree, because the baseline lists the tree.

**If it is a secret, it is already public.** Revoke it first, then remove it.

Why a script and not `detect-secrets-hook`
------------------------------------------

The hook reads the tree only, and it rewrites the baseline when line numbers
move, which fails a CI run that changed nothing. And `detect-secrets scan` on
Windows writes the baseline's file names with backslashes, which never match
the forward slashes the Linux runner reads. This script passes detect-secrets
the names `git ls-files` prints, so the baseline is the same on every machine.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

try:
    from detect_secrets.core import baseline, scan
    from detect_secrets.core.secrets_collection import SecretsCollection
    from detect_secrets.settings import get_settings
except ImportError:  # pragma: no cover
    print(
        "secret-scan: detect-secrets is not installed. "
        "pip install detect-secrets==1.5.0 unidiff==0.7.5",
        file=sys.stderr,
    )
    sys.exit(2)

BASELINE = ".secrets.baseline"
INVALID_FILE_FILTER = "detect_secrets.filters.common.is_invalid_file"


class CannotRun(Exception):
    """The check could not run. Exit 2, never a pass."""


def git(*args: str, stdin: bytes | None = None) -> str:
    result = subprocess.run(
        ["git", "-c", "core.quotepath=off", *args],
        input=stdin,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        message = result.stderr.decode("utf-8", errors="replace").strip()
        raise CannotRun(f"git {' '.join(args)} failed: {message}")
    return result.stdout.decode("utf-8", errors="replace")


def tracked_files() -> list[str]:
    return [name for name in git("ls-files", "-z").split("\0") if name]


def load_baseline(path: Path) -> SecretsCollection:
    """Reads the baseline, and makes its settings the ones every scan uses."""
    if not path.is_file():
        raise CannotRun(f"no baseline at {path}")
    loaded = baseline.load(baseline.load_from_file(str(path)), filename=str(path))
    # detect-secrets turns each name into the local form as it loads, so on
    # Windows `a/b.rs` comes back as `a\b.rs` and matches nothing git lists.
    # Back to git's form, on the key and on each finding, which compares by
    # its own copy of the name.
    known = SecretsCollection()
    for filename, secret in loaded:
        secret.filename = filename.replace("\\", "/")
        known[secret.filename].add(secret)
    return known


DIFF_OPTIONS = ("--no-color", "--no-ext-diff", "--no-textconv", "--no-renames", "--unified=0")


def scan_diff(diff: str) -> SecretsCollection:
    found = SecretsCollection()
    if diff.strip():
        found.scan_diff(diff)
    return found


def scan_tree(files: list[str]) -> SecretsCollection:
    """Every tracked file, read two ways, under the names git gives them.

    detect-secrets reads a file it can parse (YAML, INI) as parsed values, so
    a comment there is never read: a key pasted into a comment of a workflow
    file passes. A commit's diff is read line by line as written, comments
    included. So the tree is read both ways. One reason is the comment. The
    other is the baseline: a finding only the line as written shows would
    turn every commit in a range red, and the baseline, written from the tree,
    could never list it.

    The second way is the diff of the working tree against the empty tree:
    every tracked line as an added line, through the same code a commit's
    diff takes.
    """
    found = SecretsCollection()
    for name in files:
        for secret in scan.scan_file(name):
            found[name].add(secret)
    empty_tree = git("hash-object", "-t", "tree", "--stdin", stdin=b"").strip()
    for filename, secret in scan_diff(git("diff", *DIFF_OPTIONS, empty_tree, "--")):
        found[filename].add(secret)
    return found


def scan_commit(commit: str) -> SecretsCollection:
    """The lines one commit added, against its first parent."""
    return scan_diff(git("show", "--format=", *DIFF_OPTIONS, commit))


def commits_in(range_spec: str) -> list[str]:
    if ".." not in range_spec:
        raise CannotRun(f"--range wants BASE..HEAD, not {range_spec!r}")
    listed = git("rev-list", "--no-merges", "--reverse", range_spec)
    return listed.split()


def report(found: SecretsCollection, where: str) -> int:
    """Prints each finding without its value. Returns how many."""
    count = 0
    for filename, secret in found:
        line = secret.line_number or "?"
        print(f"  {where}{filename}:{line}  {secret.type}")
        count += 1
    return count


def scan_staged() -> tuple[SecretsCollection, int]:
    """What the commit being made would add: the staged files as parsed, and
    the staged lines as written. Under pre-commit the working tree holds the
    staged content, because unstaged changes are set aside while hooks run."""
    names = [n for n in git("diff", "--cached", "--name-only", "--diff-filter=AM", "-z").split("\0") if n]
    found = SecretsCollection()
    for name in names:
        for secret in scan.scan_file(name):
            found[name].add(secret)
    for filename, secret in scan_diff(git("diff", "--cached", *DIFF_OPTIONS)):
        found[filename].add(secret)
    return found, len(names)


def unpushed_commits() -> list[str]:
    """The commits about to be pushed that no remote has yet. pre-commit names
    the commit being pushed in `PRE_COMMIT_TO_REF`; outside it, `HEAD`."""
    head = os.environ.get("PRE_COMMIT_TO_REF") or "HEAD"
    return git("rev-list", "--no-merges", "--reverse", head, "--not", "--remotes").split()


ADVICE = {
    "ci": "If one is a real secret it is already public: revoke it first.",
    "staged": "Nothing is public yet: take the value out before you commit.",
    "unpushed": (
        "Nothing is public yet: take the value out of the commit that added it "
        "(a later commit that deletes it is not enough, the push carries both)."
    ),
}


def check(
    baseline_path: Path,
    range_spec: str | None,
    staged: bool = False,
    unpushed: bool = False,
) -> int:
    known = load_baseline(baseline_path)
    total = 0
    if staged:
        found, count = scan_staged()
        scope = f"{count} staged files"
        new = found - known
        if new:
            print("Secrets in the staged changes that the baseline does not list:")
            total += report(new, "")
    elif unpushed:
        scope = ""
    else:
        files = tracked_files()
        if not files:
            raise CannotRun("git ls-files printed nothing, so there is nothing to scan")
        scope = f"{len(files)} tracked files"
        new = scan_tree(files) - known
        if new:
            print("Secrets in the tracked files that the baseline does not list:")
            total += report(new, "")

    if unpushed:
        commits = unpushed_commits()
        scope = f"{len(commits)} commits no remote has"
    else:
        commits = commits_in(range_spec) if range_spec else []
        if range_spec:
            scope += f" and {len(commits)} commits in {range_spec}"
    # detect-secrets passes over any name that is not a file on disk, and it
    # applies that to a diff too. A file a later commit deleted is not on disk,
    # so the commit that added a secret to it passed: the very case the range
    # exists for. Found by a drill on GitHub, after every local test was green.
    # Off for the commits only; the tree is read before this, and every name
    # there is a tracked file.
    get_settings().disable_filters(INVALID_FILE_FILTER)
    for commit in commits:
        new = scan_commit(commit) - known
        if new:
            print(f"Secrets added by commit {commit[:12]} that the baseline does not list:")
            total += report(new, f"{commit[:12]} ")

    if total:
        advice = ADVICE["staged" if staged else "unpushed" if unpushed else "ci"]
        print(
            f"\nsecret-scan: {total} finding(s) in {scope}. {advice} "
            "If it is not a secret, see the docstring of scripts/secret-scan.py."
        )
        return 1
    print(f"secret-scan: nothing new in {scope}.")
    return 0


def write_baseline(baseline_path: Path) -> int:
    """Rewrites the findings from the tree, keeping the settings and every
    judgement (`is_secret`) already recorded."""
    old = load_baseline(baseline_path)
    files = tracked_files()
    if not files:
        raise CannotRun("git ls-files printed nothing, so there is nothing to scan")
    new = scan_tree(files)
    new.merge(old)
    baseline.save_to_file(new, str(baseline_path))
    count = sum(1 for _ in new)
    print(f"secret-scan: wrote {count} finding(s) from {len(files)} tracked files to {baseline_path}.")
    return 0


def main(argv: list[str] | None = None) -> int:
    # detect-secrets opens each file with no encoding named, so Python's
    # locale decides. On Windows that is cp1252, and a UTF-8 file with a
    # character cp1252 cannot decode is skipped as binary: read by CI, passed
    # unread on a developer's machine. UTF-8 mode makes the two agree. A child
    # process and not `os.execv`, whose exit code on Windows is the parent's.
    if os.name == "nt" and not sys.flags.utf8_mode:
        args = sys.argv[1:] if argv is None else argv
        child = [sys.executable, "-X", "utf8", str(Path(__file__).resolve()), *args]
        return subprocess.run(child, check=False).returncode

    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--range", dest="range_spec", help="the tree, and each commit in BASE..HEAD")
    modes.add_argument("--staged", action="store_true", help="what the commit being made adds (pre-commit)")
    modes.add_argument("--unpushed", action="store_true", help="each commit no remote has (pre-push)")
    modes.add_argument("--write-baseline", action="store_true", help="rewrite the findings in the baseline")
    parser.add_argument("--baseline", default=BASELINE, help="default: .secrets.baseline")
    args = parser.parse_args(argv)
    try:
        os.chdir(git("rev-parse", "--show-toplevel").strip())
        path = Path(args.baseline)
        if args.write_baseline:
            return write_baseline(path)
        return check(path, args.range_spec, staged=args.staged, unpushed=args.unpushed)
    except CannotRun as error:
        print(f"secret-scan: could not run: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
