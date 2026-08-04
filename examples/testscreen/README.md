# The defined test screen

`quality-bars.md` section 1's *frozen view painted* row is content dependent. PNG encode and PNG
decode both scale with image complexity, so a timing taken against whatever happened to be on the
desktop is a number whose precondition nobody stated. That has cost this project two findings
already, eight days apart and in the same task.

This page is the fix. It puts the rig into a known state, and the freeze path reports the encoded
PNG's byte length beside every timing so a mislabelled run is detectable rather than trusted.

## The three screens

| Screen | Query | What it is | What it bounds |
| --- | --- | --- | --- |
| PLAIN | `?screen=plain` | Flat mid grey (`#808080`) edge to edge | The floor. The pipeline with the content removed |
| DENSE | `?screen=dense` | Deterministic RGB noise from a fixed seed | The ceiling. The encoder's worst case |
| BLOCKS | `?screen=blocks` | 8x8 blocks, one random colour each | The control, see below |

**BLOCKS is not a third data point.** It exists to falsify the self description. If an unlisted
screen could not be told from a listed one by its reported byte length, the byte length would be
worth nothing. Run it and confirm it lands visibly between the other two.

Measured through the real encoder at 640x400: **PLAIN 1,987 bytes, BLOCKS 23,065, DENSE 895,515.**
PLAIN and DENSE separate by 451x, and the control sits 11.6x above the floor and a 39th of the
ceiling. Both figures are asserted in the test suite
(`the_defined_test_screens_separate_by_an_order_of_magnitude` and
`an_unlisted_screen_lands_visibly_between_the_two` in `src-tauri/src/output.rs`), so the bracket
cannot quietly stop separating.

The control was smooth vertical bands first, and that was wrong: it encoded to 2,011 bytes against
PLAIN's 1,987, a margin of 1.2 % that no reader of a rig log could tell apart. PNG filters rows
before it deflates them, so hard edged vertical bands are nearly as compressible as a flat field.

## Putting the rig into it

1. Open the page once per monitor, in any browser, and drag each window to its own screen.
2. Press `F11` on each for full screen, then `H` to hide the note.
3. Confirm no taskbar and no wallpaper is visible on **any** monitor. A freeze captures all of them,
   so one uncovered display silently reintroduces the variable the screen exists to remove.
4. Run the freeze and read the per monitor lines.

The page loads from `file://` and needs no server. It fetches nothing and loads no font.

## Reading the output

```
freeze: froze 4/4 monitor(s) in 224 ms - warm 4/4, slowest monitor: capture 5 ms, encode 218 ms
freeze:   2560x1440 at (0, 0) - capture 5 ms, encode 61 ms, png 41231 bytes, warm
```

The byte length is the run describing its own conditions. PLAIN and DENSE differ by orders of
magnitude, so no reader can mistake one for the other, and a run against an unlisted screen lands
between them and is visibly neither.

**Quote the bar as the PLAIN and DENSE pair, never as one number.** A single figure is a claim about
one desktop that nobody else has.
