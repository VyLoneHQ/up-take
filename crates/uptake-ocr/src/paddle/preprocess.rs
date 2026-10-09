//! Turning an [`RgbaBitmap`] into the tensor the detector expects.
//!
//! `architecture.md` section 3.2 opens the OCR pipeline with *"preprocess
//! (grayscale, threshold, denoise)"*. That description predates the choice of
//! PP-OCRv4 and does not survive contact with it: **the DB detector is trained
//! on three-channel colour input, normalised with ImageNet statistics**, and
//! feeding it a thresholded binary image would be feeding it something no
//! training sample looked like. Greyscale and thresholding live *inside* DB's
//! own learned filters, which is why the model is 4.7 MB rather than a
//! parameterless function.
//!
//! Recorded here rather than silently diverging from the spec: the pipeline's
//! shape is unchanged, but this stage is a resize and a normalise, not a
//! threshold. Roadmap 1.29's sharpening pass is a separate, deliberate
//! pre-filter and is not this.
//!
//! **Nothing in this file touches ONNX Runtime.** It is arithmetic over a
//! bitmap, so every rule below is tested in CI with no model present.

use uptake_core::bitmap::{BYTES_PER_PIXEL, RgbaBitmap};

/// ImageNet channel means, in RGB order, as PP-OCR's detector was trained.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
/// ImageNet channel standard deviations, in RGB order.
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// The detector's side-length quantum.
///
/// DB's backbone downsamples by 32, so a side that is not a multiple of 32
/// produces a probability map whose dimensions do not divide back cleanly. The
/// reference implementation rounds to this; so do we.
pub const SIDE_MULTIPLE: u32 = 32;

/// Default side of one detector tile, in frame pixels (`I-440`).
///
/// **The detector reads every frame at its true size**, and a frame larger than
/// a tile is read in overlapping tiles whose probability maps are joined into
/// one map for the whole frame ([`tiles`]). Cost scales with the frame's pixels
/// whatever the tile, so this is not a latency knob: it bounds the size of ONE
/// tensor, and so the memory one inference needs, which is what keeps a 4K or
/// multi-monitor area inside `quality-bars.md` section 1's *Active RAM* row.
///
/// **Measured 2026-10-09** with `ocr_smoke` on rendered screens of 13 px text,
/// peak working set of the whole process: one 3840 x 1280 band per inference
/// peaked at 1.7 GB on a 4K frame; tiles of 1024 at 467 MB; tiles of 768 at
/// 374 MB, and 352 to 370 MB on 1440p and 7680 x 1080. Time and words read
/// were the same at both tile sizes, within the run-to-run spread.
pub const DEFAULT_TILE_SIDE: u32 = 768;

/// How many pixels neighbouring tiles share, in frame pixels.
///
/// Each map pixel is taken from the tile in which it lies furthest from an
/// edge, so every kept pixel was computed with at least half of this as context
/// on every side the frame has ([`Span`]). The detector's answer near a tile's
/// own edge is the unreliable part, and this keeps it out of the joined map.
pub const TILE_OVERLAP: u32 = 128;

/// A frame resized and normalised for the detector.
///
/// ⚠️ **This deliberately carries NO scale factors, and it used to.** It held a
/// `scale_x`/`scale_y` pair computed as `source / resized`, documented at length
/// as the way boxes were mapped home. **Nothing in production ever read them**:
/// the engine computed its own factors from the *model's output* dimensions,
/// which is the correct source, because the probability map is not required to
/// be the same size as the tensor that produced it. Two rules for one quantity,
/// one of them tested and dead, the other used and untested -- found by the
/// independent review of `PR #76` and removed rather than documented.
///
/// The one rule now lives in [`DetectorInput::scale_to_source`], which the
/// engine calls and these tests cover. It is a method since `I-440`, because it
/// needs the content rectangle this value carries.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorInput {
    /// Normalised pixel data, NCHW with N = 1 and C = 3.
    pub tensor: Vec<f32>,
    /// The tensor's width. A multiple of [`SIDE_MULTIPLE`].
    pub width: u32,
    /// The tensor's height. A multiple of [`SIDE_MULTIPLE`].
    pub height: u32,
    /// How much of the tensor's width, from the left, holds the frame. The
    /// rest is padding ([`fit`]).
    pub content_width: u32,
    /// How much of the tensor's height, from the top, holds the tile.
    pub content_height: u32,
}

