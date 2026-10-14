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
   A merge commit is read for what the merge itself wrote, its conflict
   resolutions and anything added before it was committed (`git show
   --remerge-diff`): the lines it brings in from either side were added by
   commits read on their own. An octopus merge is read against its first
   parent, which reads more than it needs and misses nothing.

What the range does not do: it reads each commit's lines as written, never
parsed, so a YAML block scalar (the key on one line, the value on the next)
added and removed inside a range is not seen. The tree, and `--staged`, read
both ways.

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
`--staged` before each commit, and `--unpushed` before each push. The second
reads every commit of every local branch and tag that no remote has, because
one `git push` can carry several branches and pre-commit names only the first
to its hook. So it also catches a commit made without the first hook
(`--no-verify`, a rebase, a merge). A commit is taken as public once any remote
has it; a clone with a private second remote would need more than this.
`pre-commit install` once per clone sets up both.

In CI the workflow runs `--github`, which takes the range from the event
GitHub describes in the file named by `GITHUB_EVENT_PATH`: a pull request's
base to its head; a push's old head to its new one; and where there is no old
head (a new branch, a force push whose old head is gone) or no push at all (a
manual run), everything since the branch left `refs/remotes/origin/main`.

Usage:
    python3 scripts/secret-scan.py                      # the tree
    python3 scripts/secret-scan.py --range A..B          # the tree and A..B
    python3 scripts/secret-scan.py --github              # the tree and the event's range
    python3 scripts/secret-scan.py --staged              # the commit being made
    python3 scripts/secret-scan.py --unpushed            # commits no remote has
    python3 scripts/secret-scan.py --write-baseline      # rewrite the findings

A false positive
----------------

Read the line first. If it is not a secret, either add
`# pragma: allowlist secret` (or the comment syntax of the file) at the end of
the line, or run `--write-baseline` and set `"is_secret": false` on the new
entry. An entry judged that way stays in the baseline after its line is gone,
because the commit that added the line is still read. A line added and removed
inside a range before any baseline was written needs the pragma.

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
import json
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
    """The lines one commit added.

    An ordinary commit against its parent. A merge of two parents by
    `--remerge-diff`: against what git's own merge of the two would have
    written, so what shows is what the person merging wrote (a conflict
    resolution, a file added before the merge was committed) and not the lines
    either side brings, which their own commits carry. An octopus merge
    against its first parent, which reads everything the merge brings in:
    more than needed, never less. The first version skipped every merge, and
    a secret written into a merge passed CI and the push hook (review of
    `#140`, finding 1)."""
    parents = git("rev-list", "--parents", "-n", "1", commit).split()[1:]
    if len(parents) == 2:
        return scan_diff(git("show", "--format=", "--remerge-diff", *DIFF_OPTIONS, commit))
    if len(parents) > 2:
        return scan_diff(git("show", "--format=", "-m", "--first-parent", *DIFF_OPTIONS, commit))
    return scan_diff(git("show", "--format=", *DIFF_OPTIONS, commit))


def commits_in(range_spec: str) -> list[str]:
    if ".." not in range_spec:
        raise CannotRun(f"--range wants BASE..HEAD, not {range_spec!r}")
    return git("rev-list", "--reverse", range_spec).split()


ZERO_SHA = "0" * 40
MAIN = "refs/remotes/origin/main"


def since_main(head: str) -> str:
    """Everything on `head` that `main` does not have. The full ref name: git
    resolves a bare `origin/main` to a tag of that name first (review of
    `#140`, finding 4)."""
    git("rev-parse", "--verify", "--quiet", MAIN + "^{commit}")
    return git("merge-base", MAIN, head).strip() + ".." + head


def is_commit(sha: str) -> bool:
    done = subprocess.run(
        ["git", "cat-file", "-e", sha + "^{commit}"], capture_output=True, check=False
    )
    return done.returncode == 0


def github_range() -> str | None:
    """The range the GitHub event asks for, read from its own event file.

    `None` means there is nothing to read beyond the tree: a branch deleted."""
    name = os.environ.get("GITHUB_EVENT_NAME", "")
    path = os.environ.get("GITHUB_EVENT_PATH", "")
    if not name or not path:
        raise CannotRun("--github needs GITHUB_EVENT_NAME and GITHUB_EVENT_PATH")
    try:
        event = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise CannotRun(f"the event file {path} could not be read: {error}") from error
    try:
        if name == "pull_request":
            return f"{event['pull_request']['base']['sha']}..{event['pull_request']['head']['sha']}"
        if name == "push" and not event.get("deleted"):
            after = event["after"]
    except (KeyError, TypeError) as error:
        raise CannotRun(f"the {name} event has no {error}") from error
    if name == "push":
        if event.get("deleted"):
            return None
        before = event.get("before") or ZERO_SHA
        if before != ZERO_SHA and is_commit(before):
            return f"{before}..{after}"
        print(f"secret-scan: the push has no old head here ({before[:12]}), so the range is everything since main.")
        return since_main(after)
    return since_main("HEAD")


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
    names = [
        n
        for n in git("diff", "--cached", "--name-only", "--no-renames", "--diff-filter=AM", "-z").split("\0")
        if n
    ]
    found = SecretsCollection()
    for name in names:
        for secret in scan.scan_file(name):
            found[name].add(secret)
    for filename, secret in scan_diff(git("diff", "--cached", *DIFF_OPTIONS)):
        found[filename].add(secret)
    return found, len(names)


def unpushed_commits() -> list[str]:
    """Every commit of every local branch and tag, and of `HEAD`, that no
    remote has yet. Not only the commit being pushed: one `git push` can carry
    several branches, and pre-commit names only the first of them to its hook
    in `PRE_COMMIT_TO_REF`, so a secret on the second passed (review of
    `#140`, finding 2). That ref is still read, in case it is none of these."""
    heads = ["HEAD", "--branches", "--tags"]
    if os.environ.get("PRE_COMMIT_TO_REF"):
        heads.append(os.environ["PRE_COMMIT_TO_REF"])
    return git("rev-list", "--reverse", *heads, "--not", "--remotes").split()


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
    judgement (`is_secret`) already recorded.

    **A finding judged not a secret is kept after its line is gone.** The
    range reads commits, and a commit that added the line still adds it after
    a later one removes it: dropping the entry would turn every range that
    holds that commit red, the branch that removed the line included. A
    finding judged a secret, or never judged, is dropped when the tree no
    longer holds it."""
    old = load_baseline(baseline_path)
    files = tracked_files()
    if not files:
        raise CannotRun("git ls-files printed nothing, so there is nothing to scan")
    new = scan_tree(files)
    new.merge(old)
    for filename, secret in old:
        if secret.is_secret is False and secret not in new[filename]:
            new[filename].add(secret)
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
    modes.add_argument("--github", action="store_true", help="the tree, and the range of this GitHub event")
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
        range_spec = github_range() if args.github else args.range_spec
        return check(path, range_spec, staged=args.staged, unpushed=args.unpushed)
    except CannotRun as error:
        print(f"secret-scan: could not run: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
