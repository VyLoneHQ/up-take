#!/usr/bin/env python3
"""Tests for the PP-OCRv6 recogniser acquisition step (roadmap 1.33, ADR-0037).

Written with `PR #88` rounds 3 and 4 already in hand rather than waiting for a
reviewer to find the same class a third time. Both rounds were the same shape in
the sibling script:

* round 3 -- the shape guard's DECISION could not be falsified, because reaching
  it needed a loaded model and the `web` job has no `onnxruntime`;
* round 4 -- the decision was falsifiable and the guard's CALL SITE was not, so
  `pass  # MUTANT` at `main()` left the suite fully green.

So the decision here is a pure function over a plain list (`class_complaint`),
the guard takes a `load` seam, and one test asserts `main()` actually invokes
it. All three are drilled in `drill_r4.py`'s style before this file is called
done.

Everything below is synthetic. No model is downloaded, no `onnxruntime` is
needed, and nothing depends on which of the two is installed -- a test that
behaves differently depending on the environment is `UT-F-101`.

Run: `python3 scripts/test_acquire_ppocr_recogniser.py`
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import shutil
import sys
import tempfile
import traceback
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent

FAKE_MODEL = b"pretend this is a PP-OCRv6 recogniser"
FAKE_NAME = "PP-OCRv6_small_rec.onnx"
FAKE_DICT_NAME = "ppocr_keys_v6_small.txt"
FAKE_CLASSES = 7

#: A minimal `inference.yml`, in the real file's exact shape: a `character_dict`
#: block of `- x` lines. Five entries plus blank plus the CTC blank is
#: FAKE_CLASSES, so the matched-pair arithmetic is exercised rather than stated.
FAKE_YML = b"""Global:
  model_name: PP-OCRv6_small_rec
PostProcess:
  name: CTCLabelDecode
  character_dict:
  - a
  - b
  - '9'
  - ':'
  -
