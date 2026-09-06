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
import inspect
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
                "DETECTION_URL": "https://example.invalid/det.onnx",
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
                "DETECTION_URL": "https://example.invalid/det.onnx",
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
                "DETECTION_URL": "https://example.invalid/det.onnx",
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


def test_write_notice_REFUSES_a_pin_that_is_not_a_plain_name(module) -> None:
    """PR #88 round 6, BEHAVIOUR 1: the third execute-site, undrilled.

    With a pin of "../escaped.onnx" the notice named a file that is not the
    staged file, and the NOTE that tells an operator the staging directory is
    incomplete was SILENCED, because the escaped path existed one directory up.
    With "NUL" the notice named a device as a shipped file. Both are refused
    now, and both are driven here because the two sibling scripts refuse them
    and this one did not.
    """
    for bad in ("../escaped.onnx", "NUL", "det.onnx:stream", "det.onnx."):
        out = Path(tempfile.mkdtemp(prefix="notice-guard-"))
        try:
            pins = {
                "DETECTION_FILE_NAME": bad,
                "DETECTION_SHA256": "a" * 64,
                "DETECTION_SIZE": 9_880_512,
                "DETECTION_URL": "https://example.invalid/det.onnx",
            }
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    module.write_notice(out, pins, [])
            except SystemExit:
                continue
            raise AssertionError(bad + " was accepted as a detector file name")
        finally:
            shutil.rmtree(out, ignore_errors=True)


# ---------------------------------------------------------------------------
# `PR #88` round 11, FINDING 1 (`I-373`): `main()` converted straight into
# `--out` and ran `check_shapes` afterwards, so a refused recogniser stayed in
# the staging directory. The reviewer also recorded WHY nothing caught it --
# "nine tests and none of them touches `check_shapes` or `main()`'s ordering,
# so nothing can go red on this". These three are that.
#
# They stub the network, the converter and the shape check, which is what lets
# `main()` run end to end here at all: the real path needs a 10 MB download and
# a `paddle2onnx` install, and this suite exists to run without either.
# ---------------------------------------------------------------------------

FAKE_ONNX = b"forty-two bytes standing in for a converted model"
FAKE_DICT = b"a\nb\nc\n"


def drive_main(module, *, refuse: bool) -> dict:
    """Runs `main()` with every external dependency replaced.

    `refuse=True` makes the shape check raise the way the real one does on a
    wrong input shape. Records the directory the check was handed and what was
    in it AT THAT MOMENT, because "the check ran" and "the check ran before
    anything was staged" are different claims and only the second is the one
    `I-373` is about.
    """
    root = Path(tempfile.mkdtemp(prefix="convert-test-main-"))
    out = root / "staging"
    cache = root / "cache"
    seen: list[tuple[Path, list[str], list[str]]] = []
    names = ("fetch", "load_converter", "extract_model", "convert", "check_shapes")
    saved = {name: getattr(module, name) for name in names}
    saved_argv = sys.argv

    def fake_fetch(source, _cache):
        return b"tar-bytes" if source.name.endswith(".tar") else FAKE_DICT

    def fake_extract_model(_archive, _cache, stem):
        directory = root / ("extracted-" + stem)
        directory.mkdir(parents=True, exist_ok=True)
        return directory

    def fake_convert(_converter, _model_dir, destination):
        destination.write_bytes(FAKE_ONNX)
        return FAKE_ONNX

    def fake_check_shapes(directory, load=None):  # noqa: ARG001 - mirrors the seam
        here = Path(directory)
        already = sorted(q.name for q in out.iterdir()) if out.is_dir() else []
        seen.append((here, sorted(q.name for q in here.iterdir()), already))
        if refuse:
            raise SystemExit(
                "recogniser takes ['N', 1, 32, 'W'], expected [N, 3, 48, W]"
            )

    module.fetch = fake_fetch
    module.load_converter = lambda: object()
    module.extract_model = fake_extract_model
    module.convert = fake_convert
    module.check_shapes = fake_check_shapes
    sys.argv = [
        "convert-ppocr-models.py", "--out", str(out), "--cache", str(cache)
    ]
    code = None
    refusal = None
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                code = module.main()
            except SystemExit as error:
                refusal = str(error)
    finally:
        for name, value in saved.items():
            setattr(module, name, value)
        sys.argv = saved_argv

    staged = sorted(q.name for q in out.iterdir()) if out.is_dir() else []
    return {
        "root": root, "out": out, "staged": staged,
        "seen": seen, "code": code, "refusal": refusal,
    }


