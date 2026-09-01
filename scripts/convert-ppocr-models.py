#!/usr/bin/env python3
"""Converts PaddleOCR's official PP-OCRv4 release to ONNX, reproducibly.

This is ADR-0034's conversion step. That record chose option A -- *we* convert
the official release rather than taking a third party's ONNX -- on the argument
ADR-0032 made about ONNX Runtime, applied to the file `SPECS/architecture.md`
section 4 calls "arbitrary code". The consequence is this script: the ONNX
UP-TAKE ships is produced here, so the SHA-256 in the asset manifest pins *our*
artifact and means what ADR-0032 decision 2 says it means.

What it does
------------

Downloads three upstream files over HTTPS, verifies each against a pinned
SHA-256 before using it, converts the two Paddle inference models to ONNX, and
prints the digests of what came out. Nothing is written outside the output
directory, and a digest mismatch stops the run.

Why the source digests are pinned and the output digests are not
---------------------------------------------------------------

The inputs are somebody else's bytes arriving over a network, so a pin is the
only thing standing between us and a substituted file: that is the check. The
outputs are produced here from verified inputs by a pinned converter, so their
digests are a result to be recorded in the manifest, not a precondition to
assert. Pinning them too would mean this script could never be re-run after a
converter upgrade without editing it first, which is the failure mode where
somebody edits the constant to match whatever came out.

Reproducibility is claimed only as far as it has been observed. Two runs on one
machine with one pinned toolchain produced identical bytes. This script does not
claim bit-identical output across machines, Python versions or paddle2onnx
versions, and nothing here checks that -- so if the recorded manifest digests
stop matching after a toolchain change, the manifest is what moves, and that is
a decision rather than a re-run.

The toolchain, and why paddlepaddle is not in it
------------------------------------------------

ADR-0034 costed option A as "paddle2onnx 2.1.0 ... plus paddlepaddle", a ~100 MB
second ecosystem. It turns out not to be needed for this path, and the reason is
in paddle2onnx's own source rather than in a claim made here: `convert.py` at
1.3.1 imports `paddle` at module level, but its `export()` -- the .pdmodel to
ONNX entry point, the only one this script uses -- calls
`paddle2onnx_cpp2py_export.export` and touches nothing from `paddle`. Only
`dygraph2onnx()`, which converts a live Python model object and which we never
call, needs it. So the C++ extension is loaded directly and the dependency is
dropped. That is a smaller supply chain than the ADR anticipated, in the
direction the ADR wanted.

The cost of doing it this way, stated rather than left to be discovered: the
package's public API is bypassed, so a paddle2onnx upgrade could move the C
extension's signature with no deprecation warning. That is why the version is
pinned exactly and asserted before any conversion runs.

paddle2onnx 2.1.0 -- the version ADR-0034 named as current on 2026-08-30 -- is
NOT usable here: its C extension fails to load on Windows against paddlepaddle
3.2.2 and 3.3.1 alike with "DLL load failed while importing
paddle2onnx_cpp2py_export: The specified procedure could not be found", an ABI
break between the two packages. 1.3.1 is the newest release that converts these
artifacts on this platform, and the ADR named a version it had not run.

Usage
-----

    uv venv --python 3.12 .venv-convert
    uv pip install --python .venv-convert/Scripts/python.exe paddle2onnx==1.3.1 onnxruntime
    .venv-convert/Scripts/python.exe scripts/convert-ppocr-models.py --out dist/models

Licence obligations
-------------------

ADR-0034 obligation 3: `cargo deny` walks the crate graph and sees neither a DLL
nor a .onnx, so the notices for these artifacts do not travel with them
automatically. This script writes NOTICE-models.txt beside the output for
exactly that reason. It is generated rather than hand-maintained so it cannot
drift from the list of files actually produced.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import os
import sys
import tarfile
import urllib.request
from pathlib import Path

# The converter this script is written against, asserted before use. See the
# module docstring for why the version is exact rather than a floor.
EXPECTED_P2O_VERSION = "1.3.1"

# ONNX opset. 14 is what PP-OCRv4's operators need and what ONNX Runtime 1.17 --
# ADR-0032's floor -- supports comfortably. Raising it is a decision, not a
# tidy-up: a newer opset can emit operators an older runtime cannot load, and
# the runtime is a file we place rather than one whose version we control on
# every machine.
OPSET_VERSION = 14

UPSTREAM = "https://paddleocr.bj.bcebos.com"
PADDLEOCR_TAG = "v2.7.0"
DICTIONARY_URL = (
    "https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/"
    + PADDLEOCR_TAG
    + "/ppocr/utils/ppocr_keys_v1.txt"
)

# What the recogniser must emit once converted. This is the number that makes
# `CharacterDictionary::from_ppocr_dictionary` correct and the literal reading
# of the dictionary wrong: 6623 lines + 1 appended space + 1 CTC blank. See
# UP-TAKE I-333, and the guard in `PaddleEngine::recognise_crop` that refuses a
# model and dictionary that disagree.
EXPECTED_RECOGNISER_CLASSES = 6625


class Source:
    """One upstream file, with the digest it must have before we use it."""

    def __init__(self, name: str, url: str, sha256: str, size: int) -> None:
        self.name = name
        self.url = url
        self.sha256 = sha256
        self.size = size


# Probed 2026-09-01 by downloading each file and hashing it. The detector's
# digest is byte-identical to the one ADR-0034 recorded on 2026-08-30, which is
# that record's dated observation re-confirmed rather than restated.
#
# Nothing watches these URLs. UP-TAKE I-332 is open about exactly that: if
# PaddleOCR republishes an archive, this script goes red on the digest, and that
# red IS the notification. It is the intended behaviour rather than a fault --
# an upstream change has to reach a human.
SOURCES = [
    Source(
        "ch_PP-OCRv4_det_infer.tar",
        UPSTREAM + "/PP-OCRv4/chinese/ch_PP-OCRv4_det_infer.tar",
        "5f7217e0a89612e2f80d62f3c99a8bf5f7ae9cdc1ffd706be7dde07765627edf",
        4894720,
    ),
    Source(
        "ch_PP-OCRv4_rec_infer.tar",
        UPSTREAM + "/PP-OCRv4/chinese/ch_PP-OCRv4_rec_infer.tar",
        "830ea228e20c2b30c4db9666066c48512f67a63f5b1a32d0d33dc9170040ce7d",
        10977280,
    ),
    Source(
        "ppocr_keys_v1.txt",
        DICTIONARY_URL,
        "28b2362ad4ab2dc38769aa72feb535e3a9ddb3fd2a7585a05920e6393b1dc7f7",
        26249,
    ),
]


def digest_of(data: bytes) -> str:
    """The lowercase hex SHA-256 of some bytes."""
    return hashlib.sha256(data).hexdigest()


def fetch(source: Source, cache: Path) -> bytes:
    """Downloads a source unless it is cached, and verifies it either way.

    The verification runs on the bytes we are about to use, on every run,
    including the cached path. Verifying only on download would mean a cache
    poisoned after the fact is trusted forever, which is the whole property the
    pin exists to buy.
    """
    target = cache / source.name
    if target.exists():
        data = target.read_bytes()
    else:
        if not source.url.startswith("https://"):
            raise SystemExit("refusing a non-HTTPS source: " + source.url)
        print("  fetching " + source.url)
        with urllib.request.urlopen(source.url) as response:  # noqa: S310
            data = response.read()
        target.write_bytes(data)

    actual = digest_of(data)
    if actual != source.sha256:
        raise SystemExit(
            source.name + " does not match its pinned digest.\n"
            "  expected " + source.sha256 + "\n"
            "  actual   " + actual + "\n"
            "Upstream has republished this file, or it was tampered with in "
            "transit. This is UP-TAKE I-332's notification: do not edit the "
            "constant to make the red go away. Find out what changed."
        )
    if len(data) != source.size:
        raise SystemExit(
            source.name + " is " + str(len(data)) + " bytes, expected "
            + str(source.size)
        )
    print("  verified " + source.name + "  (" + str(len(data)) + " bytes)")
    return data


def extract_model(archive: bytes, cache: Path, stem: str) -> Path:
    """Unpacks one inference model, refusing any member that escapes the target.

    `tarfile.extractall` honours absolute paths and `..` in member names, which
    is the classic archive escape. The members are named explicitly instead --
    which also means an archive that grows a fourth file makes this loud rather
    than writing it silently.
    """
    directory = cache / stem
    directory.mkdir(parents=True, exist_ok=True)
    wanted = {"inference.pdmodel", "inference.pdiparams"}
    found = set()
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        for member in tar.getmembers():
            name = Path(member.name).name
            if name not in wanted or not member.isfile():
                continue
            if ".." in member.name or Path(member.name).is_absolute():
                raise SystemExit("refusing tar member " + repr(member.name))
            payload = tar.extractfile(member)
            if payload is None:
                raise SystemExit("tar member " + repr(member.name) + " has no content")
            (directory / name).write_bytes(payload.read())
            found.add(name)
    missing = wanted - found
    if missing:
        raise SystemExit(stem + " is missing " + str(sorted(missing)))
    return directory


def load_converter():
    """Loads paddle2onnx's C++ extension directly. See the module docstring."""
    spec = importlib.util.find_spec("paddle2onnx")
    if spec is None or not spec.submodule_search_locations:
        raise SystemExit("paddle2onnx is not installed. See this script's Usage section.")
    package = Path(list(spec.submodule_search_locations)[0])

    namespace = {}
    exec((package / "version.py").read_text(encoding="utf-8"), namespace)  # noqa: S102
    version = str(namespace.get("version", "")).strip()
    if version != EXPECTED_P2O_VERSION:
        raise SystemExit(
            "this script is written against paddle2onnx " + EXPECTED_P2O_VERSION
            + " and found " + repr(version) + ". The C extension is loaded "
            "directly, so its signature is covered by no deprecation policy -- "
            "read the module docstring before changing the pin."
        )

    extensions = [f for f in os.listdir(package) if f.endswith((".pyd", ".so"))]
    if len(extensions) != 1:
        raise SystemExit("expected one paddle2onnx extension, found " + str(extensions))
    module_spec = importlib.util.spec_from_file_location(
        "paddle2onnx_cpp2py_export", package / extensions[0]
    )
    if module_spec is None or module_spec.loader is None:
        raise SystemExit("could not load paddle2onnx's extension module")
    module = importlib.util.module_from_spec(module_spec)
    module_spec.loader.exec_module(module)
    return module


