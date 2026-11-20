# Bar harness

Real-render checks for the bar mode (roadmap 1.61) and the watching dot. Nothing here
runs in CI: it needs Windows, Edge and Node 22+, and writes its fixtures under the
gitignored `target/`.

## The bar

`bar-test.html` is the founder's test page: eleven bar styles, light and dark, three
speeds. Open it as it is to test a real watch area by hand. With `?static=1&style=round&pct=37.5&theme=dark&phase=0.4`
it draws one frame and runs nothing; `capture.mjs` uses that.

```
node tools/bar-harness/capture.mjs --out target/bar-fixtures
cargo test -p uptake-core --test bar_fixtures --release -- --ignored --nocapture
```

`capture.mjs` drives headless Edge over CDP (no packages) and writes one fixture per
style x theme x scale x margin, a luma picture of the bar's track plus page margin at every
0.5 % step (`--step`, default 0.5). Options: `--styles`, `--themes`, `--scales` (device scale, default
`1,1.5`), `--margins` (`8` is 8 CSS px of page all round, `0` a tight area, `label` includes the
percentage beside the bar, `8+1` moves the area's left and top edge one device pixel in, since
an area is rarely drawn on the 4-pixel cell grid), `--jobs`.

The ignored test feeds each fixture to a `Detector` set to *fills* with one mark at 50 % and
prints one row per fixture and scenario:

- `20s`, `60s`, `120s`: the bar fills in that time, with the readings a fill of that speed gives at
  about 30 readings a second (each picture repeated), timed with `observe_at`. A fill must give
  `Reached(50)` once, never before the bar is half full, then `Filled` once, never early.
- `again`: the area is placed over the bar at 30 %, it fills (20 % was passed and is not reported),
  empties and fills again: every moment again.
- `1.5s`, `3s`, `6s` (marquee only): the block sweeps at that pace, marks 20, 50 and 80, and must
  give nothing; the same sweep caught at other phases is counted, not judged (a block sitting at
  the far end reads as a filled bar, the bar mode's documented limit). A sweep slower than about
  6.5 s would report 20 %: its block, a third of the bar, stays joined to the start end with its
  front past 20 % for longer than the half-second calm, another documented limit (`Bar` in
  `crates/uptake-core/src/watch.rs`), so it is not among these paces.

`BAR_FILTER=round_dark` keeps the files whose name contains it. Build with `--release`: the
fixture runs are long in a debug build.

## The watching dot

```
node tools/bar-harness/dot-check.mjs
python tools/bar-harness/montage.py target/dot-check/tiles.json montage.png
```

Renders the page's own dot CSS (read from `src/routes/+page.svelte`) at device scales 1, 1.25,
1.5 and 2, resting and mid-pulse, on four backgrounds, measures how far each is from a
mirror-symmetric circle, and exits non-zero if any is not. `montage.py` (Pillow) magnifies the
tiles for looking at.
