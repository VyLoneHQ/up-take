#!/usr/bin/env python3
"""Holds `plain_file_name` to `manifest.rs`'s `is_plain_file_name`, name for name.

# Why this file exists

`rust_consts.plain_file_name` duplicates `is_plain_file_name`. There cannot be
one implementation: the Rust guards what the PRODUCT installs at run time and
the Python guards what the BUILD stages, in different languages and different
processes, and neither can call the other. Two implementations of one rule is
`F-22` / `F-37`, so the duplication has to be policed rather than trusted.

# Why it no longer reads the Rust guard's CODE

`PR #88` rounds 6, 7 and 8 each found a different way for the two to diverge
without this control noticing:

* **round 6** -- it read only the reserved-name LIST, not the rules, so any new
  rule was invisible.
* **round 7** -- it read rules with a line-anchored regex, so a rule in the
  function's trailing expression, behind an `else if`, or wrapped by `rustfmt`
  slipped past. The trailing expression is where the one existing non-`if` rule
  already lives, so the one place a rule provably was, was the one place the
  scanner did not look.
* **round 8** -- it stripped `!`, so it saw WHICH predicate a rule used and
  never whether the rule was that predicate or its negation.

Every fix was correct and every one was incomplete one shape down, because
reading Rust predicates with a Python regex is a parser problem solved without a
parser. The founder chose to replace the approach rather than patch it again.

# What replaces it

**No rule is inferred from source shape any more.** (`rust_consts.py` does read
the reserved-device LIST out of `manifest.rs`, which is data extraction and was
never the part that failed -- see round 9 below.) `uptake-assets`' `the_name_guard_cases_file_is_current`
test runs the REAL Rust guard over a systematic corpus and writes every verdict
to `crates/uptake-assets/name-guard-cases.tsv`. This control reads that file and
asserts the Python guard returns the same verdict for the same name. Divergence
is caught from either direction:

    rule added to Rust, file not regenerated  -> the RUST test fails
    rule added to Rust, file regenerated      -> THIS control fails
    rule added to Python only                 -> THIS control fails

⚠️ The third line was false for ONE thing and round 9 drilled it: the reserved
device list was restated on both sides, and the corpus draws its device cases
from the Rust list, so a stem added to the Python list alone produced no case
and every control stayed green. The version before this one had a list-agreement
test that caught exactly that, and replacing the approach deleted it.

`rust_consts.py` has no list any more -- it reads the Rust one, so "added to
Python only" is not a state the device list can be in. The three lines are about
RULES, and for rules they hold in both directions.

A rule's SHAPE is now irrelevant. `else if`, a wrapped condition, a trailing
expression and a negation all move a verdict, and a moved verdict is what this
compares.

**The residual, stated rather than left to be found:** a rule whose effect is
invisible on every name in the corpus. That is why the corpus is generated
systematically over the interesting character classes and positions rather than
hand-listed, and why both sides assert it is not degenerate.

Run: `python3 scripts/control-rust-consts.py`
"""

from __future__ import annotations

import importlib.util
import re
import sys
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
CASES = HERE.parent / "crates" / "uptake-assets" / "name-guard-cases.tsv"

REGENERATE = "UPTAKE_REGENERATE_NAME_GUARD=1 cargo test -p uptake-assets name_guard"