def convert(converter, model_dir: Path, destination: Path) -> bytes:
    """Runs one .pdmodel to ONNX conversion and writes the result."""
    blob = converter.export(
        str(model_dir / "inference.pdmodel"),
        str(model_dir / "inference.pdiparams"),
        OPSET_VERSION,
        True,   # auto_upgrade_opset
        False,  # verbose
        True,   # enable_onnx_checker
        True,   # enable_experimental_op
        True,   # enable_optimize
        {},     # custom_op_info
        "onnxruntime",
        "",     # calibration_file
        "",     # external_file
        False,  # export_fp16_model
    )
    destination.write_bytes(blob)
    return blob


def check_shapes(out_dir: Path) -> None:
    """Opens both models in ONNX Runtime and asserts the shapes UP-TAKE assumes.

    Skipped with a loud line if onnxruntime is absent, rather than failing: the
    conversion is this script's job and the check is a bonus. A SILENT skip
    would be the defect -- a check that can vanish without saying so reports
    green forever.
    """
    try:
        import onnxruntime  # noqa: PLC0415
    except ImportError:
        print(
            "  NOT CHECKED: onnxruntime is not installed, so the converted "
            "models' shapes were not verified. Install it to enable this."
        )
        return

    detector = onnxruntime.InferenceSession(
        str(out_dir / "ch_PP-OCRv4_det.onnx"), providers=["CPUExecutionProvider"]
    )
    det_in = detector.get_inputs()[0].shape
    det_out = detector.get_outputs()[0].shape
    if det_in[1] != 3:
        raise SystemExit("detector takes " + str(det_in[1]) + " channels, expected 3")
    if det_out[1] != 1:
        raise SystemExit(
            "detector emits " + str(det_out[1])
            + " channels, expected a 1-channel probability map"
        )

    recogniser = onnxruntime.InferenceSession(
        str(out_dir / "ch_PP-OCRv4_rec.onnx"), providers=["CPUExecutionProvider"]
    )
    rec_in = recogniser.get_inputs()[0].shape
    rec_out = recogniser.get_outputs()[0].shape
    if rec_in[1] != 3 or rec_in[2] != 48:
        raise SystemExit("recogniser takes " + str(rec_in) + ", expected [N, 3, 48, W]")
    if rec_out[-1] != EXPECTED_RECOGNISER_CLASSES:
        raise SystemExit(
            "recogniser emits " + str(rec_out[-1]) + " classes, expected "
            + str(EXPECTED_RECOGNISER_CLASSES) + ". The dictionary and the model "
            "are no longer a matching pair -- see UP-TAKE I-333."
        )
    print("  detector    " + str(det_in) + " -> " + str(det_out))
    print("  recogniser  " + str(rec_in) + " -> " + str(rec_out))


