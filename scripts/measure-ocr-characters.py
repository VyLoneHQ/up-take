"""Measure where the recogniser puts each character (roadmap 1.44).

1.44 lets a selection inside an OCR area start and end between single
characters. The engine knows only which of the recogniser's timesteps chose each
character, and a timestep is several source pixels wide, so two questions have
to be answered by measurement before any page work:

1. Is a timestep fine enough to tell neighbouring characters apart at screen
   sizes?
2. Where between two characters' runs does the boundary go?

This renders single lines whose true glyph positions are known, runs the real
engine over them (the `ocr_characters` example prints the raw runs), and scores
a family of boundary rules against the true ink. It prints the scores; it
decides nothing.

WHERE THE TRUTH COMES FROM
--------------------------

A character's ink is found by rendering the line up to and including it and
the line up to just before it, and taking the columns that differ. That places
each glyph where the full line places it, kerning included, without a shaping
engine. Glyphs that overlap their neighbour lose the overlapping columns to
whichever is drawn last, which is also what the eye sees.

Everything here is RENDERED text: cleaner than a real screen, so every figure is
an upper bound, as with the 1.32 cards.

USAGE
-----

    cargo build -p uptake-ocr --example ocr_characters --release
    python scripts/measure-ocr-characters.py --out dist/char-lines ^
        --exe target/release/examples/ocr_characters.exe ^
        --models src-tauri/assets/models --runtime dist/runtime/onnxruntime.dll
"""

from __future__ import annotations

import argparse
import statistics
import struct
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

FONT_DIRECTORY = Path("C:/Windows/Fonts")
FONTS = {
    "ui": "segoeui.ttf",
    "sans": "arial.ttf",
    "mono": "consola.ttf",
    "serif": "times.ttf",
    "georgia": "georgia.ttf",
}
SIZES = [12, 14, 16, 20]
POLARITIES = {"dark": ((0, 0, 0), (255, 255, 255)), "light": ((235, 235, 235), (30, 30, 30))}
PADDING = 16

#: Narrow letters next to wide ones, digits, punctuation, a path: the cases a
#: character selection gets wrong first.
TEXTS = {
    "pangram": "The quick brown fox jumps over the lazy dog.",
    "narrow": "illicit fill: little mill 1111 lilt",
    "wide": "MWmw WWW mmm Wow Mom maximum",
    "code": "src/main.rs:42 fn load(path: &Path) -> Result",
    "numbers": "Invoice 2026-09-29, total 1,234.50 EUR (VAT 20%)",
}


def load_font(file_name: str, size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(FONT_DIRECTORY / file_name), size)


def render(text: str, font: ImageFont.FreeTypeFont, foreground, background, size) -> Image.Image:
    image = Image.new("RGB", size, background)
    ImageDraw.Draw(image).text((PADDING, PADDING), text, font=font, fill=foreground)
    return image


def canvas_size(text: str, font: ImageFont.FreeTypeFont) -> tuple[int, int]:
    probe = ImageDraw.Draw(Image.new("RGB", (1, 1)))
    left, top, right, bottom = probe.textbbox((0, 0), text, font=font)
    return (right + PADDING * 2 + max(0, -left), bottom + PADDING * 2)


def glyph_ink(text: str, font, foreground, background, size) -> list[tuple[int, int] | None]:
    """Each character's ink as (first column, last column), or None for no ink."""
    inks: list[tuple[int, int] | None] = []
    previous = render("", font, foreground, background, size)
    for index in range(len(text)):
        current = render(text[: index + 1], font, foreground, background, size)
        box = ImageChops.difference(current, previous).getbbox()
        inks.append(None if box is None else (box[0], box[2] - 1))
        previous = current
    return inks


def write_rgba(image: Image.Image, path: Path) -> None:
    rgba = image.convert("RGBA")
    width, height = rgba.size
    path.write_bytes(struct.pack("<II", width, height) + rgba.tobytes())


