# The defined test screen

`quality-bars.md` section 1's *frozen view painted* row is content dependent. Encode and decode
both scale with image complexity -- in every compressed format this path can be set to -- so a
timing taken against whatever happened to be on the desktop is a number whose precondition nobody
stated. That has cost this project two findings already, eight days apart and in the same task.

This page is the fix. It puts the rig into a known state, and the freeze path reports the encoded
byte length beside every timing so a mislabelled run is detectable rather than trusted. **In
whatever format the display path is set to** — since ADR-0027 that is JPEG by default, not PNG, and
the `freeze: display stills encode as …` line printed at startup is what tells you which.

## The three screens

| Screen | Query | What it is | What it bounds |
| --- | --- | --- | --- |
| PLAIN | `?screen=plain` | Flat mid grey (`#808080`) edge to edge | The floor. The pipeline with the content removed |
| DENSE | `?screen=dense` | Deterministic RGB noise from a fixed seed | The ceiling. The encoder's worst case |
| BLOCKS | `?screen=blocks` | 8x8 blocks, one random colour each | The control, see below |

**BLOCKS is not a third data point.** It exists to falsify the self description. If an unlisted
screen could not be told from a listed one by its reported byte length, the byte length would be
worth nothing. Run it and confirm it lands visibly between the other two.

Measured through the real encoder at 640x400, **and the three formats do not behave the same**:

| Format | PLAIN | BLOCKS | DENSE | Floor to ceiling | Control's margins |
| --- | --- | --- | --- | --- | --- |
| **JPEG** (the default, ADR-0027) | 4,625 | 49,396 | 230,058 | **49.7x** | 10.7x and 4.7x |
| PNG | 1,987 | 23,065 | 895,515 | 451x | 11.6x and 39x |
| **BMP** | 1,024,054 | 1,024,054 | 1,024,054 | **1.000** | none, and that is not a rounding |

**Read the JPEG row, because that is what a default rig run produces.** All three formats are
asserted in the suite, in `src-tauri/src/output.rs`:
`the_defined_test_screens_separate_by_an_order_of_magnitude` and
`an_unlisted_screen_lands_visibly_between_the_two` pin PNG at 10x each side, and
`the_bracket_still_separates_in_the_format_that_ships` pins the *display* format at 10x endpoints
and 3x control. So the bracket cannot quietly stop separating.

⛔ **BMP is the exception and it is total.** BMP is uncompressed, so all three screens encode to
identical lengths and the byte column tells you nothing whatever about what was on screen. **A run
under `UPTAKE_FREEZE_FORMAT=bmp` cannot be used to quote a 1.9g figure**, because the whole
self-description this page exists to provide is absent there, not merely weaker.

The control was smooth vertical bands first, and that was wrong: in PNG it encoded to 2,011 bytes
against PLAIN's 1,987, a margin of 1.2 % that no reader of a rig log could tell apart. PNG filters
rows before it deflates them, so hard edged vertical bands are nearly as compressible as a flat
field. Measured in JPEG the same bands land 2.46x above PLAIN and fail the 3x bar, which is the
check doing its job on the exact screen that defeated the first one.

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
freeze:   2560x1440 at (0, 0) - capture 5 ms, encode 61 ms, 41231 bytes, warm
```

**That second line is the real format string and it was wrong here until 2026-08-04.** It used to
read `png 41231 bytes` — a field the code has never emitted, naming a format that is no longer the
default. An operator comparing their log against this page would have gone looking for the
difference. Caught in the independent review of PR #41; the emitting site is `overlay.rs`, and the
format is stated once there.

The byte length is the run describing its own conditions. PLAIN and DENSE differ by an order of
magnitude or more in both compressed formats, so no reader can mistake one for the other, and a run
against an unlisted screen lands between them and is visibly neither. **How far between depends on
the format** -- the control clears both ends by 10x in PNG and by 3x in JPEG, because JPEG's whole
span is 49.7x and 10 x 10 exceeds it. `quality-bars.md` section 1 footnote 3 carries the reasoning.

**Quote the bar as the PLAIN and DENSE pair, never as one number.** A single figure is a claim about
one desktop that nobody else has.