NOTICE_TEMPLATE = """Third-party notices for the model files distributed with UP-TAKE
================================================================

These files are NOT part of UP-TAKE's own source and are not covered by
UP-TAKE's GPL-3.0 licence. They are derived from PaddleOCR, and this notice
travels with them because cargo deny walks the Rust crate graph and sees neither
a .onnx file nor a character dictionary, so nothing else would carry it.
UP-TAKE ADR-0034, obligation 3.

Upstream
--------

PaddleOCR, by PaddlePaddle Authors, licensed under the Apache License 2.0.

    https://github.com/PaddlePaddle/PaddleOCR
    Dictionary taken at tag {tag}
    Model archives from {upstream}

What was done to them
---------------------

The .pdmodel and .pdiparams inference models from PaddleOCR's official PP-OCRv4
release were converted to ONNX with paddle2onnx {converter} (Apache License 2.0)
at opset {opset}. The weights are unchanged; the container format is not. The
character dictionary is copied byte for byte.

Apache 2.0 section 4(b) requires a derivative work to carry a notice that files
were modified: the two .onnx files below are modified forms of PaddleOCR's
released models in the sense described above.

Files
-----

{files}

The full Apache License 2.0 text is at http://www.apache.org/licenses/LICENSE-2.0
and is reproduced in PaddleOCR's own repository at the tag named above.
"""


