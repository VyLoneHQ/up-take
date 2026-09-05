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

    # This was hand-rolled and it was wrong, in two entries out of 18,708

    Round 1 of `PR #89`'s review diffed the hand-rolled version against a real
    YAML parse of the same file and found two divergences, both invisible to
    every check around them:

    * **index 6.** Upstream writes a YAML single-quoted scalar whose doubled
      quote is one escaped apostrophe. Stripping the outer pair by hand left
      **two** characters. CTC class 7 then decoded to two apostrophes, so every
      apostrophe the recogniser read came out doubled: `Kund's` as `Kund''s`.
    * **index 1748.** Upstream writes U+3000 IDEOGRAPHIC SPACE. `str.strip()`
      strips **all** Unicode whitespace, so the value became empty, and the
      blank-is-a-space rule then substituted ASCII U+0020.

    **Nothing could see either.** The entry COUNT was unaffected, so
    `check_classes` passed; `DICTIONARY_SIZE` and `DICTIONARY_SHA256` were
    measured from the corrupted output, so the pin certified the corruption
    rather than detecting it. That is the exact outcome this module's header
    claims the design prevents -- *"a change upstream shows up as a digest
    mismatch rather than as a silently different alphabet"* -- and the alphabet
    was already silently different on the first acquisition.

    So it is a real parse now. The file is valid YAML fetched from a URL this
    repository already pins, and hand-rolling a parser for it bought nothing.

    # The post-condition, which is cheap and would have caught half of it

    Every entry in the real dictionary is exactly one character; that was
    measured across all 18,708 rather than assumed. Asserting it catches the
    doubled quote outright, and catches any future scalar this code reads
    wrongly. It does **not** catch the U+3000 case -- an ASCII space is one
    character too -- which is why the parser is the fix and the post-condition
    is the belt.

    The output shape is one entry per line with no trailing newline, which is
    what PP-OCRv4's `ppocr_keys_v1.txt` had and what `recognise.rs` reads.
    """
    try:
        import yaml  # noqa: PLC0415
    except ImportError:
        raise SystemExit(
            "PyYAML is needed to read the model's inference.yml."
            "\nInstall it: python -m pip install PyYAML"
            "\nIt is NOT optional and this step does not fall back to a"
            " hand-rolled parse: PR #89 round 1 found that the hand-rolled one"
            " corrupted two entries, and every digest around it certified the"
            " corruption instead of detecting it."
        ) from None

    try:
        document = yaml.safe_load(inference_yml.decode("utf-8"))
    except Exception as error:  # noqa: BLE001 - yaml raises several types
        raise SystemExit(
            "the downloaded inference.yml is not valid YAML: " + str(error)
        ) from error

    entries = None
    if isinstance(document, dict):
        post_process = document.get("PostProcess")
        if isinstance(post_process, dict):
            entries = post_process.get("character_dict")
    if entries is None:
        raise SystemExit(
            "the downloaded inference.yml has no `PostProcess.character_dict`."
            "\nUpstream changed its layout, which means this extraction no"
            " longer describes the file. Fix the extraction; do not hand-copy a"
            " dictionary."
        )
    if not isinstance(entries, list) or not entries:
        raise SystemExit("`PostProcess.character_dict` is not a non-empty list")

    # A YAML scalar that is empty reads as None, and PaddleOCR uses that for the
    # space. Every other entry is taken verbatim from the parser.
    values = [" " if entry is None else str(entry) for entry in entries]

    wrong = [(index, value) for index, value in enumerate(values) if len(value) != 1]
    if wrong:
        detail = "\n  ".join(
            "index " + str(index) + ": " + repr(value) for index, value in wrong[:10]
        )
        raise SystemExit(
            "the character dictionary has " + str(len(wrong)) + " entries that"
            " are not exactly one character, and each one shifts what the"
            " recogniser decodes:\n  " + detail
        )
    return "\n".join(values).encode("utf-8")


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
