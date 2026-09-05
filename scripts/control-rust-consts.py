#!/usr/bin/env python3
"""Holds `plain_file_name` to `manifest.rs`'s `is_plain_file_name`, rule for rule.

# Why this file exists

Round 5 of `PR #88`'s independent review found that `rust_consts.plain_file_name`
was **a strictly weaker second implementation of a rule this repository had
already got right**. `crates/uptake-assets/src/manifest.rs`'s
`is_plain_file_name` guards exactly the same thing -- a pinned name joined to an
install directory -- and refused Windows device names, colons, trailing dots and
trailing spaces that the Python guard accepted. The weaker of the two was the
one guarding a write, and the reviewer drilled `DETECTION_FILE_NAME = "NUL"`
through to `main()` returning 0, printing *wrote 35 bytes* and *verified*, with
the staging directory **empty**.

Two implementations of one rule is `F-22` / `F-37`, and the honest fix would be
one implementation. There cannot be: the Rust guards what the PRODUCT installs
at run time and the Python guards what the BUILD stages, in different languages,
in different processes, and neither can import the other.

**So the duplication stays and stops being silent.** This control reads the
reserved-name list AND the guard's own clauses out of the Rust source, turns
each clause into a probe, and asserts the Python refuses it. A rule added to one
and not the other fails here.

⚠️ **That sentence was an overclaim until round 6 of `PR #88` caught it.** The
control read only the device LIST; the rules were hand-written Python literals,
so a new clause in `is_plain_file_name` -- a leading space, a control character,
a length bound -- left this printing `4/4 passed` while the guards diverged.
`rust_rule_probes()` is the fix, and it REFUSES rather than skips when it meets
a clause shape it does not recognise, because a parser that quietly skips one is
indistinguishable from agreement.

Run: `python3 scripts/control-rust-consts.py`
"""

from __future__ import annotations

import importlib.util
import re
import sys
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
MANIFEST = HERE.parent / "crates" / "uptake-assets" / "src" / "manifest.rs"

#: Names the Rust refuses. Each must be refused by the Python too, and each is
#: here because the reviewer drilled what it actually does on disk.
MUST_REFUSE = (
    "NUL", "nul", "CON", "AUX", "com1.txt", "LPT9.onnx", "NUL.onnx", "PRN",
    "model.onnx:stream", ":", "model.onnx.", "model.onnx ", "  ",
    "../x.onnx", "..\\x.onnx", "/abs/x", "C:x", "c:", "..", ".", "",
    "a/b.onnx", "dir\\file.onnx",
)

#: Names both must ACCEPT. Without these the control passes against a guard that
#: refuses everything, which is the vacuous-control failure this repo keeps
#: finding.
MUST_ACCEPT = (
    "PP-OCRv6_small_det.onnx",
    "PP-OCRv6_small_rec.onnx",
    "ppocr_keys_v6_small.txt",
    "onnxruntime.dll",
    "LICENSE-onnxruntime.txt",
    "console.txt",
    "nullable.onnx",
    "communication.log",
)


