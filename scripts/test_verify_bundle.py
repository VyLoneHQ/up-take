#!/usr/bin/env python3
"""Tests for the bundle checker's byte verification (BACKLOG.md I-346).

The backlog row that asked for this check carries a warning, and it is the
reason this file exists rather than a `--help` and a hope:

    This is NEW CONTROL CODE in the area that produced a defect in three
    consecutive review rounds (I-337's rounds 1, 3 and 4, each finding a hole
    in the previous round's fix).

Every one of those holes was the same shape: a control that reported success
over the thing it was not looking at. So these tests are pointed at the ways
THIS control could do that, not at the happy path:

* a resource nobody pinned and nobody declared unpinned, passing silently;
* an unpinned resource being skipped rather than named;
* a missing source file being treated as nothing to check;
* the pin extraction going blind and every comparison passing vacuously.

**Which of these has been drilled, named rather than claimed.** The digest
mismatch and the restore were driven end to end against the real 31 MB assets
and a real generated NSIS and MSI pair on 2026-09-04: appending one byte to
`ppocr_keys_v1.txt` made `verify-bundle.py` exit 1 naming that file, both
digests and the path, and restoring the file made it exit 0. The other tests
here are reasoned from the code, not observed as regressions. That distinction
is the one `test_dash_ratchet.py`'s docstring had to be corrected to make.

No 31 MB fixture. Every test below builds tiny synthetic files and a matching
synthetic pins source, then points the module's own tables at them.

Run: `python3 scripts/test_verify_bundle.py`
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import shutil
import sys
import tempfile
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load_module():
    """Imports the script under test by path, since its name has a hyphen."""
    spec = importlib.util.spec_from_file_location(
        "verify_bundle", HERE / "verify-bundle.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load verify-bundle.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def build_pins(name: str, value: str) -> str:
    """A synthetic Rust pins file, in the real files' exact syntax.

    The extraction under test is a regex over that syntax, so a shape that only
    resembles it would test a parser nobody runs.
    """
    return (
        "//! Synthetic pins.\n\n"
        "/// A digest.\n"
        'pub const ' + name + ': &str = "' + value + '";\n'
    )


class Fixture:
    """A throwaway repository shaped like the real one, for one test.

    Holds a `src-tauri/tauri.release.conf.json`, a `src-tauri/assets/` tree and
    a `crates/uptake-assets/src/` pins directory, and rebinds the module's
    tables at them. Restores every table on exit, so one test cannot leak its
    mapping into the next.
    """

    def __init__(self, module, resources: dict[str, str], pinned: dict, unpinned: dict):
        self.module = module
        self.root = Path(tempfile.mkdtemp(prefix="verify-bundle-test-"))
        self.config = self.root / "src-tauri" / "tauri.release.conf.json"
        self.config.parent.mkdir(parents=True, exist_ok=True)
        self.config.write_text(
            json.dumps({"bundle": {"resources": resources}}), encoding="utf-8"
        )
        self.pins_directory = self.root / "crates" / "uptake-assets" / "src"
        self.pins_directory.mkdir(parents=True, exist_ok=True)
        self.saved = (module.PINNED, module.UNPINNED, module.PINS_DIRECTORY)
        module.PINNED = pinned
        module.UNPINNED = unpinned
        module.PINS_DIRECTORY = self.pins_directory

    def write_asset(self, source_path: str, data: bytes) -> None:
        path = self.config.parent / source_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def write_pins(self, rust_file: str, contents: str) -> None:
        (self.pins_directory / rust_file).write_text(contents, encoding="utf-8")

    def close(self) -> None:
        self.module.PINNED, self.module.UNPINNED, self.module.PINS_DIRECTORY = self.saved
        shutil.rmtree(self.root, ignore_errors=True)


def test_matching_bytes_produce_no_problem(module) -> None:
    payload = b"pretend this is a model"
    fixture = Fixture(
        module,
        {"assets/model.onnx": "model.onnx"},
        {"assets/model.onnx": ("pins.rs", "MODEL_SHA256")},
        {},
    )
    try:
        fixture.write_asset("assets/model.onnx", payload)
        fixture.write_pins("pins.rs", build_pins("MODEL_SHA256", digest(payload)))
        problems, unverified = module.byte_problems(
            fixture.config, module.resource_map(fixture.config)
        )
        assert problems == [], problems
        assert unverified == [], unverified
    finally:
        fixture.close()


def test_one_changed_byte_is_refused(module) -> None:
    """The gap I-346 names: someone edits `src-tauri/assets/` after acquisition."""
    fixture = Fixture(
        module,
        {"assets/model.onnx": "model.onnx"},
        {"assets/model.onnx": ("pins.rs", "MODEL_SHA256")},
        {},
    )
    try:
        fixture.write_asset("assets/model.onnx", b"tampered")
        fixture.write_pins("pins.rs", build_pins("MODEL_SHA256", digest(b"original")))
        problems, _ = module.byte_problems(
            fixture.config, module.resource_map(fixture.config)
        )
        assert len(problems) == 1, problems
        assert "does NOT match its pinned digest" in problems[0]
        # Both digests are printed. A refusal that says "mismatch" without
        # saying which two values it compared cannot be acted on.
        assert digest(b"original") in problems[0]
        assert digest(b"tampered") in problems[0]
    finally:
        fixture.close()


def test_a_resource_with_no_pin_and_no_declaration_is_refused(module) -> None:
    """The silent-skip failure, which is the shape all three I-337 holes took.

    A resource added to the release config and to neither table must REFUSE
    rather than pass unchecked. Without this, adding a bundled file is enough
    to opt it out of verification and nothing says so.
    """
    fixture = Fixture(module, {"assets/new.bin": "new.bin"}, {}, {})
    try:
        fixture.write_asset("assets/new.bin", b"whatever")
        problems, unverified = module.byte_problems(
            fixture.config, module.resource_map(fixture.config)
        )
        assert len(problems) == 1, problems
        assert "neither pinned in PINNED nor listed in UNPINNED" in problems[0]
        assert unverified == []
    finally:
        fixture.close()


def test_a_declared_unpinned_resource_is_named_rather_than_hidden(module) -> None:
    fixture = Fixture(
        module,
        {"assets/generated.txt": "generated.txt"},
        {},
        {"assets/generated.txt": "generated by a script, no upstream digest"},
    )
    try:
        fixture.write_asset("assets/generated.txt", b"whatever")
        problems, unverified = module.byte_problems(
            fixture.config, module.resource_map(fixture.config)
        )
        assert problems == [], problems
        assert len(unverified) == 1, unverified
        assert "assets/generated.txt" in unverified[0]
        # The reason travels with it. A bare name tells a reader something was
        # skipped and not whether that was intended.
        assert "no upstream digest" in unverified[0]
    finally:
        fixture.close()


def test_a_missing_source_file_is_refused_not_skipped(module) -> None:
    """A pinned resource that is not on disk must not read as nothing to check."""
    fixture = Fixture(
        module,
        {"assets/model.onnx": "model.onnx"},
        {"assets/model.onnx": ("pins.rs", "MODEL_SHA256")},
        {},
    )
    try:
        fixture.write_pins("pins.rs", build_pins("MODEL_SHA256", digest(b"x")))
        problems, _ = module.byte_problems(
            fixture.config, module.resource_map(fixture.config)
        )
        assert len(problems) == 1, problems
        assert "is not on disk" in problems[0]
    finally:
        fixture.close()


def test_a_renamed_pin_constant_stops_the_run(module) -> None:
    """The parser going blind must be loud.

    If the extraction silently returned nothing, every comparison above would
    pass vacuously and this whole file would be a control that cannot go red.
    """
    fixture = Fixture(
        module,
        {"assets/model.onnx": "model.onnx"},
        {"assets/model.onnx": ("pins.rs", "MODEL_SHA256")},
        {},
    )
    try:
        fixture.write_asset("assets/model.onnx", b"x")
        fixture.write_pins("pins.rs", build_pins("RENAMED_SHA256", digest(b"x")))
        try:
            module.byte_problems(fixture.config, module.resource_map(fixture.config))
        except SystemExit as exit_error:
            assert "MODEL_SHA256" in str(exit_error)
        else:
            raise AssertionError("a missing pin constant must stop the run")
    finally:
        fixture.close()


def test_an_empty_resource_map_is_refused(module) -> None:
    """A config naming nothing must not report success over nothing."""
    fixture = Fixture(module, {}, {}, {})
    try:
        fixture.config.write_text(
            json.dumps({"bundle": {"resources": {}}}), encoding="utf-8"
        )
        try:
            module.resource_map(fixture.config)
        except SystemExit as exit_error:
            assert "names no bundle.resources" in str(exit_error)
        else:
            raise AssertionError("an empty resource map must stop the run")
    finally:
        fixture.close()


def test_the_real_tables_cover_the_real_release_config(module) -> None:
    """The synthetic tests would all pass with the REAL tables gone stale.

    This is the one that keeps them honest: every resource the shipping config
    names must be in exactly one of the two tables, and every pin named must be
    readable out of the real Rust source. It needs no assets on disk, so it
    runs in CI, where they do not exist.
    """
    resources = module.resource_map(module.RELEASE_CONFIG)
    assert resources, "the real release config names no resources"
    for source_path in resources:
        pinned = source_path in module.PINNED
        unpinned = source_path in module.UNPINNED
        assert pinned != unpinned, (
            source_path + " must be in exactly one of PINNED and UNPINNED"
        )
    digests = module.pinned_digests()
    assert set(digests) == set(module.PINNED)
    for name, value in digests.items():
        assert len(value) == 64, name + " has a digest that is not 64 hex characters"
        assert value == value.lower(), name + " has a digest that is not lowercase"


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
