#!/usr/bin/env python3
"""Tests for the model conversion step's pin reading and licence notice.

Written because round 2 of `PR #88`'s independent review found that `ADR-0036`
added real new logic to `convert-ppocr-models.py` -- `read_detector_pins()` and
the notice listing -- with **no test file for this script ever having existed**,
and none of the gate commands touching it. The only thing that runs the code is
a CI job that needs a network download and a `paddle2onnx` install, and which
asserts nothing about what it produced.

That is the same finding round 1 made about `acquire-ppocr-detector.py`, landing
in the very commit whose purpose was closing it. Twice is the argument for a
file rather than a promise.

WHAT THIS COVERS, AND WHAT IT DELIBERATELY DOES NOT

It covers the two pieces `ADR-0036` added and the property they exist for: the
licence notice names **all three** shipped files, including the one this script
no longer acquires. That property has no other guard anywhere -- `cargo deny`
walks the crate graph and sees no `.onnx` at all, which is why the notice exists.

It does NOT convert anything. No `paddle2onnx`, no download, no archives. The
conversion itself is exercised by the bundle CI job against real upstream bytes,
and duplicating that here would need the toolchain this suite exists to run
without.

Run: `python3 scripts/test_convert_ppocr_models.py`
"""

from __future__ import annotations

import importlib.util
import shutil
import sys
import tempfile
import contextlib
import io
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent

FAKE_DETECTOR = "PP-OCRv6_small_det.onnx"
FAKE_DIGEST = "d" * 64


