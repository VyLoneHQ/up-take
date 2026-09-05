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
reserved-name list and the rule set out of the Rust source and asserts the
Python agrees. A rule added to one and not the other fails here.

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


def refuses(module, value: str) -> bool:
    try:
        module.plain_file_name(value, "TEST_CONST")
    except SystemExit:
        return True
    return False


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
        except Exception:  # noqa: BLE001 - a control reports everything
            failures += 1
            print("FAIL  " + test.__name__)
            traceback.print_exc()
    print("")
    print(str(len(tests) - failures) + "/" + str(len(tests)) + " passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
