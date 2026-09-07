#!/usr/bin/env python3
"""Tests for the ground-truth card renderer's text corpus (roadmap `1.33`).

# Why this file exists

`1.33` replaces the recogniser because the shipped one is Chinese, and it rules
out `en_PP-OCRv4_rec` on one specific ground: that dictionary is 95 characters
and **cannot represent `ä`, `ö` or `ß`**. On 2026-09-05 the card corpus was
measured and found to contain **no non-ASCII character at all**, so the harness
scored a recogniser that cannot spell German exactly as well as one that can.
The decision rested on a property the measurement could not see.

The German keys close that. These tests defend the two things about them that
would fail SILENTLY if a later change got them wrong.

1. **The default grid stays 192 cards.** Every headline figure this project has
   recorded is over that set -- CER 0.018, exact 87.0 %, empty 0.0 % as of the
   `ADR-0037` PP-OCRv6 swap (2026-09-05). Adding German to
   the default would take it to 288 and make a before/after comparison across
   the recogniser swap a comparison of two different card sets, with nothing
   announcing it. `DEFAULT_TEXT_KEYS` is the guard and this is its test.

2. **The German keys actually carry the characters they exist for.** A test set
   that has been quietly ASCII-fied still renders, still scores, and no longer
   measures anything. `ä`, `ö`, `ü` and `ß` are asserted by name.

# Why the PIL import is stubbed unconditionally

The renderer imports Pillow at module scope and exits if it is missing, and CI
does not install it. A test that skips when Pillow is absent would skip in every
environment it runs in, which is `UT-F-101` -- found on 2026-09-04 inside the
fix for the previous round's finding, on a suite that reported a total and hid
the difference. So the stub is installed **always**, in every environment,
rather than as a fallback: the behaviour here does not depend on what is
installed, and there is no skip branch to be blind in.

Nothing under test needs Pillow. These are assertions about two module-level
constants.

Run: `python3 scripts/test_render_ocr_cards.py`
"""

from __future__ import annotations

import importlib.util
import sys
import traceback
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent

#: The four keys the grid has rendered since `1.32`. Written out rather than
#: derived, so this test disagrees with the module when the module changes.
ORIGINAL_KEYS = ("letters", "digits", "invoice", "terminal")

#: 4 texts x 4 fonts x 6 sizes x 2 polarities. The number every recorded figure
#: is over.
EXPECTED_GRID_CARDS = 192


