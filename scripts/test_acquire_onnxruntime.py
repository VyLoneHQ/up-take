#!/usr/bin/env python3
"""Tests for the ONNX Runtime acquisition step.

Written because round 1 of `I-337`'s independent review found the script's
central invariant to be true per file and false across the set. The doc comment
claimed *"nothing that fails a check ever reaches the staging directory"*; the
loop wrote each member as it verified it, so an archive whose DLL matched and
whose `LICENSE` did not left a verified runtime on disk **with no licence beside
it** and exited 1. That is the precise state the licence obligation exists to
prevent, and nothing downstream can see it: `cargo deny` walks the crate graph,
the packaging test reads `tauri.conf.json` rather than the disk, and the
application verifies the DLL's digest and not the notices'.

The fix was two phases. These tests are what stop it regressing, and
`test_a_failing_member_leaves_nothing_behind` is the review's own drill turned
into a control.

**No 80 MB download.** Every test builds a tiny synthetic archive and a matching
synthetic pins file, then points the module's `PINS_SOURCE` at it. That is what
lets these run in CI on every push, and it also means they test the script's
LOGIC rather than one particular release's bytes.

**One of these has been drilled, and naming which is the honest form of the
claim.** `test_a_failing_member_leaves_nothing_behind` was confirmed red by
reverting `acquire-onnxruntime.py` to the write-as-you-verify shape the review
found: it fails with *"NOTHING may be staged when any member fails, and these
were: ['onnxruntime.dll']"*. The other six are reasoned, not observed. That
distinction is exactly what `scripts/test_dash_ratchet.py`'s own docstring had
to be corrected to make, after a review took its coverage claim at its word and
found an untested guard behind it: a coverage claim is a claim, and the honest
form names the set it covers.

A test that cannot go red is the defect this repository keeps finding
(`UT-F-40`, `UT-F-44`, `UT-F-52`, `UT-F-75`).

Run: `python3 scripts/test_acquire_onnxruntime.py`
"""

from __future__ import annotations

import hashlib
import importlib.util
import io
import shutil
import sys
import tempfile
import traceback
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent

#: Stand-in contents. Small, distinct, and nothing like each other, so a test
#: that mixes two files up fails on the digest rather than passing by accident.
FAKE = {
    "RUNTIME": b"pretend this is onnxruntime.dll",
    "LICENCE": b"pretend this is the MIT licence",
    "NOTICES": b"pretend these are the third-party notices",
}

#: The installed names, matching the real module's own vocabulary.
NAMES = {
    "RUNTIME": "onnxruntime.dll",
    "LICENCE": "LICENSE-onnxruntime.txt",
    "NOTICES": "ThirdPartyNotices-onnxruntime.txt",
}

VERSION = "9.9.9"