impl DetectorInput {
    /// The tensor's shape, as `ort` wants it: `[batch, channels, height, width]`.
    #[must_use]
    pub fn shape(&self) -> [usize; 4] {
        [1, 3, self.height as usize, self.width as usize]
    }

    /// The factors that map a coordinate in the detector's output map back to
    /// the source tile.
    ///
    /// **The map covers the whole tensor and the tile only its content
    /// rectangle** (`I-440`), so a map pixel is `padded / map` tensor pixels and
    /// a tensor pixel is `source / content` tile pixels. Since the detector
    /// reads at true size, `source` and `content` are equal and a factor is
    /// `padded / map`, which is `1.0` for PP-OCRv6_small; the general form is
    /// kept so a model with another stride still lands its boxes. A box the
    /// detector finds in the padding maps past the tile's edge, where the
    /// caller ignores it.
    ///
    /// Takes the **map's** dimensions rather than assuming the tensor's,
    /// because the caller reads them off the model's actual output shape. For
    /// PP-OCRv4's detector the two agree, and so do they for PP-OCRv6_small's (a
    /// 320x480 input gives a 320x480 map, measured 2026-09-15), but nothing
    /// enforces that and a model with a different stride would silently place
    /// every box wrong.
    ///
    /// A zero map dimension yields a factor of `0.0` rather than an infinity, so
    /// a degenerate output collapses boxes to a point the size filter drops
    /// instead of poisoning them with `NaN`.
    #[must_use]
    pub fn scale_to_source(
        &self,
        source_width: u32,
        source_height: u32,
        map_width: usize,
        map_height: usize,
    ) -> (f32, f32) {
        let factor = |source: u32, content: u32, padded: u32, map: usize| -> f32 {
            if map == 0 || content == 0 {
                0.0
            } else {
                (source as f32 * padded as f32) / (map as f32 * content as f32)
            }
        };
        (
            factor(source_width, self.content_width, self.width, map_width),
            factor(source_height, self.content_height, self.height, map_height),
        )
    }
}

/// Where a tile goes in the detector's tensor: its own size, and the tensor
/// around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    /// The tile's width, unchanged.
    pub content_width: u32,
    /// The tile's height, unchanged.
    pub content_height: u32,
    /// The tensor's width: the content's, rounded UP to a multiple of
    /// [`SIDE_MULTIPLE`].
    pub width: u32,
    /// The tensor's height, rounded up the same way.
    pub height: u32,
}

/// Chooses where a tile goes in the detector's tensor: at its true size, at the
/// top left, padded **up** to the next multiple of [`SIDE_MULTIPLE`] on each
/// side and never stretched.
///
/// # Why true size, and what it replaced (`I-440`)
///
/// This scaled a frame's longer side down to 960 px, as PaddleOCR's own
/// preprocessing does, and before that it also stretched each side to the
/// nearest multiple of 32. **Both shrank the text with the frame**, so whether
/// an area read at all depended on its size: the founder's 2385 x 110 editor
/// strip put 13 px text at about 5 px and read 6 words, and a 2560 x 1440 or
/// 3840 x 2160 screen of 13 px text read none. A pixel budget in place of the
/// 960 px side only moves the size at which that happens. **Reading at true
/// size keeps text the size it is on screen for an area of any size**, and the
/// price is that the detector's cost grows with the area's pixels: a small area
/// costs what it did, a large one more. Frames larger than a tile are split by
/// [`tiles`], so one tensor stays bounded.
#[must_use]
pub fn fit(width: u32, height: u32) -> Fit {
    let (content_width, content_height) = (width.max(1), height.max(1));
    Fit {
        content_width,
        content_height,
        width: round_up_to_multiple(content_width),
        height: round_up_to_multiple(content_height),
    }
}

/// Rounds a side up to the next multiple of [`SIDE_MULTIPLE`], never zero.
const fn round_up_to_multiple(side: u32) -> u32 {
    let multiples = side.div_ceil(SIDE_MULTIPLE);
    let multiples = if multiples == 0 { 1 } else { multiples };
    multiples.saturating_mul(SIDE_MULTIPLE)
}

