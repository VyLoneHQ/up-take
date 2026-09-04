//! Turning DB's probability map into text boxes.
//!
//! PP-OCRv4's detector is a Differentiable Binarization network. It does not
//! emit boxes: it emits one `float` per pixel, the probability that the pixel is
//! inside a text region, on a map the same size as the resized input. Everything
//! that turns that into quads is here, and **none of it needs ONNX Runtime** --
//! the input is a slice of floats, so every rule below is tested against maps
//! this file's own tests draw by hand.
//!
//! The stages, in order, each with UP-TAKE's default:
//!
//! 1. **Binarize** at `threshold` (0.2).
//! 2. **Connect** the surviving pixels into regions (4-connectivity).
//! 3. **Fit** a minimum-area rectangle to each region.
//! 4. **Score** each region by the mean probability under its box, and drop it
//!    below `box_threshold` (0.4).
//! 5. **Unclip** by `unclip_ratio` (1.5), because DB is trained on shrunk
//!    regions.
//! 6. **Drop** boxes thinner than `min_size` (3 px), which are noise.
//! 7. **Scale** back to source-frame coordinates.
//!
//! ⚠️ **Stages 1 and 4 no longer use PP-OCR's numbers.** They said 0.3 and 0.6,
//! which is upstream's configuration for the detector this crate ships, until
//! 2026-09-04. UP-TAKE reads screens rather than photographed documents and the
//! upstream values discard legible screen text; [`DetectorOptions`]'s own header
//! carries the measurement and the reasoning. The other four are unchanged and
//! unmeasured.

use super::quad::{PointF, Quad, min_area_rect};

