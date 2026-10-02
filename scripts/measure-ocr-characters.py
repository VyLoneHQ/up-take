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
import math
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
    """The font, laid out with Pillow's BASIC engine on every machine.

    `glyph_ink` takes a character's ink as the difference between two prefixes
    rendered separately, which is only that character's ink if adding it never
    changes an earlier glyph. RAQM shaping breaks that: "fi" in "fill" becomes a
    ligature and the difference covers both letters. Pillow uses RAQM wherever
    it was built with it, so without this pin the ground truth would depend on
    the machine (review of `#119`, round 4).
    """
    return ImageFont.truetype(
        str(FONT_DIRECTORY / file_name), size, layout_engine=ImageFont.Layout.BASIC
    )


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


#: The shipping constant, `CHARACTER_CUT_SHIFT` in `recognise.rs`, restated
#: here only so the parity check below can say whether `cells` still mirrors it.
SHIPPED_SHIFT = 0.35

#: The sweep's default candidates. They must include `SHIPPED_SHIFT` and 0.40,
#: the value it was chosen over (`recognise.rs`: both held 99.4% of glyph
#: centres, and 0.35 had the smaller cut error), so the documented invocation
#: reproduces that comparison (review of `#119`, round 5).
DEFAULT_SHIFTS = (-0.5, 0.0, 0.25, 0.3, 0.35, 0.4, 0.5, 0.75, 1.0)


#: Float rounding at half pixels legitimately moves a few cells: 4 of 6026 on
#: 2026-09-29. A shift 0.01 away from the engine's moved 336, so 0.2% separates
#: the two by two orders of magnitude.
PARITY_TOLERANCE = 0.002


def parity_verdict(compared: int, differ: int) -> str | None:
    """None when the sweep mirrors the engine, else why it does not."""
    if compared == 0:
        return "no glyph was comparable, so parity was not checked at all"
    if differ > compared * PARITY_TOLERANCE:
        return f"{differ} of {compared} cells differ, over {PARITY_TOLERANCE:.1%}"
    return None


def parity(truth: dict[str, dict], lines: dict[str, list[dict]]) -> bool:
    """Checks that `cells` at the shipped shift reproduces the engine's outlines.

    The shift sweep is only evidence for the shipped constant if it scores the
    shipped algorithm; the first version did not (review of `#119`, F1). Each
    inked glyph's cell is rounded as `point_from` rounds (half away from zero)
    and compared with the `glyph` record for the same character. Returns
    False, and `main` exits 1, when they disagree or nothing was compared
    (review of `#119`, round 2: a check that only prints cannot fail).
    """
    compared = differ = 0
    for name, expected in sorted(truth.items()):
        found = lines.get(name, [])
        placed = GLYPHS.get(name, [])
        if len(found) != 1 or not placed or {glyph["block"] for glyph in placed} != {0}:
            continue
        if "".join(char["text"] for char in found[0]["chars"]) != expected["text"]:
            continue
        mirrored = [
            cell
            for cell, char in zip(cells(found[0], SHIPPED_SHIFT), found[0]["chars"])
            if char["text"] and not char["text"].isspace()
        ]
        if len(mirrored) != len(placed):
            # Every cell of the longer side is unmatched, and is charged as
            # such: counting a whole line as one difference let it pass under
            # the tolerance (review of `#119`, round 3).
            unmatched = max(len(mirrored), len(placed))
            compared += unmatched
            differ += unmatched
            continue
        for (left, right), glyph in zip(mirrored, placed):
            compared += 1
            rounded = (math.floor(left + 0.5), math.floor(right + 0.5))
            if rounded != (glyph["left"], glyph["right"]):
                differ += 1
    print(f"parity at shift {SHIPPED_SHIFT}: {differ} of {compared} glyph cells differ from the engine's")
    verdict = parity_verdict(compared, differ)
    if verdict is not None:
        print(f"PARITY FAILED: {verdict}. The sweep does not score the shipped algorithm.")
        return False
    return True


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

    Mirrors the engine, `DecodedText::words` then `character_spans`, so a shift
    is scored on the algorithm that ships (review of `#119`, F1): words meet at
    the middle timestep index of the whole whitespace run between them, the
    first word starts at the quad's left edge and the last ends at its right,
    and ONLY the cuts between two characters inside one word move with `shift`,
    clamped into the word. A whitespace character gets an empty cell at its
    word boundary, since it names no ink. Fractions map along the quad's top
    edge.
    """
    total = line["timesteps"]
    x0, _, x1, _ = line["corners"][:4]
    chars = line["chars"]
    is_space = [char["text"] != "" and char["text"].isspace() for char in chars]
    # Split into words exactly as `words()` does.
    words: list[list[int]] = []
    gaps: list[tuple[int, int] | None] = []  # the whitespace run before each word
    current: list[int] = []
    gap: tuple[int, int] | None = None
    pending_gap: tuple[int, int] | None = None
    for index, char in enumerate(chars):
        if char["text"] == "":
            continue
        if is_space[index]:
            gap = (gap[0], char["last"]) if gap else (char["first"], char["last"])
            continue
        if gap is not None and current:
            words.append(current)
            gaps.append(pending_gap)
            pending_gap = gap
            current = []
        elif gap is not None:
            pending_gap = None
        gap = None
        current.append(index)
    if current:
        words.append(current)
        gaps.append(pending_gap)
    placed: list[tuple[float, float] | None] = [None] * len(chars)
    word_start = 0.0
    for number, members in enumerate(words):
        if number + 1 < len(words):
            first, last = gaps[number + 1]
            word_end = (first + last) / 2 / total
        else:
            word_end = 1.0
        begin = word_start
        for position, index in enumerate(members):
            if position + 1 < len(members):
                following = chars[members[position + 1]]
                middle = (chars[index]["last"] + following["first"]) / 2
                finish = min(word_end, max(begin, (middle + shift) / total))
            else:
                finish = word_end
            placed[index] = (begin, finish)
            begin = finish
        word_start = word_end
    result = []
    for index, cell in enumerate(placed):
        if cell is None:
            # Whitespace or an empty entry: an empty cell at the preceding cut.
            previous = next((c for c in reversed(placed[:index]) if c is not None), (0.0, 0.0))
            cell = (previous[1], previous[1])
        a, b = cell
        result.append((x0 + (x1 - x0) * a, x0 + (x1 - x0) * b))
    return result


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
    parser.add_argument("--shifts", default=",".join(str(shift) for shift in DEFAULT_SHIFTS))
    arguments = parser.parse_args()
    truth = render_all(arguments.out)
    lines = run_engine(arguments.exe, arguments.models, arguments.runtime, arguments.out)
    score(truth, lines, [float(value) for value in arguments.shifts.split(",")])
    score_engine(truth)
    return 0 if parity(truth, lines) else 1


if __name__ == "__main__":
    raise SystemExit(main())
