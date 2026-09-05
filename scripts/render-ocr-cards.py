#!/usr/bin/env python3
"""Render OCR ground-truth cards: known text, at known sizes, polarities and fonts.

Roadmap task 1.32, BACKLOG.md I-351. The pipeline has returned text since 1.31
and nothing has ever measured whether the text is right. I-341 cannot be
answered before something does, because an accuracy bar cannot be set on an
unmeasured pipeline.

WHAT THIS IS NOT
----------------

It is not a screenshot corpus. Every card here is text this script rendered, so
the ground truth is exact by construction rather than by somebody transcribing a
screenshot and being trusted. That is the whole reason to render rather than
collect: a corpus whose labels are themselves a human's reading measures the
labeller as much as the engine.

The cost of that choice is stated rather than hidden. Rendered text is CLEANER
than most screen text: no subpixel smearing from a different renderer, no JPEG
ringing, no scaled-down UI, no video compression. So a figure measured here is
an UPPER BOUND on what the same pipeline does on a real desktop, and it must
never be quoted as "UP-TAKE's accuracy" without that sentence attached.

USAGE
-----

    python scripts/render-ocr-cards.py --out dist/cards

Writes one `.rgba` per card plus `cards.tsv`, the manifest the Rust harness
reads. The `.rgba` format is the flat one `crates/uptake-ocr/examples/ocr_smoke.rs`
documents: little-endian u32 width, little-endian u32 height, then width * height
* 4 bytes of RGBA. Kept identical on purpose, so the two tools read one format
and a card can be dropped straight into the smoke example when one misreads.

WHY TSV AND NOT JSON
--------------------

So the Rust side needs no JSON dependency. `uptake-ocr`'s whole point is that it
has almost none, and the smoke example says so about its image format for the
same reason. There is exactly ONE manifest rather than a TSV for the harness and
a JSON for humans: two representations that must agree is the shape the
independent review of PR #82 found in `Recognition`, where a public field and a
private index drifted apart and whole lines vanished with no error. TSV loads
into pandas or a spreadsheet unchanged, so the second file would buy nothing.

No field may contain a tab. The card texts are checked for one before writing,
rather than trusted.
"""

from __future__ import annotations

import argparse

import struct
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover - the message is the whole handling
    sys.exit(
        "Pillow is needed to render the cards: python -m pip install Pillow\n"
        "It is a harness dependency only. Nothing UP-TAKE ships imports it."
    )


# The strings. Each is chosen for a failure the founder reported at the rig on
# 2026-09-03, not for coverage of an alphabet.
#
#   letters   -- "digits are read reliably, letters are not" was the report.
#                A pangram puts every letter in one line so a per-letter defect
#                cannot hide behind a lucky word.
#   digits    -- the half that was reported as working. It is here to be the
#                control: if digits also score badly, the harness is wrong
#                before the engine is.
#   invoice   -- the actual use case, and the one whose errors cost money. The
#                smoke test read "Invoice2026-09-01" and "Tota1:", so the space
#                and the l/1 confusion are both already-observed failures.
#   terminal  -- small light-on-dark monospace, which is the case that failed
#                worst at the rig.
#
# The two German keys below are NOT in the default set -- see DEFAULT_TEXT_KEYS.
#
#   german    -- umlauts and the eszett, which is the character class roadmap
#                `1.33` uses to RULE OUT `en_PP-OCRv4_rec`: that dictionary is
#                95 characters and cannot represent `ä`, `ö` or `ß`. Until this
#                key existed the harness scored a recogniser that cannot spell
#                German exactly as well as one that can, so the measurement
#                could not see the constraint the decision rests on.
#   rechnung  -- the same case as `invoice` in Austrian form: comma decimal,
#                full stop as the thousands separator, and the euro sign. The
#                founder is Austrian and this is the string he would actually
#                point the product at.
TEXTS: dict[str, str] = {
    "letters": "The quick brown fox jumps over the lazy dog",
    "digits": "0123456789 0123456789",
    "invoice": "Invoice 2026-09-04 Total: 1,284.50 EUR",
    "terminal": "error[E0308]: mismatched types",
    "german": "Größe 42 Straße Häuser Öl Übung grün weiß schön",
    "rechnung": "Rechnung 2026-09-05 Betrag: 1.284,50 EUR Umsatzsteuer",
}

#: The keys the grid renders when `--texts` is not given.
#:
#: German is deliberately EXCLUDED from the default. Adding it to the grid would
#: take the set from 192 cards to 288 and move every headline figure this
#: project has recorded -- CER 0.140, exact 66.7 %, empty 7.8 % on 2026-09-05 --
#: so a before/after comparison across the recogniser swap would be measuring
#: two different card sets. Render German with `--texts german,rechnung --out
#: dist/cards-de` and report it as its own population.
DEFAULT_TEXT_KEYS: tuple[str, ...] = ("letters", "digits", "invoice", "terminal")