/// The knobs DB's post-processing exposes.
///
/// Named constants rather than magic numbers at the call site: every one of
/// these is a value somebody chose, and a session that changes one should have
/// to say which and why.
///
/// # Two of these are NO LONGER PP-OCR's defaults, and that is deliberate
///
/// `threshold` and `box_threshold` were `0.3` and `0.6`, which is exactly what
/// Baidu ships in `inference.yml` for `PP-OCRv4_mobile_det` **and** for
/// `PP-OCRv5_mobile_det`. Nothing was mis-transcribed: the implementation was
/// faithful and the values were upstream's.
///
/// **Upstream tunes them for photographed documents. UP-TAKE reads screens**,
/// and on screen text `0.6` throws away text that is plainly there. Measured on
/// 2026-09-04 against 192 rendered ground-truth cards (`1.32`'s harness), with
/// nothing else changed:
///
/// | `threshold` / `box_threshold` | CER | exact | empty |
/// | --- | --- | --- | --- |
/// | 0.3 / 0.6 (upstream) | 0.330 | 54.2 % | 25.5 % |
/// | 0.3 / 0.4 | 0.140 | 67.2 % | 8.9 % |
/// | **0.2 / 0.4 (here)** | **0.140** | **66.7 %** | **7.8 %** |
/// | 0.2 / 0.3 | 0.133 | 67.2 % | 7.3 % |
///
/// `box_threshold` is the load-bearing one. On a width sweep holding the text
/// pixel-identical and varying only the surrounding canvas, cards read out of
/// 12, at the shipping `limit_side_len`:
///
/// | | `box` 0.6 | `box` 0.4 |
/// | --- | --- | --- |
/// | `threshold` 0.3 | 4 | **12** |
/// | `threshold` 0.2 | 6 | **12** |
///
/// `0.4` reads all twelve at either `threshold`; `0.6` reads neither column
/// fully. Reproduce with `render-ocr-cards.py --width-sweep`.
///
/// *(This said "`0.6` read 4 of 12 and `0.4` read 12 of 12, at either
/// `threshold`". The second half was right and the first was not: 4 is the
/// figure at `threshold` 0.3 and 6 at 0.2. Caught by running the sweep through
/// the shipped tool while committing it, which is the whole reason the
/// independent review of `PR #87` asked for it to be shipped.)*
///
/// **The feared trade did not happen** *on this fixture*. Loosening a filter
/// should buy fewer silent empties at the cost of more confident nonsense;
/// instead the exact-match rate went UP by thirteen points. Read the next
/// section before concluding the trade does not exist.
///
/// # Why 0.4, when the fixture says lower is always better
///
/// Sweeping `box_threshold` down from 0.5 with `threshold` at 0.2:
///
/// | `box_threshold` | CER | empty |
/// | --- | --- | --- |
/// | 0.50 | 0.197 | 13.5 % |
/// | 0.45 | 0.158 | 9.9 % |
/// | 0.40 | 0.140 | 7.8 % |
/// | 0.30 | 0.133 | 7.3 % |
/// | 0.25 and below, to 0.05 | 0.132 | 6.8 % |
///
/// **It never turns.** The curve improves monotonically and then plateaus, so
/// the fixture's own optimum is "as low as you like" -- which is not a result,
/// it is the shape of a test set that cannot measure the cost.
///
/// The cards are clean text on plain backgrounds with **nothing to falsely
/// detect**. A real screen has window borders, icons, table rules and UI chrome,
/// and a low box threshold picks those up as text. That failure cannot appear
/// here, so this fixture can only ever show the benefit of loosening and never
/// the price. `BACKLOG.md` `I-367` is the row for giving it distractors.
///
/// So the value is NOT taken from the plateau. `0.4` is the loosest value
/// **Baidu itself ships** across the PP-OCRv6 tiers -- `tiny` uses 0.4, `small`
/// and `medium` use 0.45 -- which is the only evidence available about where the
/// real cost begins, and it is external to our own test set.
///
/// *(An earlier revision of this comment said `0.2 / 0.4` simply "is PP-OCRv6's
/// own published configuration". The independent review of `PR #87` checked all
/// three v6 configs and found only the smallest tier uses 0.4; the two larger
/// ones use 0.45, a value that was not in the table at all. It has been measured
/// since -- 0.45 is worse than 0.4 here -- but the point stands that the
/// justification was stated more precisely than the evidence supported.)*
///
/// # What was NOT changed, and why that matters
///
/// `unclip_ratio` stays `1.5` although v6 uses `1.4`, and `min_size` stays `3.0`.
/// **Neither was measured.** Changing an unmeasured constant in the same commit
/// as a measured one is how a result stops being attributable.
///
/// Found by the founder at the rig, 2026-09-04: an OCR area wider than roughly
/// 700 px stopped reading. `BACKLOG.md` `I-363`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectorOptions {
    /// Probability above which a pixel counts as text. PP-OCR's `det_db_thresh`,
    /// lowered from upstream's `0.3` for screen text -- see the type's header.
    pub threshold: f32,
    /// Mean-probability floor for a whole box. PP-OCR's `det_db_box_thresh`,
    /// lowered from upstream's `0.6` for screen text. **This is the one that
    /// mattered** -- see the type's header for the measurement.
    pub box_threshold: f32,
    /// How far to grow each box. PP-OCR's `det_db_unclip_ratio`.
    pub unclip_ratio: f32,
    /// Shortest side, in resized pixels, a box may have and survive.
    pub min_size: f32,
}

impl Default for DetectorOptions {
    fn default() -> Self {
        Self {
            threshold: 0.2,
            box_threshold: 0.4,
            unclip_ratio: 1.5,
            min_size: 3.0,
        }
    }
}

/// One detected text region, in source-frame coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectedBox {
    /// Where the text sits, clockwise from top-left.
    pub quad: Quad,
    /// Mean probability under the box, before unclipping. Higher is more certain.
    pub score: f32,
}

/// A probability map: one `f32` per pixel, row-major.
///
/// Borrowed rather than owned because it comes straight out of an `ort` tensor
/// and copying a 960x544 map per frame is 2 MB of pointless memcpy on the
/// latency path.
#[derive(Debug, Clone, Copy)]
pub struct ProbabilityMap<'a> {
    /// The probabilities, row-major, `width * height` of them.
    pub data: &'a [f32],
    /// Map width in pixels.
    pub width: usize,
    /// Map height in pixels.
    pub height: usize,
}

impl ProbabilityMap<'_> {
    /// The probability at `(x, y)`, or `0.0` outside the map.
    #[must_use]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        if x >= self.width || y >= self.height {
            return 0.0;
        }
        self.data.get(y * self.width + x).copied().unwrap_or(0.0)
    }

    /// Whether the map's declared dimensions match the data it carries.
    ///
    /// Checked rather than trusted: the dimensions come from the model's output
    /// shape and the data from its buffer, and a mismatch means every index
    /// below reads the wrong row. Failing loudly here beats returning boxes that
    /// are subtly, unfalsifiably wrong.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.data.len() == self.width * self.height
    }
}

