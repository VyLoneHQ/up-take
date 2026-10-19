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
   (`scan_tree` has the details). A file that is not plain UTF-8 text is
   read more ways (`texts`): as UTF-16 where it looks like UTF-16, as bytes
   (Latin-1, one character per byte) whenever it is not UTF-8, and with every
   NUL removed whenever it holds one, so ASCII in it reads as written in any
   encoding, UTF-16 and UTF-32 included, broken or not. Of detect-secrets'
   filters, only
   two may drop a finding: the `pragma: allowlist secret` a person writes
   (on the line, or `pragma: allowlist nextline secret` on the line above),
   and the baseline's patterns for three exact line shapes (`KEEP_FILTERS`).
   The result counts the files git calls binary that no text decoding read,
   which were read as bytes only.
2. **The range** (`--range BASE..HEAD`). The lines each commit in the range
   added, one commit at a time. Its files only: commit and tag messages are
   as public and are not read (fourteenth review of `#140`, finding 4).
   This is what catches a secret that a branch
   added and then removed: the tree is clean, and the commit is still public.
   A merge commit is read for what the merge itself wrote, its conflict
   resolutions and anything added before it was committed (`git show
   --remerge-diff`): the lines it brings in from either side were added by
   commits read on their own. An octopus merge is read against its first
   parent, which reads more than it needs and misses nothing. A file git
   prints as binary (UTF-16, text `.gitattributes` marks binary, an image) is
   read whole from the commit, as text or, like the tree, as bytes.

Every read takes each file both ways: line by line as written, and as
detect-secrets parses it (both of its passes), so a YAML block scalar or an
unquoted `key = value` is seen in the tree, in a commit and staged alike.

A finding is refused unless `.secrets.baseline` lists it **and a person has
judged it** (`"is_secret": false`). The baseline is detect-secrets' own file,
and it holds two things: the settings (which detectors run, and which lines
are passed over), and the findings. A finding matches its baseline entry by
file, type and a hash of the value, never by line number, so moving a line
does not break the match. An entry nobody judged passes nothing, so running
`--write-baseline` cannot make a real secret pass.

What a finding prints is the file, the line, the detector and, for a range, the
commit. **Never the value.** The log of a public repository's CI run is public
too, and a check that printed what it found would publish it a second time.

Exit codes: `0` nothing new; `1` at least one finding the baseline does not
pass; `2` the check could not run (no baseline, a git error, nothing to scan,
or any other error, whose traceback is printed).
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

In CI the workflow runs `--github`, which takes the commits from the event
GitHub describes in the file named by `GITHUB_EVENT_PATH`: a pull request's
base to its head; for a push to main, the whole history of main; for a push to
any other branch or tag, and for a manual run, everything since it left
`refs/remotes/origin/main`. Never a push's old head to its new one, because a
push GitHub ran nothing for would then be in no range. A commit that has left
every branch (a force push, a deleted branch) is read by no run, though GitHub
still serves it by sha; the push hook is what reads it (`github_commits`).
A push whose last commit carries a skip marker starts no run, so its commits
are read by the next run that reaches them: a push to the same branch, a pull
request, a push to main, or the daily run. The daily run (`schedule`) reads
main as a push to main does, and every other branch and tag on the commits it
carries that main does not, each judged by main's baseline and its own
(`check_every_ref`). GitHub's documentation also describes no `pull_request`
run while a pull request has a merge conflict, and no push event when more
than three tags are pushed at once; neither is probed here.

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
entry. `--write-baseline` reads the tree and every commit of the history, so
a line that is gone can be listed too, and an entry judged not a secret stays
after its line is gone, because the commit that added it is still read.

**If CI found a secret, it is already public.** Revoke it first, then remove
it. Removing it from GitHub does not remove it from copies already taken,
mirrors included; revoking is the step that works everywhere. The commit that
added it stays in the history, and a push to main reads all of it, so once it
is revoked, run `--write-baseline` and set `"is_secret": false` on its entry:
a revoked value opens nothing, and that is the judgement the entry records
(tenth review of `#140`, finding 3).

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
import functools
import json
import os
import re
import subprocess
import sys
import tempfile
import traceback
from pathlib import Path, PurePosixPath

# Python puts this script's own folder first on the import path, so a file a
# pull request adds beside it (`scripts/unidiff/__init__.py`) was imported in
# place of the installed library, and could make every read find nothing
# (eleventh review of `#140`, finding 2). The folder holds no module this
# script needs, so it leaves the path before anything is imported from it.
_HERE = os.path.normcase(os.path.dirname(os.path.abspath(__file__)))
sys.path[:] = [p for p in sys.path if os.path.normcase(os.path.abspath(p or os.curdir)) != _HERE]

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
# The only detect-secrets filters allowed to drop a finding. Each is explicit:
# the `pragma: allowlist secret` comment a person writes on a line (or
# `pragma: allowlist nextline secret` on the line above it, which commits and
# the staged change honour only on the real line above: `scan_changes`), and
# the patterns in the baseline that match three exact line shapes (lockfile
# hashes and the baseline's own hashes, written for those files but applied
# to a line of that exact shape in any file: ninth review). Every other
# filter is off, including any a later detect-secrets adds, because each one dropped findings with
# nobody judging them, and the reviews of `#140` found them one at a time:
#
# - by file name, before a line is read: every `.lock` file, `.svg`, `.css`
#   and other formats taken for media (second review), any path holding
#   `swagger`, any file called `.secrets.baseline` (fourth), and any name
#   that is not a file on disk, which is every file a later commit deleted
#   (the drill on GitHub) and every name git quotes;
# - by a network answer: every match of a verifiable type (AWS, Slack,
#   Stripe, Telegram and others) was sent to the provider and dropped on any
#   answer but "valid", a rate limit included (fifth);
# - by heuristics on the line or the value: a line shaped like a call
#   (`personal_token("ghp_...")` in Rust, sixth review), a value after `id`,
#   inside `{...}` or `<...>`, after `$`, a sequential or UUID-shaped string.
#   They apply to every detector, the exact provider patterns included.
#
# Measured before turning the heuristics off: three new findings in the whole
# tree, all alphabet strings in tests, judged in the baseline.
KEEP_FILTERS = (
    "detect_secrets.filters.allowlist.is_line_allowlisted",
    "detect_secrets.filters.regex.should_exclude_line",
)