def test_a_FAILED_shape_check_stages_NOTHING(module) -> None:
    """`I-373` itself. Round 11 drilled the refusal and found the model and the
    dictionary both sitting in `--out` afterwards, with no licence notice
    beside them."""
    result = drive_main(module, refuse=True)
    try:
        assert result["refusal"] is not None, "the refusal did not propagate"
        assert "recogniser takes" in result["refusal"], result["refusal"]
        assert result["staged"] == [], (
            "a refused conversion left " + str(result["staged"])
            + " in the staging directory"
        )
    finally:
        shutil.rmtree(result["root"], ignore_errors=True)


def test_a_PASSING_shape_check_stages_the_model_and_the_dictionary(module) -> None:
    """The other half, and it is not decoration: a `main()` that staged nothing
    at all would pass the test above."""
    result = drive_main(module, refuse=False)
    try:
        assert result["code"] == 0, "clean run returned " + str(result["code"])
        for wanted in ("ch_PP-OCRv4_rec.onnx", "ppocr_keys_v1.txt"):
            assert wanted in result["staged"], (
                wanted + " was not staged; got " + str(result["staged"])
            )
        staged_model = result["out"] / "ch_PP-OCRv4_rec.onnx"
        assert staged_model.read_bytes() == FAKE_ONNX, "staged the wrong bytes"
    finally:
        shutil.rmtree(result["root"], ignore_errors=True)


def test_the_shape_check_runs_OUTSIDE_the_staging_directory(module) -> None:
    """The ordering fix, stated as the property rather than as the code.

    Without this, `main()` could stage the files, check them in place, and
    delete them again on refusal -- which would pass the first test while
    leaving the window `I-373` is about. The check must be handed a directory
    that is NOT `--out`, and the files must already be in it."""
    result = drive_main(module, refuse=False)
    try:
        assert len(result["seen"]) == 1, (
            "check_shapes was called " + str(len(result["seen"])) + " times"
        )
        directory, contents, already_staged = result["seen"][0]
        assert directory.resolve() != result["out"].resolve(), (
            "the shape check was handed the staging directory itself"
        )
        assert "ch_PP-OCRv4_rec.onnx" in contents, (
            "the check ran before the model was written; it saw " + str(contents)
        )
        assert already_staged == [], (
            "the staging directory already held " + str(already_staged)
            + " when the shape check ran"
        )
    finally:
        shutil.rmtree(result["root"], ignore_errors=True)


# ---------------------------------------------------------------------------
# `PR #88` round 12, the BEHAVIOUR finding, and it is the same class one turn
# later: round 11's fix gave `check_shapes` a `load` seam AND a docstring saying
# the seam meant "a stub loader drives every branch below with no onnxruntime,
# no model and no file" -- and shipped no test that uses it. Every test above
# stubs `check_shapes` out wholesale to drive `main()`'s ordering, so the
# function's own branches ran nowhere.
#
# Drilled by round 12, and reproduced here before fixing. With the suite at
# 12/12 the following each left it GREEN:
#
#   both shape refusals deleted                        12/12 passed
#   the unloadable-ONNX refusal deleted                12/12 passed
#   the loud onnxruntime-absent skip made SILENT       12/12 passed
#
# The third is not in round 12's report; it turned up running the drill. It is
# the one this function's own docstring calls "the defect" in as many words.
#
# `acquire-ppocr-detector.py`'s twin `check_shape` has had five such tests since
# round 3 found exactly this hole in exactly that function. The seam was copied
# across and the tests that make a seam worth having were not, in the commit
# whose subject was "in BOTH scripts". These are that, mirrored one for one.
# ---------------------------------------------------------------------------


class _StubSession:
    """Stands in for an onnxruntime InferenceSession, shapes only."""

    def __init__(self, shape_in, shape_out) -> None:
        self._in = shape_in
        self._out = shape_out

    def get_inputs(self):
        return [type("I", (), {"shape": self._in})()]

    def get_outputs(self):
        return [type("O", (), {"shape": self._out})()]


def _loader(shape_in, shape_out):
    return lambda path: _StubSession(shape_in, shape_out)


