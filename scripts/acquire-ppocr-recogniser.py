#!/usr/bin/env python3
"""Fetches the PP-OCRv6 recogniser and derives its character dictionary.

Roadmap `1.33`, [`ADR-0037`]. The sibling of `acquire-ppocr-detector.py`, and
deliberately its shape: same pin extraction, same verify-before-write order,
same refusal style. `ADR-0037` supersedes `ADR-0034` for the recogniser as well
as the detector, so **nothing in UP-TAKE's OCR path is converted here any
more** and `convert-ppocr-models.py` produces nothing this product ships.

# Two artifacts, two different kinds of digest, and the difference matters

`RECOGNITION_FILE_NAME` is **Baidu's bytes**. Its digest proves the file arrived
intact and unaltered, and that is all a hash over someone else's artifact can
ever prove.

`DICTIONARY_FILE_NAME` is **ours**. PP-OCRv6 does not publish the character list
as a standalone file the way PP-OCRv4 did; it lives inside the model's own
`inference.yml` as the `character_dict` block. This script downloads that and
extracts it, so `DICTIONARY_SHA256` is a digest over *our extraction*. That is
the same distinction `ADR-0034` drew about conversion, and it is why the
extraction has to be deterministic: an upstream change must surface as a digest
mismatch rather than as a quietly different alphabet.

# The class-count check is the part that would otherwise fail silently

`RECOGNITION_CLASS_COUNT` and the dictionary are a matched pair. UP-TAKE `I-333`
is the record of getting it wrong by one: the engine refuses a mismatch at load
time, which is the right behaviour and is also the last possible moment. Here it
is checked at acquisition, against the model's own ONNX output dimension, so a
recogniser and a dictionary that do not belong together are refused before they
reach the staging directory.

**That guard takes a `load` seam, and the seam is the point.** `PR #88` rounds 3
and 4 were both this class in the sibling script: a guard whose refusal branch
executed in no CI job, then a guard whose *call site* was unexercised. Both are
avoided here by construction rather than by care, and
`test_acquire_ppocr_recogniser.py` drills both.

# Usage

    python3 scripts/acquire-ppocr-recogniser.py --out src-tauri/assets/models

[`ADR-0037`]: ../Projects/UP-TAKE/DECISIONS/ADR-0037-the-ocr-models-are-the-v6-small-pair.md
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import rust_consts  # noqa: E402

#: Read from the Rust source rather than restated, for the reason every other
#: pin is: two literals that must agree is how one gets rotated and the other
#: does not.
PINS_SOURCE = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "uptake-assets"
    / "src"
    / "ppocr.rs"
)

REQUIRED_STRINGS = (
    "RECOGNITION_FILE_NAME",
    "RECOGNITION_SHA256",
    "RECOGNITION_URL",
    "DICTIONARY_FILE_NAME",
    "DICTIONARY_SHA256",
    "DICTIONARY_URL",
)
REQUIRED_NUMBERS = ("RECOGNITION_SIZE", "DICTIONARY_SIZE")


def read_pins() -> dict[str, object]:
    """Extracts the recogniser's pins. Refuses rather than returning a partial map."""
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
    classes = re.search(
        r"pub const RECOGNITION_CLASS_COUNT:\s*usize\s*=\s*([0-9_]+)\s*;", source
    )
    if classes is None:
        raise SystemExit(
            "could not find `pub const RECOGNITION_CLASS_COUNT: usize` in "
            + str(PINS_SOURCE)
        )
    pins["RECOGNITION_CLASS_COUNT"] = int(classes.group(1).replace("_", ""))
    return pins


def fetch(url: str) -> bytes:
    """Downloads `url` into memory. HTTPS asserted at the line that opens a socket."""
    if not url.startswith("https://"):
        raise SystemExit("refusing to fetch over a non-HTTPS URL: " + url)
    print("  fetching " + url)
    with urllib.request.urlopen(url) as response:  # noqa: S310 - scheme asserted above
        return bytes(response.read())


def check(data: bytes, digest: str, size: int, what: str) -> None:
    """Refuses on either mismatch, naming both, before anything is written."""
    actual = hashlib.sha256(data).hexdigest()
    if len(data) != size:
        raise SystemExit(
            what + " is " + str(len(data)) + " bytes, and the pin says "
            + str(size) + ".\nNothing was written."
        )
    if actual != digest:
        raise SystemExit(
            what + " hashes to\n  " + actual + "\nand the pin says\n  " + digest
            + "\nNothing was written."
        )
    print("  verified " + what + "  (" + str(len(data)) + " bytes)")


def extract_dictionary(inference_yml: bytes) -> bytes:
    """Derives the character list from the model's own `inference.yml`.

    Deterministic on purpose: this output is what `DICTIONARY_SHA256` pins, so
    any wobble here reads as a corrupted download rather than as what it is.

    One line per entry, no trailing newline, which is the shape PP-OCRv4's
    `ppocr_keys_v1.txt` had and which `recognise.rs` reads. An entry that is
    blank in the YAML is the space character; PaddleOCR quotes it inconsistently
    across releases, so both forms are handled.
    """
    text = inference_yml.decode("utf-8")
    block = re.search(r"character_dict:\s*\n((?:[ \t]*-[ \t]?.*\n)+)", text)
    if block is None:
        raise SystemExit(
            "the downloaded inference.yml has no `character_dict:` block.\n"
            "Upstream changed its layout, which means the extraction below no"
            " longer describes the file. Fix the extraction; do not hand-copy a"
            " dictionary."
        )
    entries: list[str] = []
    for line in block.group(1).splitlines():
        value = line.strip()[1:]
        if value.startswith(" "):
            value = value[1:]
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
            value = value[1:-1]
        entries.append(value if value != "" else " ")
    if not entries:
        raise SystemExit("the `character_dict:` block is empty")
    return "\n".join(entries).encode("utf-8")