def load_module():
    """Imports the script under test by path, since its name has a hyphen."""
    sys.path.insert(0, str(HERE))
    spec = importlib.util.spec_from_file_location(
        "convert_ppocr_models", HERE / "convert-ppocr-models.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load convert-ppocr-models.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def build_pins(*, digest_name: str = "DETECTION_SHA256") -> str:
    """A synthetic `ppocr.rs`, in the real file's exact syntax."""
    return (
        "//! Synthetic pins.\n\n"
        'pub const DETECTION_FILE_NAME: &str = "' + FAKE_DETECTOR + '";\n'
        "pub const " + digest_name + ": &str =\n"
        '    "' + FAKE_DIGEST + '";\n'
        "pub const DETECTION_SIZE: u64 = 9_880_512;\n"
        'pub const DETECTION_URL: &str = "https://example.invalid/d.onnx";\n'
    )


class Pins:
    """Points the module's pin source at a synthetic file for one test."""

    def __init__(self, module, text: str):
        self.module = module
        self.root = Path(tempfile.mkdtemp(prefix="convert-test-"))
        source = self.root / "crates" / "uptake-assets" / "src"
        source.mkdir(parents=True)
        (source / "ppocr.rs").write_text(text, encoding="utf-8")
        # `read_detector_pins` resolves the path from the script's own location,
        # so the module's `__file__` is what has to move.
        self.saved = module.__file__
        module.__file__ = str(self.root / "scripts" / "convert-ppocr-models.py")

    def close(self) -> None:
        self.module.__file__ = self.saved
        shutil.rmtree(self.root, ignore_errors=True)


def test_the_detector_is_named_even_though_this_script_never_writes_it(module) -> None:
    """The property the whole file exists for.

    `ADR-0036` moved the detector to a different acquisition step. If the notice
    were built from what THIS script produces, the detector would vanish from a
    licence obligation and nothing downstream would notice.
    """
    listing = module.notice_listing(
        {
            "DETECTION_FILE_NAME": FAKE_DETECTOR,
            "DETECTION_SIZE": 9_880_512,
            "DETECTION_SHA256": FAKE_DIGEST,
        },
        [("ch_PP-OCRv4_rec.onnx", 10_812_334, "a" * 64)],
    )
    assert FAKE_DETECTOR in listing, "the detector must be named in the notice"
    assert "ch_PP-OCRv4_rec.onnx" in listing
    assert FAKE_DIGEST in listing, "its digest must be named too"


def test_the_detector_is_marked_as_redistributed_unchanged(module) -> None:
    """Apache 2.0 section 4(b) distinguishes modified files from unmodified
    ones. The converted recogniser is modified; Baidu's detector is not, and the
    notice must not say one sentence covering both."""
    listing = module.notice_listing(
        {
            "DETECTION_FILE_NAME": FAKE_DETECTOR,
            "DETECTION_SIZE": 1,
            "DETECTION_SHA256": FAKE_DIGEST,
        },
        [("ch_PP-OCRv4_rec.onnx", 2, "a" * 64)],
    )
    # Each file is exactly three lines: name, "sha256 <hex>", "<n> bytes<note>".
    # The note rides on the SIZE line, not the name line -- the first version of
    # this test read the name line and failed for that reason rather than for a
    # defect in the listing.
    lines = listing.splitlines()
    blocks = {
        lines[i].strip(): "\n".join(lines[i : i + 3])
        for i in range(0, len(lines), 3)
    }
    assert set(blocks) == {FAKE_DETECTOR, "ch_PP-OCRv4_rec.onnx"}, sorted(blocks)

    assert "redistributed unchanged" in blocks[FAKE_DETECTOR], blocks[FAKE_DETECTOR]
    assert "redistributed unchanged" not in blocks["ch_PP-OCRv4_rec.onnx"], (
        "the CONVERTED recogniser must not be marked unmodified: "
        + blocks["ch_PP-OCRv4_rec.onnx"]
    )


def test_every_produced_file_reaches_the_listing(module) -> None:
    """A listing that silently drops an entry is the failure mode; assert the
    count rather than only the presence of the one we happen to look for."""
    produced = [(f"file{n}.bin", n, str(n) * 64) for n in range(1, 4)]
    listing = module.notice_listing(
        {
            "DETECTION_FILE_NAME": FAKE_DETECTOR,
            "DETECTION_SIZE": 9,
            "DETECTION_SHA256": FAKE_DIGEST,
        },
        produced,
    )
    for name, _, digest in produced:
        assert name in listing, name
        assert digest in listing, name
    assert listing.count("sha256") == len(produced) + 1, (
        "one digest line per file, detector included"
    )


def test_a_renamed_pin_constant_stops_the_run(module) -> None:
    """The pin reader going blind must be loud. If it returned nothing, the
    notice would be built over a partial mapping and the tests above would pass
    on whatever was left."""
    pins = Pins(module, build_pins(digest_name="RENAMED_SHA256"))
    try:
        try:
            module.read_detector_pins()
        except SystemExit as error:
            assert "DETECTION_SHA256" in str(error), str(error)
        else:
            raise AssertionError("a missing pin constant must stop the run")
    finally:
        pins.close()


def test_a_missing_pins_file_stops_the_run(module) -> None:
    saved = module.__file__
    root = Path(tempfile.mkdtemp(prefix="convert-test-nopins-"))
    module.__file__ = str(root / "scripts" / "convert-ppocr-models.py")
    try:
        try:
            module.read_detector_pins()
        except SystemExit as error:
            assert "cannot find" in str(error), str(error)
        else:
            raise AssertionError("an absent pins file must stop the run")
    finally:
        module.__file__ = saved
        shutil.rmtree(root, ignore_errors=True)


def test_the_real_pins_still_parse(module) -> None:
    """The synthetic tests would all pass against a reader that had gone blind
    to the REAL file, so this is the one that keeps them honest."""
    pins = module.read_detector_pins()
    assert str(pins["DETECTION_FILE_NAME"]).endswith(".onnx")
    assert len(str(pins["DETECTION_SHA256"])) == 64
    assert isinstance(pins["DETECTION_SIZE"], int) and pins["DETECTION_SIZE"] > 0


def test_write_notice_NAMES_EVERY_FILE_including_the_detector(module) -> None:
    """PR #88 round 4, F2: replacing the listing with a constant was 6/6 green.

    The listing was drilled as a function in round 2 and its CALL SITE was not,
    so a notice naming no file at all would have been written silently. This
    drives the composition and the write together.
    """
    out = Path(tempfile.mkdtemp(prefix="notice-test-"))
    try:
        pins = {
            "DETECTION_FILE_NAME": "PP-OCRv6_small_det.onnx",
            "DETECTION_SHA256": "a" * 64,
            "DETECTION_SIZE": 9_880_512,
        }
        produced = [("ch_PP-OCRv4_rec.onnx", 10_812_334, "b" * 64),
                    ("ppocr_keys_v1.txt", 26_249, "c" * 64)]
        written = module.write_notice(out, pins, produced)

        assert (out / "NOTICE-models.txt").is_file(), "no notice was written"
        for name, _, _ in produced:
            assert name in written, name + " is missing from the notice"
        assert pins["DETECTION_FILE_NAME"] in written, (
            "the DETECTOR is missing from the notice, which is the licence "
            "obligation this listing exists to carry"
        )
    finally:
        shutil.rmtree(out, ignore_errors=True)


def test_write_notice_SAYS_SO_when_the_detector_is_not_staged(module) -> None:
    """The NOTE whose deletion was also 6/6 green (round 4, M6)."""
    out = Path(tempfile.mkdtemp(prefix="notice-test-"))
    try:
        pins = {
            "DETECTION_FILE_NAME": "PP-OCRv6_small_det.onnx",
            "DETECTION_SHA256": "a" * 64,
            "DETECTION_SIZE": 9_880_512,
        }
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            module.write_notice(out, pins, [])
        assert "NOTE:" in printed.getvalue(), (
            "the detector is absent from the staging directory and nothing said so"
        )

        # And the opposite: staged, so no NOTE. Without this the assertion above
        # would pass against a script that printed the note unconditionally.
        (out / pins["DETECTION_FILE_NAME"]).write_bytes(b"staged")
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            module.write_notice(out, pins, [])
        assert "NOTE:" not in printed.getvalue(), (
            "the detector IS staged and the note fired anyway"
        )
    finally:
        shutil.rmtree(out, ignore_errors=True)


def main() -> int:
    module = load_module()
    tests = [value for name, value in globals().items() if name.startswith("test_")]
    failures = 0
    for test in tests:
        try:
            test(module)
            print("ok    " + test.__name__)
        except Exception:  # noqa: BLE001 - a test runner reports everything
            failures += 1
            print("FAIL  " + test.__name__)
            traceback.print_exc()
    print("")
    print(str(len(tests) - failures) + "/" + str(len(tests)) + " passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
