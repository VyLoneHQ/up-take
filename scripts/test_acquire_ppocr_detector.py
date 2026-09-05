#!/usr/bin/env python3
"""Tests for the PP-OCRv6 detector acquisition step (ADR-0036).

Written because round 1 of `PR #88`'s independent review classified their
absence as BEHAVIOUR, and it was right to. `acquire-ppocr-detector.py`'s own
docstring says it works "exactly as `acquire-onnxruntime.py` does for the
runtime" -- and that sibling has seven tests covering exactly this case list,
while this script had none. The property the whole change's provenance story
rests on, *nothing that fails a check reaches the staging directory*, was true
when drilled by hand and protected by nothing afterwards.

The reviewer also noted that the commit claiming "acquisition tests 7/7" was
true only of the OTHER script and could read as covering this one. It could.

**No 9.8 MB fixture.** Every test builds a tiny synthetic payload and a matching
synthetic pins file, then points the module's `PINS_SOURCE` at it. That is what
lets these run in CI on a job with no assets, and it means they test the
script's LOGIC rather than one particular release's bytes.

**Which of these has been drilled, named rather than claimed.** All of the
refusal paths below were driven by hand against the real 9,880,512-byte
detector on 2026-09-04 before these tests existed -- wrong size, same-size bit
flip, and a wrong-shaped model -- and each refused with nothing written. These
tests are those drills turned into controls. `test_the_real_pins_still_parse`
is the one that keeps the rest honest: the synthetic tests would all pass
against a parser that had gone blind to the real file.

Run: `python3 scripts/test_acquire_ppocr_detector.py`
"""

from __future__ import annotations

import hashlib
import importlib.util
import shutil
import sys
import tempfile
import contextlib
import inspect
import io
import traceback
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent

#: A stand-in payload. Small, and nothing like a real ONNX file, so a test that
#: accidentally reached for the real detector fails on the digest rather than
#: passing by accident.
FAKE = b"pretend this is a PP-OCRv6 detector"

FAKE_NAME = "PP-OCRv6_small_det.onnx"
FAKE_URL = "https://example.invalid/detector.onnx"


