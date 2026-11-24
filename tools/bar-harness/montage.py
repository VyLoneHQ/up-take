"""Writes a magnified montage of tiles.json (dot-check.mjs) for looking at: python montage.py tiles.json out.png [filter]."""
import base64, json, sys
from PIL import Image, ImageDraw
tiles = json.load(open(sys.argv[1]))
if len(sys.argv) > 3:
    tiles = [t for t in tiles if sys.argv[3] in t['label']]
Z = 12
cols = 4
rows = (len(tiles) + cols - 1) // cols
sheet = Image.new('RGB', (cols * (12 * Z + 8), rows * (12 * Z + 22)), (200, 60, 200))
d = ImageDraw.Draw(sheet)
for i, t in enumerate(tiles):
    im = Image.frombytes('RGB', (12, 12), base64.b64decode(t['rgb'])).resize((12 * Z, 12 * Z), Image.NEAREST)
    x, y = (i % cols) * (12 * Z + 8) + 4, (i // cols) * (12 * Z + 22) + 16
    sheet.paste(im, (x, y)); d.text((x, y - 13), t['label'], fill=(255, 255, 255))
sheet.save(sys.argv[2])
