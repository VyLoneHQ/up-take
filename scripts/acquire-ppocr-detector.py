#!/usr/bin/env python3
"""Acquires the PP-OCRv6 detector UP-TAKE ships, and verifies every byte of it.

This is `ADR-0036`'s acquisition step. That record decided the DETECTOR is taken
as Baidu's own published ONNX rather than converted here, so this script
downloads one file and checks it against a pin, exactly as
`acquire-onnxruntime.py` does for the runtime.

Why the detector is downloaded and the recogniser is converted
-------------------------------------------------------------

`ADR-0034` chose to convert PaddleOCR's official release ourselves rather than
take a conversion, because the alternative on the table was **a third party's**
ONNX and "a checksum here pins their bytes and proves nothing about provenance".
That objection is about an unaccountable converter and it still holds.

Baidu publishing PP-OCRv6 as ONNX under its own name is a different thing, and
`ADR-0034` never considered it. So the split is deliberate:

    detector    -> downloaded here, digest pins BAIDU's artifact
    recogniser  -> converted by convert-ppocr-models.py, digest pins OURS
    dictionary  -> copied byte for byte by that same script

⚠️ **A reader of `convert-ppocr-models.py` will find it covers only two of the
three files.** That is stated in its header too. Two acquisition mechanisms
where there was one is the cost `ADR-0036` accepted knowingly.

What a digest mismatch means here, and what it does not
------------------------------------------------------

The pin cannot distinguish "Baidu republished the file" from "somebody
substituted it", and it must not try. Both are refusals. If this script starts
refusing a clean download, that is a **decision to make**, not a constant to
edit: read `ADR-0036` first.

Usage
-----

    python scripts/acquire-ppocr-detector.py --out src-tauri/assets/models

Add `--file <path>` to verify a copy already on disk instead of downloading.
The check is identical either way.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import urllib.request
from pathlib import Path

# Sibling module. This script and its tests run as `python scripts/<name>.py`
# from the repository root, which puts `scripts/` on sys.path.
import rust_consts

#: Where the pins live. One source, read rather than copied -- the same
#: discipline `acquire-onnxruntime.py` applies to the runtime's pins, and for
#: the same reason: a second hand-maintained copy of a digest drifts the moment
#: the pin moves, and drifts silently in the worse direction.
PINS_SOURCE = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "uptake-assets"
    / "src"
    / "ppocr.rs"
)

#: The constants this script needs, and the type each is expected to have.
#: Named explicitly so a REMOVED constant is an error here rather than a
#: KeyError three functions later.
REQUIRED_STRINGS = ("DETECTION_FILE_NAME", "DETECTION_SHA256", "DETECTION_URL")
REQUIRED_NUMBERS = ("DETECTION_SIZE",)


def read_pins() -> dict[str, object]:
    """Extracts the detector's pins from the Rust source.

    Raises `SystemExit` rather than returning a partial mapping. A parser that
    silently finds nothing is a control that cannot go red.
    """
    if not PINS_SOURCE.is_file():
        raise SystemExit("cannot find the pins at " + str(PINS_SOURCE))
    source = PINS_SOURCE.read_text(encoding="utf-8")
    pins: dict[str, object] = {}
    for name in REQUIRED_STRINGS:
        value = rust_consts.string_const(source, name)
        if value is None:
            raise SystemExit(
                "could not find `pub const " + name + ": &str` in "
                + str(PINS_SOURCE)
                + ".\nThe pins moved and this script cannot read them. Fix the"
                " extraction rather than copying the value here."
            )
        pins[name] = value
    for name in REQUIRED_NUMBERS:
        number = rust_consts.u64_const(source, name)
        if number is None:
            raise SystemExit(
                "could not find `pub const " + name + ": u64` in " + str(PINS_SOURCE)
            )
        pins[name] = number
    return pins


def fetch(url: str) -> bytes:
    """Downloads `url` into memory.

    HTTPS is asserted here as well as pinned in the Rust source, because this is
    the line that opens a socket. Asserting it at the point of use is what makes
    it a check rather than a description -- `acquire-onnxruntime.py` says the
    same thing about the same line.
    """
    if not url.startswith("https://"):
        raise SystemExit("refusing to fetch over a non-HTTPS URL: " + url)
    print("  fetching " + url)
    with urllib.request.urlopen(url) as response:  # noqa: S310 - scheme asserted above
        return response.read()


def check(data: bytes, expected_digest: str, expected_size: int) -> None:
    """Refuses `data` unless it matches both pins.

    Size first, because it produces the more useful message: a wrong-file
    download reports "this is not the file we expected" rather than a hash
    mismatch that could mean either corruption or the wrong artifact.
    """
    if len(data) != expected_size:
        raise SystemExit(
            "the detector is the wrong size.\n"
            "  expected " + str(expected_size) + " bytes\n"
            "  actual   " + str(len(data)) + " bytes\n"
            "This is usually the wrong file rather than a corrupted one."
        )
    actual = hashlib.sha256(data).hexdigest()
    if actual != expected_digest:
        raise SystemExit(
            "the detector does not match its pinned digest.\n"
            "  expected " + expected_digest + "\n"
            "  actual   " + actual + "\n"
            "Refusing to use it. Nothing has been written.\n"
            "\n"
            "This does NOT tell you whether Baidu republished the file or\n"
            "somebody substituted it, and it is not meant to. Read ADR-0036\n"
            "before changing the pin."
        )


def shape_complaint(shape_in: list, shape_out: list) -> str | None:
    """What is wrong with these tensor shapes for a DB detector, or `None`.

    # This is split out because a test could not otherwise reach it

    Round 2 of `PR #88`'s review deleted BOTH refusals inside [`check_shape`]
    and the suite stayed at 7/7 green. The test meant to cover them skipped:
    locally it needs a gitignored model file to use as a wrong-shaped stand-in,
    and in the CI job it runs from, `onnxruntime` is not installed at all. So in
    every position it is actually run from, its assertion never executed.

    That is the "control that cannot go red" class this project keeps finding,
    reappearing inside the fix for the previous instance of it -- on the guard
    this script's own docstring calls the one that matters most.

    The decision is a value now, and the shapes are plain lists. No model, no
    `onnxruntime`, no file. [`check_shape`] loads and prints; this decides.
    """
    if shape_in[1] != 3:
        return (
            "detector takes " + str(shape_in[1]) + " channels, expected 3."
            "\nThe pinned file is not the network this pipeline feeds."
        )
    if shape_out[1] != 1:
        return (
            "detector emits " + str(shape_out[1])
            + " channels, expected a 1-channel probability map."
            "\nDB post-processing in detect.rs reads one probability per pixel."
        )
    return None


def check_shape(path: Path) -> None:
    """Refuses a detector whose tensor shapes are not what this pipeline feeds.

    # Why a byte check is not enough here, and was enough before

    `ADR-0034`'s conversion produced the ONNX from a model this project had
    already integrated, so its shapes could only change if we changed them. This
    file is **somebody else's build of a different model generation**, and the
    digest proves only that the bytes arrived intact -- it says nothing about
    whether they describe a network `preprocess.rs` can feed or `detect.rs` can
    read.

    So the shape contract is asserted: three input channels (RGB, normalised)
    and a single-channel probability map out, which is what DB post-processing
    consumes. This check MOVED here from `convert-ppocr-models.py` along with
    the detector, and it matters more on bytes we did not produce than on the
    ones we did.

    Skipped, loudly, when `onnxruntime` is absent -- the same choice the
    converter makes, and for the same reason: a check that can vanish without
    saying so reports green forever.
    """
    try:
        import onnxruntime  # noqa: PLC0415
    except ImportError:
        print(
            "  NOT CHECKED: onnxruntime is not installed, so the detector's"
            " shapes were not verified. Install it to enable this."
        )
        return

    try:
        session = onnxruntime.InferenceSession(
            str(path), providers=["CPUExecutionProvider"]
        )
    except Exception as error:  # noqa: BLE001 - onnxruntime raises several types
        # A refusal, not a traceback. The digest has already matched, so
        # reaching here means the PINNED bytes are not loadable ONNX at all --
        # which is a far more alarming condition than a mismatch and deserves a
        # sentence rather than a stack trace.
        #
        # Found by this script's own tests (`PR #88` round 1 asked for them):
        # the happy-path test fed it a small synthetic payload and got an
        # unhandled exception instead of a verdict.
        raise SystemExit(
            "the pinned detector is not loadable as ONNX: " + str(error)
            + "\nThe digest matched, so these ARE the pinned bytes -- which means"
            " the pin itself names something that is not a model."
        ) from error
    shape_in = session.get_inputs()[0].shape
    shape_out = session.get_outputs()[0].shape
    complaint = shape_complaint(shape_in, shape_out)
    if complaint is not None:
        raise SystemExit(complaint)
    print("  shapes " + str(shape_in) + " -> " + str(shape_out))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("src-tauri/assets/models"),
        help="staging directory the installer packages"
        " (default: src-tauri/assets/models)",
    )
    parser.add_argument(
        "--file",
        type=Path,
        default=None,
        help="verify this file instead of downloading; checked identically",
    )
    arguments = parser.parse_args()

    pins = read_pins()
    file_name = str(pins["DETECTION_FILE_NAME"])
    print("PP-OCRv6 detector: " + file_name)

    if arguments.file is not None:
        print("  reading " + str(arguments.file))
        data = arguments.file.read_bytes()
    else:
        data = fetch(str(pins["DETECTION_URL"]))

    # Verified BEFORE anything is written. `acquire-onnxruntime.py`'s own tests
    # exist because an earlier version of that script wrote each member as it
    # verified it, which left a verified runtime on disk with no licence beside
    # it. Nothing that fails a check reaches the staging directory.
    check(data, str(pins["DETECTION_SHA256"]), int(pins["DETECTION_SIZE"]))

    arguments.out.mkdir(parents=True, exist_ok=True)
    target = arguments.out / file_name
    target.write_bytes(data)
    print("  wrote " + str(target) + "  (" + str(len(data)) + " bytes)")
    # After the write, because onnxruntime loads from a path rather than from
    # bytes. The digest already passed, so what is on disk is the pinned file;
    # this is asserting what that file IS, not whether it arrived intact.
    check_shape(target)
    print("")
    print(
        "Verified against the pin in crates/uptake-assets/src/ppocr.rs."
        " These are Baidu's bytes, not ours (ADR-0036)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