class CannotRun(Exception):
    """The check could not run. Exit 2, never a pass."""


def git_bytes(*args: str, stdin: bytes | None = None) -> bytes:
    result = subprocess.run(
        ["git", "-c", "core.quotepath=off", *args],
        input=stdin,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        message = result.stderr.decode("utf-8", errors="replace").strip()
        raise CannotRun(f"git {' '.join(args)} failed: {message}")
    return result.stdout


def git(*args: str, stdin: bytes | None = None) -> str:
    return git_bytes(*args, stdin=stdin).decode("utf-8", errors="replace")


def git_names(*args: str) -> str:
    """Output that names paths, decoded so each byte comes back: a name that
    is not UTF-8 keeps its bytes as surrogates instead of turning into U+FFFD,
    which named no file, so reading it failed and the run exited 2 (tenth
    review of `#140`, finding 2). A name goes back to a file through
    `entries` for a blob, and through the file system for the tree and the
    staged parse; nothing passes one to git as an argument."""
    return git_bytes(*args).decode("utf-8", errors="surrogateescape")


def shown(name: str) -> str:
    """A name as printed: a byte that is not UTF-8 as `\\xNN`, since a
    surrogate cannot be written to a strict stdout."""
    return name.encode("utf-8", errors="surrogateescape").decode("utf-8", errors="backslashreplace")


def tracked_files() -> list[str]:
    """The tracked names, keeping each byte (`git_names`): a name that is not
    UTF-8, decoded with U+FFFD, named no file on disk, and the tree read
    skipped it in silence (fifteenth review of `#140`, finding 2)."""
    return [name for name in git_names("ls-files", "-z").split("\0") if name]


def load_baseline(path: Path) -> SecretsCollection:
    """Reads the baseline, and makes its settings the ones every scan uses."""
    if not path.is_file():
        raise CannotRun(f"no baseline at {path}")
    loaded = baseline.load(baseline.load_from_file(str(path)), filename=str(path))
    # Every filter but the two KEEP_FILTERS is off (the reason is there).
    # `parsed` is only given files that exist.
    settings = get_settings()
    settings.disable_filters(*[f for f in list(settings.filters) if f not in KEEP_FILTERS])
    # detect-secrets turns each name into the local form as it loads, so on
    # Windows `a/b.rs` comes back as `a\b.rs` and matches nothing git lists.
    # Back to git's form, on the key and on each finding, which compares by
    # its own copy of the name.
    known = SecretsCollection()
    for filename, secret in loaded:
        secret.filename = filename.replace("\\", "/")
        known[secret.filename].add(secret)
    return known


# The prefixes are pinned: a diff's file names come from its `+++ b/...` lines,
# and `diff.mnemonicPrefix`, `diff.noprefix` or `diff.dstPrefix` in someone's
# git config would otherwise change them (third review of `#140`, finding 2).
DIFF_OPTIONS = (
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--no-renames",
    "--unified=0",
    "--src-prefix=a/",
    "--dst-prefix=b/",
)


def scan_diff(diff: str) -> SecretsCollection:
    found = SecretsCollection()
    if diff.strip():
        found.scan_diff(diff)
    return found


def decodings(data: bytes) -> list[str]:
    """Every way the bytes read as text; empty for a real binary. Each is
    scanned, so a guess between two encodings never decides what is read.

    Three kinds of text reach here. UTF-16, which neither read can see: Python
    cannot decode it as UTF-8, so detect-secrets drops the file as binary, and
    git finds NUL bytes in it and prints no lines. Windows PowerShell 5.1
    writes it by default (`>`, `Out-File`). A text file `.gitattributes` marks
    `binary`, such as `*.svg` here, whose diff git prints as `Bin`. And UTF-8
    text with NUL bytes in it, which git also calls binary (third review of
    `#140`, finding 1).
    UTF-16 is recognised by its byte-order mark, or, without one, by nearly
    every other byte being zero, which is what ASCII text in UTF-16 looks like
    and what no image in this repository does. UTF-8 with enough NULs passes
    that test too, so UTF-8 is always tried as well, and both are read (eighth
    review of `#140`, finding 1: such a file was read as UTF-16 garbage, or
    not at all)."""
    found = []
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):
        try:
            found.append(data.decode("utf-16"))
        except UnicodeDecodeError:
            pass
    if b"\x00" in data:
        for encoding, zeros in (("utf-16-le", data[1::2]), ("utf-16-be", data[0::2])):
            if zeros and zeros.count(0) >= 0.9 * len(zeros):
                try:
                    found.append(data.decode(encoding))
                except UnicodeDecodeError:
                    pass
    try:
        found.append(data.decode("utf-8"))
    except UnicodeDecodeError:
        pass
    return found


