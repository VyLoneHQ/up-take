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


def plain_file_name(value: str, const_name: str) -> str:
    """Returns `value` if it is a bare file name; raises `SystemExit` otherwise.

    # Why a pinned constant is validated at all

    Round 4 of `PR #88`'s independent review drilled it: a pinned name is joined
    straight onto the operator's `--out` path, and nothing checked it was a
    plain name. With `DETECTION_FILE_NAME = "../../escaped-outside-out.onnx"`
    the write landed two directories ABOVE the staging directory, and the digest
    and size checks both passed -- they check the bytes, not the destination.

    The input is a repository-controlled constant, so this is not a route in
    from outside: anyone who can change `ppocr.rs` can change the script beside
    it. It is validated anyway because the cost is four lines, and because a
    path assembled from a constant that is never checked is the kind of thing
    that stops being repository-controlled the first time someone generates it.

    Rejects separators of both kinds, `..`, absolute paths and drive letters --
    on every platform rather than the running one, since the pin travels.
    """
    if value in ("", ".", ".."):
        raise SystemExit(
            const_name + " is " + repr(value) + ", which is not a file name."
        )
    if "/" in value or "\\" in value or "\x00" in value:
        raise SystemExit(
            const_name + " is " + repr(value) + ", which contains a path"
            " separator.\nA pin names a file inside the staging directory; it"
            " does not choose where that directory is."
        )
    if len(value) > 1 and value[1] == ":":
        raise SystemExit(
            const_name + " is " + repr(value) + ", which names a drive."
        )
    return value