def render_all(out: Path) -> dict[str, dict]:
    out.mkdir(parents=True, exist_ok=True)
    for stale in out.glob("*.rgba"):
        stale.unlink()
    truth: dict[str, dict] = {}
    for text_key, text in TEXTS.items():
        for font_key, font_file in FONTS.items():
            for size_px in SIZES:
                font = load_font(font_file, size_px)
                size = canvas_size(text, font)
                for polarity, (foreground, background) in POLARITIES.items():
                    name = f"{text_key}_{font_key}_{size_px}px_{polarity}.rgba"
                    write_rgba(render(text, font, foreground, background, size), out / name)
                    truth[name] = {
                        "text": text,
                        "font": font_key,
                        "size": size_px,
                        "ink": glyph_ink(text, font, foreground, background, size),
                    }
    return truth


#: What `Engine::recognise` itself placed, per file: the `glyph` records.
GLYPHS: dict[str, list[dict]] = {}


def score_engine(truth: dict[str, dict]) -> None:
    """Scores the character outlines the engine ships, whole pixels and all."""
    glyphs = hits = boundaries = wrong = 0
    distances: list[float] = []
    lines = 0
    for name, expected in sorted(truth.items()):
        placed = GLYPHS.get(name, [])
        inked = [
            (char, box, position)
            for position, (char, box) in enumerate(zip(expected["text"], expected["ink"]))
            if box is not None
        ]
        if not placed or {glyph["block"] for glyph in placed} != {0}:
            continue
        if "".join(glyph["text"] for glyph in placed) != "".join(char for char, _, _ in inked):
            continue
        lines += 1
        for glyph, (_, box, _) in zip(placed, inked):
            glyphs += 1
            centre = (box[0] + box[1] + 1) / 2
            if glyph["left"] <= centre < glyph["right"]:
                hits += 1
            else:
                distances.append(min(abs(centre - glyph["left"]), abs(centre - glyph["right"])))
        # A pointer at the middle of the true gap between two neighbours in the
        # same word must snap to the cut between exactly those two.
        cuts = [glyph["right"] for glyph in placed[:-1]]
        for index in range(len(inked) - 1):
            (_, left_box, left_at), (_, right_box, right_at) = inked[index], inked[index + 1]
            if right_at != left_at + 1:
                continue  # a space between them: a word boundary, 1.40's and not scored here
            boundaries += 1
            middle = (left_box[1] + 1 + right_box[0]) / 2
            nearest = min(range(len(cuts)), key=lambda k: abs(cuts[k] - middle))
            if nearest != index:
                wrong += 1
    if not glyphs:
        print("engine output: no line read exactly as one block")
        return
    print(
        f"engine output ({lines} lines, whole pixels): centre in own cell {hits}/{glyphs} "
        f"({100 * hits / glyphs:.1f}%), misses off by median "
        f"{statistics.median(distances) if distances else 0:.2f} px; "
        f"pointer at a true gap snaps to the wrong cut {wrong}/{boundaries} "
        f"({100 * wrong / boundaries:.1f}%)"
    )


