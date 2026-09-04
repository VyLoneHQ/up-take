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
