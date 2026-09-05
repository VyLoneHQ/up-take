#!/usr/bin/env python3
"""Writes `NOTICE-models.txt` from the pins, naming exactly what ships.

# Why this replaced the conversion script's copy of the job

[`ADR-0037`] took the RECOGNISER to Baidu's published ONNX, as `ADR-0036` had
already taken the detector, so **nothing in UP-TAKE's OCR path is converted
here any more**. `convert-ppocr-models.py` was kept alive for one reason -- it
wrote this notice -- and round 1 of `PR #89`'s review found that the notice it
wrote had gone wrong in the worst possible direction:

    PP-OCRv6_small_det.onnx   named, and shipped
    ch_PP-OCRv4_rec.onnx      named, and NOT shipped
    ppocr_keys_v1.txt         named, and NOT shipped
    PP-OCRv6_small_rec.onnx   SHIPPED, and not named
    ppocr_keys_v6_small.txt   SHIPPED, and not named

An Apache-2.0 section 4 notice that omits two of the four files it must cover is
a licence defect, not a documentation one, and `cargo deny` cannot see it
because it walks the crate graph and sees neither a `.onnx` nor a dictionary.
`verify-bundle.py` lists `NOTICE-models.txt` under `UNPINNED`, so its contents
were never checked either.

**So the notice is built from the pins**, which are the same constants the
installer's `resources` map and `verify-bundle.py` read. A file that ships and
is not named here is now impossible without changing the pins, and a file named
here that does not ship is impossible for the same reason.

Deleting the conversion step also stops every build downloading a 10.9 MB
PP-OCRv4 archive to produce two files nobody receives.

# Usage

    python3 scripts/write-model-notice.py --out src-tauri/assets/models

[`ADR-0037`]: ../Projects/UP-TAKE/DECISIONS/ADR-0037-the-ocr-models-are-the-v6-small-pair.md
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import rust_consts  # noqa: E402

PINS_SOURCE = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "uptake-assets"
    / "src"
    / "ppocr.rs"
)

#: Every asset this notice must cover, as (pin prefix, what was done to it).
#: Driven off the pins so the listing cannot disagree with what is installed.
COVERED = (
    ("DETECTION", "redistributed unchanged"),
    ("RECOGNITION", "redistributed unchanged"),
    ("DICTIONARY", "extracted from the recogniser's published inference.yml"),
)

NOTICE_TEMPLATE = """Third-party notices for the model files distributed with UP-TAKE
================================================================

These files are NOT part of UP-TAKE's own source and are not covered by
UP-TAKE's GPL-3.0 licence. They are PaddleOCR's, and this notice travels with
them because cargo deny walks the Rust crate graph and sees neither a .onnx file
nor a character dictionary, so nothing else would carry it.
UP-TAKE ADR-0034 obligation 3, as amended by ADR-0036 and ADR-0037.

Upstream
--------

PaddleOCR, by PaddlePaddle Authors, licensed under the Apache License 2.0.

    https://github.com/PaddlePaddle/PaddleOCR

Both models are PaddlePaddle's own ONNX builds of PP-OCRv6, published at:

    {urls}

What was done to them
---------------------

Apache 2.0 section 4(b) requires a notice that files were modified. Some of
these were and some were not, so they are listed apart rather than under one
sentence that would be half wrong.

REDISTRIBUTED UNCHANGED. Both models are downloaded and verified byte for byte
against the digests below. Nothing is converted: ADR-0036 took the detector to
PaddlePaddle's own ONNX and ADR-0037 took the recogniser the same way, so the
paddle2onnx conversion earlier versions of UP-TAKE performed is gone.

MODIFIED HERE. The character dictionary is EXTRACTED from the recogniser's own
published inference.yml, whose character_dict block PaddleOCR does not also
publish as a standalone file. The characters are upstream's, in upstream's
order; the container is one entry per line. The digest below therefore pins an
artifact this project produced, which is the distinction ADR-0034 drew about
conversion and which survives it.

Files
-----

{files}

The full Apache License 2.0 text is at http://www.apache.org/licenses/LICENSE-2.0
and is reproduced in PaddleOCR's own repository.
"""


def read_pins() -> dict[str, object]:
    """Reads every pin this notice covers. Refuses on a partial read."""
    if not PINS_SOURCE.is_file():
        raise SystemExit("cannot find the pins at " + str(PINS_SOURCE))
    source = PINS_SOURCE.read_text(encoding="utf-8")
    pins: dict[str, object] = {}
    for prefix, _ in COVERED:
        for suffix in ("FILE_NAME", "SHA256"):
            name = prefix + "_" + suffix
            value = rust_consts.string_const(source, name)
            if suffix == "FILE_NAME" and value is not None:
                # PR #88 round 6, BEHAVIOUR 1, carried across the rewrite.
                # That finding was a pinned name joined onto a directory in the
                # script this one REPLACES, and deleting that script would have
                # taken the class member with it while leaving the class open
                # here. Nothing is joined to a path in this file, but the notice
                # NAMES these files as the ones distributed: a pin of
                # "../escaped.onnx" or "NUL" would put that in an Apache-2.0
                # section 4 notice as a shipped artifact.
                value = rust_consts.plain_file_name(value, name)
            if value is None:
                raise SystemExit(
                    "could not find `pub const " + name + ": &str` in "
                    + str(PINS_SOURCE)
                    + ".\nThe pins moved and this script cannot read them, which"
                    " would write a notice that omits a shipped file."
                )
            pins[name] = value
        size = rust_consts.u64_const(source, prefix + "_SIZE")
        if size is None:
            raise SystemExit(
                "could not find `pub const " + prefix + "_SIZE: u64` in "
                + str(PINS_SOURCE)
            )
        pins[prefix + "_SIZE"] = size
    for prefix in ("DETECTION", "RECOGNITION"):
        url = rust_consts.string_const(source, prefix + "_URL")
        if url is None:
            raise SystemExit(
                "could not find `pub const " + prefix + "_URL: &str` in "
                + str(PINS_SOURCE)
            )
        pins[prefix + "_URL"] = url
    return pins


def build_listing(pins: dict[str, object]) -> str:
    """One block per covered file: name, size, digest, and what was done to it."""
    blocks = []
    for prefix, treatment in COVERED:
        name = str(pins[prefix + "_FILE_NAME"])
        blocks.append(
            name
            + "\n    "
            + treatment
            + "\n    size   "
            + str(pins[prefix + "_SIZE"])
            + "\n    sha256 "
            + str(pins[prefix + "_SHA256"])
        )
    return "\n\n".join(blocks)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("src-tauri/assets/models"),
        help="staging directory the installer packages"
        " (default: src-tauri/assets/models)",
    )
    arguments = parser.parse_args()

    pins = read_pins()
    urls = "\n    ".join(
        str(pins[prefix + "_URL"]) for prefix in ("DETECTION", "RECOGNITION")
    )
    text = NOTICE_TEMPLATE.format(urls=urls, files=build_listing(pins))

    arguments.out.mkdir(parents=True, exist_ok=True)
    target = arguments.out / "NOTICE-models.txt"
    target.write_text(text, encoding="utf-8", newline="\n")
    print("wrote " + str(target))
    for prefix, _ in COVERED:
        print("  names " + str(pins[prefix + "_FILE_NAME"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
