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


def rust_rule_probes() -> list[tuple[str, str]]:
    """Reads `is_plain_file_name`'s RULES out of the Rust and turns each into a probe.

    # Why this exists, and what it replaces

    `PR #88` round 6, BEHAVIOUR 2. This file's docstring claimed it read "the
    reserved-name list **and the rule set**" out of the Rust and that "a rule
    added to one and not the other fails here". Only the list was read. The
    rules were hand-written Python literals in `MUST_REFUSE`, so a rule added to
    `is_plain_file_name` and not to `plain_file_name` left this control printing
    `4/4 passed` -- the exact `F-22` / `F-37` silence the file said it had ended,
    still present for every rule except one.

    So the rules are extracted now. Each character the Rust rejects becomes a
    probe built around it, and a rule whose shape this parser does not recognise
    is a REFUSAL rather than a silent omission -- otherwise the parser going
    blind would look identical to agreement.

    Returns (probe, why) pairs. The probes are synthesised from what the Rust
    actually says, so a new `name.contains('|')` clause upstream produces a
    `a|b.onnx` probe here with no edit to this file.
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
    text = body.group(1)

    probes: list[tuple[str, str]] = []
    recognised = 0

    # `name.is_empty() || name == "." || name == ".."`
    for literal in re.findall(r'name == "([^"]*)"', text):
        probes.append((literal, "Rust rejects the literal " + repr(literal)))
        recognised += 1
    if "name.is_empty()" in text:
        probes.append(("", "Rust rejects an empty name"))
        recognised += 1

    # `name.contains('X')`
    for char in re.findall(r"name\.contains\('(\\?.)'\)", text):
        actual = {"\\\\": "\\", "\\'": "'", "\\n": "\n"}.get(char, char)
        probes.append(("a" + actual + "b.onnx", "Rust rejects names containing " + repr(actual)))
        recognised += 1

    # `name.ends_with('X')`
    for char in re.findall(r"name\.ends_with\('(\\?.)'\)", text):
        actual = {"\\\\": "\\", "\\'": "'"}.get(char, char)
        probes.append(("model.onnx" + actual, "Rust rejects names ending in " + repr(actual)))
        recognised += 1

    # the reserved-device check, whatever it is spelled as
    if "RESERVED_DEVICE_NAMES" in text:
        for reserved in rust_reserved_names():
            probes.append((reserved, "Rust rejects the device stem " + reserved))
            probes.append((reserved + ".onnx", "Rust rejects " + reserved + " with an extension"))
        recognised += 1

    # Every guard CLAUSE must be one this parser understands, checked per clause.
    #
    # Counting probes against clauses was the first attempt and it was wrong:
    # nine probes come from four clauses, so a fifth clause still satisfied
    # 9 >= 5 and passed. The drill caught it -- `name.len() > 255` added to the
    # Rust left this control green, which is the exact silence it exists to end.
    #
    # An unrecognised clause is a REFUSAL, not a skip. A parser that quietly
    # passes over a rule is indistinguishable from one that agrees with it.
    known = (
        re.compile(r"name\.is_empty\(\)"),
        re.compile(r'name == "[^"]*"'),
        re.compile(r"name\.contains\('(?:\\.|[^'])'\)"),
        re.compile(r"name\.ends_with\('(?:\\.|[^'])'\)"),
    )
    for condition in re.findall(r"^\s*if (.+?) \{", text, re.M):
        residue = condition
        for pattern in known:
            residue = pattern.sub("", residue)
        residue = residue.replace("||", "").replace("&&", "").strip()
        if residue:
            raise SystemExit(
                "is_plain_file_name has a guard clause this control cannot read:"
                + chr(10) + "    if " + condition
                + chr(10) + "Unrecognised part: " + repr(residue)
                + chr(10) + "A rule it cannot read is a rule it cannot police."
                " Teach the extraction the new shape rather than leaving it"
                " silently unpoliced."
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