def texts(data: bytes) -> tuple[list[str], bool]:
    """Every way the bytes read as text, and whether none did.

    When none does, the bytes themselves as Latin-1, which maps each byte to
    one character, so an ASCII secret inside an image, a database or Latin-1
    text git calls binary is read as written. Without it a `.gitattributes`
    line marking a Latin-1 file binary turned red into green (fourteenth
    review of `#140`, finding 1).

    And whenever there is a NUL, the bytes with every NUL removed, as Latin-1:
    ASCII in UTF-16 or UTF-32 of any shape then reads as written. A UTF-16
    file cut to an odd length or holding one unpaired surrogate fails the
    strict decode, and UTF-32 decodes as UTF-16 into letters with NULs between
    them; both passed every read (fifteenth review of `#140`, finding 1)."""
    found = decodings(data)
    as_bytes = not found
    if b"\x00" in data:
        found.append(data.replace(b"\x00", b"").decode("latin-1"))
    # And whenever the bytes are not UTF-8, whatever else decoded them: a file
    # that opens with a UTF-16 byte-order mark and has no NUL decodes as
    # UTF-16 into CJK, so its ASCII reached no detector (sixteenth review of
    # `#140`, finding 1). With this and the NUL-stripped read, ASCII anywhere
    # in a file is read as written, as one byte per character or with NULs
    # between, whatever the file's encoding claims.
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        found.append(data.decode("latin-1"))
    return found, as_bytes


def scan_text(filename: str, text: str) -> SecretsCollection:
    """Every line of `text`, through the same code a commit's diff takes: as a
    diff that adds the whole file. Lines are split and numbered as git does,
    on `\n` alone.

    The diff names the file `file` and its extension, which is all detect-
    secrets reads of a name, and each finding takes the real name after. The
    real name in the header broke unidiff's split of it: a name holding
    ` b/`, a quoted name with a space, or a newline crashed the run
    (thirteenth review of `#140`, finding 1)."""
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    lines = [line.removesuffix("\r") for line in lines]
    found = SecretsCollection()
    if not lines:
        return found
    neutral = copy_name(filename)
    header = f"diff --git a/{neutral} b/{neutral}\n--- /dev/null\n+++ b/{neutral}\n"
    # The lines as written, and the same lines read leniently (`lenient`).
    for read in (lines, lenient(lines)):
        body = "".join("+" + line + "\n" for line in read)
        for _, secret in scan_diff(f"{header}@@ -0,0 +1,{len(read)} @@\n{body}"):
            secret.filename = filename
            found[filename].add(secret)
    return found


# `key = value`, `key: value`, `key=value` or `export key=value`, in a YAML
# list item or not, the key one word: the line shape INI, .env, .properties,
# TOML and YAML share. Groups: the indent, the key, the rest of the line.
PAIR = re.compile(r"^(\s*)(?:-\s+)?(?:export\s+)?([A-Za-z0-9_.\-]+)\s*[=:]\s*(.*?)\s*$")
# A YAML block scalar's key: `key: |` or `key: >`, in a list item or not.
BLOCK = re.compile(r"^(\s*)(?:-\s+)?([A-Za-z0-9_.\-]+)\s*:\s*[|>][-+0-9]*\s*(?:#.*)?$")


def _value_and_comment(rest: str) -> tuple[str, str]:
    """A value without its quotes, and the comment after it, kept to be
    written back after the quoted value, so a `pragma: allowlist secret` on
    the line still holds. A comment starts at whitespace and `#` or `;`
    (twentieth review of `#140`, finding 1: `token: V  # prod` was handed on
    as one value, which no detector's quoted-string pattern takes)."""
    if rest[:1] in ("'", '"'):
        close = rest.find(rest[0], 1)
        if close > 0:
            return rest[1:close], rest[close + 1 :]
        return rest[1:], ""
    parts = re.split(r"(?=\s+[#;])", rest, maxsplit=1)
    return parts[0], parts[1] if len(parts) > 1 else ""


def lenient(lines: list[str]) -> list[str]:
    """The lines as a parser would hand them to the detectors, without needing
    the parser to accept the file. Each `key = value` line becomes
    `key = "value"` (a trailing comment written back after it, a list item's
    `- ` dropped), each line of a YAML block scalar becomes `key: "line"`, and
    so does each indented line after a `key:` with no value of its own, which
    YAML reads as that key's value. Each comment stays as written (a `pragma:
    allowlist nextline secret` keeps its next line), and every other line is
    empty, so each keeps its number.

    detect-secrets quotes a value only through its YAML or INI parse, and
    falls back to the raw lines when that parse refuses the file: a `%` in a
    value, a repeated key or section, a character PyYAML calls non-printable,
    one syntax error anywhere, or an INI file with no `[section]` under a known
    extension such as `.env.example`. An unquoted value or a block scalar's
    value then reached no detector. Rounds 14 to 18 closed such triggers one
    at a time; this read does not depend on any of them (nineteenth review of
    `#140`, findings 1 and 2). The shapes of a value it reads are its limit:
    a value it does not recognise as one is read only as written (the
    twentieth review found three, now read)."""
    out = [""] * len(lines)
    # (indent, key, literal): a block scalar (literal: every deeper line is
    # its value, `#` included), or a key whose value is on the deeper lines.
    block: tuple[int, str, bool] | None = None
    for i, line in enumerate(lines):
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())
        if block is not None and stripped and indent <= block[0]:
            block = None
        if block is not None and block[2]:
            if stripped:
                out[i] = f'{block[1]}: "{stripped}"'
            continue
        if stripped.startswith(("#", ";", "//")):
            out[i] = line
            continue
        head = BLOCK.match(line)
        if head:
            block = (len(head[1]), head[2], True)
            continue
        pair = PAIR.match(line)
        if pair:
            value, comment = _value_and_comment(pair[3])
            if value:
                out[i] = f'{pair[2]} = "{value}"{comment}'
            elif not pair[3]:
                block = (len(pair[1]), pair[2], False)
            continue
        if block is not None and stripped:
            value, comment = _value_and_comment(stripped)
            if value:
                out[i] = f'{block[1]}: "{value}"{comment}'
    return out


def binary_paths(*diff_args: str) -> list[str]:
    """The added or changed paths a diff prints as binary (`-` for its line
    counts in `--numstat`)."""
    out = git_names(*diff_args, "--numstat", "-z", "--no-renames", "--diff-filter=AMT")
    paths = []
    for record in out.split("\0"):
        parts = record.split("\t", 2)
        if len(parts) == 3 and parts[0] == "-":
            paths.append(parts[2])
    return paths