/// Extracts text boxes from a probability map.
///
/// `scale_x` and `scale_y` map a coordinate in **this map** to the source frame,
/// and the caller gets them from [`super::preprocess::scale_to_source`] using the
/// model's own output dimensions. Returns boxes in **source-frame** coordinates,
/// clamped to `(source_width, source_height)`.
///
/// *(This said the factors "come from `DetectorInput`" until 2026-08-30. That was
/// false of the only caller, which had always computed them from the output map
/// -- and `DetectorInput`'s fields were dead. Found by the independent review of
/// `PR #76`.)*
///
/// Returns an empty vector for an inconsistent map rather than indexing past the
/// buffer -- see [`ProbabilityMap::is_consistent`].
#[must_use]
pub fn boxes_from_map(
    map: &ProbabilityMap<'_>,
    options: DetectorOptions,
    scale_x: f32,
    scale_y: f32,
    source_width: f32,
    source_height: f32,
) -> Vec<DetectedBox> {
    if !map.is_consistent() || map.width == 0 || map.height == 0 {
        return Vec::new();
    }

    let mut visited = vec![false; map.data.len()];
    let mut boxes = Vec::new();

    for start_y in 0..map.height {
        for start_x in 0..map.width {
            let index = start_y * map.width + start_x;
            if visited[index] || map.at(start_x, start_y) < options.threshold {
                continue;
            }
            let region = flood_fill(map, options.threshold, &mut visited, start_x, start_y);
            if region.len() < 3 {
                // Fewer than three pixels cannot define a rectangle, and a
                // one-pixel speck is noise by construction.
                continue;
            }
            if let Some(detected) = box_for_region(
                map,
                &region,
                options,
                scale_x,
                scale_y,
                source_width,
                source_height,
            ) {
                boxes.push(detected);
            }
        }
    }
    boxes
}

/// Collects one connected region of above-threshold pixels, marking it visited.
///
/// **4-connectivity, matching the reference implementation.** 8-connectivity
/// would join two lines of text that touch only at a diagonal pixel, which on
/// tightly-leaded text is one merged box where there should be two -- and a
/// merged box recognises as one run of text with the second line's words
/// interleaved.
///
/// Iterative rather than recursive: a region can be the whole map, and a
/// recursive fill over 500k pixels is a stack overflow, which in an always-on
/// tray app is `architecture.md` section 5's "a panic is a lost session".
fn flood_fill(
    map: &ProbabilityMap<'_>,
    threshold: f32,
    visited: &mut [bool],
    start_x: usize,
    start_y: usize,
) -> Vec<(usize, usize)> {
    let mut region = Vec::new();
    let mut stack = vec![(start_x, start_y)];
    visited[start_y * map.width + start_x] = true;

    while let Some((x, y)) = stack.pop() {
        region.push((x, y));
        let neighbours = [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ];
        for (nx, ny) in neighbours {
            if nx >= map.width || ny >= map.height {
                continue;
            }
            let index = ny * map.width + nx;
            if visited[index] || map.at(nx, ny) < threshold {
                continue;
            }
            visited[index] = true;
            stack.push((nx, ny));
        }
    }
    region
}

/// Fits, scores, filters, unclips and rehomes one region.
fn box_for_region(
    map: &ProbabilityMap<'_>,
    region: &[(usize, usize)],
    options: DetectorOptions,
    scale_x: f32,
    scale_y: f32,
    source_width: f32,
    source_height: f32,
) -> Option<DetectedBox> {
    // The region's pixels as corner-inclusive points: a pixel at (x, y) covers
    // the unit square from (x, y) to (x+1, y+1), and using only its top-left
    // corner would shrink every box by a pixel in each direction.
    let mut points = Vec::with_capacity(region.len() * 2);
    for &(x, y) in region {
        points.push(PointF::new(x as f32, y as f32));
        points.push(PointF::new(x as f32 + 1.0, y as f32 + 1.0));
    }

    let fitted = min_area_rect(&points)?;
    let (_, short_side) = fitted.side_lengths();
    if short_side < options.min_size {
        return None;
    }

    let score = mean_probability(map, &fitted);
    if score < options.box_threshold {
        return None;
    }

    let grown = fitted.unclip(options.unclip_ratio);
    let (_, grown_short) = grown.side_lengths();
    if grown_short < options.min_size {
        return None;
    }

    let home = grown
        .scaled(scale_x, scale_y)
        .clamped(source_width, source_height);
    Some(DetectedBox { quad: home, score })
}