def load_rust_consts():
    spec = importlib.util.spec_from_file_location("rust_consts", HERE / "rust_consts.py")
    if spec is None or spec.loader is None:
        raise SystemExit("could not load rust_consts.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def rust_reserved_names() -> tuple[str, ...]:
    """Reads RESERVED_DEVICE_NAMES out of the Rust rather than restating it."""
    if not MANIFEST.is_file():
        raise SystemExit("cannot find " + str(MANIFEST))
    source = MANIFEST.read_text(encoding="utf-8")
    block = re.search(
        r"const RESERVED_DEVICE_NAMES:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\];",
        source,
        re.S,
    )
    if block is None:
        raise SystemExit(
            "could not find RESERVED_DEVICE_NAMES in " + str(MANIFEST)
            + ".\nThe Rust list moved and this control cannot read it. Fix the"
            " extraction rather than hard-coding the list here, which is the"
            " duplication this file exists to police."
        )
    return tuple(re.findall(r'"([^"]+)"', block.group(1)))


#: Everything in `is_plain_file_name`'s body that is STRUCTURE rather than a rule.
#: Stripped after the recognised predicates, so whatever is left is a rule this
#: control does not police. Order matters: longest first, so a prefix of one
#: does not eat another.
_SCAFFOLDING = (
    "let stem = name.split('.').next().unwrap_or(name);",
    ".iter()",
    ".any(|reserved| stem.eq_ignore_ascii_case(reserved))",
    "RESERVED_DEVICE_NAMES",
    "return false;",
    "return true;",
    "} else if",
    "else if",
    "else",
    "if",
    "||",
    "&&",
    "!",
    "{",
    "}",
    "(",
    ")",
)


def rust_rule_probes() -> list[tuple[str, str]]:
    """Reads `is_plain_file_name`'s RULES out of the Rust and turns each into a probe.

    # Why this exists, and what it replaces

    `PR #88` round 6, BEHAVIOUR 2. This file claimed it read "the reserved-name
    list **and the rule set**" out of the Rust and that "a rule added to one and
    not the other fails here". Only the list was read. The rules were
    hand-written Python literals, so a rule added to `is_plain_file_name` and not
    to `plain_file_name` left this control printing `4/4 passed` -- the exact
    `F-22` / `F-37` silence the file said it had ended.

    # And why the FIRST fix for that was still wrong, in three shapes

    `PR #88` round 7 and `PR #89` round 2, independently. The first version
    scanned clauses with a LINE-ANCHORED pattern, `^\\s*if (.+?) \\{`, which sees
    only a rule written as a single-line top-level `if`. Three shapes slipped
    past it, each drilled with the Rust rule added and the Python left alone,
    each leaving the control green at `5/5`:

    * **the trailing expression.** The reserved-device rule is not an `if` at
      all -- it is the function's final expression. So the one place a rule
      provably already lives was the one place the scanner did not look, and
      anyone adding the next rule beside the last one lands there.
    * **`} else if name.starts_with(' ') {`.** Ordinary Rust; the line does not
      begin with `if`.
    * **a `rustfmt`-wrapped condition.** The first clause is already three
      disjuncts long, and a fourth takes it past 100 columns, at which point the
      formatter breaks the line and the pattern stops matching.

    The control's green was conditional on a formatting choice nothing enforces.

    # So this does not parse SHAPES at all

    It collects every recognised PREDICATE from the whole body regardless of
    line structure, then strips those predicates and the structural scaffolding
    and asserts **nothing is left**. A rule in any shape is either recognised
    and probed, or it is residue and refused. `else if`, a wrapped condition and
    a trailing expression are all just text by then.

    Returns (probe, why) pairs synthesised from what the Rust actually says, so a
    new `contains('|')` upstream produces an `a|b.onnx` probe with no edit here.
    """
    if not MANIFEST.is_file():
        raise SystemExit("cannot find " + str(MANIFEST))
    source = MANIFEST.read_text(encoding="utf-8")
    body = re.search(
        r"fn is_plain_file_name\(name: &str\) -> bool \{(.*?)\n\}", source, re.S
    )
    if body is None:
        raise SystemExit(
            "could not find `fn is_plain_file_name` in " + str(MANIFEST)
            + ".\nThe Rust guard moved and this control cannot read its rules,"
            " which is the divergence it exists to detect. Fix the extraction."
        )
    text = re.sub(r"//[^\n]*", "", body.group(1))

    probes: list[tuple[str, str]] = []
    residue = text

    def take(pattern: str, build) -> None:
        """Collect probes from every match anywhere in the body, then remove it."""
        nonlocal residue
        for match in re.finditer(pattern, text):
            probes.extend(build(match))
        residue = re.sub(pattern, "", residue)

    def unescape(char: str) -> str:
        return {"\\\\": "\\", "\\'": "'", "\\n": "\n", "\\t": "\t", "\\0": "\0"}.get(char, char)

    take(
        r'name == "([^"]*)"',
        lambda m: [(m.group(1), "Rust rejects the literal " + repr(m.group(1)))],
    )
    take(
        r"name\.is_empty\(\)",
        lambda m: [("", "Rust rejects an empty name")],
    )
    take(
        r"name\.contains\('(\\.|[^'])'\)",
        lambda m: [
            ("a" + unescape(m.group(1)) + "b.onnx",
             "Rust rejects names containing " + repr(unescape(m.group(1))))
        ],
    )
    take(
        r"name\.ends_with\('(\\.|[^'])'\)",
        lambda m: [
            ("model.onnx" + unescape(m.group(1)),
             "Rust rejects names ending in " + repr(unescape(m.group(1))))
        ],
    )
    take(
        r"name\.starts_with\('(\\.|[^'])'\)",
        lambda m: [
            (unescape(m.group(1)) + "model.onnx",
             "Rust rejects names starting with " + repr(unescape(m.group(1))))
        ],
    )
    if "RESERVED_DEVICE_NAMES" in text:
        for reserved in rust_reserved_names():
            probes.append((reserved, "Rust rejects the device stem " + reserved))
            probes.append((reserved + ".onnx", "Rust rejects " + reserved + " with an extension"))

    for token in _SCAFFOLDING:
        residue = residue.replace(token, "")
    residue = residue.strip()

    if residue:
        raise SystemExit(
            "is_plain_file_name contains a rule this control cannot read:"
            + chr(10) + "    " + repr(residue[:200])
            + chr(10) + "A rule it cannot read is a rule it cannot police, and a"
            " control that skips one silently is indistinguishable from one that"
            " agrees. Teach the extraction the new predicate rather than leaving"
            " it unpoliced."
        )
    if not probes:
        raise SystemExit("read no rules at all from is_plain_file_name")
    return probes


def refuses(module, value: str) -> bool:
    try:
        module.plain_file_name(value, "TEST_CONST")
    except SystemExit:
        return True
    return False


def test_EVERY_RULE_the_rust_states_is_enforced_by_the_python(module) -> None:
    """The finding this control was written for, and did not cover.

    `MUST_REFUSE` below is a hand-written list; this reads the Rust guard's own
    clauses and synthesises a probe per rule, so a rule added to
    `is_plain_file_name` and not to `plain_file_name` goes red here without
    anyone editing this file.
    """
    probes = rust_rule_probes()
    assert probes, "read NO rules from the Rust guard; the extraction is blind"
    leaked = [
        (probe, why) for probe, why in probes if not refuses(module, probe)
    ]
    assert not leaked, (
        "plain_file_name ACCEPTED inputs the Rust guard refuses:"
        + "".join(
            chr(10) + "  " + repr(probe) + " -- " + why
            for probe, why in leaked[:10]
        )
    )


def test_the_reserved_lists_agree(module) -> None:
    rust = rust_reserved_names()
    python = tuple(module.RESERVED_DEVICE_NAMES)
    assert rust, "read an EMPTY list from the Rust; the extraction is blind"
    assert set(rust) == set(python), (
        "the device lists have diverged.\n  only in Rust:   "
        + str(sorted(set(rust) - set(python)))
        + "\n  only in Python: "
        + str(sorted(set(python) - set(rust)))
    )


def test_every_name_the_rust_refuses_is_refused(module) -> None:
    leaked = [name for name in MUST_REFUSE if not refuses(module, name)]
    assert not leaked, "plain_file_name ACCEPTED " + str(leaked)


def test_ordinary_names_are_still_accepted(module) -> None:
    """The control on the control. A guard that refuses everything passes the
    test above and breaks every acquisition step."""
    refused = [name for name in MUST_ACCEPT if refuses(module, name)]
    assert not refused, "plain_file_name REFUSED ordinary names " + str(refused)


def test_the_real_pinned_names_pass(module) -> None:
    """The names actually shipping. If the guard ever refuses one of these, CI
    goes red here rather than in an acquisition step nobody reads."""
    ppocr = HERE.parent / "crates" / "uptake-assets" / "src" / "ppocr.rs"
    source = ppocr.read_text(encoding="utf-8")
    names = re.findall(r'pub const \w*FILE_NAME: &str = "([^"]+)";', source)
    assert names, "no pinned file names found; the extraction is blind"
    for name in names:
        module.plain_file_name(name, "pinned")


def main() -> int:
    module = load_rust_consts()
    tests = [value for name, value in globals().items() if name.startswith("test_")]
    failures = 0
    for test in tests:
        try:
            test(module)
            print("ok    " + test.__name__)
        # BaseException, not Exception. PR #88 round 6, PROSE 5: SystemExit
        # derives from BaseException, and SystemExit is exactly what
        # plain_file_name and rust_reserved_names raise -- so the first refusal
        # aborted the whole run with no FAIL line, no summary, and every later
        # test unrun. CI still went red, so no false green was reachable; what
        # was lost is the diagnostics this comment promises.
        except BaseException:  # noqa: BLE001, B036 - a control reports everything
            failures += 1
            print("FAIL  " + test.__name__)
            traceback.print_exc()
    print("")
    print(str(len(tests) - failures) + "/" + str(len(tests)) + " passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