@functools.lru_cache(maxsize=8)
def entries(revision: str) -> dict[str, tuple[str, str]]:
    """Every path at `revision` (a commit, or empty for the index's stage 0),
    with its mode and object id, from one listing and no pathspec.

    A path is never handed back to git to name a file. `git show <rev>:<path>`
    failed for a name that is not UTF-8, which cannot survive the round trip
    through a command line on Windows (tenth review of `#140`, finding 2), and
    a pathspec is a glob, so `ls-files -- '*x'` also matched a submodule
    pointer `!x` and a file named `*x.yml` was skipped (finding 4). A file
    named `0:x` is no longer read as `x` either (eighth review)."""
    if revision:
        # <mode> SP <type> SP <object> TAB <path>
        out, object_field = git_bytes("ls-tree", "-r", "-z", "--full-tree", revision), 2
    else:
        # <mode> SP <object> SP <stage> TAB <path>
        out, object_field = git_bytes("ls-files", "-s", "-z"), 1
    table = {}
    for record in out.split(b"\0"):
        meta, tab, name = record.partition(b"\t")
        fields = meta.split()
        if not tab or len(fields) != 3 or (not revision and fields[2] != b"0"):
            continue
        path = name.decode("utf-8", errors="surrogateescape")
        table[path] = (fields[0].decode("ascii"), fields[object_field].decode("ascii"))
    return table


def blob(revision: str, path: str) -> bytes:
    """The file at `revision`: a commit, or empty for the index."""
    entry = entries(revision).get(path)
    if entry is None:
        raise CannotRun(f"{shown(path)} is not in {revision or 'the index'}")
    return git_bytes("cat-file", "blob", entry[1])


def copy_name(path: str) -> str:
    """The name a scratch copy of `path` takes: `file` and the path's own
    extension, which is all detect-secrets reads to choose how it parses a
    file (`determine_file_type`). Not the path's own name, which Windows may
    refuse to create (`*x.yml`, `a:b.yml`)."""
    suffix = PurePosixPath(path).suffix
    return "file" + suffix if re.fullmatch(r"\.[A-Za-z0-9_-]{1,16}", suffix) else "file"


# Bytes no text holds: every C0 control but tab, newline and carriage
# return, and DEL.
CONTROL = bytes(b for b in range(32) if b not in (9, 10, 13)) + b"\x7f"
# What detect-secrets parses as INI by its extension (`determine_file_type`):
# CONFIG, INI and PROPERTIES.
INI_FAMILY = {".cnf", ".conf", ".cfg", ".cf", ".ini", ".properties"}


def parsed_copy(scratch: Path, n: int, path: str, data: bytes) -> list:
    """`parsed` on the bytes of `path`, written to a scratch copy, with each
    finding named `path`. When the bytes are not UTF-8, also on each way they
    read as text (`texts`), written as UTF-8: detect-secrets opens a file as
    UTF-8 and reads nothing of one that is not, so a block scalar or an
    unquoted value in a UTF-16 or Latin-1 YAML or INI file passed every read
    (seventeenth review of `#140`, finding 1)."""
    bases = [data]
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        bases += [text.encode("utf-8", errors="replace") for text in texts(data)[0]]
    # Each also without a UTF-8 byte-order mark and without control bytes
    # (all but tab, newline and carriage return), which keeps every line where
    # it was: configparser takes `\ufeff[s]` for no section, and PyYAML refuses
    # a file holding one NUL or one terminal colour code, and either way the
    # parsed read found nothing (eighteenth review of `#140`, findings 1 and 2).
    variants: list[bytes] = []
    for base in bases:
        for variant in (base, base.removeprefix(b"\xef\xbb\xbf").translate(None, CONTROL)):
            if variant not in variants:
                variants.append(variant)
    # An INI-family file is also parsed under a name with no extension, which
    # detect-secrets reads as a file of no known type and parses as INI with a
    # section header added: under its own name a file whose first line is not
    # `[section]` is not parsed at all (finding 1).
    names = [copy_name(path)]
    if PurePosixPath(path).suffix.lower() in INI_FAMILY:
        names.append("file")
    out = []
    k = 0
    for variant in variants:
        for name in names:
            copy = scratch / str(n) / str(k) / name
            copy.parent.mkdir(parents=True)
            copy.write_bytes(variant)
            out.extend(parsed(str(copy)))
            k += 1
    for secret in out:
        secret.filename = path
    return out


def scan_blobs(revision: str, paths: list[str]) -> SecretsCollection:
    """Each path at `revision` (`<commit>` or empty for the index), whole:
    in every way it reads as text, and as detect-secrets parses it. The
    parsed read is what `.gitattributes` marking a YAML or INI file `binary`
    took away from a commit's read (tenth review of `#140`, finding 5)."""
    found = SecretsCollection()
    with tempfile.TemporaryDirectory() as scratch:
        for n, path in enumerate(paths):
            if is_gitlink(revision, path):
                continue
            data = blob(revision, path)
            for text in texts(data)[0]:
                for filename, secret in scan_text(path, text):
                    found[filename].add(secret)
            for secret in parsed_copy(Path(scratch), n, path, data):
                found[path].add(secret)
    return found