/// The mean probability under a box, over its axis-aligned bounding rectangle.
///
/// **This is the reference implementation's `box_score_fast`, and the
/// approximation is deliberate rather than a shortcut.** Scoring the exact
/// rotated quad means rasterising it; scoring its bounding box costs a nested
/// loop and, for the near-axis-aligned boxes screen text produces, differs
/// negligibly. PP-OCR ships `fast` as its default for the same reason.
///
/// A box whose bounding rectangle falls entirely outside the map scores `0.0`,
/// which the caller's threshold then rejects.
fn mean_probability(map: &ProbabilityMap<'_>, quad: &Quad) -> f32 {
    let (min_x, min_y, max_x, max_y) = quad.bounds();
    let x0 = min_x.floor().max(0.0) as usize;
    let y0 = min_y.floor().max(0.0) as usize;
    let x1 = (max_x.ceil() as usize).min(map.width);
    let y1 = (max_y.ceil() as usize).min(map.height);

    if x0 >= x1 || y0 >= y1 {
        return 0.0;
    }
    let mut total = 0.0;
    let mut count = 0_u32;
    for y in y0..y1 {
        for x in x0..x1 {
            total += map.at(x, y);
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        total / count as f32
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A map of `width x height` zeros with filled rectangles painted on.
    fn map_with(
        width: usize,
        height: usize,
        rects: &[(usize, usize, usize, usize, f32)],
    ) -> Vec<f32> {
        let mut data = vec![0.0_f32; width * height];
        for &(x, y, w, h, value) in rects {
            for row in y..(y + h).min(height) {
                for column in x..(x + w).min(width) {
                    data[row * width + column] = value;
                }
            }
        }
        data
    }

    fn detect(data: &[f32], width: usize, height: usize) -> Vec<DetectedBox> {
        let map = ProbabilityMap {
            data,
            width,
            height,
        };
        boxes_from_map(
            &map,
            DetectorOptions::default(),
            1.0,
            1.0,
            width as f32,
            height as f32,
        )
    }

    #[test]
    fn a_blank_map_finds_nothing_and_is_not_an_error() {
        assert!(detect(&vec![0.0; 40 * 40], 40, 40).is_empty());
    }

    #[test]
    fn one_solid_block_becomes_one_box() {
        let data = map_with(40, 40, &[(10, 12, 20, 6, 0.9)]);
        let found = detect(&data, 40, 40);
        assert_eq!(found.len(), 1, "found {found:?}");
        assert!(found[0].score > 0.6);
    }

    #[test]
    fn two_separated_blocks_stay_two_boxes() {
        // A clear gap of zeros between them.
        let data = map_with(60, 40, &[(4, 10, 16, 6, 0.9), (36, 10, 16, 6, 0.9)]);
        let found = detect(&data, 60, 40);
        assert_eq!(found.len(), 2, "found {found:?}");
    }

    #[test]
    fn four_connectivity_keeps_diagonally_touching_blocks_apart() {
        // Two blocks meeting at a single corner. Under 8-connectivity these
        // merge into one box; under 4-connectivity, which is what the reference
        // implementation uses, they must stay separate.
        let mut data = map_with(40, 40, &[(4, 4, 8, 8, 0.9)]);
        for row in 12..20 {
            for column in 12..20 {
                data[row * 40 + column] = 0.9;
            }
        }
        let found = detect(&data, 40, 40);
        assert_eq!(
            found.len(),
            2,
            "diagonal touch merged the regions: {found:?}"
        );
    }

    #[test]
    fn a_block_below_the_pixel_threshold_is_invisible() {
        // 0.1 is under the 0.2 binarize threshold, so no pixel survives.
        // ⚠️ This block was 0.2 against a 0.3 threshold until `I-363` lowered
        // the threshold to 0.2. Left at 0.2 the test would have been asserting
        // against the boundary itself rather than under it, which passes or
        // fails on the comparison operator rather than on the property.
        let data = map_with(40, 40, &[(10, 12, 20, 6, 0.1)]);
        assert!(detect(&data, 40, 40).is_empty());
    }

    #[test]
    fn a_block_over_the_pixel_threshold_but_under_the_box_threshold_is_dropped() {
        // Every pixel is 0.3: above binarize (0.2), below box mean (0.4).
        // ⚠️ This block was 0.45 against 0.3 / 0.6 until `I-363`. At the new
        // thresholds 0.45 is ABOVE the box mean, so the old value would have
        // made this test assert the opposite of its own name.
        let data = map_with(40, 40, &[(10, 12, 20, 6, 0.3)]);
        assert!(
            detect(&data, 40, 40).is_empty(),
            "the box-mean threshold did not reject a weak region"
        );
    }

    #[test]
    fn a_hairline_region_is_dropped_by_the_min_size_filter() {
        // One pixel tall: the short side is 1, under min_size 3.
        let data = map_with(40, 40, &[(5, 20, 25, 1, 0.95)]);
        assert!(detect(&data, 40, 40).is_empty());
    }

    #[test]
    fn the_box_is_grown_past_the_lit_pixels_by_the_unclip() {
        // DB fires on a shrunk region, so the reported box must be BIGGER than
        // the pixels that produced it. This is the test that would catch unclip
        // being skipped entirely.
        let data = map_with(80, 80, &[(20, 30, 30, 10, 0.95)]);
        let found = detect(&data, 80, 80);
        assert_eq!(found.len(), 1);
        let (min_x, min_y, max_x, max_y) = found[0].quad.bounds();
        assert!(min_x < 20.0, "left edge {min_x} was not grown past 20");
        assert!(min_y < 30.0, "top edge {min_y} was not grown past 30");
        assert!(max_x > 50.0, "right edge {max_x} was not grown past 50");
        assert!(max_y > 40.0, "bottom edge {max_y} was not grown past 40");
    }

    #[test]
    fn boxes_come_back_in_source_coordinates() {
        // The map is half the size of the source frame in each direction.
        let data = map_with(40, 40, &[(10, 12, 20, 8, 0.95)]);
        let map = ProbabilityMap {
            data: &data,
            width: 40,
            height: 40,
        };
        let found = boxes_from_map(&map, DetectorOptions::default(), 2.0, 2.0, 80.0, 80.0);
        assert_eq!(found.len(), 1);
        let (min_x, _, max_x, _) = found[0].quad.bounds();
        // The lit pixels span x = 10..30 in map space, so 20..60 in source
        // space, then grown outward by the unclip.
        assert!(min_x < 20.0 && min_x > 5.0, "left edge {min_x}");
        assert!(max_x > 60.0 && max_x < 75.0, "right edge {max_x}");
    }

    #[test]
    fn boxes_are_clamped_to_the_frame() {
        // A block hard against the edge: unclipping pushes it outside, and the
        // result must not name a pixel the bitmap does not have.
        let data = map_with(40, 40, &[(0, 0, 20, 8, 0.95)]);
        let found = detect(&data, 40, 40);
        assert_eq!(found.len(), 1);
        let (min_x, min_y, max_x, max_y) = found[0].quad.bounds();
        assert!(min_x >= 0.0 && min_y >= 0.0, "box escaped the top-left");
        assert!(
            max_x <= 40.0 && max_y <= 40.0,
            "box escaped the bottom-right"
        );
    }

    #[test]
    fn an_inconsistent_map_returns_nothing_rather_than_reading_past_the_buffer() {
        let data = vec![0.9_f32; 10];
        let map = ProbabilityMap {
            data: &data,
            width: 40,
            height: 40,
        };
        assert!(!map.is_consistent());
        assert!(boxes_from_map(&map, DetectorOptions::default(), 1.0, 1.0, 40.0, 40.0).is_empty());
    }

    #[test]
    fn a_full_map_region_does_not_overflow_the_stack() {
        // The flood fill is iterative for exactly this case: every pixel lit, so
        // the region is the whole map. A recursive fill dies here.
        let data = vec![0.95_f32; 200 * 200];
        let found = detect(&data, 200, 200);
        assert_eq!(found.len(), 1, "found {}", found.len());
    }
}