/// One stretch of a frame along one axis, read by the detector as part of a
/// tile (`I-440`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// The first pixel of the stretch.
    pub start: u32,
    /// How many pixels it holds.
    pub len: u32,
    /// The first pixel this stretch answers for in the joined map: pixels
    /// `keep_from..keep_to` are taken from this stretch and from no other.
    pub keep_from: u32,
    /// One past the last pixel this stretch answers for.
    pub keep_to: u32,
}

/// Splits `total` pixels into overlapping stretches no longer than `size`.
///
/// A frame that fits is one stretch. Otherwise it is the fewest stretches of at
/// most `size` that cover it while each neighbour shares at least `overlap`
/// pixels, all of one length and spread evenly from the first pixel to the
/// last. **Even, not greedy**: a greedy split of 1080 rows into 1024 leaves a
/// second stretch of rows 56..1080 that re-reads almost the whole first one,
/// where two stretches of 604 share only the overlap.
///
/// **The keep ranges tile the frame with no gap and no overlap**, and the
/// boundary between two of them is the middle of the pixels their stretches
/// share, so a kept pixel is at least half the overlap from its stretch's edge
/// wherever the frame continues past that edge.
///
/// `overlap` is clamped below `size`, so the stretches always advance.
#[must_use]
pub fn spans(total: u32, size: u32, overlap: u32) -> Vec<Span> {
    let size = size.max(SIDE_MULTIPLE);
    if total <= size {
        return vec![Span {
            start: 0,
            len: total,
            keep_from: 0,
            keep_to: total,
        }];
    }
    let overlap = overlap.min(size - SIDE_MULTIPLE);
    // The fewest stretches of `size` that cover `total` with `overlap` shared.
    let count = (total - overlap).div_ceil(size - overlap);
    let gaps = count - 1;
    // The shortest length at which `count` evenly spread stretches still share
    // `overlap`; `size` always qualifies, by the choice of `count`.
    let mut len = (total + gaps * overlap).div_ceil(count).min(size);
    while len < size && len - (total - len).div_ceil(gaps) < overlap {
        len += 1;
    }
    let starts: Vec<u32> = (0..count)
        .map(|index| {
            let start = u64::from(index) * u64::from(total - len) / u64::from(gaps);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "at most total - len, which is a u32"
            )]
            let start = start as u32;
            start
        })
        .collect();

    let mut result: Vec<Span> = Vec::with_capacity(starts.len());
    for (index, &start) in starts.iter().enumerate() {
        let keep_from = result.last().map_or(0, |previous| previous.keep_to);
        let keep_to = match starts.get(index + 1) {
            // The middle of the pixels this stretch shares with the next one.
            Some(&next) => (next + start + len) / 2,
            None => total,
        };
        result.push(Span {
            start,
            len,
            keep_from,
            keep_to,
        });
    }
    result
}

/// One rectangle of a frame, read by the detector on its own (`I-440`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// Its columns.
    pub columns: Span,
    /// Its rows.
    pub rows: Span,
}

/// Splits a frame into overlapping tiles no larger than `side` on either side.
///
/// The tiles are every pairing of [`spans`] along the two axes, row by row, so
/// their keep rectangles tile the frame and each map pixel is taken from
/// exactly one tile. A frame within `side` both ways is one tile.
#[must_use]
pub fn tiles(width: u32, height: u32, side: u32, overlap: u32) -> Vec<Tile> {
    let columns = spans(width, side, overlap);
    spans(height, side, overlap)
        .into_iter()
        .flat_map(|rows| columns.iter().map(move |&columns| Tile { columns, rows }))
        .collect()
}