def unquote(path: str) -> str:
    """A path as git writes it in a diff header: C-quoted when it holds a
    double quote, a backslash or a control character, with octal escapes for
    bytes. Back to the path itself."""
    if not (len(path) >= 2 and path[0] == path[-1] == '"'):
        return path
    body, out, i = path[1:-1], bytearray(), 0
    simple = {"n": 10, "t": 9, "r": 13, "a": 7, "b": 8, "f": 12, "v": 11, "\\": 92, '"': 34}
    while i < len(body):
        if body[i] == "\\" and i + 1 < len(body):
            if body[i + 1] in "01234567":
                out.append(int(body[i + 1 : i + 4], 8))
                i += 4
                continue
            out.append(simple.get(body[i + 1], ord(body[i + 1])))
            i += 2
            continue
        out += body[i].encode("utf-8", errors="surrogateescape")
        i += 1
    name = out.decode("utf-8", errors="surrogateescape")
    return name[2:] if name.startswith(("a/", "b/")) else name


def added_lines(diff: str) -> dict[str, set[int]]:
    """For each file of a diff that is still there after it, the numbers of
    the lines it adds; an empty set for a file it only deleted lines from.
    Those are read too: the parsed read can find a value no single line holds,
    and deleting a line can join two others into one, as a YAML string does
    across a trailing backslash (twelfth review of `#140`, finding 2).

    Read here and not by unidiff, which takes the name from the `diff --git
    a/X b/X` line and split it in the wrong place for a name holding ` b/` or
    a quoted name with a space (thirteenth review of `#140`, finding 1). The
    name is taken from the `+++` line, which holds it alone, and the added
    lines from each hunk's `+start,count`: with no context lines (`DIFF_OPTIONS`)
    every line a hunk shows on the new side is an added one. Only the header,
    before a file's first hunk, is read for `+++`: an added line whose text
    starts `++ ` is printed as `+++ ` too."""
    out: dict[str, set[int]] = {}
    path, header = None, False
    for line in diff.split("\n"):
        if line.startswith("diff --git "):
            path, header = None, True
        elif header and line.startswith("+++ "):
            # git ends a name holding a space with a tab, for `patch`.
            name = line[4:].removesuffix("\t")
            if name != "/dev/null":
                path = unquote(name) if name.startswith('"') else name.removeprefix("b/")
                out.setdefault(path, set())
        elif line.startswith("@@"):
            header = False
            if path is None:
                continue
            hunk = HUNK.match(line)
            if hunk is None:
                raise CannotRun(f"a hunk header git printed could not be read: {line!r}")
            start, count = int(hunk[1]), 1 if hunk[2] is None else int(hunk[2])
            out[path].update(range(start, start + count))
    return out


