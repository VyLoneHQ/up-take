"""Reads `pub const` values out of Rust source, so a pin lives in one place.

Every digest UP-TAKE pins lives in `crates/uptake-assets/src/`, because that is
where the application reads it at load time. The build-time scripts need the
same values, and copying them into Python would make two literals that must
agree, which is how a pin gets rotated in one file and not the other.

`scripts/acquire-onnxruntime.py` has read the pins out of the source since it
was written. This module is that extraction, moved here unchanged in shape when
`scripts/verify-bundle.py` needed the same thing for a second file
(`ppocr.rs`), rather than growing a second regex beside the first.

**Not a Rust parser and not pretending to be.** It matches an anchored `pub
const NAME: TYPE = VALUE;` and nothing else. That is enough for a file of flat
literals, and the failure mode when it stops being enough is a caller getting
`None` and refusing, not a wrong value.
"""

from __future__ import annotations

import re

#: Matches the whole declaration, anchored on the const name so a doc comment
#: quoting the same digest cannot be picked up instead. The optional newline
#: after `=` is not cosmetic: `ppocr.rs` wraps its 64-character digests onto
#: their own line, and a pattern without it silently finds nothing there.
_STRING = r'pub const {name}:\s*&str\s*=\s*\n?\s*"([^"]*)"\s*;'
_NUMBER = r"pub const {name}:\s*u64\s*=\s*([0-9_]+)\s*;"


def string_const(source: str, name: str) -> str | None:
    """The value of `pub const <name>: &str`, or `None` if it is not there.

    `None` rather than an exception: the callers want to name the file they
    were reading and say what to do about it, and they say different things.
    """
    match = re.search(_STRING.format(name=re.escape(name)), source)
    return None if match is None else match.group(1)


def u64_const(source: str, name: str) -> int | None:
    """The value of `pub const <name>: u64`, or `None` if it is not there.

    Underscore separators are stripped: they are how the sizes are written in
    the source (`79_645_520`) and `int()` rejects them.
    """
    match = re.search(_NUMBER.format(name=re.escape(name)), source)
    return None if match is None else int(match.group(1).replace("_", ""))


#: Windows device names that are not file names, whatever the extension.
#:
#: **Mirrored from `crates/uptake-assets/src/manifest.rs`'s
#: `RESERVED_DEVICE_NAMES`.** That duplication is a cost and is taken
#: deliberately: the Rust list guards what the PRODUCT installs and this one
#: guards what the BUILD stages, and neither can import the other. What makes it
#: safe is `control-rust-consts.py`, which reads both and fails if they diverge
#: -- so a rule added to one and not the other is a red control rather than the
#: `F-22` / `F-37` silence.
RESERVED_DEVICE_NAMES = (
    "CON", "PRN", "AUX", "NUL",
    "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
)


def plain_file_name(value: str, const_name: str) -> str:
    """Returns `value` if it is a bare file name; raises `SystemExit` otherwise.

    # Why a pinned constant is validated at all

    Round 4 of `PR #88`'s independent review drilled it: a pinned name is joined
    straight onto the operator's `--out` path, and nothing checked it was a
    plain name. With `DETECTION_FILE_NAME = "../../escaped-outside-out.onnx"`
    the write landed two directories ABOVE the staging directory, and the digest
    and size checks both passed -- they check the bytes, not the destination.

    # Why the rules are what they are, and not the four this started with

    Round 5 found the first version was **a strictly weaker second
    implementation of a rule this repository had already got right**.
    `manifest.rs`'s `is_plain_file_name` guards exactly the same thing -- a
    pinned name joined to an install directory -- and refused several inputs
    this accepted. The weaker of the two was the one guarding a write.

    Each accepted class was drilled on Windows and each is worse than it looks:

    * `NUL`, `CON`, `LPT9.onnx` reach a **device**, not a file. `write_bytes`
      succeeds, `exists()` is True, reading back gives an empty string, and the
      name is absent from the directory listing. The run printed *wrote 35
      bytes* and *verified*, and left the staging directory **empty**.
    * `model.onnx:stream` writes into an alternate data stream, leaving a
      **zero-byte** `model.onnx` for anything that later hashes the staged file.
    * `model.onnx.` and `model.onnx ` are stripped by the Win32 path parser, so
      the name printed is not the name on disk.

    This now mirrors `is_plain_file_name` rule for rule, including rejecting
    both separators on every platform: a pin written on Windows must not become
    an escape when the same file is read on Linux, and the device names are
    rejected everywhere for the same reason in reverse.
    """
    if value in ("", ".", ".."):
        raise SystemExit(
            const_name + " is " + repr(value) + ", which is not a file name."
        )
    if "/" in value or "\\" in value or ":" in value:
        raise SystemExit(
            const_name + " is " + repr(value) + ", which contains a path"
            " separator or a colon."
            "\nA pin names a file inside the staging directory; it does not"
            " choose where that directory is, and a colon opens an alternate"
            " data stream that leaves a zero-byte file behind."
        )
    if value.endswith(".") or value.endswith(" "):
        raise SystemExit(
            const_name + " is " + repr(value) + ", which ends in a dot or a"
            " space."
            "\nThe Win32 path parser strips both, so the name written would not"
            " be the name pinned."
        )
    stem = value.split(".")[0]
    if stem.upper() in RESERVED_DEVICE_NAMES:
        raise SystemExit(
            const_name + " is " + repr(value) + ", whose stem is the Windows"
            " device " + stem.upper() + "."
            "\nWriting there succeeds, reports a byte count, and discards every"
            " byte: the staging directory is left empty and nothing says so."
        )
    return value
