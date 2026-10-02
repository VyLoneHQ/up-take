#!/usr/bin/env python3
"""Tests for `scripts/draw-ocr-characters.py` (roadmap `1.44`, the rig picture).

The picture is judged by eye, so the one way it can mislead is by drawing too
little: a parse that silently drops records draws a screenshot with no boxes,
which reads as the engine finding nothing. These pin the parse:

1. `word` and `glyph` outlines are read, per file, in order.
2. A `glyph` record without its outline (an example binary built before the
   outline was added) is refused, not skipped.
3. An outline with the wrong number of values is refused.

Pillow is stubbed unconditionally, as in `test_render_ocr_cards.py`: nothing
under test draws, and a skip when Pillow is absent would skip in CI.

Run: `python3 scripts/test_draw_ocr_characters.py`
"""

from __future__ import annotations

import importlib.util
import sys
import traceback
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load_module():
    """Imports the drawing script by path, with a stub PIL."""
    names = ("PIL", "PIL.Image", "PIL.ImageDraw")
    for name in names:
        sys.modules[name] = types.ModuleType(name)
    for name in names[1:]:
        setattr(sys.modules["PIL"], name.split(".")[1], sys.modules[name])
    spec = importlib.util.spec_from_file_location("draw_ocr_characters", HERE / "draw-ocr-characters.py")
    if spec is None or spec.loader is None:
        raise SystemExit("could not load draw-ocr-characters.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


OUTPUT = "\n".join(
    [
        "line\t00-a.rgba\t0\t40\t0.000 0.000 10.000 0.000 10.000 5.000 0.000 5.000",
        "word\t00-a.rgba\t0\t10 20 50 20 50 40 10 40",
        "glyph\t00-a.rgba\t0\t61\t10\t30\t10 20 30 20 30 40 10 40",
        "glyph\t00-a.rgba\t0\t62\t30\t50\t30 20 50 20 50 40 30 40",
        "word\t01-b.rgba\t0\t5 5 9 5 9 9 5 9",
    ]
)


def raises(function, *args) -> bool:
    try:
        function(*args)
    except ValueError:
        return True
    return False


def test_the_stub_did_not_hide_an_empty_module(module) -> None:
    assert hasattr(module, "parse"), "parse did not load; the stub hid a real failure"


def test_outlines_are_read_per_file(module) -> None:
    shapes = module.parse(OUTPUT)
    assert shapes["00-a.rgba"]["words"] == [[(10, 20), (50, 20), (50, 40), (10, 40)]], shapes
    assert shapes["00-a.rgba"]["glyphs"] == [
        [(10, 20), (30, 20), (30, 40), (10, 40)],
        [(30, 20), (50, 20), (50, 40), (30, 40)],
    ], shapes
    assert shapes["01-b.rgba"] == {"words": [[(5, 5), (9, 5), (9, 9), (5, 9)]], "glyphs": []}, shapes


def test_a_glyph_without_its_outline_is_refused(module) -> None:
    stale = "glyph\t00-a.rgba\t0\t61\t10\t30"
    assert raises(module.parse, stale), "a stale binary's glyph record was accepted"


def test_an_outline_needs_eight_values(module) -> None:
    assert raises(module.corners, "1 2 3 4 5 6 7")
    assert module.corners("1 2 3 4 5 6 7 8") == [(1, 2), (3, 4), (5, 6), (7, 8)]


def main() -> int:
    module = load_module()
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    failed = 0
    for test in tests:
        try:
            test(module)
        except Exception:  # noqa: BLE001 -- report every failure, then exit non-zero
            failed += 1
            print(f"FAIL  {test.__name__}")
            traceback.print_exc()
        else:
            print(f"ok    {test.__name__}")
    print(f"\n{len(tests) - failed}/{len(tests)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