def load_module():
    """Imports the script under test by path, since its name has a hyphen."""
    spec = importlib.util.spec_from_file_location(
        "acquire_onnxruntime", HERE / "acquire-onnxruntime.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load acquire-onnxruntime.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def build_archive(members: dict[str, bytes]) -> bytes:
    """A zip shaped like the real release: one versioned root directory."""
    root = "onnxruntime-win-x64-" + VERSION
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(root + "/lib/onnxruntime.dll", members["RUNTIME"])
        archive.writestr(root + "/LICENSE", members["LICENCE"])
        archive.writestr(root + "/ThirdPartyNotices.txt", members["NOTICES"])
    return buffer.getvalue()


def build_pins(archive: bytes, members: dict[str, bytes]) -> str:
    """A synthetic `onnxruntime.rs`, pinning the synthetic archive.

    Written in the real file's exact syntax, because the extraction under test
    is a regex over that syntax. A shape that only resembles it would test a
    parser nobody runs.
    """
    lines = [
        'pub const VERSION: &str = "' + VERSION + '";',
        'pub const ARCHIVE_URL: &str = "https://example.invalid/onnxruntime-'
        + VERSION
        + '.zip";',
        'pub const ARCHIVE_SHA256: &str = "' + hashlib.sha256(archive).hexdigest() + '";',
        "pub const ARCHIVE_SIZE: u64 = " + str(len(archive)) + ";",
    ]
    for key, name in NAMES.items():
        body = members[key]
        lines.append("pub const " + key + '_FILE_NAME: &str = "' + name + '";')
        lines.append(
            "pub const "
            + key
            + '_SHA256: &str = "'
            + hashlib.sha256(body).hexdigest()
            + '";'
        )
        lines.append("pub const " + key + "_SIZE: u64 = " + str(len(body)) + ";")
    return "\n".join(lines) + "\n"


class Harness:
    """A temporary directory holding a pins file, an archive and an output dir."""

    def __init__(self, members: dict[str, bytes], pin_members: dict[str, bytes]):
        self.directory = Path(tempfile.mkdtemp(prefix="acquire-ort-test-"))
        self.archive_bytes = build_archive(members)
        self.archive = self.directory / "release.zip"
        self.archive.write_bytes(self.archive_bytes)
        self.pins = self.directory / "onnxruntime.rs"
        # The pins describe `pin_members`, the archive contains `members`. They
        # are the same object in the happy path and deliberately different when
        # a test is driving a refusal.
        self.pins.write_text(
            build_pins(self.archive_bytes, pin_members), encoding="utf-8"
        )
        self.out = self.directory / "assets"

    def run(self, module) -> int:
        """Runs `main()` with the module pointed at this harness."""
        module.PINS_SOURCE = self.pins
        argv = sys.argv
        sys.argv = [
            "acquire-onnxruntime.py",
            "--out",
            str(self.out),
            "--archive",
            str(self.archive),
        ]
        try:
            return int(module.main() or 0)
        except SystemExit as raised:
            # `SystemExit` here usually carries a MESSAGE, not a number: every
            # refusal in the script under test is `raise SystemExit("...")`,
            # which Python prints and exits 1 for. `int()` on that message
            # raises `ValueError` and would report the script's correct refusal
            # as a broken test -- which is exactly what the first version of
            # this harness did, on three tests at once.
            code = raised.code
            if code is None:
                return 0
            if isinstance(code, int):
                return code
            return 1
        finally:
            sys.argv = argv

    def cleanup(self) -> None:
        shutil.rmtree(self.directory, ignore_errors=True)


def test_a_good_archive_stages_all_three_files(module) -> None:
    harness = Harness(FAKE, FAKE)
    try:
        assert harness.run(module) == 0, "a matching archive must succeed"
        for name in NAMES.values():
            assert (harness.out / name).is_file(), name + " was not staged"
        assert (
            harness.out / NAMES["RUNTIME"]
        ).read_bytes() == FAKE["RUNTIME"], "the staged bytes are not the archive's"
    finally:
        harness.cleanup()


def test_a_failing_member_leaves_nothing_behind(module) -> None:
    """The review's own drill, as a control.

    The archive's DLL matches its pin and the licence does not. Before the fix
    this wrote a verified `onnxruntime.dll` and then exited 1, leaving a runtime
    with no licence beside it. Removing the two-phase split in
    `acquire-onnxruntime.py` turns this red.
    """
    archive_members = dict(FAKE)
    archive_members["LICENCE"] = b"the wrong licence entirely"
    harness = Harness(archive_members, FAKE)
    try:
        assert harness.run(module) == 1, "a member that fails its pin must be refused"
        staged = list(harness.out.glob("*")) if harness.out.exists() else []
        assert staged == [], (
            "NOTHING may be staged when any member fails, and these were: "
            + str([p.name for p in staged])
        )
    finally:
        harness.cleanup()


def test_a_tampered_archive_is_refused_before_extraction(module) -> None:
    harness = Harness(FAKE, FAKE)
    try:
        tampered = bytearray(harness.archive.read_bytes())
        tampered[len(tampered) // 2] ^= 0xFF
        harness.archive.write_bytes(bytes(tampered))
        assert harness.run(module) == 1, "a tampered archive must be refused"
        assert not harness.out.exists(), "nothing may be staged from a tampered archive"
    finally:
        harness.cleanup()


def test_a_stale_file_from_an_earlier_version_is_replaced(module) -> None:
    """A staging directory must never describe two releases at once."""
    harness = Harness(FAKE, FAKE)
    try:
        harness.out.mkdir(parents=True)
        stale = harness.out / NAMES["RUNTIME"]
        stale.write_bytes(b"a runtime from some previous version")
        assert harness.run(module) == 0
        assert stale.read_bytes() == FAKE["RUNTIME"], "the stale file was not replaced"
    finally:
        harness.cleanup()


def test_a_renamed_pin_is_refused_rather_than_silently_missed(module) -> None:
    """`I-96`'s shape: a source-reading control must go red when it goes blind."""
    harness = Harness(FAKE, FAKE)
    try:
        harness.pins.write_text(
            harness.pins.read_text(encoding="utf-8").replace(
                "RUNTIME_SHA256", "RUNTIME_DIGEST"
            ),
            encoding="utf-8",
        )
        assert harness.run(module) == 1, "a pin this script cannot find must be fatal"
        assert not harness.out.exists()
    finally:
        harness.cleanup()


def test_a_non_https_url_is_refused_at_the_socket(module) -> None:
    """ADR-0032 decision 2 says HTTPS only, and `fetch` is where that is true."""
    try:
        module.fetch("http://example.invalid/onnxruntime.zip")
    except SystemExit as refusal:
        assert "non-HTTPS" in str(refusal), "the refusal must say why"
        return
    raise AssertionError("a plain-http URL must be refused before any socket opens")


def test_the_real_pins_still_parse(module) -> None:
    """The synthetic tests above would all pass against a parser that had gone
    blind to the REAL file, so this is the one that keeps them honest."""
    real = HERE.parent / "crates" / "uptake-assets" / "src" / "onnxruntime.rs"
    pins = module.parse_pins(real.read_text(encoding="utf-8"))
    assert pins["RUNTIME_FILE_NAME"] == "onnxruntime.dll"
    assert isinstance(pins["ARCHIVE_SIZE"], int) and pins["ARCHIVE_SIZE"] > 0
    assert str(pins["ARCHIVE_URL"]).startswith("https://")


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
