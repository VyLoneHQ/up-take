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
from pathlib import Path

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


#: Where the reserved device stems are DEFINED. There is no second copy.
#:
#: `PR #88` round 9 found the copy that used to live here was a real divergence
#: hole: the shared corpus draws its device cases from the RUST list, so a stem
#: added to the PYTHON list alone produced no corpus case, and every control
#: stayed green while the two guards genuinely disagreed. Drilled with `CONIN$`.
#:
#: Worse, that hole was a REGRESSION this file introduced. The version before it
#: carried `test_the_reserved_lists_agree`, which compared the two lists as sets
#: and would have caught it; replacing the approach deleted the check along with
#: the parser it sat beside, and the new three-line contract then asserted the
#: case it no longer covered.
#:
#: So the list is not duplicated and then policed. It is read from the Rust,
#: which is the only place it is written. A stem added there is here on the next
#: run, and "added to Python only" is not a state that exists.
#:
#: ⚠️ **This is DATA extraction, not the predicate parsing rounds 7 and 8
#: killed.** A `const NAME: [&str; N] = [...]` literal is a list; reading it was
#: never the part that failed. What failed was inferring RULES from the shape of
#: Rust code, and nothing here does that -- the rules are compared by verdict, on
#: the corpus, by `control-rust-consts.py`.
_MANIFEST = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "uptake-assets"
    / "src"
    / "manifest.rs"
)

_RESERVED_CACHE: tuple[str, ...] | None = None


def reserved_device_names() -> tuple[str, ...]:
    """The Rust guard's `RESERVED_DEVICE_NAMES`, read from its one definition.

    Raises `SystemExit` rather than returning an empty tuple. An empty list here
    would make `plain_file_name` accept every device name silently, which is the
    failure this whole family of checks exists to prevent.
    """
    global _RESERVED_CACHE  # noqa: PLW0603 - a read-through cache of a file constant
    if _RESERVED_CACHE is not None:
        return _RESERVED_CACHE
    if not _MANIFEST.is_file():
        raise SystemExit(
            "cannot find " + str(_MANIFEST) + ", which is where the reserved"
            " device names are defined. Without it this guard would accept"
            " every device name."
        )
    source = _MANIFEST.read_text(encoding="utf-8")
    block = re.search(
        r"const RESERVED_DEVICE_NAMES:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\];",
        source,
        re.S,
    )
    if block is None:
        raise SystemExit(
            "could not find `const RESERVED_DEVICE_NAMES` in " + str(_MANIFEST)
            + ".\nIt moved or was renamed. Fix this extraction rather than"
            " pasting the list back here: a second copy is what round 9 found."
        )
    names = tuple(re.findall(r'"([^"]+)"', block.group(1)))
    if not names:
        raise SystemExit(
            "read an EMPTY reserved-device list from " + str(_MANIFEST)
            + ". A blind extraction and a genuinely empty list are"
            " indistinguishable to every caller, so this refuses."
        )
    _RESERVED_CACHE = names
    return names


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

    * `NUL`, and `CON` when the path is relative, reach a **device** rather than
      a file. `write_bytes` succeeds, `exists()` is True, `is_file()` is False,
      reading back gives nothing, and the name is absent from the directory
      listing. The run printed *wrote 35 bytes* and *verified* and left the
      staging directory **empty**.
    * `model.onnx:stream` writes into an alternate data stream, leaving a
      **zero-byte** `model.onnx` for anything that later hashes the staged file.
    * `model.onnx.` and `model.onnx ` are stripped by the Win32 path parser, so
      the name printed is not the name on disk.

    ⚠️ **`NUL.onnx` IS AN ORDINARY FILE, and this docstring said otherwise.**
    `PR #88` round 8 caught the claim and a probe on Windows 11 Pro 26200
    confirmed it: `NUL.onnx`, `nul.onnx`, `CON.txt`, `COM1.txt`, `LPT9.onnx` and
    `NUL.tar.gz` all take 35 bytes, read 35 back and appear in the listing. The
    device is reached only by the BARE stem, and even that depends on how the
    path is formed -- with a relative name `CON` hits the device, and through
    Python's absolute-path handling it does not, while `NUL` does both ways.

    **The extension names are still refused, and the reason is now the honest
    one.** Not "they reach a device" -- they do not. The rule is a flat one
    because the behaviour is path-form dependent and platform dependent, a flat
    rule costs nothing, and the alternative is a guard whose correctness turns
    on which runtime opened the file. `manifest.rs` states the same corrected
    version; the two must agree, and `name-guard-cases.tsv` is what makes them.

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
    if stem.upper() in reserved_device_names():
        raise SystemExit(
            const_name + " is " + repr(value) + ", whose stem is the Windows"
            " device " + stem.upper() + "."
            "\nWriting there succeeds, reports a byte count, and discards every"
            " byte: the staging directory is left empty and nothing says so."
        )
    return value