def onnxruntime_session(path: Path):
    """Opens `path` with onnxruntime. The default loader for [`check_classes`]."""
    import onnxruntime  # noqa: PLC0415

    return onnxruntime.InferenceSession(str(path), providers=["CPUExecutionProvider"])


def class_complaint(shape_out, expected: int) -> str | None:
    """The decision, as a value over plain data. No model, no onnxruntime, no file.

    Separated from [`check_classes`] for the reason `PR #88` round 3 made
    explicit: a decision that can only be reached through a loaded model is a
    decision no test in the `web` job can falsify.
    """
    if len(shape_out) < 3:
        return (
            "the recogniser's output has " + str(len(shape_out))
            + " dimensions, expected 3 (batch, timesteps, classes)."
        )
    classes = shape_out[-1]
    if not isinstance(classes, int):
        return (
            "the recogniser's class count is dynamic (" + repr(classes)
            + "), so it cannot be checked against RECOGNITION_CLASS_COUNT."
            "\nA CTC head with a symbolic alphabet size is not the shape this"
            " pipeline decodes."
        )
    if classes != expected:
        return (
            "the recogniser emits " + str(classes) + " classes and"
            " RECOGNITION_CLASS_COUNT is " + str(expected) + "."
            "\nThe model and the dictionary are a MATCHED PAIR (UP-TAKE I-333)."
            " Off by one here shifts every character the engine decodes."
        )
    return None


def check_classes(path: Path, expected: int, load=onnxruntime_session) -> None:
    """Refuses a recogniser whose alphabet does not match the pinned dictionary.

    Skipped, loudly, when `onnxruntime` is absent -- the absence arrives as an
    `ImportError` out of `load`, which is why that arm comes first. `load` is a
    seam, not a convenience: it is what makes every branch below reachable from
    a test with no `onnxruntime` installed.
    """
    try:
        session = load(path)
    except ImportError:
        print(
            "  NOT CHECKED: onnxruntime is not installed, so the recogniser's"
            " class count was not verified against the dictionary."
        )
        return
    except Exception as error:  # noqa: BLE001 - onnxruntime raises several types
        raise SystemExit(
            "the pinned recogniser is not loadable as ONNX: " + str(error)
            + "\nThe digest matched, so these ARE the pinned bytes -- which means"
            " the pin itself names something that is not a model."
        ) from error
    shape_out = session.get_outputs()[0].shape
    complaint = class_complaint(shape_out, expected)
    if complaint is not None:
        raise SystemExit(complaint)
    print("  classes " + str(shape_out[-1]) + ", matching RECOGNITION_CLASS_COUNT")


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
        "--model-file",
        type=Path,
        default=None,
        help="verify this file instead of downloading the model; checked identically",
    )
    parser.add_argument(
        "--yml-file",
        type=Path,
        default=None,
        help="extract from this inference.yml instead of downloading it",
    )
    arguments = parser.parse_args()

    pins = read_pins()
    # Validated before either is joined onto --out. PR #88 round 4, F3: a decoy
    # pin of "../../escaped.onnx" wrote above the staging directory, with the
    # digest and size checks passing, because they check bytes not destination.
    model_name = rust_consts.plain_file_name(
        str(pins["RECOGNITION_FILE_NAME"]), "RECOGNITION_FILE_NAME"
    )
    dictionary_name = rust_consts.plain_file_name(
        str(pins["DICTIONARY_FILE_NAME"]), "DICTIONARY_FILE_NAME"
    )
    print("PP-OCRv6 recogniser: " + model_name)

    if arguments.model_file is not None:
        print("  reading " + str(arguments.model_file))
        model = arguments.model_file.read_bytes()
    else:
        model = fetch(str(pins["RECOGNITION_URL"]))
    check(
        model,
        str(pins["RECOGNITION_SHA256"]),
        int(pins["RECOGNITION_SIZE"]),  # type: ignore[arg-type]
        model_name,
    )

    if arguments.yml_file is not None:
        print("  reading " + str(arguments.yml_file))
        inference_yml = arguments.yml_file.read_bytes()
    else:
        inference_yml = fetch(str(pins["DICTIONARY_URL"]))
    dictionary = extract_dictionary(inference_yml)
    check(
        dictionary,
        str(pins["DICTIONARY_SHA256"]),
        int(pins["DICTIONARY_SIZE"]),  # type: ignore[arg-type]
        dictionary_name,
    )

    # Both verified before either is written. `acquire-onnxruntime.py`'s tests
    # exist because an earlier version wrote each member as it verified it,
    # which left a verified model on disk with no dictionary beside it -- and a
    # model without its matching dictionary is the I-333 failure on disk.
    arguments.out.mkdir(parents=True, exist_ok=True)
    model_target = arguments.out / model_name
    model_target.write_bytes(model)
    print("  wrote " + str(model_target))
    dictionary_target = arguments.out / dictionary_name
    dictionary_target.write_bytes(dictionary)
    print("  wrote " + str(dictionary_target))

    # After the write, because onnxruntime loads from a path. The digests have
    # already passed, so this asserts what the file IS rather than whether it
    # arrived intact.
    check_classes(model_target, int(pins["RECOGNITION_CLASS_COUNT"]))  # type: ignore[arg-type]
    print("")
    print(
        "Verified against the pins in crates/uptake-assets/src/ppocr.rs."
        " The model is Baidu's bytes; the dictionary is this repository's"
        " extraction of them (ADR-0037)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
