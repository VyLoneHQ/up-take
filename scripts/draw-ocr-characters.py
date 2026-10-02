#!/usr/bin/env python3
"""Draw where the engine places each character onto a real screenshot (roadmap `1.44`).

`1.44` says to measure character boxes "on the `1.32` cards and on the rig,
before the page work". `measure-ocr-characters.py` covers the rendered half:
its lines are made the way the cards are, with each glyph's ink known exactly.
A real screen has no such ground truth, so its half cannot be a score. It is a
picture: the screenshot enlarged, every word's outline in one colour and every
character's in two alternating ones, for a person to judge whether the boxes sit
on the letters. The founder chose this on 2026-10-02 (review of `#119`, round 6).

Usage, after `cargo build --release -p uptake-ocr --example ocr_characters`:

    python scripts/draw-ocr-characters.py ^
        --runtime C:/_CORE/up-take/dist/runtime/onnxruntime.dll ^
        --models src-tauri/assets/models --out dist/char-pictures shot.png

Writes `<out>/<image name>-characters.png` for each image and prints its path.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw

#: Enlargement, nearest-neighbour: a 12 px glyph is about 7 px wide, too small to
#: judge a one-pixel edge at 1x.
SCALE = 4

WORD_COLOUR = (255, 190, 0)
#: Alternating, so two neighbouring character boxes never look like one.
CHARACTER_COLOURS = ((255, 0, 200), (0, 190, 255))


def corners(field: str) -> list[tuple[int, int]]:
    """`x0 y0 x1 y1 x2 y2 x3 y3` as four points."""
    values = [int(value) for value in field.split()]
    if len(values) != 8:
        raise ValueError(f"an outline needs 8 numbers, got {len(values)}: {field!r}")
    return list(zip(values[0::2], values[1::2]))


def parse(stdout: str) -> dict[str, dict[str, list]]:
    """The `word` and `glyph` outlines from `ocr_characters`, per file.

    A `glyph` record without its outline is an example binary from before the
    outline was added. That is refused rather than skipped: skipping would draw
    a picture with no characters, which reads as the engine finding none.
    """
    shapes: dict[str, dict[str, list]] = {}
    for record in stdout.splitlines():
        fields = record.split("\t")
        if fields[0] == "word":
            if len(fields) != 4:
                raise ValueError(f"a word record needs 4 fields: {record!r}")
            shapes.setdefault(fields[1], {"words": [], "glyphs": []})["words"].append(corners(fields[3]))
        elif fields[0] == "glyph":
            if len(fields) != 7:
                raise ValueError(f"a glyph record without its outline (a stale binary?): {record!r}")
            shapes.setdefault(fields[1], {"words": [], "glyphs": []})["glyphs"].append(corners(fields[6]))
    return shapes


def scaled(points: list[tuple[int, int]]) -> list[tuple[int, int]]:
    return [(x * SCALE, y * SCALE) for x, y in points]


def draw(image: Image.Image, shapes: dict[str, list]) -> Image.Image:
    big = image.convert("RGB").resize((image.width * SCALE, image.height * SCALE), Image.NEAREST)
    pen = ImageDraw.Draw(big)
    for outline in shapes["words"]:
        pen.polygon(scaled(outline), outline=WORD_COLOUR, width=2)
    for index, outline in enumerate(shapes["glyphs"]):
        pen.polygon(scaled(outline), outline=CHARACTER_COLOURS[index % 2], width=1)
    return big


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("images", nargs="+", type=Path)
    parser.add_argument("--out", type=Path, default=Path("dist/char-pictures"))
    parser.add_argument("--exe", type=Path, default=Path("target/release/examples/ocr_characters.exe"))
    parser.add_argument("--models", type=Path, default=Path("src-tauri/assets/models"))
    parser.add_argument("--runtime", type=Path, required=True)
    arguments = parser.parse_args()

    frames = arguments.out / "frames"
    frames.mkdir(parents=True, exist_ok=True)
    for stale in frames.glob("*.rgba"):
        stale.unlink()
    sources: dict[str, Image.Image] = {}
    for index, path in enumerate(arguments.images):
        image = Image.open(path).convert("RGBA")
        name = f"{index:02}-{path.stem}.rgba"
        header = image.width.to_bytes(4, "little") + image.height.to_bytes(4, "little")
        (frames / name).write_bytes(header + image.tobytes())
        sources[name] = image

    result = subprocess.run(
        [
            str(arguments.exe),
            "--models",
            str(arguments.models),
            "--runtime",
            str(arguments.runtime),
            "--lines",
            str(frames),
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    if result.returncode != 0:
        sys.exit(f"ocr_characters failed ({result.returncode}): {result.stderr.strip()}")
    shapes = parse(result.stdout)

    for name, image in sources.items():
        found = shapes.get(name, {"words": [], "glyphs": []})
        target = (arguments.out / f"{Path(name).stem}-characters.png").resolve()
        draw(image, found).save(target)
        print(f"{len(found['words'])} words, {len(found['glyphs'])} characters: {target}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