/// Normalises one tile of a frame into the detector's input tensor, at true
/// size.
///
/// Returns `None` for an empty bitmap or an empty tile -- there is no tensor
/// for zero pixels, and a caller that got one has a bug upstream rather than an
/// empty recognition. A tile reaching past the frame is cut to it.
///
/// # Alpha is ignored, deliberately
///
/// The detector wants RGB. A captured frame is opaque, and where it is not, the
/// honest choice is to read the colour channels as they stand rather than
/// composite against a background nobody chose: compositing onto white would
/// invent contrast the model then reports as text.
#[must_use]
pub fn detector_input(bitmap: &RgbaBitmap, tile: Tile) -> Option<DetectorInput> {
    let tile_width = tile
        .columns
        .len
        .min(bitmap.width().saturating_sub(tile.columns.start));
    let tile_height = tile
        .rows
        .len
        .min(bitmap.height().saturating_sub(tile.rows.start));
    if tile_width == 0 || tile_height == 0 {
        return None;
    }
    let Fit {
        content_width,
        content_height,
        width,
        height,
    } = fit(tile_width, tile_height);

    let plane = width as usize * height as usize;
    // The padding is left at 0.0, which after normalisation is the ImageNet
    // mean colour: the value that carries the least signal into a network
    // trained on mean-subtracted input (`I-440`).
    let mut tensor = vec![0.0_f32; plane * 3];
    let pixels = bitmap.pixels();
    let row_bytes = bitmap.width() as usize * BYTES_PER_PIXEL;
    let left_byte = tile.columns.start as usize * BYTES_PER_PIXEL;
    let content_bytes = content_width as usize * BYTES_PER_PIXEL;
    for y in 0..content_height as usize {
        let from = (tile.rows.start as usize + y) * row_bytes + left_byte;
        let Some(row) = pixels.get(from..from + content_bytes) else {
            break;
        };
        for (x, pixel) in row.chunks_exact(BYTES_PER_PIXEL).enumerate() {
            let destination = y * width as usize + x;
            for channel in 0..3 {
                let raw = f32::from(pixel[channel]) / 255.0;
                tensor[channel * plane + destination] = (raw - MEAN[channel]) / STD[channel];
            }
        }
    }

    Some(DetectorInput {
        tensor,
        width,
        height,
        content_width,
        content_height,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use uptake_core::geometry::Size;

    /// A bitmap filled with one RGBA colour.
    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> RgbaBitmap {
        let pixels = rgba
            .iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * BYTES_PER_PIXEL)
            .collect();
        RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap()
    }

    /// A fit of `content` inside a tensor of `padded`, both `(width, height)`.
    fn fitted(content: (u32, u32), padded: (u32, u32)) -> Fit {
        Fit {
            content_width: content.0,
            content_height: content.1,
            width: padded.0,
            height: padded.1,
        }
    }

    /// The detector input for a frame's first tile, as the engine builds it.
    fn read(bitmap: &RgbaBitmap) -> Option<DetectorInput> {
        let first = tiles(
            bitmap.width(),
            bitmap.height(),
            DEFAULT_TILE_SIDE,
            TILE_OVERLAP,
        )[0];
        detector_input(bitmap, first)
    }

    #[test]
    fn a_small_frame_keeps_its_size_and_is_padded_to_one_multiple() {
        assert_eq!(fit(10, 4), fitted((10, 4), (32, 32)));
    }

    #[test]
    fn sides_keep_their_size_and_are_padded_up() {
        // 100 x 40 is not stretched to 96 x 32 any more (I-440): it stays
        // 100 x 40 inside a 128 x 64 tensor.
        assert_eq!(fit(100, 40), fitted((100, 40), (128, 64)));
        // A side already on the multiple gains no padding.
        assert_eq!(fit(64, 96), fitted((64, 96), (64, 96)));
    }

    #[test]
    fn a_large_frame_is_not_scaled_down() {
        // I-440: 1920 x 1080 was scaled to 960 x 540, halving its text.
        assert_eq!(fit(1920, 1080), fitted((1920, 1080), (1920, 1088)));
        // The founder's 2385 x 110 strip was scaled to 960 x 44, which put 13
        // px text at about 5 px; it now keeps its text height.
        assert_eq!(fit(2385, 110), fitted((2385, 110), (2400, 128)));
    }

    #[test]
    fn the_tensor_holds_the_content_with_less_than_one_multiple_of_padding() {
        for (width, height) in [
            (1, 1),
            (33, 65),
            (1920, 1080),
            (3840, 1280),
            (7, 4000),
            (2385, 110),
            (4000, 7),
        ] {
            let fit = fit(width, height);
            assert_eq!((fit.content_width, fit.content_height), (width, height));
            assert_eq!(fit.width % SIDE_MULTIPLE, 0, "{width}x{height}");
            assert_eq!(fit.height % SIDE_MULTIPLE, 0, "{width}x{height}");
            assert!(fit.width - fit.content_width < SIDE_MULTIPLE);
            assert!(fit.height - fit.content_height < SIDE_MULTIPLE);
        }
    }

    #[test]
    fn a_frame_that_fits_is_one_stretch_that_keeps_every_pixel() {
        for total in [1, 110, 700, DEFAULT_TILE_SIDE] {
            assert_eq!(
                spans(total, DEFAULT_TILE_SIDE, TILE_OVERLAP),
                vec![Span {
                    start: 0,
                    len: total,
                    keep_from: 0,
                    keep_to: total,
                }],
                "total {total}"
            );
        }
    }

    #[test]
    fn a_long_axis_is_stretches_that_cover_it_and_overlap() {
        // 1440 rows in 1024: two stretches of 784, sharing rows 656..784 and
        // no more, with the keep boundary in the middle of them.
        let split = spans(1440, 1024, 128);
        assert_eq!(split.len(), 2);
        assert_eq!((split[0].start, split[1].start), (0, 656));
        assert_eq!((split[0].len, split[1].len), (784, 784));
        assert_eq!(split[0].keep_to, 720);
        assert_eq!(split[1].keep_from, 720);
        // 1080 rows: a greedy split re-read rows 56..1024; even, they are two
        // stretches of 604 sharing 128.
        let split = spans(1080, 1024, 128);
        assert_eq!(split.len(), 2);
        assert_eq!((split[0].len, split[1].start), (604, 476));

        for total in [1025, 1440, 2160, 3840, 4321, 7680] {
            for (size, overlap) in [(1024, 128), (64, 32), (500, 499), (300, 1000)] {
                let split = spans(total, size, overlap);
                let label = format!("{total} in {size}/{overlap}");
                assert_eq!(split[0].start, 0, "{label}");
                assert_eq!(split[0].keep_from, 0, "{label}");
                let last = split.last().unwrap();
                assert_eq!(last.start + last.len, total, "{label}");
                assert_eq!(last.keep_to, total, "{label}");
                for span in &split {
                    assert!(span.len <= size.max(SIDE_MULTIPLE), "{label}");
                }
                for pair in split.windows(2) {
                    // The stretches advance, share at least the overlap, and
                    // the keep ranges tile the axis.
                    assert!(pair[1].start > pair[0].start, "{label}");
                    let shared = pair[0].start + pair[0].len - pair[1].start;
                    assert!(shared >= overlap.min(size - SIDE_MULTIPLE), "{label}");
                    assert_eq!(pair[0].keep_to, pair[1].keep_from, "{label}");
                }
            }
        }
    }

    #[test]
    fn every_pixel_is_kept_once_with_half_the_overlap_as_context() {
        // The joined map's promise: each pixel comes from exactly one stretch,
        // and that stretch reaches at least half the overlap past it on each
        // side where the frame does.
        for (total, size, overlap) in [(3840, 1024, 128), (2160, 1024, 128), (700, 128, 64)] {
            let split = spans(total, size, overlap);
            for at in 0..total {
                let keepers: Vec<&Span> = split
                    .iter()
                    .filter(|span| span.keep_from <= at && at < span.keep_to)
                    .collect();
                assert_eq!(keepers.len(), 1, "pixel {at} of {total}");
                let span = keepers[0];
                let margin = overlap / 2;
                assert!(at - span.start >= margin.min(at), "pixel {at}: {span:?}");
                let past = span.start + span.len - 1 - at;
                assert!(past >= margin.min(total - 1 - at), "pixel {at}: {span:?}");
            }
        }
    }

    #[test]
    fn tiles_pair_every_row_stretch_with_every_column_stretch() {
        let grid = tiles(3840, 2160, 1024, 128);
        let columns = spans(3840, 1024, 128);
        let rows = spans(2160, 1024, 128);
        assert_eq!(grid.len(), columns.len() * rows.len());
        assert_eq!(
            grid[0],
            Tile {
                columns: columns[0],
                rows: rows[0]
            }
        );
        assert_eq!(
            grid[1],
            Tile {
                columns: columns[1],
                rows: rows[0]
            }
        );
        assert_eq!(tiles(800, 600, 1024, 128).len(), 1);
    }

    #[test]
    fn a_tile_reads_its_own_rows_and_columns_of_the_frame() {
        // Red is the column, green the row, so the tensor's corner names the
        // tile's corner.
        let (width, height) = (2000_u32, 1500_u32);
        let pixels = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [(x % 251) as u8, (y % 241) as u8, 0, 255]))
            .collect();
        let frame = RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap();
        // The last tile, so neither its rows nor its columns start at 0.
        let tile = *tiles(width, height, 1024, 128).last().unwrap();
        assert!(tile.columns.start > 0 && tile.rows.start > 0);
        let input = detector_input(&frame, tile).unwrap();
        assert_eq!(
            (input.content_width, input.content_height),
            (tile.columns.len, tile.rows.len)
        );
        let plane = input.width as usize * input.height as usize;
        let at = |channel: usize, x: u32, y: u32| {
            input.tensor[channel * plane + y as usize * input.width as usize + x as usize]
        };
        let (last_x, last_y) = (tile.columns.len - 1, tile.rows.len - 1);
        for (x, y) in [(0_u32, 0_u32), (last_x, 0), (0, last_y), (517, 300)] {
            let red =
                (f32::from(((tile.columns.start + x) % 251) as u8) / 255.0 - MEAN[0]) / STD[0];
            let green = (f32::from(((tile.rows.start + y) % 241) as u8) / 255.0 - MEAN[1]) / STD[1];
            assert!((at(0, x, y) - red).abs() < 1e-4, "red at ({x}, {y})");
            assert!((at(1, x, y) - green).abs() < 1e-4, "green at ({x}, {y})");
        }
    }

    #[test]
    fn the_frame_is_sampled_unstretched_and_the_padding_is_the_mean() {
        // A frame lands pixel for pixel: column x of the tensor is column x of
        // the frame. Before I-440 a 100 px wide frame was stretched onto 96
        // columns, so this would read a different column.
        let (width, height) = (100_u32, 40_u32);
        let pixels = (0..height)
            .flat_map(|_| (0..width).flat_map(|x| [x as u8 * 2, 0, 0, 255]))
            .collect();
        let frame = RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap();
        let input = read(&frame).unwrap();
        assert_eq!((input.width, input.height), (128, 64));
        assert_eq!((input.content_width, input.content_height), (100, 40));
        let red = |x: usize, y: usize| input.tensor[y * input.width as usize + x];
        for x in [0_usize, 1, 50, 99] {
            let expected = (f32::from(x as u8 * 2) / 255.0 - MEAN[0]) / STD[0];
            assert!((red(x, 0) - expected).abs() < 1e-4, "column {x}");
            assert!((red(x, 39) - expected).abs() < 1e-4, "column {x}, last row");
        }
        // Right of the content and below it: the mean, in every channel.
        let plane = input.width as usize * input.height as usize;
        for (x, y) in [(100_usize, 0_usize), (127, 39), (0, 40), (127, 63)] {
            for channel in 0..3 {
                let value = input.tensor[channel * plane + y * input.width as usize + x];
                assert!(value.abs() < f32::EPSILON, "({x}, {y}) channel {channel}");
            }
        }
    }

    #[test]
    fn scale_to_source_maps_the_content_edge_to_the_frame_edge() {
        // A strip like I-440's, within one tile, through a map the size of the
        // tensor: one map pixel is one frame pixel, and the padding maps past
        // the frame, where the caller ignores it.
        let input = read(&solid(700, 110, [0, 0, 0, 255])).unwrap();
        assert_eq!((input.width, input.height), (704, 128));
        let (scale_x, scale_y) = input.scale_to_source(700, 110, 704, 128);
        assert!((scale_x - 1.0).abs() < 1e-6, "x {scale_x}");
        assert!((scale_y - 1.0).abs() < 1e-6, "y {scale_y}");
        assert!(128.0 * scale_y > 110.0, "the padding maps past the frame");
    }

    #[test]
    fn scale_to_source_maps_a_map_coordinate_home() {
        // An 80 x 60 frame is 80 x 60 content in a 96 x 64 tensor; a 48 x 32
        // map is half the tensor, so a map pixel is two tensor pixels.
        let input = read(&solid(80, 60, [0, 0, 0, 255])).unwrap();
        let (scale_x, scale_y) = input.scale_to_source(80, 60, 48, 32);
        assert!((scale_x - 2.0).abs() < 1e-6, "scale_x was {scale_x}");
        assert!((scale_y - 2.0).abs() < 1e-6, "scale_y was {scale_y}");
    }

    #[test]
    fn scale_to_source_survives_a_degenerate_map_without_infinities() {
        // A model that returned a zero dimension must not poison every box with
        // an infinity or a NaN -- the size filter's comparisons would then be
        // silently false rather than rejecting.
        let input = read(&solid(80, 60, [0, 0, 0, 255])).unwrap();
        let (scale_x, scale_y) = input.scale_to_source(80, 60, 0, 0);
        assert!(scale_x.is_finite() && scale_y.is_finite());
        assert!((scale_x - 0.0).abs() < f32::EPSILON);
        assert!((scale_y - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scale_to_source_reads_the_map_and_not_the_resize() {
        // The regression this function exists to prevent: if the model emits a
        // map at HALF the input resolution, the factors must double. Computing
        // from the tensor instead would return 1.0 and place every box at half
        // its true distance from the origin.
        let input = read(&solid(960, 960, [0, 0, 0, 255])).unwrap();
        let (from_half_map, _) = input.scale_to_source(960, 960, 480, 480);
        let (from_full_map, _) = input.scale_to_source(960, 960, 960, 960);
        assert!((from_half_map - 2.0).abs() < 1e-6, "was {from_half_map}");
        assert!((from_full_map - 1.0).abs() < 1e-6, "was {from_full_map}");
    }

    #[test]
    fn the_tensor_is_nchw_and_the_right_length() {
        let input = read(&solid(64, 64, [0, 0, 0, 255])).unwrap();
        assert_eq!(input.shape(), [1, 3, 64, 64]);
        assert_eq!(input.tensor.len(), 3 * 64 * 64);
    }

    #[test]
    fn normalisation_maps_black_and_white_to_the_imagenet_extremes() {
        // Black: (0 - mean) / std. White: (1 - mean) / std. Checked per channel,
        // because a channel-order slip (RGB vs BGR) is invisible on grey input
        // and this is the cheapest place to catch it.
        let black = read(&solid(32, 32, [0, 0, 0, 255])).unwrap();
        let white = read(&solid(32, 32, [255, 255, 255, 255])).unwrap();
        let plane = 32 * 32;
        for channel in 0..3 {
            let expected_black = (0.0 - MEAN[channel]) / STD[channel];
            let expected_white = (1.0 - MEAN[channel]) / STD[channel];
            assert!(
                (black.tensor[channel * plane] - expected_black).abs() < 1e-4,
                "channel {channel} black"
            );
            assert!(
                (white.tensor[channel * plane] - expected_white).abs() < 1e-4,
                "channel {channel} white"
            );
        }
    }

    #[test]
    fn the_channels_are_rgb_and_not_bgr() {
        // A pure-red frame: after normalisation channel 0 must be the bright one.
        let input = read(&solid(32, 32, [255, 0, 0, 255])).unwrap();
        let plane = 32 * 32;
        let red = input.tensor[0];
        let green = input.tensor[plane];
        let blue = input.tensor[2 * plane];
        assert!(red > green, "red {red} should exceed green {green}");
        assert!(red > blue, "red {red} should exceed blue {blue}");
    }

    #[test]
    fn an_empty_frame_yields_no_tensor_rather_than_an_empty_one() {
        let empty = RgbaBitmap::from_pixels(Size::new(0, 0), Vec::new());
        if let Some(bitmap) = empty {
            assert!(read(&bitmap).is_none());
        }
    }
}