# Font pixel sizes. 7 is here because the founder read 7 px text successfully at
# the rig, which falsified the resolution hypothesis and is worth keeping under
# measurement rather than assuming.
SIZES: tuple[int, ...] = (7, 10, 14, 20, 28, 40)

# Polarity, as (background, foreground). Not pure black and white: real screen
# text almost never is, and a pipeline tuned on maximum contrast would flatter
# itself here.
POLARITIES: dict[str, tuple[tuple[int, int, int], tuple[int, int, int]]] = {
    "dark-on-light": ((246, 246, 246), (24, 24, 24)),
    "light-on-dark": ((28, 28, 30), (232, 232, 232)),
}

# Fonts, by what they stand for rather than by name.
FONTS: dict[str, str] = {
    "sans": "arial.ttf",
    "mono": "consola.ttf",
    "ui": "segoeui.ttf",
    "serif": "times.ttf",
}

FONT_DIRECTORY = Path("C:/Windows/Fonts")

# Padding around the text, in pixels. Generous on purpose: the detector is a
# segmentation model and text flush against an edge is a different measurement
# from text on a card, which is not the one this harness is for.
PADDING = 16


def load_font(file_name: str, size: int) -> ImageFont.FreeTypeFont:
    path = FONT_DIRECTORY / file_name
    if not path.exists():
        sys.exit(
            f"font not found: {path}\n"
            "The card set names Windows' own fonts. On a machine without them, "
            "pass --fonts to name others, and say so beside any figure you "
            "report: a different renderer is a different measurement."
        )
    return ImageFont.truetype(str(path), size)


def render(text: str, font: ImageFont.FreeTypeFont, polarity: str) -> Image.Image:
    background, foreground = POLARITIES[polarity]
    # Measured before the canvas is made, so nothing is clipped. `textbbox` on a
    # throwaway 1x1 image is the documented way to ask.
    probe = ImageDraw.Draw(Image.new("RGB", (1, 1)))
    left, top, right, bottom = probe.textbbox((0, 0), text, font=font)
    width = (right - left) + PADDING * 2
    height = (bottom - top) + PADDING * 2
    image = Image.new("RGB", (width, height), background)
    draw = ImageDraw.Draw(image)
    draw.text((PADDING - left, PADDING - top), text, font=font, fill=foreground)
    return image


def glyph_height(text: str, font: ImageFont.FreeTypeFont) -> int:
    """The ink height of `text`, which is what I-350's threshold was measured in.

    NOT the font's pixel size: a 40 px font renders a line of lowercase letters
    considerably shorter than 40 px, and the two were conflated once already.
    Recorded per card so a later reading of these results compares like with
    like.
    """
    probe = ImageDraw.Draw(Image.new("RGB", (1, 1)))
    _, top, _, bottom = probe.textbbox((0, 0), text, font=font)
    return bottom - top


def write_rgba(image: Image.Image, path: Path) -> None:
    rgba = image.convert("RGBA")
    width, height = rgba.size
    with path.open("wb") as handle:
        handle.write(struct.pack("<II", width, height))
        handle.write(rgba.tobytes())


# The manifest columns, in order. `text` is last because it is the only free
# form field, so a reader scanning the left of the file sees the conditions.
COLUMNS = (
    "file",
    "text_key",
    "font",
    "font_file",
    "size_px",
    "glyph_height_px",
    "polarity",
    "width",
    "height",
    "text",
)


def write_manifest(cards: list[dict[str, object]], path: Path) -> None:
    lines = ["\t".join(COLUMNS)]
    for card in cards:
        values = [str(card[column]) for column in COLUMNS]
        for column, value in zip(COLUMNS, values):
            # Checked rather than assumed. A tab in any field silently shifts
            # every column after it, and the harness would then compare
            # recognised text against a font name without noticing.
            if "\t" in value or "\n" in value:
                sys.exit(f"card field {column!r} contains a tab or newline: {value!r}")
        lines.append("\t".join(values))
    # newline="" so this file is LF on Windows too: it is read by a Rust harness
    # and compared across machines, and CRLF would make two identical runs
    # differ by their line endings.
    path.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="")


#: The width-sweep set: one line of text, identical in every card, on canvases
#: of increasing width. Chosen to straddle `limit_side_len` (960) so the
#: downscaling boundary is inside the range rather than at its edge.
SWEEP_WIDTHS = (608, 672, 736, 800, 864, 928, 992, 1056, 1120, 1184, 1248, 1312)

#: The line the sweep uses. Six words at a comfortable size: the point is that
#: the GLYPHS never change, so any difference in the reading is caused by the
#: empty space around them and nothing else.
SWEEP_TEXT = "The quick brown fox jumps over"
SWEEP_SIZE = 18