"""
EXPECTED_DICT = b"a\nb\n9\n:\n "


def load_module():
    """Imports the script under test by path, since its name is hyphenated."""
    spec = importlib.util.spec_from_file_location(
        "acquire_ppocr_recogniser", HERE / "acquire-ppocr-recogniser.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load acquire-ppocr-recogniser.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def build_pins(
    model: bytes,
    dictionary: bytes,
    *,
    name: str = FAKE_NAME,
    dict_name: str = FAKE_DICT_NAME,
    classes: int = FAKE_CLASSES,
    digest_name: str = "RECOGNITION_SHA256",
) -> str:
    """A synthetic `ppocr.rs` in the real file's exact syntax.

    The extraction under test is a regex over that syntax, so a shape that only
    resembled it would be testing a parser nobody runs. `digest_name` is a
    parameter so a test can RENAME a constant and check the script refuses.
    """
    return (
        'pub const RECOGNITION_FILE_NAME: &str = "' + name + '";\n'
        "pub const " + digest_name + ": &str =\n    \"" + digest(model) + "\";\n"
        "pub const RECOGNITION_SIZE: u64 = " + str(len(model)) + ";\n"
        'pub const RECOGNITION_URL: &str =\n    "https://example.invalid/rec.onnx";\n'
        'pub const DICTIONARY_FILE_NAME: &str = "' + dict_name + '";\n'
        'pub const DICTIONARY_SHA256: &str =\n    "' + digest(dictionary) + '";\n'
        "pub const DICTIONARY_SIZE: u64 = " + str(len(dictionary)) + ";\n"
        'pub const DICTIONARY_URL: &str =\n    "https://example.invalid/inference.yml";\n'
        "pub const RECOGNITION_CLASS_COUNT: usize = " + str(classes) + ";\n"
    )


class Fixture:
    """A throwaway directory with a synthetic pins file, for one test."""

    def __init__(self, module, pins_text: str):
        self.module = module
        self.root = Path(tempfile.mkdtemp(prefix="acquire-rec-test-"))
        self.pins = self.root / "ppocr.rs"
        self.pins.write_text(pins_text, encoding="utf-8")
        self.out = self.root / "models"
        self.saved = module.PINS_SOURCE
        module.PINS_SOURCE = self.pins

    def write(self, name: str, data: bytes) -> Path:
        path = self.root / name
        path.write_bytes(data)
        return path

    def close(self) -> None:
        self.module.PINS_SOURCE = self.saved
        shutil.rmtree(self.root, ignore_errors=True)


def run_with(module, fixture: Fixture, model: bytes, yml: bytes) -> int:
    """Invokes `main()` the way the CI step does, from local files."""
    saved_argv = sys.argv
    sys.argv = [
        "acquire-ppocr-recogniser.py",
        "--model-file", str(fixture.write("model.bin", model)),
        "--yml-file", str(fixture.write("inference.yml", yml)),
        "--out", str(fixture.out),
    ]
    try:
        return module.main()
    finally:
        sys.argv = saved_argv


# --- the extraction -------------------------------------------------------


def test_the_dictionary_is_extracted_exactly(module) -> None:
    """DICTIONARY_SHA256 pins THIS output, so its shape is the contract."""
    assert module.extract_dictionary(FAKE_YML) == EXPECTED_DICT


def test_a_blank_entry_becomes_a_space_not_an_empty_line(module) -> None:
    """PaddleOCR quotes the space inconsistently; an empty line would shift
    every index after it, which is the I-333 failure in its quietest form."""
    assert module.extract_dictionary(FAKE_YML).endswith(b"\n ")


def test_a_yml_without_the_block_is_REFUSED(module) -> None:
    try:
        module.extract_dictionary(b"Global:\n  model_name: x\n")
    except SystemExit as exit_:
        assert "character_dict" in str(exit_)
        return
    raise AssertionError("a yml with no character_dict block was accepted")


# --- the class-count decision, as a value ---------------------------------


def test_a_matching_class_count_is_accepted(module) -> None:
    """The control. Without it every refusal test below could pass vacuously."""
    assert module.class_complaint(["N", "T", 18710], 18710) is None


def test_a_mismatched_class_count_COMPLAINS_and_cites_the_pair(module) -> None:
    complaint = module.class_complaint(["N", "T", 6625], 18710)
    assert complaint is not None and "I-333" in complaint


def test_a_dynamic_class_count_COMPLAINS(module) -> None:
    complaint = module.class_complaint(["N", "T", "vocab"], 18710)
    assert complaint is not None and "dynamic" in complaint


def test_a_two_dimensional_output_COMPLAINS(module) -> None:
    assert module.class_complaint(["N", 18710], 18710) is not None


# --- the guard's branches, through the seam -------------------------------


def _session(shape_out):
    return lambda path: types.SimpleNamespace(
        get_outputs=lambda: [types.SimpleNamespace(shape=shape_out)]
    )


def test_check_classes_REFUSES_a_mismatched_model(module) -> None:
    try:
        module.check_classes(Path("x.onnx"), 18710, load=_session(["N", "T", 6625]))
    except SystemExit as exit_:
        assert "MATCHED PAIR" in str(exit_)
        return
    raise AssertionError("a mismatched recogniser was accepted")


def test_check_classes_accepts_a_matching_model(module) -> None:
    module.check_classes(Path("x.onnx"), 18710, load=_session(["N", "T", 18710]))


def test_an_unloadable_model_is_REFUSED_rather_than_swallowed(module) -> None:
    def explode(path):
        raise RuntimeError("not an ONNX file")

    try:
        module.check_classes(Path("x.onnx"), 18710, load=explode)
    except SystemExit as exit_:
        assert "not loadable" in str(exit_)
        return
    raise AssertionError("an unloadable model was accepted")


def test_a_missing_onnxruntime_skips_LOUDLY_and_does_not_refuse(module) -> None:
    def missing(path):
        raise ImportError("No module named 'onnxruntime'")

    printed = io.StringIO()
    with contextlib.redirect_stdout(printed):
        module.check_classes(Path("x.onnx"), 18710, load=missing)
    assert "NOT CHECKED" in printed.getvalue(), "skipped silently, which is the failure"


def test_the_default_loader_is_the_real_one(module) -> None:
    """A seam whose default drifted would make every test above a fiction."""
    import inspect

    default = inspect.signature(module.check_classes).parameters["load"].default
    assert default is module.onnxruntime_session


# --- main(), and its wiring ----------------------------------------------


def test_both_files_are_staged_on_the_happy_path(module) -> None:
    fixture = Fixture(module, build_pins(FAKE_MODEL, EXPECTED_DICT))
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                run_with(module, fixture, FAKE_MODEL, FAKE_YML)
            except SystemExit:
                pass  # the synthetic payload is not loadable ONNX; that is fine
        assert (fixture.out / FAKE_NAME).is_file(), "the model was not staged"
        assert (fixture.out / FAKE_DICT_NAME).is_file(), "the dictionary was not staged"
        assert (fixture.out / FAKE_DICT_NAME).read_bytes() == EXPECTED_DICT
    finally:
        fixture.close()


def test_main_ACTUALLY_INVOKES_the_class_guard(module) -> None:
    """PR #88 round 4, F1, applied before a reviewer has to find it here.

    A guard wired to nothing is decorative, and deleting its call site is
    invisible to every other test in this file.
    """
    fixture = Fixture(module, build_pins(FAKE_MODEL, EXPECTED_DICT))
    seen: list[tuple[Path, int]] = []
    real = module.check_classes
    module.check_classes = lambda path, expected, **_: seen.append((Path(path), expected))
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                run_with(module, fixture, FAKE_MODEL, FAKE_YML)
            except SystemExit:
                pass
    finally:
        module.check_classes = real
        fixture.close()
    assert seen, "main() staged the recogniser without invoking check_classes"
    assert seen[0][0].name == FAKE_NAME
    assert seen[0][1] == FAKE_CLASSES, (
        "the guard was called with " + str(seen[0][1]) + ", not the pinned count"
    )


def test_a_bad_model_digest_writes_NOTHING(module) -> None:
    # SAME LENGTH, different content, so the digest check is what fires rather
    # than the size check in front of it. A size mismatch is a different
    # failure and it already has its own path.
    corrupt = bytes(FAKE_MODEL[:-1]) + bytes([FAKE_MODEL[-1] ^ 0xFF])
    fixture = Fixture(module, build_pins(corrupt, EXPECTED_DICT))
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                run_with(module, fixture, FAKE_MODEL, FAKE_YML)
            except SystemExit as exit_:
                assert "hashes to" in str(exit_)
                assert not fixture.out.exists(), "refused and still wrote"
                return
        raise AssertionError("a mismatched model digest was accepted")
    finally:
        fixture.close()


def test_a_bad_DICTIONARY_digest_writes_NOTHING_INCLUDING_THE_MODEL(module) -> None:
    """The ordering guarantee, and the reason it matters.

    The model verifies first. If the dictionary is written only after its own
    check but the model was written after ITS check, a dictionary failure leaves
    a verified model on disk with no dictionary beside it -- which is exactly
    the I-333 mismatch, on disk, staged for the installer. `acquire-onnxruntime`
    has tests for this because an earlier version of it did precisely that.
    """
    corrupt = bytes(EXPECTED_DICT[:-1]) + bytes([EXPECTED_DICT[-1] ^ 0xFF])
    fixture = Fixture(module, build_pins(FAKE_MODEL, corrupt))
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                run_with(module, fixture, FAKE_MODEL, FAKE_YML)
            except SystemExit as exit_:
                assert "hashes to" in str(exit_)
                assert not fixture.out.exists(), (
                    "the dictionary failed and the MODEL was staged anyway"
                )
                return
        raise AssertionError("a mismatched dictionary digest was accepted")
    finally:
        fixture.close()


def test_a_renamed_pin_stops_the_run(module) -> None:
    fixture = Fixture(
        module,
        build_pins(FAKE_MODEL, EXPECTED_DICT, digest_name="RECOGNITION_SHA256_OLD"),
    )
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            try:
                run_with(module, fixture, FAKE_MODEL, FAKE_YML)
            except SystemExit as exit_:
                assert "RECOGNITION_SHA256" in str(exit_)
                return
        raise AssertionError("a missing pin did not stop the run")
    finally:
        fixture.close()


def test_a_pin_that_escapes_the_staging_directory_is_REFUSED(module) -> None:
    """PR #88 round 4, F3. Checked for BOTH names, not just the model's."""
    for field in ("name", "dict_name"):
        fixture = Fixture(
            module,
            build_pins(FAKE_MODEL, EXPECTED_DICT, **{field: "../../escaped-rec.onnx"}),
        )
        escaped = fixture.root.parent / "escaped-rec.onnx"
        escaped.unlink(missing_ok=True)
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                try:
                    run_with(module, fixture, FAKE_MODEL, FAKE_YML)
                except SystemExit as exit_:
                    assert "separator" in str(exit_), str(exit_)
                    assert not escaped.exists(), "refused and still wrote outside --out"
                    continue
            raise AssertionError(field + " containing .. was accepted")
        finally:
            escaped.unlink(missing_ok=True)
            fixture.close()


def test_a_non_https_url_is_refused_at_the_socket(module) -> None:
    try:
        module.fetch("http://example.invalid/rec.onnx")
    except SystemExit as exit_:
        assert "non-HTTPS" in str(exit_)
        return
    raise AssertionError("an http URL was fetched")


def test_the_real_pins_still_parse(module) -> None:
    """The synthetic tests would all pass against a parser gone blind to the
    REAL file, so this is the one that keeps them honest."""
    pins = read_real_pins(module)
    assert str(pins["RECOGNITION_FILE_NAME"]).endswith(".onnx")
    assert len(str(pins["RECOGNITION_SHA256"])) == 64
    assert str(pins["RECOGNITION_URL"]).startswith("https://")
    assert str(pins["DICTIONARY_URL"]).startswith("https://")
    assert isinstance(pins["RECOGNITION_SIZE"], int) and pins["RECOGNITION_SIZE"] > 0
    assert isinstance(pins["RECOGNITION_CLASS_COUNT"], int)
    assert pins["RECOGNITION_CLASS_COUNT"] > 1


def read_real_pins(module):
    return module.read_pins()


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