def load_rust_consts():
    spec = importlib.util.spec_from_file_location("rust_consts", HERE / "rust_consts.py")
    if spec is None or spec.loader is None:
        raise SystemExit("could not load rust_consts.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def unescape(text: str) -> str:
    """Reverses the Rust side's `escape`. The two must agree or every case is wrong."""
    out: list[str] = []
    index = 0
    while index < len(text):
        char = text[index]
        if char != "\\":
            out.append(char)
            index += 1
            continue
        if index + 1 >= len(text):
            raise SystemExit("trailing backslash in " + repr(text))
        marker = text[index + 1]
        if marker == "x":
            out.append(chr(int(text[index + 2 : index + 4], 16)))
            index += 4
        elif marker in "\\tnr":
            out.append({"\\": "\\", "t": "\t", "n": "\n", "r": "\r"}[marker])
            index += 2
        else:
            raise SystemExit("unknown escape \\" + marker + " in " + repr(text))
    return "".join(out)


def read_cases() -> list[tuple[str, bool]]:
    """(name, expected_plain) per row.

    Refuses a file it cannot read rather than returning a short list: a short
    list is a control that passes against anything it did not load.
    """
    if not CASES.is_file():
        raise SystemExit(
            "cannot find " + str(CASES) + "."
            "\nIt is generated by the Rust guard's own test. Regenerate it:"
            "\n  " + REGENERATE
        )
    cases: list[tuple[str, bool]] = []
    for number, line in enumerate(CASES.read_text(encoding="utf-8").splitlines(), 1):
        if not line or line.startswith("#"):
            continue
        if "\t" not in line:
            raise SystemExit(str(CASES) + ":" + str(number) + " has no tab: " + repr(line))
        raw, verdict = line.rsplit("\t", 1)
        if verdict not in ("plain", "refused"):
            raise SystemExit(
                str(CASES) + ":" + str(number) + " has verdict " + repr(verdict)
                + ", expected 'plain' or 'refused'"
            )
        cases.append((unescape(raw), verdict == "plain"))
    if not cases:
        raise SystemExit(
            str(CASES) + " parsed to ZERO cases. A control that compares nothing"
            " passes against anything. Regenerate it:\n  " + REGENERATE
        )
    return cases


def python_accepts(module, name: str) -> bool:
    try:
        module.plain_file_name(name, "TEST_CONST")
    except SystemExit:
        return False
    return True


def test_the_two_guards_agree_on_every_case(module) -> None:
    """The whole control.

    A rule on either side that the other lacks moves a verdict here, whatever
    shape it is written in -- which is the property three rounds of regex
    parsing never had.
    """
    cases = read_cases()
    disagreements = [
        (name, expected)
        for name, expected in cases
        if python_accepts(module, name) != expected
    ]
    if disagreements:
        detail = "".join(
            chr(10) + "    " + repr(name) + ": Rust says "
            + ("plain" if expected else "refused")
            + ", Python says " + ("refused" if expected else "plain")
            for name, expected in disagreements[:10]
        )
        raise AssertionError(
            "the two name guards disagree on " + str(len(disagreements))
            + " of " + str(len(cases)) + " cases:" + detail + chr(10)
            + "One side has a rule the other does not. Fix"
            " rust_consts.plain_file_name to match; if the RUST changed"
            " deliberately, regenerate the cases file first:" + chr(10)
            + "  " + REGENERATE
        )


def test_the_corpus_is_not_degenerate(module) -> None:
    """A corpus that is all one verdict would pass against a guard answering a
    constant. The Rust side asserts this too; asserted here as well because this
    is the side that would silently benefit from a degenerate file."""
    cases = read_cases()
    refused = sum(1 for _, plain in cases if not plain)
    accepted = len(cases) - refused
    assert refused > 20, "only " + str(refused) + " refused cases; corpus is degenerate"
    assert accepted > 5, "only " + str(accepted) + " accepted cases; corpus is degenerate"


def test_the_cases_file_is_machine_generated(module) -> None:
    """A hand-edited cases file would let someone silence a disagreement by
    editing the expectation instead of the guard."""
    header = chr(10).join(CASES.read_text(encoding="utf-8").splitlines()[:5])
    assert "GENERATED" in header, "the cases file has lost its generated-by header"
    assert "Do not hand-edit" in header
    assert "UPTAKE_REGENERATE_NAME_GUARD" in header, (
        "the header no longer names the regeneration command"
    )


def test_the_real_pinned_names_pass(module) -> None:
    """The names actually shipping. If the guard ever refuses one, CI goes red
    here rather than in an acquisition step nobody reads."""
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
        except BaseException:  # noqa: BLE001, B036 - a control reports everything
            failures += 1
            print("FAIL  " + test.__name__)
            traceback.print_exc()
    print("")
    print(str(len(tests) - failures) + "/" + str(len(tests)) + " passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