def write_width_sweep(out: Path) -> int:
    """Renders one line of text on canvases of increasing width.

    # Why this is a separate mode rather than another axis of the grid

    The grid varies what the text IS. This varies what surrounds it, holding the
    text pixel-identical, which is the only way to show that a reading failure
    is caused by the frame rather than by the content. The founder's rig report
    on 2026-09-04 was about area WIDTH ("smaller than 700px"), and the grid
    cannot express that question at all.

    # Why it exists as shipped tooling rather than a scratch script

    It was a scratch script, and the independent review of `PR #87` was right to
    call that out: the detector's threshold change is justified in a doc comment
    partly by "0.6 read 4 of 12 cards and 0.4 read 12 of 12", and no committed
    tool reproduced those figures. A number baked into a public type's
    documentation that nobody can re-derive is an assertion wearing a
    measurement's clothes.
    """
    out.mkdir(parents=True, exist_ok=True)
    font = load_font(FONTS["ui"], SWEEP_SIZE)
    probe = ImageDraw.Draw(Image.new("RGB", (1, 1)))
    left, top, right, bottom = probe.textbbox((0, 0), SWEEP_TEXT, font=font)
    text_width, text_height = right - left, bottom - top
    height = text_height + PADDING * 2
    background, foreground = POLARITIES["dark-on-light"]

    cards = []
    for width in SWEEP_WIDTHS:
        if width < text_width + PADDING * 2:
            sys.exit(
                f"canvas width {width} cannot hold {text_width} px of text plus padding;"
                " the sweep must never crop the line it is holding constant"
            )
        image = Image.new("RGB", (width, height), background)
        ImageDraw.Draw(image).text(
            (PADDING - left, PADDING - top), SWEEP_TEXT, font=font, fill=foreground
        )
        name = f"w{width:04d}.rgba"
        write_rgba(image, out / name)
        cards.append(
            {
                "file": name,
                "text": SWEEP_TEXT,
                "text_key": f"w{width:04d}",
                "font": "ui",
                "font_file": FONTS["ui"],
                "size_px": SWEEP_SIZE,
                "glyph_height_px": text_height,
                "polarity": "dark-on-light",
                "width": width,
                "height": height,
            }
        )

    write_manifest(cards, out / "cards.tsv")
    print(f"{len(cards)} width-sweep cards written to {out}")
    print(f"the text is {text_width}x{text_height} px in every one; only the canvas differs")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default="dist/cards", help="output directory")
    parser.add_argument(
        "--sizes",
        default=",".join(str(size) for size in SIZES),
        help="comma-separated font pixel sizes",
    )
    parser.add_argument(
        "--fonts",
        default=",".join(FONTS),
        help=f"comma-separated font keys from {sorted(FONTS)}",
    )
    parser.add_argument(
        "--texts",
        default=",".join(DEFAULT_TEXT_KEYS),
        help=f"comma-separated text keys from {sorted(TEXTS)}",
    )
    parser.add_argument(
        "--width-sweep",
        action="store_true",
        help="render the constant-text, varying-canvas set instead of the grid",
    )
    arguments = parser.parse_args()

    if arguments.width_sweep:
        return write_width_sweep(Path(arguments.out))

    sizes = [int(size) for size in arguments.sizes.split(",") if size]
    font_keys = [key for key in arguments.fonts.split(",") if key]
    text_keys = [key for key in arguments.texts.split(",") if key]

    for key in font_keys:
        if key not in FONTS:
            sys.exit(f"unknown font key {key!r}; known: {sorted(FONTS)}")
    for key in text_keys:
        if key not in TEXTS:
            sys.exit(f"unknown text key {key!r}; known: {sorted(TEXTS)}")

    out = Path(arguments.out)
    out.mkdir(parents=True, exist_ok=True)

    cards = []
    for text_key in text_keys:
        text = TEXTS[text_key]
        for font_key in font_keys:
            for size in sizes:
                font = load_font(FONTS[font_key], size)
                for polarity in POLARITIES:
                    name = f"{text_key}_{font_key}_{size}px_{polarity}.rgba"
                    image = render(text, font, polarity)
                    write_rgba(image, out / name)
                    cards.append(
                        {
                            "file": name,
                            "text": text,
                            "text_key": text_key,
                            "font": font_key,
                            "font_file": FONTS[font_key],
                            "size_px": size,
                            "glyph_height_px": glyph_height(text, font),
                            "polarity": polarity,
                            "width": image.width,
                            "height": image.height,
                        }
                    )

    manifest_path = out / "cards.tsv"
    write_manifest(cards, manifest_path)
    print(f"{len(cards)} cards written to {out}")
    print(f"manifest: {manifest_path}")
    print(
        "Rendered text, not captured text: every figure measured against this "
        "set is an UPPER BOUND on real screen text."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