def run_engine(exe: Path, models: Path, runtime: Path, out: Path) -> dict[str, list[dict]]:
    result = subprocess.run(
        [str(exe), "--models", str(models), "--runtime", str(runtime), "--lines", str(out)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    if result.returncode != 0:
        sys.exit(f"ocr_characters failed ({result.returncode}): {result.stderr.strip()}")
    lines: dict[str, list[dict]] = {}
    for record in result.stdout.splitlines():
        fields = record.split("\t")
        if fields[0] == "glyph":
            text = "".join(chr(int(code, 16)) for code in fields[3].split("+") if code)
            GLYPHS.setdefault(fields[1], []).append(
                {"block": int(fields[2]), "text": text, "left": int(fields[4]), "right": int(fields[5])}
            )
        elif fields[0] == "line":
            corners = [float(value) for value in fields[4].split()]
            lines.setdefault(fields[1], []).append(
                {"timesteps": int(fields[3]), "corners": corners, "chars": []}
            )
        elif fields[0] == "char":
            text = "".join(chr(int(code, 16)) for code in fields[3].split("+") if code)
            lines[fields[1]][int(fields[2])]["chars"].append(
                {"text": text, "first": int(fields[4]), "last": int(fields[5])}
            )
    return lines


def cells(line: dict, shift: float) -> list[tuple[float, float]]:
    """Each emitted character's cell in source x, for boundary rule `shift`.

    The boundary between two consecutive characters is the midpoint of the
    first one's last timestep and the second's first, plus `shift` timesteps,
    as a fraction of the line, mapped along the quad's top edge. The first cell
    starts at the quad's left edge and the last ends at its right, as 1.40's
    words do.
    """
    total = line["timesteps"]
    x0, _, x1, _ = line["corners"][:4]
    chars = line["chars"]
    cuts = [0.0]
    for left, right in zip(chars, chars[1:]):
        u = ((left["last"] + right["first"]) / 2 + shift) / total
        cuts.append(min(1.0, max(0.0, u)))
    cuts.append(1.0)
    return [(x0 + (x1 - x0) * a, x0 + (x1 - x0) * b) for a, b in zip(cuts, cuts[1:])]


def score(truth: dict[str, dict], lines: dict[str, list[dict]], shifts: list[float]) -> None:
    usable: list[tuple[str, dict]] = []
    misread = 0
    for name, expected in sorted(truth.items()):
        found = lines.get(name, [])
        if len(found) != 1:
            misread += 1
            continue
        read = "".join(char["text"] for char in found[0]["chars"])
        if read != expected["text"]:
            misread += 1
            continue
        usable.append((name, found[0]))
    print(f"lines: {len(truth)} rendered, {len(usable)} read exactly, {misread} not (skipped)")
    if not usable:
        return

    widths = [
        (line["corners"][2] - line["corners"][0]) / line["timesteps"] for _, line in usable
    ]
    print(
        f"timestep width in source px: median {statistics.median(widths):.2f}, "
        f"min {min(widths):.2f}, max {max(widths):.2f}"
    )
    runs = [char["last"] - char["first"] + 1 for _, line in usable for char in line["chars"]]
    print(f"run length in timesteps: median {statistics.median(runs)}, max {max(runs)}")

    for shift in shifts:
        glyphs = 0
        hits = 0
        misses_by_size: dict[int, int] = {}
        errors: list[float] = []
        cut_own = 0
        boundaries = 0
        for name, line in usable:
            expected = truth[name]
            ink = expected["ink"]
            placed = cells(line, shift)
            for index, box in enumerate(ink):
                if box is None:
                    continue
                glyphs += 1
                centre = (box[0] + box[1] + 1) / 2
                start, end = placed[index]
                if start <= centre < end:
                    hits += 1
                else:
                    misses_by_size[expected["size"]] = misses_by_size.get(expected["size"], 0) + 1
            # Boundaries between two inked neighbours: error from the middle of
            # the gap between their ink, and whether the cut enters either.
            for index in range(len(ink) - 1):
                left, right = ink[index], ink[index + 1]
                if left is None or right is None:
                    continue
                boundaries += 1
                cut = placed[index][1]
                middle = (left[1] + 1 + right[0]) / 2
                errors.append(cut - middle)
                if cut < left[1] + 1 - 1 or cut > right[0] + 1:
                    cut_own += 1
        by_size = ", ".join(f"{size}px {count}" for size, count in sorted(misses_by_size.items()))
        print(
            f"shift {shift:+.2f} ts: centre in own cell {hits}/{glyphs} "
            f"({100 * hits / glyphs:.1f}%), misses by size [{by_size or 'none'}]; "
            f"cut error px median {statistics.median(errors):+.2f}, "
            f"mean |err| {statistics.mean(abs(e) for e in errors):.2f}; "
            f"cuts into ink >1px {cut_own}/{boundaries}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--out", type=Path, default=Path("dist/char-lines"))
    parser.add_argument("--exe", type=Path, default=Path("target/release/examples/ocr_characters.exe"))
    parser.add_argument("--models", type=Path, default=Path("src-tauri/assets/models"))
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--shifts", default="-0.5,0,0.25,0.5,0.75,1.0")
    arguments = parser.parse_args()
    truth = render_all(arguments.out)
    lines = run_engine(arguments.exe, arguments.models, arguments.runtime, arguments.out)
    score(truth, lines, [float(value) for value in arguments.shifts.split(",")])
    score_engine(truth)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