def main() -> int:
    parser = argparse.ArgumentParser(description="Convert PP-OCRv4 to ONNX.")
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("dist/models"),
        help="where to write the converted models (default: dist/models)",
    )
    parser.add_argument(
        "--cache",
        type=Path,
        default=Path("dist/.ppocr-cache"),
        help="where to keep the verified upstream downloads",
    )
    arguments = parser.parse_args()

    out_dir = arguments.out
    cache = arguments.cache
    out_dir.mkdir(parents=True, exist_ok=True)
    cache.mkdir(parents=True, exist_ok=True)

    print("Verifying upstream sources")
    payloads = {source.name: fetch(source, cache) for source in SOURCES}

    print("Loading the converter")
    converter = load_converter()
    print("  paddle2onnx " + EXPECTED_P2O_VERSION + ", opset " + str(OPSET_VERSION))

    print("Converting")
    produced = []
    for archive_name, stem, output_name in (
        ("ch_PP-OCRv4_det_infer.tar", "det", "ch_PP-OCRv4_det.onnx"),
        ("ch_PP-OCRv4_rec_infer.tar", "rec", "ch_PP-OCRv4_rec.onnx"),
    ):
        model_dir = extract_model(payloads[archive_name], cache, stem)
        blob = convert(converter, model_dir, out_dir / output_name)
        produced.append((output_name, len(blob), digest_of(blob)))
        print("  " + output_name + "  (" + str(len(blob)) + " bytes)")

    dictionary = payloads["ppocr_keys_v1.txt"]
    (out_dir / "ppocr_keys_v1.txt").write_bytes(dictionary)
    produced.append(("ppocr_keys_v1.txt", len(dictionary), digest_of(dictionary)))
    print("  ppocr_keys_v1.txt  (" + str(len(dictionary)) + " bytes, copied unchanged)")

    print("Checking the converted models against UP-TAKE's assumptions")
    check_shapes(out_dir)

    listing = "\n".join(
        "  " + name + "\n    sha256 " + digest + "\n    " + str(size) + " bytes"
        for name, size, digest in produced
    )
    (out_dir / "NOTICE-models.txt").write_text(
        NOTICE_TEMPLATE.format(
            tag=PADDLEOCR_TAG,
            upstream=UPSTREAM,
            converter=EXPECTED_P2O_VERSION,
            opset=OPSET_VERSION,
            files=listing,
        ),
        encoding="utf-8",
    )
    print("  wrote " + str(out_dir / "NOTICE-models.txt"))

    print("\nManifest values -- these are what belong in AssetManifest:")
    for name, size, digest in produced:
        print("  " + name)
        print("    size   " + str(size))
        print("    sha256 " + digest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
