#!/usr/bin/env python3
"""Tests for the model licence notice (PR #89 round 1, F2).

# The defect this exists to make impossible

`ADR-0037` moved the recogniser to Baidu's published ONNX, and the notice was
still being written by the conversion script from what that script produced. So
it named `ch_PP-OCRv4_rec.onnx` and `ppocr_keys_v1.txt`, which are **not
shipped**, and omitted `PP-OCRv6_small_rec.onnx` and `ppocr_keys_v6_small.txt`,
which **are**. An Apache-2.0 section 4 notice missing two of the four files it
must cover is a licence defect, and nothing could see it: `cargo deny` walks the
crate graph and sees no `.onnx`, and `verify-bundle.py` lists the notice under
`UNPINNED` so its contents are never checked.

So the load-bearing test here is not that the notice is well-formed. It is
`test_the_notice_names_exactly_what_the_installer_bundles`, which reads
`tauri.release.conf.json` and compares. Any future asset added to the installer
and not to the notice fails there.

Run: `python3 scripts/test_write_model_notice.py`
"""

from __future__ import annotations

import importlib.util
import json
import shutil
import sys
import tempfile
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
RELEASE_CONF = ROOT / "src-tauri" / "tauri.release.conf.json"


def load_module():
    spec = importlib.util.spec_from_file_location(
        "write_model_notice", HERE / "write-model-notice.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load write-model-notice.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def written(module) -> str:
    out = Path(tempfile.mkdtemp(prefix="notice-test-"))
    try:
        argv = sys.argv
        sys.argv = ["write-model-notice.py", "--out", str(out)]
        try:
            module.main()
        finally:
            sys.argv = argv
        return (out / "NOTICE-models.txt").read_text(encoding="utf-8")
    finally:
        shutil.rmtree(out, ignore_errors=True)


def bundled_model_files() -> set[str]:
    """The model assets the installer actually carries, from its own config."""
    resources = json.loads(RELEASE_CONF.read_text(encoding="utf-8"))["bundle"]["resources"]
    names = set()
    for source in resources:
        if "/models/" not in source:
            continue
        name = source.rsplit("/", 1)[-1]
        if name == "NOTICE-models.txt":
            continue  # the notice does not name itself
        names.add(name)
    return names


def test_the_notice_names_exactly_what_the_installer_bundles(module) -> None:
    """The one that would have caught F2, and the reason this file exists."""
    text = written(module)
    bundled = bundled_model_files()
    assert bundled, "read NO model resources from the release config; the test is blind"

    missing = sorted(name for name in bundled if name not in text)
    assert not missing, (
        "the installer ships these and the notice does not name them, which is "
        "an Apache-2.0 section 4 defect: " + str(missing)
    )

    # And the other direction, which is how F2 actually presented: the notice
    # named two files nobody receives.
    pins = module.read_pins()
    named = {str(pins[prefix + "_FILE_NAME"]) for prefix, _ in module.COVERED}
    phantom = sorted(named - bundled)
    assert not phantom, (
        "the notice names files the installer does not ship: " + str(phantom)
    )


def test_every_covered_file_carries_a_digest_and_a_size(module) -> None:
    text = written(module)
    pins = module.read_pins()
    for prefix, _ in module.COVERED:
        digest = str(pins[prefix + "_SHA256"])
        assert digest in text, prefix + "'s digest is missing from the notice"
        assert str(pins[prefix + "_SIZE"]) in text, prefix + "'s size is missing"


def test_the_notice_states_what_was_modified_and_what_was_not(module) -> None:
    """Apache 2.0 section 4(b). Both halves must be present and distinguished:
    the models are redistributed unchanged, the dictionary is extracted here."""
    text = written(module)
    assert "REDISTRIBUTED UNCHANGED" in text
    assert "MODIFIED HERE" in text
    assert "extracted" in text.lower()


def test_a_missing_pin_REFUSES_rather_than_writing_a_short_notice(module) -> None:
    """A partial read is the failure mode that produced F2 in the first place:
    a notice that is written, looks fine, and omits a file."""
    saved = module.PINS_SOURCE
    scratch = Path(tempfile.mkdtemp(prefix="notice-pins-"))
    try:
        blind = scratch / "ppocr.rs"
        blind.write_text("pub const NOTHING: &str = \"x\";\n", encoding="utf-8")
        module.PINS_SOURCE = blind
        try:
            module.read_pins()
        except SystemExit as exit_:
            assert "could not find" in str(exit_)
            return
        raise AssertionError("a pins file with no pins was accepted")
    finally:
        module.PINS_SOURCE = saved
        shutil.rmtree(scratch, ignore_errors=True)


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