def load_module():
    """Imports the script under test by path, since its name has a hyphen."""
    spec = importlib.util.spec_from_file_location(
        "acquire_ppocr_detector", HERE / "acquire-ppocr-detector.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load acquire-ppocr-detector.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def build_pins(payload: bytes, *, name: str = FAKE_NAME, digest_name: str = "DETECTION_SHA256") -> str:
    """A synthetic `ppocr.rs`, in the real file's exact syntax.

    The extraction under test is a regex over that syntax, so a shape that only
    resembles it would test a parser nobody runs. `digest_name` is a parameter
    so a test can RENAME a constant and check the script refuses rather than
    carrying on with a partial mapping.
    """
    return (
        "//! Synthetic pins.\n\n"
        '/// The detector.\n'
        'pub const DETECTION_FILE_NAME: &str = "' + name + '";\n'
        "/// Its digest.\n"
        "pub const " + digest_name + ": &str =\n"
        '    "' + hashlib.sha256(payload).hexdigest() + '";\n'
        "/// Its size.\n"
        "pub const DETECTION_SIZE: u64 = " + str(len(payload)) + ";\n"
        "/// Where it comes from.\n"
        'pub const DETECTION_URL: &str = "' + FAKE_URL + '";\n'
    )


class Fixture:
    """A throwaway directory with a synthetic pins file, for one test."""

    def __init__(self, module, pins_text: str):
        self.module = module
        self.root = Path(tempfile.mkdtemp(prefix="acquire-det-test-"))
        self.pins = self.root / "ppocr.rs"
        self.pins.write_text(pins_text, encoding="utf-8")
        self.out = self.root / "models"
        self.saved = module.PINS_SOURCE
        module.PINS_SOURCE = self.pins

    def write_payload(self, data: bytes) -> Path:
        path = self.root / "payload.bin"
        path.write_bytes(data)
        return path

    def close(self) -> None:
        self.module.PINS_SOURCE = self.saved
        shutil.rmtree(self.root, ignore_errors=True)


def run_with(module, fixture: Fixture, payload_path: Path) -> int:
    """Invokes `main()` with the arguments the CI step uses."""
    saved_argv = sys.argv
    sys.argv = [
        "acquire-ppocr-detector.py",
        "--file",
        str(payload_path),
        "--out",
        str(fixture.out),
    ]
    try:
        return module.main()
    finally:
        sys.argv = saved_argv


def test_a_matching_file_is_staged(module) -> None:
    """The positive control: a file that matches its pin is written.

    ⚠️ The synthetic payload is not valid ONNX, so the shape check refuses it
    AFTER the write. That is the correct behaviour and this test asserts both
    halves: the bytes reach the staging directory, and the shape guard still
    speaks up. **The refusal is a SystemExit rather than a traceback only
    because this test found it was a traceback** -- `check_shape` did not handle
    a failed load until `PR #88` round 1 asked for these tests.
    """
    fixture = Fixture(module, build_pins(FAKE))
    try:
        try:
            run_with(module, fixture, fixture.write_payload(FAKE))
        except SystemExit as error:
            assert "not loadable as ONNX" in str(error), (
                "a synthetic payload must be refused CLEANLY by the shape check,"
                " not crash it: " + str(error)
            )
        staged = fixture.out / FAKE_NAME
        assert staged.is_file(), "the verified file must be written"
        assert staged.read_bytes() == FAKE, "the staged bytes must be the verified ones"
    finally:
        fixture.close()


def test_a_wrong_size_file_is_refused_and_nothing_is_written(module) -> None:
    fixture = Fixture(module, build_pins(FAKE))
    try:
        try:
            run_with(module, fixture, fixture.write_payload(FAKE + b"!"))
        except SystemExit as error:
            assert "wrong size" in str(error), str(error)
        else:
            raise AssertionError("a wrong-size file must be refused")
        assert not fixture.out.exists(), (
            "NOTHING may be staged when the check fails, and the output directory"
            " must not even be created"
        )
    finally:
        fixture.close()


def test_a_same_size_corruption_is_refused_and_nothing_is_written(module) -> None:
    """The case a size check alone would pass.

    Drilled by hand against the real detector on 2026-09-04 with one byte XORed;
    this is that drill as a control.
    """
    fixture = Fixture(module, build_pins(FAKE))
    try:
        corrupted = bytearray(FAKE)
        corrupted[0] ^= 0xFF
        try:
            run_with(module, fixture, fixture.write_payload(bytes(corrupted)))
        except SystemExit as error:
            assert "pinned digest" in str(error), str(error)
            # The refusal must not invite editing the constant.
            assert "ADR-0036" in str(error), "the refusal must point at the record"
        else:
            raise AssertionError("a same-size corruption must be refused")
        assert not fixture.out.exists(), "nothing may be staged when the digest fails"
    finally:
        fixture.close()


def test_a_renamed_pin_stops_the_run(module) -> None:
    """The parser going blind must be loud.

    If the extraction silently returned nothing, every check above would pass
    vacuously and this whole file would be a control that cannot go red.
    """
    fixture = Fixture(module, build_pins(FAKE, digest_name="RENAMED_SHA256"))
    try:
        try:
            run_with(module, fixture, fixture.write_payload(FAKE))
        except SystemExit as error:
            assert "DETECTION_SHA256" in str(error), str(error)
        else:
            raise AssertionError("a missing pin constant must stop the run")
    finally:
        fixture.close()


def test_a_non_https_url_is_refused_at_the_socket(module) -> None:
    """ADR-0032 decision 2 says HTTPS only, and this is the line that opens a
    socket. Asserted at the point of use rather than trusted from the pin."""
    try:
        module.fetch("http://example.invalid/detector.onnx")
    except SystemExit as error:
        assert "non-HTTPS" in str(error), str(error)
    else:
        raise AssertionError("a non-HTTPS URL must be refused before any request")


def test_the_detectors_own_shapes_are_accepted(module) -> None:
    """The positive control for the shape rule, on the real detector's shapes.

    Copied from an actual run: `[dyn, 3, dyn, dyn] -> [dyn, 1, dyn, dyn]`.
    Symbolic dimensions are strings in onnxruntime, so they are strings here.
    """
    verdict = module.shape_complaint(
        ["DynamicDimension.0", 3, "DynamicDimension.1", "DynamicDimension.2"],
        ["ConvTranspose_459_o0__d0", 1, "ConvTranspose_459_o0__d2", "d3"],
    )
    assert verdict is None, verdict


def test_a_model_with_the_wrong_input_channels_is_refused(module) -> None:
    verdict = module.shape_complaint(["N", 1, "H", "W"], ["N", 1, "H", "W"])
    assert verdict is not None and "expected 3" in verdict, verdict


def test_a_model_that_is_not_a_probability_map_is_refused(module) -> None:
    """The recogniser's shape, which is what this guard exists to catch.

    ⚠️ **THIS TEST USED TO SKIP IN EVERY ENVIRONMENT IT RAN IN.** It loaded a
    gitignored model file through `onnxruntime`; locally the file was absent and
    in the CI job this suite runs from the package is not installed, so its
    assertion never executed. Round 2 of `PR #88`'s review proved it by deleting
    both refusals and watching the suite stay green at 7/7.

    It drives `shape_complaint` directly now: plain lists, no model, no import,
    no file. It runs everywhere or it fails everywhere.
    """
    verdict = module.shape_complaint(["N", 3, "H", "W"], ["N", 6625, "T"])
    assert verdict is not None, "a non-detector's output shape must be refused"
    assert "probability map" in verdict, verdict


def test_the_real_pins_still_parse(module) -> None:
    """The synthetic tests would all pass against a parser that had gone blind
    to the REAL file, so this is the one that keeps them honest."""
    pins = module.read_pins()
    assert str(pins["DETECTION_FILE_NAME"]).endswith(".onnx")
    assert len(str(pins["DETECTION_SHA256"])) == 64
    assert str(pins["DETECTION_URL"]).startswith("https://")
    assert isinstance(pins["DETECTION_SIZE"], int) and pins["DETECTION_SIZE"] > 0


# ---------------------------------------------------------------------------
# The shape guard's WIRING (PR #88 round 3, F1)
#
# Round 2 made the shape DECISION falsifiable -- `shape_complaint` is a pure
# function over two lists -- and left the INVOCATION unguarded. The reviewer
# drilled three mutations and each left the suite 9/9 green:
#
#   W1  delete the body of `check_shape`
#   W2  `raise SystemExit(complaint)` -> `pass`
#   W3  swallow the unloadable-ONNX SystemExit
#
# The guard only ever runs in the `build` job, where onnxruntime is installed
# and the real detector passes, so its refusal branch executed in no job at all.
# `check_shape` now takes a `load` seam and these drive every branch of it with
# no onnxruntime, no model and no file.
# ---------------------------------------------------------------------------


class _StubSession:
    """The two-method surface `check_shape` actually uses."""

    def __init__(self, shape_in, shape_out):
        self._in = shape_in
        self._out = shape_out

    def get_inputs(self):
        return [types.SimpleNamespace(shape=self._in)]

    def get_outputs(self):
        return [types.SimpleNamespace(shape=self._out)]


def _loader(shape_in, shape_out):
    return lambda path: _StubSession(shape_in, shape_out)


GOOD_IN = ["N", 3, "H", "W"]
GOOD_OUT = ["N", 1, "H", "W"]


def test_a_correctly_shaped_detector_is_accepted(module) -> None:
    """The control that stops the three below passing vacuously."""
    module.check_shape(Path("no-such-file.onnx"), load=_loader(GOOD_IN, GOOD_OUT))


def test_the_wrong_input_channels_are_REFUSED_through_check_shape(module) -> None:
    """W1 and W2: the complaint must reach a SystemExit, not just be computed."""
    try:
        module.check_shape(Path("x.onnx"), load=_loader(["N", 1, "H", "W"], GOOD_OUT))
    except SystemExit as exit_:
        assert "channels" in str(exit_), "refused, but not about the channel count"
        return
    raise AssertionError("a 1-channel input was accepted by check_shape")


def test_a_non_probability_map_output_is_REFUSED_through_check_shape(module) -> None:
    try:
        module.check_shape(Path("x.onnx"), load=_loader(GOOD_IN, ["N", 3, "H", "W"]))
    except SystemExit as exit_:
        assert "probability map" in str(exit_), "refused, but not about the output"
        return
    raise AssertionError("a 3-channel output was accepted by check_shape")


def test_an_unloadable_model_is_REFUSED_rather_than_swallowed(module) -> None:
    """W3: a load failure after the digest matched is a refusal, not a skip."""

    def explode(path):
        raise RuntimeError("not an ONNX file")

    try:
        module.check_shape(Path("x.onnx"), load=explode)
    except SystemExit as exit_:
        assert "not loadable" in str(exit_)
        return
    raise AssertionError("an unloadable model was accepted by check_shape")


def test_a_missing_onnxruntime_skips_LOUDLY_and_does_not_refuse(module) -> None:
    """The one branch that must NOT raise -- and must still announce itself."""

    def missing(path):
        raise ImportError("No module named 'onnxruntime'")

    stdout = io.StringIO()
    with contextlib.redirect_stdout(stdout):
        module.check_shape(Path("x.onnx"), load=missing)
    assert "NOT CHECKED" in stdout.getvalue(), "skipped silently, which is the whole failure"


def test_the_default_loader_is_the_real_one(module) -> None:
    """A seam whose default drifted would make every test above a fiction."""
    signature = inspect.signature(module.check_shape)
    assert signature.parameters["load"].default is module.onnxruntime_session


def test_main_ACTUALLY_INVOKES_the_shape_guard(module) -> None:
    """PR #88 round 4, F1: `pass  # MUTANT` at the call site left the suite 15/15.

    Round 3 made every BRANCH of `check_shape` reachable. It did not make the
    one line that connects the guard to the pipeline reachable, so deleting
    `check_shape(target)` from `main()` changed nothing any gate could see -- the
    only observable difference was a print that nothing asserts on.

    The existing positive control structurally cannot catch it: it wraps
    `run_with` in a bare `try/except SystemExit` with no `else`, and it has to,
    because with onnxruntime installed the synthetic payload raises "not
    loadable" and without it the guard prints NOT CHECKED and returns. It must
    tolerate both, so it cannot assert the guard ran.

    A recorder can, and it works in both environments because it does not care
    what the loader does.
    """
    fixture = Fixture(module, build_pins(FAKE))
    seen: list[Path] = []
    real = module.check_shape
    module.check_shape = lambda target, **_: seen.append(Path(target))
    try:
        payload = fixture.write_payload(FAKE)
        try:
            run_with(module, fixture, payload)
        except SystemExit:
            pass
    finally:
        module.check_shape = real
        fixture.close()

    assert seen, (
        "main() staged the detector without invoking check_shape; the guard is "
        "wired to nothing"
    )
    assert seen[0].name == FAKE_NAME, (
        "check_shape was called on " + seen[0].name + ", not the staged file"
    )


def test_a_pin_that_escapes_the_staging_directory_is_REFUSED(module) -> None:
    """PR #88 round 4, F3: the decoy wrote two directories above --out."""
    fixture = Fixture(
        module, build_pins(FAKE, name="../../escaped-outside-out.onnx")
    )
    # `../../` from <root>/models lands in <root>'s PARENT, which is the shared
    # temp directory rather than anything this fixture owns. So the target is
    # cleared first and removed after: an earlier run that escaped would
    # otherwise fail this test for the wrong reason, and a run that tidied up
    # would let it PASS for the wrong reason. Found by drilling this very test.
    escaped = fixture.root.parent / "escaped-outside-out.onnx"
    escaped.unlink(missing_ok=True)
    try:
        payload = fixture.write_payload(FAKE)
        try:
            run_with(module, fixture, payload)
        except SystemExit as exit_:
            assert "separator" in str(exit_), (
                "refused, but not for the path separator: " + str(exit_)
            )
            assert not escaped.exists(), "refused, and still wrote outside --out"
            return
        raise AssertionError("a pin containing .. was accepted and written")
    finally:
        escaped.unlink(missing_ok=True)
        fixture.close()


def main() -> int:
    module = load_module()
    tests = [value for name, value in globals().items() if name.startswith("test_")]
    failures = 0
    for test in tests:
        try:
            test(module)
            print("ok    " + test.__name__)
        # BaseException, not Exception: SystemExit derives from it, and a
        # refusal in the code under test raises SystemExit -- which would
        # abort the whole run with no FAIL line, no summary and every later
        # test unrun. PR #88 round 6 PROSE 5 fixed this in ONE runner;
        # PR #89 round 2 found the three beside it untouched.
        except BaseException:  # noqa: BLE001, B036 - a test runner reports everything
            failures += 1
            print("FAIL  " + test.__name__)
            traceback.print_exc()
    print("")
    print(str(len(tests) - failures) + "/" + str(len(tests)) + " passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
