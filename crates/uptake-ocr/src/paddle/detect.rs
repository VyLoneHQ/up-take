//! Turning DB's probability map into text boxes.
//!
//! PP-OCRv4's detector is a Differentiable Binarization network. It does not
//! emit boxes: it emits one `float` per pixel, the probability that the pixel is
//! inside a text region, on a map the same size as the resized input. Everything
//! that turns that into quads is here, and **none of it needs ONNX Runtime** --
//! the input is a slice of floats, so every rule below is tested against maps
//! this file's own tests draw by hand.
//!
//! The stages, in order, each with the reference implementation's default:
//!
//! 1. **Binarize** at `threshold` (0.3).
//! 2. **Connect** the surviving pixels into regions (4-connectivity).
//! 3. **Fit** a minimum-area rectangle to each region.
//! 4. **Score** each region by the mean probability under its box, and drop it
//!    below `box_threshold` (0.6).
//! 5. **Unclip** by `unclip_ratio` (1.5), because DB is trained on shrunk
//!    regions.
//! 6. **Drop** boxes thinner than `min_size` (3 px), which are noise.
//! 7. **Scale** back to source-frame coordinates.

use super::quad::{PointF, Quad, min_area_rect};

/// The knobs DB's post-processing exposes, with PP-OCR's defaults.
///
/// Named constants rather than magic numbers at the call site: every one of
/// these is a value the reference implementation chose, and a session that
/// changes one should have to say which.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectorOptions {
    /// Probability above which a pixel counts as text. PP-OCR's `det_db_thresh`.
    pub threshold: f32,
    /// Mean-probability floor for a whole box. PP-OCR's `det_db_box_thresh`.
    pub box_threshold: f32,
    /// How far to grow each box. PP-OCR's `det_db_unclip_ratio`.
    pub unclip_ratio: f32,
    /// Shortest side, in resized pixels, a box may have and survive.
    pub min_size: f32,
}

impl Default for DetectorOptions {
    fn default() -> Self {
        Self {
            threshold: 0.3,
            box_threshold: 0.6,
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
/// `scale_x` and `scale_y` come from [`super::preprocess::DetectorInput`] and
/// map resized coordinates home. Returns boxes in **source-frame** coordinates,
/// clamped to `(source_width, source_height)`.
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
        // 0.2 is under the 0.3 binarize threshold, so no pixel survives.
        let data = map_with(40, 40, &[(10, 12, 20, 6, 0.2)]);
        assert!(detect(&data, 40, 40).is_empty());
    }

    #[test]
    fn a_block_over_the_pixel_threshold_but_under_the_box_threshold_is_dropped() {
        // Every pixel is 0.45: above binarize (0.3), below box mean (0.6).
        let data = map_with(40, 40, &[(10, 12, 20, 6, 0.45)]);
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
