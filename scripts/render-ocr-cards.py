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
TEXTS: dict[str, str] = {
    "letters": "The quick brown fox jumps over the lazy dog",
    "digits": "0123456789 0123456789",
    "invoice": "Invoice 2026-09-04 Total: 1,284.50 EUR",
    "terminal": "error[E0308]: mismatched types",
}

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
        default=",".join(TEXTS),
        help=f"comma-separated text keys from {sorted(TEXTS)}",
    )
    arguments = parser.parse_args()

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