HUNK = re.compile(r"@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


def parsed(filename: str) -> list:
    """detect-secrets' own read of a file on disk, both of its passes: the
    file as its type parses (YAML, INI), then the eager INI parse that quotes
    an unquoted `key = value`. Its `scan_file` runs the second pass only when
    the first found nothing, so one judged finding in a file switched the
    second off and a new unquoted value beside it passed (ninth review of
    `#140`, finding 1)."""
    out = []
    try:
        for lines in scan._get_lines_from_file(filename):
            out.extend(
                scan._process_line_based_plugins(
                    lines=list(enumerate(lines, start=1)), filename=filename
                )
            )
    except OSError:
        pass
    return out


def is_gitlink(revision: str, path: str) -> bool:
    """A submodule pointer, whose content is another repository's commit and
    not a file: there is no blob to read (ninth review of `#140`, finding 3)."""
    return entries(revision).get(path, ("",))[0] == "160000"


def parsed_before(scratch: Path, n: int, path: str, before: list[str]) -> set[tuple[str, str]]:
    """What the parsed read finds in `path` at each revision in `before` (a
    commit's parents, or HEAD for the index), as (type, hash) pairs."""
    seen = set()
    for k, revision in enumerate(before):
        if path not in entries(revision) or is_gitlink(revision, path):
            continue
        for secret in parsed_copy(scratch / f"before-{n}", k, path, blob(revision, path)):
            seen.add((secret.type, secret.secret_hash))
    return seen


def scan_changes(diff: str, revision: str, before: list[str]) -> SecretsCollection:
    """The lines a diff adds, each read inside its whole file at `revision`
    (a commit, or empty for the index), and only the findings on added lines
    kept. For the parsed read, also any finding whose value the file did not
    hold at `before` (the parents, or HEAD for the index): detect-secrets
    numbers a YAML block scalar's finding at its key's line, so a commit that
    changed only the value line had no added line to match, and a secret
    written into an existing `password: |` passed every commit read (eleventh
    review of `#140`, finding 1).

    Not detect-secrets' own diff reader, which numbers a line's neighbours from
    the list of added lines only: a `pragma: allowlist nextline secret`
    anywhere in a commit could then silence a secret on another line (seventh
    review of `#140`, finding 1). Read in its file, a line's neighbour is the
    line above it.

    Each file is read both ways, as the tree is: line by line as written, and
    as detect-secrets parses it, from a copy under the file's own name so it
    parses by type. So a YAML block scalar or an unquoted `key = value` added
    and removed inside a range is read too."""
    found = SecretsCollection()
    if not diff.strip():
        return found
    with tempfile.TemporaryDirectory() as scratch:
        for n, (path, numbers) in enumerate(added_lines(diff).items()):
            if is_gitlink(revision, path):
                continue
            data = blob(revision, path)
            for text in texts(data)[0]:
                for filename, secret in scan_text(path, text):
                    if secret.line_number in numbers:
                        found[filename].add(secret)
            old = parsed_before(Path(scratch), n, path, before)
            for secret in parsed_copy(Path(scratch), n, path, data):
                if secret.line_number in numbers or (secret.type, secret.secret_hash) not in old:
                    found[path].add(secret)
    return found


def scan_tree(files: list[str], as_bytes: list[str] | None = None) -> SecretsCollection:
    """Every tracked file, read two ways, under the names git gives them.

    detect-secrets reads a file it can parse (YAML, INI) as parsed values, so
    a comment there is never read: a key pasted into a comment of a workflow
    file passes. A commit's diff is read line by line as written, comments
    included. So the tree is read both ways. One reason is the comment. The
    other is the baseline: a finding only the line as written shows would
    turn every commit in a range red, and the baseline, written from the tree,
    could never list it.

    The second way is each file's lines as written, through the same code a
    commit's lines take (`scan_text`): a file git calls text as git would
    print it, and every file in every way it reads (`texts`) when it has
    a NUL or a byte-order mark or git calls it binary, so a YAML file
    `.gitattributes` marks binary has its comments read (eleventh review of
    `#140`, finding 3). One file at a time and not as one diff of the tree,
    whose file names unidiff split in the wrong place (thirteenth review,
    finding 1).
    """
    found = SecretsCollection()
    empty_tree = git("hash-object", "-t", "tree", "--stdin", stdin=b"").strip()
    binary = set(binary_paths("diff", empty_tree))
    with tempfile.TemporaryDirectory() as scratch:
        for n, name in enumerate(files):
            if not Path(name).is_file():
                continue
            data = Path(name).read_bytes()
            # From a scratch copy, so a file that is not UTF-8 is parsed in
            # every way it reads as text too (`parsed_copy`).
            for secret in parsed_copy(Path(scratch), n, name, data):
                found[name].add(secret)
            lines = [] if name in binary else [data.decode("utf-8", errors="replace")]
            if name in binary or b"\x00" in data or data.startswith((b"\xff\xfe", b"\xfe\xff")):
                read, only_bytes = texts(data)
                lines += read
                # Counted from the read itself, so the figure cannot say a file
                # was read that was not (fifteenth review of `#140`, A3).
                if only_bytes and name in binary and as_bytes is not None:
                    as_bytes.append(name)
            for text in lines:
                for filename, secret in scan_text(name, text):
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
        found = scan_changes(
            git_names("show", "--format=", "--remerge-diff", *DIFF_OPTIONS, commit), commit, parents
        )
    elif len(parents) > 2:
        found = scan_changes(
            git_names("show", "--format=", "-m", "--first-parent", *DIFF_OPTIONS, commit),
            commit,
            parents[:1],
        )
    else:
        found = scan_changes(git_names("show", "--format=", *DIFF_OPTIONS, commit), commit, parents)
    # What git prints as `Bin` has no lines in the diff: UTF-16 text, and text
    # `.gitattributes` marks binary. Read those whole, from the commit's blob.
    # Against the first parent for a merge: more than the merge wrote, never less.
    against = ["diff-tree", "-r", "--no-commit-id"]
    against += ["--root"] if not parents else ["-m", "--first-parent"] if len(parents) > 1 else []
    for filename, secret in scan_blobs(commit, binary_paths(*against, commit)):
        found[filename].add(secret)
    return found


def commits_in(range_spec: str) -> list[str]:
    if ".." not in range_spec:
        raise CannotRun(f"--range wants BASE..HEAD, not {range_spec!r}")
    return git("rev-list", "--reverse", range_spec).split()


MAIN = "refs/remotes/origin/main"
MAIN_BRANCH = "refs/heads/main"


def since_main(head: str) -> str:
    """Everything on `head` that `main` does not have. The full ref name: git
    resolves a bare `origin/main` to a tag of that name first (review of
    `#140`, finding 4)."""
    git("rev-parse", "--verify", "--quiet", MAIN + "^{commit}")
    return head + " --not " + MAIN


def github_commits() -> tuple[list[str] | None, str]:
    """The commits the GitHub event asks for, read from its own event file,
    and a label for them. `None` is a deleted branch: the tree only.

    - A pull request: its base to its head.
    - A push to main: **the whole history of main**. A push to any other
      branch, or a tag: **everything since it left main**.

    A push is never read from its old head to its new one. GitHub starts no
    run for a push whose last commit says `[skip ci]` (or `[ci skip]`, `[no
    ci]`, `[skip actions]`, `[actions skip]`), and a run can fail to start;
    the next run's old head is then the unread push's new head, and those
    commits would be in no range ever again (second review of `#140`, finding
    2). A run that did not happen cannot shorten either range above, as long
    as its commits are still on the branch. What no run here can read is a
    commit that has left every branch, by a force push or a deleted branch:
    GitHub still serves it by sha, and only the push hook, before it was
    pushed, reads it (third review of `#140`, finding 4).

    The whole history of main was 124 commits on 2026-10-14. With each
    changed file also parsed as it was before its commit (eleventh review)
    and every text also read leniently (nineteenth), reading 152 commits
    took 8 min 3 s on a Windows workstation on 2026-10-18 (twentieth review,
    finding 2); before those it was about 1 min 15 s to 1 min 35 s. The runner was not measured (fourth review of `#140`,
    finding 4, which corrected "takes seconds"; the fifth, finding 2, which
    corrected the count; the twelfth and nineteenth, which corrected the
    time).
    Findings in it that are not secrets are listed in the baseline, which
    `--write-baseline` writes from the history too."""
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
            spec = f"{event['pull_request']['base']['sha']}..{event['pull_request']['head']['sha']}"
            return commits_in(spec), f"commits in {spec}"
        if name == "push":
            if event.get("deleted"):
                return None, ""
            ref, after = event["ref"], event["after"]
        else:
            ref, after = event.get("ref") or os.environ.get("GITHUB_REF", ""), "HEAD"
    except (KeyError, TypeError) as error:
        raise CannotRun(f"the {name} event has no {error}") from error
    if ref == MAIN_BRANCH:
        return git(
            "rev-list", "--reverse", after
        ).split(), f"commits of main's whole history ({after[:12]})"
    # `rev-list <head> --not main`, not `merge-base..head`: a branch that
    # shares no history with main (a `gh-pages`) has no merge base, and the
    # run failed with nothing read (seventh review of `#140`, finding 2).
    spec = since_main(after)
    return git(
        "rev-list", "--reverse", *spec.split()
    ).split(), f"commits not on main ({after[:12]})"


def every_ref() -> list[str]:
    """Every branch and tag GitHub holds, as the scheduled run's checkout has
    them: `refs/remotes/origin/*` but `HEAD` and main, and `refs/tags/*`.
    Never `refs/pull/*`: a pull request's head is read while it is open, and
    a closed one's commits were read then too (`I-467`, round 10 of `#140`,
    finding 3: a finding only a closed pull request held kept the run red,
    and nothing could make it pass)."""
    names = git(
        "for-each-ref", "--format=%(refname)", "refs/remotes/origin/", "refs/tags/"
    ).split()
    return [n for n in names if n not in (MAIN, "refs/remotes/origin/HEAD")]


def own_judgements(ref: str) -> SecretsCollection:
    """The entries `ref`'s own `.secrets.baseline` judged not to be secrets.

    Only the entries: its settings are never applied, so a branch cannot turn
    a detector off or widen an exclusion for itself, let alone for the run.
    A branch with no baseline, or one that cannot be read, judges nothing,
    and its commits are judged by main's alone: stricter, never looser."""
    try:
        data = json.loads(git_bytes("show", f"{ref}^{{commit}}:{BASELINE}").decode("utf-8"))
        loaded = SecretsCollection.load_from_baseline(data)
    except (CannotRun, ValueError, KeyError, TypeError, AttributeError):
        return SecretsCollection()
    own = SecretsCollection()
    for filename, secret in loaded:
        secret.filename = filename.replace("\\", "/")
        own[secret.filename].add(secret)
    return judged_not_secret(own)


def check_every_ref(baseline_path: Path) -> int:
    """The scheduled run: every branch and tag, each judged on its own.

    A push whose last commit carries a skip marker starts no run, so its
    commits wait for the next run that reaches them, and a branch nobody
    pushes to again and opens no pull request for is reached by none (`I-467`).
    This reads all of them once a day:

    - main as a push to main is read: the tree and its whole history, judged
      by main's baseline;
    - each other branch and each tag: the commits it carries that main does
      not (`rev-list <ref> --not main`), judged by main's baseline and by that
      ref's own judgements, and by no other ref's. The first daily run trusted
      every pull request head's baseline for every branch, so one pull request
      could silence a finding on another (round 10 of `#140`, finding 1).

    A commit two refs carry is read once and judged once for each of them.
    A tag that names no commit is listed and not read."""
    listed = load_baseline(baseline_path)
    for filename, secret in listed:
        LISTED[filename].add(secret)
    known = judged_not_secret(listed)
    git("rev-parse", "--verify", "--quiet", MAIN + "^{commit}")
    files = tracked_files()
    if not files:
        raise CannotRun("git ls-files printed nothing, so there is nothing to scan")
    total = 0
    new = scan_tree(files) - known
    if new:
        print("Secrets in main's tracked files that the baseline does not pass:")
        total += report(new, "")

    read: dict[str, SecretsCollection] = {}

    def judged(commit: str, against: SecretsCollection, where: str) -> int:
        if commit not in read:
            read[commit] = scan_commit(commit)
        new = read[commit] - against
        if not new:
            return 0
        print(f"Secrets added by commit {commit[:12]} {where} that the baseline does not pass:")
        return report(new, f"{commit[:12]} ")

    for commit in git("rev-list", "--reverse", MAIN).split():
        total += judged(commit, known, "on main")
    refs = every_ref()
    for ref in refs:
        try:
            git("rev-parse", "--verify", "--quiet", ref + "^{commit}")
        except CannotRun:
            print(f"  {ref} names no commit and is not read")
            continue
        own = own_judgements(ref)
        both = SecretsCollection()
        for collection in (known, own):
            for filename, secret in collection:
                both[filename].add(secret)
        for commit in git("rev-list", "--reverse", ref + "^{commit}", "--not", MAIN).split():
            total += judged(commit, both, f"on {ref}")

    scope = (
        f"{len(files)} tracked files of main, {len(read)} commits, "
        f"and {len(refs)} branches and tags"
    )
    if total:
        print(
            f"\nsecret-scan: {total} finding(s) in {scope}. {ADVICE['ci']} "
            "If it is not a secret, see the docstring of scripts/secret-scan.py."
        )
        return 1
    print(f"secret-scan: nothing new in {scope}.")
    return 0


LISTED: SecretsCollection = SecretsCollection()


def report(found: SecretsCollection, where: str) -> int:
    """Prints each finding without its value. Returns how many."""
    count = 0
    for filename, secret in found:
        line = secret.line_number or "?"
        note = ""
        for entry in LISTED[filename]:
            if entry == secret:
                note = (
                    "  (in the baseline, judged a secret)"
                    if entry.is_secret
                    else "  (in the baseline, not judged yet)"
                )
        print(f"  {where}{shown(filename)}:{line}  {secret.type}{note}")
        count += 1
    return count


def has_head() -> bool:
    """Whether HEAD names a commit; not in a repository with no commit yet."""
    return subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", "HEAD^{commit}"], capture_output=True, check=False
    ).returncode == 0


