"""Acquires the ONNX Runtime build UP-TAKE ships, and verifies every byte of it.

This is ADR-0032 decision 2's "documented, checksummed step" for the runtime:
pinned SHA-256, verified before use, HTTPS only. ADR-0035 then decided the
runtime ships INSIDE the installer, so what this produces is a staging
directory the Tauri bundler packages, not a first-run download.

Between those two records the step was never written. Until 2026-09-02 nothing
in this repository referred to `onnxruntime.dll` except the code that looked for
one, so an installer built from a clean checkout carried no runtime, no licence
and no third-party notices, and nothing said so.

Usage
-----

    python scripts/acquire-onnxruntime.py --out src-tauri/assets

Add `--archive <path>` to extract from an archive already on disk instead of
downloading it. The digest check is identical either way: a local file that does
not match the pin is refused exactly as a tampered download would be.

Why the pins are not in this file
---------------------------------

Every digest, size, URL and file name comes from
`crates/uptake-assets/src/onnxruntime.rs`, read out of the source at run time.
That file is what the APPLICATION verifies against before it loads the runtime,
so a copy here would be a second hand-maintained statement of the same fact --
and the two would agree today and drift the moment the pin moves. The failure
would be silent in the worse direction: this script would fetch and bless an
archive the application then refuses.

`parse_pins` throws rather than returning an empty dict if the extraction stops
matching, which is the shape UP-TAKE `I-96` asks for: a control that reads
source must go red when it can no longer read it, not pass on nothing.

What this deliberately does NOT do
----------------------------------

It does not convert the PP-OCRv4 models -- that is `convert-ppocr-models.py`,
which has its own toolchain and its own pinned sources. Both write into the same
staging directory and neither knows about the other.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import shutil
import sys
import urllib.request
import zipfile
from pathlib import Path

#: Where the pins live. One source, read rather than copied.
PINS_SOURCE = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "uptake-assets"
    / "src"
    / "onnxruntime.rs"
)

#: The constants this script needs, and the type each is expected to have.
#: Naming them explicitly means a REMOVED constant is an error here rather than
#: a `KeyError` three functions later.
REQUIRED_STRINGS = (
    "VERSION",
    "ARCHIVE_URL",
    "ARCHIVE_SHA256",
    "RUNTIME_FILE_NAME",
    "RUNTIME_SHA256",
    "LICENCE_FILE_NAME",
    "LICENCE_SHA256",
    "NOTICES_FILE_NAME",
    "NOTICES_SHA256",
)
REQUIRED_NUMBERS = (
    "ARCHIVE_SIZE",
    "RUNTIME_SIZE",
    "LICENCE_SIZE",
    "NOTICES_SIZE",
)

#: Which member of the archive becomes which installed file. The archive's
#: entries all sit under a single versioned directory, which is why the version
#: is interpolated rather than hardcoded.
MEMBERS = {
    "RUNTIME": "lib/onnxruntime.dll",
    "LICENCE": "LICENSE",
    "NOTICES": "ThirdPartyNotices.txt",
}


def parse_pins(source: str) -> dict[str, object]:
    """Extracts the pinned constants from the Rust source.

    Raises `SystemExit` if any required constant is missing, rather than
    returning a partial mapping. A parser that silently finds nothing is a
    control that cannot go red.
    """
    pins: dict[str, object] = {}

    for name in REQUIRED_STRINGS:
        # Non-greedy up to the closing quote, and anchored on the const name, so
        # a doc comment quoting the same text cannot be picked up instead.
        match = re.search(
            r'pub const ' + name + r':\s*&str\s*=\s*\n?\s*"([^"]*)"\s*;',
            source,
        )
        if match is None:
            raise SystemExit(
                "could not find `pub const " + name + ": &str` in "
                + str(PINS_SOURCE)
                + ".\nThe pins moved and this script cannot read them. Fix the"
                " extraction rather than copying the value here."
            )
        pins[name] = match.group(1)

    for name in REQUIRED_NUMBERS:
        match = re.search(
            r'pub const ' + name + r':\s*u64\s*=\s*([0-9_]+)\s*;',
            source,
        )
        if match is None:
            raise SystemExit(
                "could not find `pub const " + name + ": u64` in "
                + str(PINS_SOURCE)
                + ".\nThe pins moved and this script cannot read them."
            )
        pins[name] = int(match.group(1).replace("_", ""))

    return pins


def digest_of(data: bytes) -> str:
    """The SHA-256 of `data`, lowercase hex."""
    return hashlib.sha256(data).hexdigest()


def check(what: str, data: bytes, expected_digest: str, expected_size: int) -> None:
    """Refuses `data` unless it matches both pins.

    Size as well as digest, and the size is checked FIRST because it produces
    the more useful message: a wrong-file download reports "this is not the file
    we expected" rather than a hash mismatch that could mean either corruption or
    the wrong artifact entirely.
    """
    if len(data) != expected_size:
        raise SystemExit(
            what + " is the wrong size.\n"
            "  expected " + str(expected_size) + " bytes\n"
            "  actual   " + str(len(data)) + " bytes\n"
            "This is usually the wrong file rather than a corrupted one."
        )
    actual = digest_of(data)
    if actual != expected_digest:
        raise SystemExit(
            what + " does not match its pinned digest.\n"
            "  expected " + expected_digest + "\n"
            "  actual   " + actual + "\n"
            "Refusing to use it. Nothing has been written."
        )


def fetch(url: str) -> bytes:
    """Downloads `url` into memory.

    HTTPS is asserted here as well as pinned in the Rust source, because this is
    the line that actually opens a socket and ADR-0032 decision 2 says HTTPS
    only. Asserting it at the point of use is what makes it a check rather than
    a description.
    """
    if not url.startswith("https://"):
        raise SystemExit("refusing to fetch over a non-HTTPS URL: " + url)
    print("  fetching " + url)
    with urllib.request.urlopen(url) as response:  # noqa: S310 - scheme asserted above
        return response.read()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("src-tauri/assets"),
        help="staging directory the installer packages (default: src-tauri/assets)",
    )
    parser.add_argument(
        "--archive",
        type=Path,
        default=None,
        help="use this archive instead of downloading; verified identically",
    )
    arguments = parser.parse_args()

    if not PINS_SOURCE.is_file():
        raise SystemExit("cannot find the pins at " + str(PINS_SOURCE))
    pins = parse_pins(PINS_SOURCE.read_text(encoding="utf-8"))

    print("ONNX Runtime " + str(pins["VERSION"]))

    if arguments.archive is not None:
        print("  reading " + str(arguments.archive))
        archive_bytes = arguments.archive.read_bytes()
    else:
        archive_bytes = fetch(str(pins["ARCHIVE_URL"]))

    check(
        "the archive",
        archive_bytes,
        str(pins["ARCHIVE_SHA256"]),
        int(pins["ARCHIVE_SIZE"]),  # type: ignore[arg-type]
    )
    print("  archive digest ok")

    out: Path = arguments.out
    out.mkdir(parents=True, exist_ok=True)

    root = "onnxruntime-win-x64-" + str(pins["VERSION"])
    written: list[Path] = []
    import io as _io

    with zipfile.ZipFile(_io.BytesIO(archive_bytes)) as archive:
        for key, member in MEMBERS.items():
            name = str(pins[key + "_FILE_NAME"])
            path = root + "/" + member
            try:
                extracted = archive.read(path)
            except KeyError:
                raise SystemExit(
                    "the archive does not contain " + path + ".\n"
                    "Its layout changed, which means the pinned version moved"
                    " without this script's MEMBERS map moving with it."
                ) from None
            # Verified AFTER extraction and BEFORE writing, so nothing that
            # fails a check ever reaches the staging directory. ADR-0032's
            # invariant, in the words uptake-assets uses for it: unverified
            # bytes never become a usable file.
            check(
                name,
                extracted,
                str(pins[key + "_SHA256"]),
                int(pins[key + "_SIZE"]),  # type: ignore[arg-type]
            )
            destination = out / name
            destination.write_bytes(extracted)
            written.append(destination)
            print("  wrote " + str(destination) + "  (" + str(len(extracted)) + " bytes)")

    print("")
    print("Verified and staged " + str(len(written)) + " file(s) in " + str(out) + ".")
    print(
        "The two notice files are a LICENCE OBLIGATION, not documentation:"
        " ONNX Runtime is MIT and bundles other people's code, and `cargo deny`"
        " cannot see either file because it walks the crate graph."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