def load_module():
    """Imports the renderer by path, with a stub PIL, since its name is hyphenated."""
    for name in ("PIL", "PIL.Image", "PIL.ImageDraw", "PIL.ImageFont"):
        stub = types.ModuleType(name)
        # Attribute access on the stub returns another stub rather than raising,
        # so a module-level `from PIL import Image, ImageDraw, ImageFont` binds.
        sys.modules[name] = stub
    sys.modules["PIL"].Image = sys.modules["PIL.Image"]
    sys.modules["PIL"].ImageDraw = sys.modules["PIL.ImageDraw"]
    sys.modules["PIL"].ImageFont = sys.modules["PIL.ImageFont"]

    spec = importlib.util.spec_from_file_location(
        "render_ocr_cards", HERE / "render-ocr-cards.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load render-ocr-cards.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_the_stub_did_not_hide_an_empty_module(module) -> None:
    """The stub must not be able to make every other test here vacuous."""
    assert hasattr(module, "TEXTS"), "TEXTS did not load; the stub hid a real failure"
    assert hasattr(module, "DEFAULT_TEXT_KEYS"), "DEFAULT_TEXT_KEYS did not load"
    assert len(module.TEXTS) >= len(ORIGINAL_KEYS)


def test_default_keys_are_exactly_the_original_four(module) -> None:
    """The 192-card denominator every recorded figure is over."""
    assert tuple(module.DEFAULT_TEXT_KEYS) == ORIGINAL_KEYS, (
        "the default grid changed; every recorded CER/exact/empty figure is over "
        + str(ORIGINAL_KEYS)
        + " and would no longer be comparable"
    )
    cards = (
        len(module.DEFAULT_TEXT_KEYS)
        * len(module.FONTS)
        * len(module.SIZES)
        * len(module.POLARITIES)
    )
    assert cards == EXPECTED_GRID_CARDS, (
        "the default grid renders " + str(cards) + " cards, not " + str(EXPECTED_GRID_CARDS)
    )


def test_every_default_key_is_a_real_text(module) -> None:
    for key in module.DEFAULT_TEXT_KEYS:
        assert key in module.TEXTS, "default key " + key + " is not in TEXTS"


def test_german_is_available_but_not_in_the_default_grid(module) -> None:
    for key in ("german", "rechnung"):
        assert key in module.TEXTS, key + " is missing from TEXTS"
        assert key not in module.DEFAULT_TEXT_KEYS, (
            key + " is in the default grid, which moves the 192-card denominator"
        )


def test_the_german_keys_carry_the_characters_they_exist_for(module) -> None:
    """`1.33` rules a recogniser out on ä/ö/ß. The set must contain them."""
    german = "".join(module.TEXTS[key] for key in ("german", "rechnung"))
    # Named rather than written literally: this message is printed by CI, and a
    # non-ASCII byte on a redirected stream is a known hazard on this machine.
    required = {
        "ä": "LATIN SMALL LETTER A WITH DIAERESIS",
        "ö": "LATIN SMALL LETTER O WITH DIAERESIS",
        "ü": "LATIN SMALL LETTER U WITH DIAERESIS",
        "ß": "LATIN SMALL LETTER SHARP S",
        "Ö": "LATIN CAPITAL LETTER O WITH DIAERESIS",
        "Ü": "LATIN CAPITAL LETTER U WITH DIAERESIS",
    }
    for character, name in required.items():
        assert character in german, (
            "no card contains " + name + " (U+" + format(ord(character), "04X") + "), "
            "so the harness cannot see the character class 1.33 uses to choose a "
            "recogniser"
        )


def test_no_text_contains_a_tab(module) -> None:
    """A tab shifts every manifest column after it, silently."""
    for key, text in module.TEXTS.items():
        assert "\t" not in text, key + " would break cards.tsv"


def test_no_manifest_text_contains_a_newline(module) -> None:
    """The invariant that MOVED on 2026-09-07 rather than being dropped.

    This test used to forbid a newline anywhere in `TEXTS`, which is why the
    corpus was single-line and why the harness could not see `I-387`.
    Multi-line texts are allowed now; what must still hold is that the MANIFEST
    form carries no newline, because `write_manifest` refuses one and is right
    to.
    """
    for key, text in module.TEXTS.items():
        rendered = module.manifest_text(text)
        assert "\n" not in rendered, key + " would break cards.tsv"
        assert "\t" not in rendered, key + " would break cards.tsv"


def test_the_corpus_actually_contains_multi_line_text(module) -> None:
    """The whole point of `I-387`: a fixture that CAN contain the failure.

    A floor rather than an exact set, so adding a case does not go red -- but a
    corpus that silently loses its multi-line cards does.
    """
    assert len(module.MULTILINE_TEXT_KEYS) >= 3, (
        "the corpus has lost its multi-line cards, which is the state that made "
        "I-387 invisible to every figure this harness ever produced"
    )
    for key in module.MULTILINE_TEXT_KEYS:
        assert "\n" in module.TEXTS[key], (
            key + " is in MULTILINE_TEXT_KEYS and has one line"
        )


def test_multiline_keys_are_derived_not_hand_listed(module) -> None:
    """`MULTILINE_TEXT_KEYS` must agree with `TEXTS` by construction."""
    expected = tuple(key for key, text in module.TEXTS.items() if "\n" in text)
    assert module.MULTILINE_TEXT_KEYS == expected


def test_the_multi_line_set_spreads_its_line_counts(module) -> None:
    """Different line counts rather than three cards of the same shape.

    If reading degrades with the number of lines, a spread shows WHERE. Cards
    that all had the same count would only show THAT.
    """
    counts = {module.TEXTS[key].count("\n") + 1 for key in module.MULTILINE_TEXT_KEYS}
    assert len(counts) >= 3, "line counts are " + repr(sorted(counts))


def test_multi_line_keys_stay_out_of_the_default_grid(module) -> None:
    """The rule German already follows, for the same reason.

    Including them would move every headline figure recorded to date, so a
    before/after across a later change would compare two different card sets.
    """
    for key in module.MULTILINE_TEXT_KEYS:
        assert key not in module.DEFAULT_TEXT_KEYS, key + " would move the baseline"


def test_manifest_text_collapses_every_run_of_whitespace(module) -> None:
    """It must reduce to what the Rust harness's `normalise` reduces to.

    That function is `text.split_whitespace().collect().join(" ")` applied to
    BOTH sides of the comparison, so anything left behind here is a mismatch the
    engine would be blamed for.
    """
    assert module.manifest_text("a\nb") == "a b"
    assert module.manifest_text("a\n\nb") == "a b"
    assert module.manifest_text("  a \t b  ") == "a b"
    assert module.manifest_text("a\r\nb") == "a b"
    assert module.manifest_text("one") == "one"


def main() -> int:
    module = load_module()
    tests = [value for name, value in globals().items() if name.startswith("test_")]
    failures = 0
    for test in tests:
        try:
            test(module)
            print("ok    " + test.__name__)
        # BaseException, not Exception: SystemExit derives from it, and a
        # refusal in the code under test raises SystemExit -- which would
        # abort the whole run with no FAIL line, no summary and every later
        # test unrun. PR #88 round 6 PROSE 5 fixed this in ONE runner;
        # PR #89 round 2 FINDING 9 found the three beside it untouched.
        except BaseException:  # noqa: BLE001, B036 - a test runner reports everything
            failures += 1
            print("FAIL  " + test.__name__)
            traceback.print_exc()
    print("")
    print(str(len(tests) - failures) + "/" + str(len(tests)) + " passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