def scan_staged() -> tuple[SecretsCollection, int]:
    """What the commit being made would add: the staged files as parsed, and
    the staged lines as written. Under pre-commit the working tree holds the
    staged content, because unstaged changes are set aside while hooks run."""
    names = [
        n
        for n in git_names(
            "diff", "--cached", "--name-only", "--no-renames", "--diff-filter=AMT", "-z"
        ).split("\0")
        if n
    ]
    found = SecretsCollection()
    with tempfile.TemporaryDirectory() as scratch:
        for n, name in enumerate(names):
            if not Path(name).is_file():
                continue
            for secret in parsed_copy(Path(scratch), n, name, Path(name).read_bytes()):
                found[name].add(secret)
    before = ["HEAD"] if has_head() else []
    for filename, secret in scan_changes(git_names("diff", "--cached", *DIFF_OPTIONS), "", before):
        found[filename].add(secret)
    for filename, secret in scan_blobs("", binary_paths("diff", "--cached")):
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
        "Nothing is public yet: take the value out of the commit that added it, "
        "on whichever local branch holds it. This hook reads every local branch "
        "and tag no remote has, not only the one being pushed, and a later "
        "commit that deletes the value is not enough."
    ),
}


def judged_not_secret(listed: SecretsCollection) -> SecretsCollection:
    """The baseline entries a person has read and judged not to be secrets.
    Only these pass. An entry `--write-baseline` added and nobody judged, or
    one judged a secret, is refused like an unlisted finding: otherwise one
    run of `--write-baseline` would make a real secret pass."""
    judged = SecretsCollection()
    for filename, secret in listed:
        if secret.is_secret is False:
            judged[filename].add(secret)
    return judged


