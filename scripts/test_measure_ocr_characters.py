#!/usr/bin/env python3
"""Tests for the character measurement's own logic (roadmap `1.44`).

`scripts/measure-ocr-characters.py` is the evidence for `CHARACTER_CUT_SHIFT`,
so the parts of it that could be wrong without anybody seeing are tested here,
with no model and no runtime:

1. **The sweep mirrors the engine.** Round 1 of the review of `#119` found it
   scoring a different algorithm (shifting word edges the engine never moves).
   `cells` is pinned here to the same hand-computed line the Rust test
   `characters_share_their_words_edges_and_each_others` uses.
2. **The script's copy of the constant matches the Rust one.** It is read from
   `recognise.rs`, so a change to either side alone fails here.
3. **The parity check can fail.** Round 2 found it only printed. Its verdict
   is tested for a pass, for too many differing cells, and for nothing
   compared at all.

Pillow is stubbed unconditionally, as in `test_render_ocr_cards.py`: nothing
under test renders, and a skip when Pillow is absent would skip in CI.

Run: `python3 scripts/test_measure_ocr_characters.py`
"""

from __future__ import annotations

import importlib.util
import re
import sys
import traceback
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
RECOGNISE = HERE.parent / "crates" / "uptake-ocr" / "src" / "paddle" / "recognise.rs"


def load_module():
    """Imports the measurement script by path, with a stub PIL."""
    names = ("PIL", "PIL.Image", "PIL.ImageChops", "PIL.ImageDraw", "PIL.ImageFont")
    for name in names:
        sys.modules[name] = types.ModuleType(name)
    for name in names[1:]:
        setattr(sys.modules["PIL"], name.split(".")[1], sys.modules[name])
    spec = importlib.util.spec_from_file_location(
        "measure_ocr_characters", HERE / "measure-ocr-characters.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load measure-ocr-characters.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def two_words_line() -> dict:
    """"ab ba" over 10 timesteps on a box from x = 100 to x = 300.

    The same decode as `two_words()` in `paddle/mod.rs`'s tests.
    """
    run = lambda text, first, last: {"text": text, "first": first, "last": last}  # noqa: E731
    return {
        "timesteps": 10,
        "corners": [100.0, 50.0, 300.0, 50.0, 300.0, 70.0, 100.0, 70.0],
        "chars": [run("a", 0, 0), run("b", 1, 1), run(" ", 4, 6), run("b", 7, 7), run("a", 8, 8)],
    }


def close(a: float, b: float) -> bool:
    return abs(a - b) < 1e-6


def test_cells_mirror_the_engine(module) -> None:
    placed = module.cells(two_words_line(), module.SHIPPED_SHIFT)
    shift = module.SHIPPED_SHIFT
    # Word 1 ends at the middle index of the space run, 5 / 10: x = 200.
    # Inside it, a|b is cut at (0.5 + shift) / 10 of 200 px.
    cut_1 = 100 + 200 * (0.5 + shift) / 10
    cut_2 = 100 + 200 * (7.5 + shift) / 10
    expected = [(100, cut_1), (cut_1, 200), (200, 200), (200, cut_2), (cut_2, 300)]
    assert len(placed) == len(expected), placed
    for got, want in zip(placed, expected):
        assert close(got[0], want[0]) and close(got[1], want[1]), (placed, expected)


def test_word_edges_do_not_move_with_the_shift(module) -> None:
    # F1 of round 1: the first version moved these.
    for shift in (0.0, 0.35, 1.0):
        placed = module.cells(two_words_line(), shift)
        assert close(placed[0][0], 100) and close(placed[1][1], 200), (shift, placed)
        assert close(placed[3][0], 200) and close(placed[4][1], 300), (shift, placed)


def test_the_scripts_shift_is_the_engines(module) -> None:
    found = re.findall(
        r"^pub const CHARACTER_CUT_SHIFT: f32 = ([0-9.]+);$",
        RECOGNISE.read_text(encoding="utf-8"),
        flags=re.MULTILINE,
    )
    assert len(found) == 1, f"expected one CHARACTER_CUT_SHIFT in recognise.rs, found {found}"
    assert float(found[0]) == module.SHIPPED_SHIFT, (found[0], module.SHIPPED_SHIFT)


def test_parity_passes_on_rounding_noise(module) -> None:
    assert module.parity_verdict(6026, 4) is None


def test_parity_fails_on_a_different_algorithm(module) -> None:
    # A shift 0.01 away moved 336 of 6026 cells on 2026-09-29.
    assert module.parity_verdict(6026, 336) is not None


def test_parity_fails_when_nothing_was_compared(module) -> None:
    assert module.parity_verdict(0, 0) is not None


def main() -> int:
    module = load_module()
    tests = [
        value
        for name, value in sorted(globals().items())
        if name.startswith("test_") and callable(value)
    ]
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