# What `PaddleEngine::recognise_crop` feeds and reads: [N, 3, 48, W] in, and a
# last dimension equal to EXPECTED_RECOGNISER_CLASSES out.
REC_IN = ["N", 3, 48, "W"]
REC_OUT = ["N", "T", 6625]


def test_a_correctly_shaped_recogniser_is_accepted(module) -> None:
    """The control that stops every test below passing vacuously."""
    assert module.EXPECTED_RECOGNISER_CLASSES == REC_OUT[-1], (
        "this suite's expected class count drifted from the script's"
    )
    with contextlib.redirect_stdout(io.StringIO()):
        module.check_shapes(Path("nowhere"), load=_loader(REC_IN, REC_OUT))


def test_the_wrong_input_CHANNELS_are_REFUSED(module) -> None:
    try:
        module.check_shapes(
            Path("nowhere"), load=_loader(["N", 1, 48, "W"], REC_OUT)
        )
    except SystemExit as exit_:
        assert "expected [N, 3, 48, W]" in str(exit_), str(exit_)
        return
    raise AssertionError("a 1-channel input was accepted by check_shapes")


def test_the_wrong_input_HEIGHT_is_REFUSED(module) -> None:
    """48 is the recogniser's fixed crop height. A model taking 32 is a
    different model, and the digest cannot say so."""
    try:
        module.check_shapes(
            Path("nowhere"), load=_loader(["N", 3, 32, "W"], REC_OUT)
        )
    except SystemExit as exit_:
        assert "expected [N, 3, 48, W]" in str(exit_), str(exit_)
        return
    raise AssertionError("a 32-high input was accepted by check_shapes")


def test_a_MISMATCHED_CLASS_COUNT_is_REFUSED(module) -> None:
    """`I-333`: the dictionary and the model are a matching pair, and this is
    the check that keeps them one."""
    try:
        module.check_shapes(
            Path("nowhere"), load=_loader(REC_IN, ["N", "T", 96])
        )
    except SystemExit as exit_:
        assert "classes" in str(exit_), str(exit_)
        assert "I-333" in str(exit_), "refused without naming the pairing rule"
        return
    raise AssertionError("a 96-class recogniser was accepted by check_shapes")


def test_an_unloadable_model_is_REFUSED_rather_than_swallowed(module) -> None:
    """A load failure here means the CONVERTER produced something that is not
    ONNX. That is a refusal with a sentence, not a traceback."""

    def explode(path):
        raise RuntimeError("not an ONNX file")

    try:
        module.check_shapes(Path("nowhere"), load=explode)
    except SystemExit as exit_:
        assert "not loadable as ONNX" in str(exit_), str(exit_)
        return
    raise AssertionError("an unloadable model was accepted by check_shapes")


def test_a_missing_onnxruntime_skips_LOUDLY_and_does_not_refuse(module) -> None:
    """The one branch that must NOT raise, and must still announce itself.

    The function's docstring: "A SILENT skip would be the defect -- a check that
    can vanish without saying so reports green forever." Nothing held it to that
    until now; deleting the print left the suite 12/12."""

    def missing(path):
        raise ImportError("No module named 'onnxruntime'")

    stdout = io.StringIO()
    with contextlib.redirect_stdout(stdout):
        module.check_shapes(Path("nowhere"), load=missing)
    assert "NOT CHECKED" in stdout.getvalue(), (
        "skipped silently, which is the whole failure"
    )


def test_the_default_loader_is_the_real_one(module) -> None:
    """A seam whose default drifted would make every test above a fiction."""
    signature = inspect.signature(module.check_shapes)
    assert signature.parameters["load"].default is module.onnxruntime_session


def test_check_shapes_reads_the_file_out_of_the_directory_it_is_GIVEN(module) -> None:
    """The seam and the staging fix meet here: `check_shapes` takes a directory
    and must look inside THAT one, or the scratch-directory ordering buys
    nothing."""
    seen = []

    def record(path):
        seen.append(Path(path))
        return _StubSession(REC_IN, REC_OUT)

    with contextlib.redirect_stdout(io.StringIO()):
        module.check_shapes(Path("some") / "scratch", load=record)
    assert len(seen) == 1, "the loader was called " + str(len(seen)) + " times"
    assert seen[0] == Path("some") / "scratch" / "ch_PP-OCRv4_rec.onnx", seen[0]


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