def check(
    baseline_path: Path,
    commits: list[str] | None,
    label: str = "",
    staged: bool = False,
    unpushed: bool = False,
) -> int:
    listed = load_baseline(baseline_path)
    for filename, secret in listed:
        LISTED[filename].add(secret)
    known = judged_not_secret(listed)
    total = 0
    if staged:
        found, count = scan_staged()
        scope = f"{count} staged files"
        new = found - known
        if new:
            print("Secrets in the staged changes that the baseline does not pass:")
            total += report(new, "")
    elif unpushed:
        scope = ""
    else:
        files = tracked_files()
        if not files:
            raise CannotRun("git ls-files printed nothing, so there is nothing to scan")
        as_bytes: list[str] = []
        new = scan_tree(files, as_bytes) - known
        scope = f"{len(files)} tracked files ({len(as_bytes)} of them binary, read as bytes)"
        if new:
            print("Secrets in the tracked files that the baseline does not pass:")
            total += report(new, "")

    if unpushed:
        commits = unpushed_commits()
        scope = f"{len(commits)} commits no remote has"
    elif commits is not None:
        scope += f" and {len(commits)} {label}"
    commits = commits or []
    for commit in commits:
        new = scan_commit(commit) - known
        if new:
            print(f"Secrets added by commit {commit[:12]} that the baseline does not pass:")
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
    """Rewrites the findings from the tree and from every commit of HEAD's
    history, keeping the settings and every judgement (`is_secret`) already
    recorded.

    **So a judgement outlives its line.** The range reads commits, and a
    commit that added a line still adds it after a later one removes it. The
    history read finds the line again in that commit, and the judgement is
    carried over to it. New entries arrive unjudged and pass nothing."""
    old = load_baseline(baseline_path)
    files = tracked_files()
    if not files:
        raise CannotRun("git ls-files printed nothing, so there is nothing to scan")
    new = scan_tree(files)
    # And every commit of HEAD's history: a push to main reads all of it, so
    # what was ever committed has to be listable, not only what is there now.
    # New entries arrive unjudged and pass nothing until a person judges them.
    for commit in git("rev-list", "--reverse", "HEAD").split():
        for filename, secret in scan_commit(commit):
            new[filename].add(secret)
    new.merge(old)
    baseline.save_to_file(new, str(baseline_path))
    count = sum(1 for _ in new)
    print(
        f"secret-scan: wrote {count} finding(s) from {len(files)} tracked files to {baseline_path}."
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    # detect-secrets opens each file with no encoding named, so Python's
    # locale decides. On Windows that is cp1252, and a UTF-8 file with a
    # character cp1252 cannot decode is skipped as binary: read by CI, passed
    # unread on a developer's machine. UTF-8 mode makes the two agree. A child
    # process and not `os.execv`, whose exit code on Windows is the parent's.
    if os.name == "nt" and not sys.flags.utf8_mode:
        args = sys.argv[1:] if argv is None else argv
        # `-I` is passed on when it was given, as the hooks give it (twelfth
        # review of `#140`, a note).
        isolated = ["-I"] if sys.flags.isolated else []
        child = [sys.executable, *isolated, "-X", "utf8", str(Path(__file__).resolve()), *args]
        return subprocess.run(child, check=False).returncode

    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--range", dest="range_spec", help="the tree, and each commit in BASE..HEAD")
    modes.add_argument(
        "--github", action="store_true", help="the tree, and the range of this GitHub event"
    )
    modes.add_argument(
        "--staged", action="store_true", help="what the commit being made adds (pre-commit)"
    )
    modes.add_argument(
        "--unpushed", action="store_true", help="each commit no remote has (pre-push)"
    )
    modes.add_argument(
        "--write-baseline", action="store_true", help="rewrite the findings in the baseline"
    )
    parser.add_argument("--baseline", default=BASELINE, help="default: .secrets.baseline")
    args = parser.parse_args(argv)
    try:
        os.chdir(git("rev-parse", "--show-toplevel").strip())
        path = Path(args.baseline)
        if args.write_baseline:
            return write_baseline(path)
        if args.github and os.environ.get("GITHUB_EVENT_NAME") == "schedule":
            return check_every_ref(path)
        if args.github:
            commits, label = github_commits()
        elif args.range_spec:
            commits, label = commits_in(args.range_spec), f"commits in {args.range_spec}"
        else:
            commits, label = None, ""
        return check(path, commits, label, staged=args.staged, unpushed=args.unpushed)
    except CannotRun as error:
        print(f"secret-scan: could not run: {error}", file=sys.stderr)
        return 2
    except Exception:  # noqa: BLE001
        # Anything else is a run that did not finish, not a finding: exit 2,
        # as the docstring promises, where a traceback exited 1 (thirteenth
        # review of `#140`, finding 1).
        traceback.print_exc()
        print("secret-scan: could not run: an unexpected error, above", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
