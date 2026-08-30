//! Quadrilaterals, and the geometry the detector's post-processing needs.
//!
//! PP-OCR's detection head emits a probability map, not boxes. Turning that map
//! into boxes is entirely ours: binarize, find connected regions, fit a rotated
//! rectangle to each, then expand it. **Every function here is pure and takes no
//! model**, which is what makes this file testable without ONNX Runtime, without
//! a model file, and in CI -- see the tests at the bottom.
//!
//! # Why quads and not polygons
//!
//! PP-OCR offers two box types. `poly` traces the contour and offsets it as a
//! general polygon, which needs a full polygon-offsetting implementation
//! (Vatti/Clipper) to be correct. `quad` fits a **rotated rectangle** and is the
//! reference implementation's default. This file implements `quad`, and the
//! choice is deliberate rather than a simplification: for a rectangle the
//! offset distance has a closed form (see [`Quad::unclip`]), so it is exact
//! rather than approximate, and a wrong-but-plausible polygon offset invented
//! here is exactly the kind of thing that looks right until it silently clips a
//! descender.

use std::cmp::Ordering;

/// A point in image space, subpixel.
///
/// `f32` rather than the integer [`uptake_core::geometry::Point`] on purpose:
/// these coordinates come out of a resized probability map and are scaled back
/// to source pixels, so rounding at each step would accumulate. The conversion
/// to integers happens once, at the boundary where a box becomes a
/// [`crate::engine::TextBlock`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointF {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
}

impl PointF {
    /// Creates a point.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Euclidean distance to another point.
    #[must_use]
    pub fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// Four corners, in clockwise order starting from the top-left.
///
/// **The ordering is part of the type's meaning, not a convention callers may
/// vary.** Rectification reads corner 0 as top-left and corner 1 as top-right to
/// decide which way the text runs; a quad wound the other way produces a
/// horizontally mirrored crop, which the recogniser will happily turn into
/// confident nonsense. [`Quad::from_unordered`] is the only constructor that
/// accepts arbitrary input, and it establishes the order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// The four corners, clockwise from top-left.
    pub corners: [PointF; 4],
}

impl Quad {
    /// Builds a quad from four corners already in clockwise-from-top-left order.
    #[must_use]
    pub const fn new(corners: [PointF; 4]) -> Self {
        Self { corners }
    }

    /// Orders four arbitrary corners clockwise from the top-left.
    ///
    /// The rule is the reference implementation's: sort by `x`, which splits the
    /// points into a left pair and a right pair, then within each pair the
    /// smaller `y` is the upper corner.
    ///
    /// ⚠️ **Near 45 degrees this is a convention, not a truth.** For a quad
    /// rotated far enough, the `x`-split can put both ends of one edge on the
    /// same side, and the winding stays self-consistent without matching what a
    /// human would call "top-left". That is the reference implementation's
    /// behaviour too, and rectification depends only on the self-consistency.
    #[must_use]
    pub fn from_unordered(mut points: [PointF; 4]) -> Self {
        points.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal));
        let (left, right) = (&points[..2], &points[2..]);
        let (top_left, bottom_left) = if left[0].y <= left[1].y {
            (left[0], left[1])
        } else {
            (left[1], left[0])
        };
        let (top_right, bottom_right) = if right[0].y <= right[1].y {
            (right[0], right[1])
        } else {
            (right[1], right[0])
        };
        Self::new([top_left, top_right, bottom_right, bottom_left])
    }

    /// The polygon's area, by the shoelace formula.
    #[must_use]
    pub fn area(&self) -> f32 {
        let mut sum = 0.0;
        for index in 0..4 {
            let a = self.corners[index];
            let b = self.corners[(index + 1) % 4];
            sum += a.x.mul_add(b.y, -(b.x * a.y));
        }
        sum.abs() / 2.0
    }

    /// The perimeter.
    #[must_use]
    pub fn perimeter(&self) -> f32 {
        (0..4)
            .map(|index| self.corners[index].distance(self.corners[(index + 1) % 4]))
            .sum()
    }

    /// The axis-aligned bounding box, as `(min_x, min_y, max_x, max_y)`.
    #[must_use]
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for corner in &self.corners {
            min_x = min_x.min(corner.x);
            min_y = min_y.min(corner.y);
            max_x = max_x.max(corner.x);
            max_y = max_y.max(corner.y);
        }
        (min_x, min_y, max_x, max_y)
    }

    /// The longer of the two side-pair lengths, and the shorter, as
    /// `(long, short)`.
    ///
    /// Rectification uses this for the output crop size: a line of text is
    /// longer along the reading direction than across it.
    #[must_use]
    pub fn side_lengths(&self) -> (f32, f32) {
        let top = self.corners[0].distance(self.corners[1]);
        let right = self.corners[1].distance(self.corners[2]);
        let bottom = self.corners[2].distance(self.corners[3]);
        let left = self.corners[3].distance(self.corners[0]);
        let horizontal = top.max(bottom);
        let vertical = right.max(left);
        (horizontal.max(vertical), horizontal.min(vertical))
    }

    /// Whether all four corners are right angles, within a tolerance.
    ///
    /// The precondition [`Quad::unclip`] depends on. Tolerance rather than
    /// equality because the corners arrive from floating-point projections onto
    /// an orthonormal basis, where a true rectangle lands a few ULPs off square;
    /// `1e-3` on a normalised dot product is far tighter than any real
    /// non-rectangle and far looser than that accumulated error.
    ///
    /// A degenerate quad with a zero-length edge reports `true`: it has no angle
    /// to be wrong about, and `unclip` returns it unchanged anyway.
    #[must_use]
    pub fn is_rectangular(&self) -> bool {
        (0..4).all(|index| {
            let previous = self.corners[(index + 3) % 4];
            let corner = self.corners[index];
            let next = self.corners[(index + 1) % 4];
            let (ax, ay) = (previous.x - corner.x, previous.y - corner.y);
            let (bx, by) = (next.x - corner.x, next.y - corner.y);
            let (length_a, length_b) = (ax.hypot(ay), bx.hypot(by));
            if length_a <= f32::EPSILON || length_b <= f32::EPSILON {
                return true;
            }
            (ax.mul_add(bx, ay * by) / (length_a * length_b)).abs() < 1e-3
        })
    }

    /// Expands the quad outwards by PP-OCR's unclip rule.
    ///
    /// # The rule
    ///
    /// DB's probability map is trained to fire on a *shrunk* version of each text
    /// region, so a detected box is systematically too tight and must be grown
    /// back before the crop is handed to the recogniser. The reference
    /// implementation offsets the polygon outward by
    ///
    /// ```text
    /// distance = area * unclip_ratio / perimeter
    /// ```
    ///
    /// # Why this is exact here rather than approximate
    ///
    /// Offsetting a general polygon by a distance needs a clipping library. A
    /// **rectangle** does not: growing it by `distance` on all four sides moves
    /// each side along its own outward normal, which this computes directly. The
    /// implementation walks the two edges meeting at each corner, takes their
    /// outward normals, and moves the corner by their sum -- the exact
    /// intersection of the two offset sides. That identity is the whole argument
    /// for `quad` over `poly` in this module's header.
    ///
    /// A degenerate quad (zero perimeter) is returned unchanged rather than
    /// producing `NaN` corners: a zero-size region is dropped downstream by the
    /// size filter, and poisoning it with `NaN` first would make that
    /// comparison silently false instead.
    ///
    /// # Precondition: the quad must be a rectangle
    ///
    /// ⚠️ **The exactness above holds for rectangles and ONLY for rectangles.**
    /// On a non-rectangular quad the corner offsets come out **non-uniform** --
    /// measured on a kite, 11.28 to 21.72 against an intended uniform 17.48 --
    /// and nothing about the result looks wrong. That is the failure this
    /// module's header warns about for the polygon case it declined to
    /// implement, reappearing on the path it did.
    ///
    /// Every caller inside this crate satisfies it, and structurally rather than
    /// by convention: the only call site passes the output of [`min_area_rect`],
    /// which builds its corners from extremes in an orthonormal basis and so
    /// cannot return anything but a rectangle. But [`Quad::new`] is `pub`, this
    /// method is `pub`, and the module is reachable from outside the crate, so
    /// an external caller can violate it.
    ///
    /// ⚠️ **The `debug_assert!` below does NOT protect the shipped binary, and
    /// this paragraph claimed it did until round 2 of the review.** It said the
    /// assert *"turns that into a test-build failure rather than a plausible
    /// wrong answer"* -- for an **external** caller, which is precisely the
    /// caller least likely to be running a debug build. `debug_assert!` is
    /// compiled out under `--release`, and UP-TAKE ships a release build
    /// (`LEGAL-AND-COMMERCE.md` section 5, the SignPath-signed installer). So an
    /// external caller violating this precondition in a shipped build gets no
    /// panic, no error, and exactly the plausible wrong answer the old sentence
    /// promised was prevented.
    ///
    /// **What the assert is actually worth:** it catches a violation in this
    /// crate's own tests and in any debug build, which is where a new internal
    /// caller would be written and run first. That is real and it is narrow.
    /// **What would close it properly** is a fallible constructor -- a
    /// `Quad::rectangle()` returning `Option`, with `unclip` taking that type
    /// instead -- so the precondition lives in the type rather than in a
    /// runtime check that ships disabled. Not done here: it changes a public
    /// API for a caller that does not exist yet, and the honest move is to say
    /// so rather than to leave a guard overstated. Recorded as `I-335`.
    ///
    /// *(Added 2026-08-30 after an independent review verified the exactness
    /// claim, verified that the current call graph upholds it, and pointed out
    /// that nothing in the type or the code says so. Corrected the same day by
    /// round 2 of the same review, which read the release-build semantics the
    /// first version had not.)*
    #[must_use]
    pub fn unclip(&self, ratio: f32) -> Self {
        debug_assert!(
            self.is_rectangular(),
            "Quad::unclip is exact only for rectangles; this quad has non-right              corners and the offsets it produces will be silently non-uniform"
        );
        let perimeter = self.perimeter();
        if perimeter <= f32::EPSILON {
            return *self;
        }
        let distance = self.area() * ratio / perimeter;

        // Outward normal of the edge from corner i to corner i+1. The corners
        // wind clockwise in a y-down image space, so the outward side of an edge
        // travelling (dx, dy) is (dy, -dx) normalised.
        let outward_normal = |index: usize| {
            let a = self.corners[index];
            let b = self.corners[(index + 1) % 4];
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let length = dx.hypot(dy);
            if length <= f32::EPSILON {
                (0.0, 0.0)
            } else {
                (dy / length, -dx / length)
            }
        };

        let mut grown = self.corners;
        for (index, corner) in grown.iter_mut().enumerate() {
            // Corner `index` is shared by the edge that ends at it (index-1) and
            // the edge that starts at it (index).
            let (incoming_x, incoming_y) = outward_normal((index + 3) % 4);
            let (outgoing_x, outgoing_y) = outward_normal(index);
            corner.x += (incoming_x + outgoing_x) * distance;
            corner.y += (incoming_y + outgoing_y) * distance;
        }
        Self::new(grown)
    }

    /// Scales every corner by independent x and y factors.
    ///
    /// The detector runs on a resized copy of the frame, so every box comes back
    /// in the resized image's coordinates and has to be mapped home. Two factors
    /// rather than one because the resize is not required to preserve aspect
    /// ratio: it snaps each dimension to a multiple of 32 independently, so the
    /// two ratios genuinely differ.
    #[must_use]
    pub fn scaled(&self, factor_x: f32, factor_y: f32) -> Self {
        let mut corners = self.corners;
        for corner in &mut corners {
            corner.x *= factor_x;
            corner.y *= factor_y;
        }
        Self::new(corners)
    }

    /// Clamps every corner into `[0, width] x [0, height]`.
    ///
    /// Unclipping can push a box past the frame edge, and a crop that reads
    /// outside the bitmap is either a panic or, worse, a silent wrap onto the
    /// next row.
    #[must_use]
    pub fn clamped(&self, width: f32, height: f32) -> Self {
        let mut corners = self.corners;
        for corner in &mut corners {
            corner.x = corner.x.clamp(0.0, width);
            corner.y = corner.y.clamp(0.0, height);
        }
        Self::new(corners)
    }
}

/// The convex hull of a point set, counter-clockwise, by monotone chain.
///
/// Returns fewer than three points only when the input is degenerate (empty, a
/// single point, or all points collinear), which callers treat as "no box here".
#[must_use]
pub fn convex_hull(points: &[PointF]) -> Vec<PointF> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| {
        a.x.partial_cmp(&b.x)
            .unwrap_or(Ordering::Equal)
            .then(a.y.partial_cmp(&b.y).unwrap_or(Ordering::Equal))
    });
    sorted.dedup_by(|a, b| (a.x - b.x).abs() < f32::EPSILON && (a.y - b.y).abs() < f32::EPSILON);
    if sorted.len() < 3 {
        return sorted;
    }

    let cross = |o: PointF, a: PointF, b: PointF| {
        (a.x - o.x).mul_add(b.y - o.y, -((a.y - o.y) * (b.x - o.x)))
    };

    let mut hull: Vec<PointF> = Vec::with_capacity(sorted.len() * 2);
    for &point in &sorted {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0 {
            hull.pop();
        }
        hull.push(point);
    }
    let lower_len = hull.len() + 1;
    for &point in sorted.iter().rev().skip(1) {
        while hull.len() >= lower_len
            && cross(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0
        {
            hull.pop();
        }
        hull.push(point);
    }
    hull.pop();
    hull
}

/// The minimum-area rectangle enclosing a point set, by rotating calipers.
///
/// Returns `None` for a degenerate set -- fewer than three distinct points, or
/// all of them collinear -- because there is no rectangle to report, and
/// returning a zero-area one would hand the size filter a box to reject for the
/// wrong reason.
///
/// # Why the hull edges are the only angles worth trying
///
/// The minimum-area enclosing rectangle always has a side flush with an edge of
/// the convex hull. That is a theorem, not a heuristic, and it is what turns a
/// continuous search over rotations into a loop over at most `hull.len()`
/// candidates.
#[must_use]
pub fn min_area_rect(points: &[PointF]) -> Option<Quad> {
    let hull = convex_hull(points);
    if hull.len() < 3 {
        return None;
    }

    let mut best: Option<(f32, Quad)> = None;
    for index in 0..hull.len() {
        let a = hull[index];
        let b = hull[(index + 1) % hull.len()];
        let (edge_x, edge_y) = (b.x - a.x, b.y - a.y);
        let length = edge_x.hypot(edge_y);
        if length <= f32::EPSILON {
            continue;
        }
        // Unit vector along this hull edge, and its normal.
        let (ux, uy) = (edge_x / length, edge_y / length);
        let (nx, ny) = (-uy, ux);

        let mut min_along = f32::INFINITY;
        let mut max_along = f32::NEG_INFINITY;
        let mut min_across = f32::INFINITY;
        let mut max_across = f32::NEG_INFINITY;
        for &point in &hull {
            let along = point.x.mul_add(ux, point.y * uy);
            let across = point.x.mul_add(nx, point.y * ny);
            min_along = min_along.min(along);
            max_along = max_along.max(along);
            min_across = min_across.min(across);
            max_across = max_across.max(across);
        }
        let area = (max_along - min_along) * (max_across - min_across);
        if best
            .as_ref()
            .is_some_and(|(best_area, _)| area >= *best_area)
        {
            continue;
        }
        // Rebuild the four corners in image space from the (along, across)
        // extremes: a point at (u, n) in the edge frame is u*unit + n*normal.
        let corner = |along: f32, across: f32| {
            PointF::new(
                along.mul_add(ux, across * nx),
                along.mul_add(uy, across * ny),
            )
        };
        let quad = Quad::from_unordered([
            corner(min_along, min_across),
            corner(max_along, min_across),
            corner(max_along, max_across),
            corner(min_along, max_across),
        ]);
        best = Some((area, quad));
    }
    best.map(|(_, quad)| quad)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Builds an axis-aligned quad, clockwise from top-left.
    fn rect(x: f32, y: f32, width: f32, height: f32) -> Quad {
        Quad::new([
            PointF::new(x, y),
            PointF::new(x + width, y),
            PointF::new(x + width, y + height),
            PointF::new(x, y + height),
        ])
    }

    fn assert_close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn area_and_perimeter_of_an_axis_aligned_rectangle_are_the_schoolbook_values() {
        let quad = rect(10.0, 20.0, 30.0, 4.0);
        assert_close(quad.area(), 120.0, 1e-3);
        assert_close(quad.perimeter(), 68.0, 1e-3);
    }

    #[test]
    fn area_is_orientation_independent() {
        // The shoelace formula is signed; area() must not be.
        let clockwise = rect(0.0, 0.0, 6.0, 2.0);
        let counter_clockwise = Quad::new([
            clockwise.corners[0],
            clockwise.corners[3],
            clockwise.corners[2],
            clockwise.corners[1],
        ]);
        assert_close(clockwise.area(), counter_clockwise.area(), 1e-4);
    }

    #[test]
    fn from_unordered_puts_the_corners_clockwise_from_top_left() {
        let scrambled = [
            PointF::new(10.0, 6.0), // bottom-left
            PointF::new(4.0, 2.0),  // top-left
            PointF::new(10.0, 2.0), // top-right
            PointF::new(4.0, 6.0),  // bottom-left? no -- x=4 pairs with (4,2)
        ];
        let quad = Quad::from_unordered(scrambled);
        assert_eq!(quad.corners[0], PointF::new(4.0, 2.0));
        assert_eq!(quad.corners[1], PointF::new(10.0, 2.0));
        assert_eq!(quad.corners[2], PointF::new(10.0, 6.0));
        assert_eq!(quad.corners[3], PointF::new(4.0, 6.0));
    }

    #[test]
    fn side_lengths_reports_the_reading_direction_first() {
        // A wide, short box: the long side is the horizontal one.
        let (long, short) = rect(0.0, 0.0, 40.0, 8.0).side_lengths();
        assert_close(long, 40.0, 1e-3);
        assert_close(short, 8.0, 1e-3);

        // A tall, narrow box: long is still the longer, whichever axis it is on.
        let (long, short) = rect(0.0, 0.0, 8.0, 40.0).side_lengths();
        assert_close(long, 40.0, 1e-3);
        assert_close(short, 8.0, 1e-3);
    }

    #[test]
    fn unclip_grows_each_side_by_exactly_the_reference_distance() {
        // This is the test that pins the closed form. For a w x h rectangle,
        // distance = area * ratio / perimeter, and every side must move OUT by
        // that distance -- so width grows by 2*distance, height likewise.
        let (width, height, ratio) = (40.0_f32, 10.0_f32, 1.5_f32);
        let quad = rect(100.0, 200.0, width, height);
        let distance = (width * height) * ratio / (2.0 * (width + height));

        let grown = quad.unclip(ratio);
        let (min_x, min_y, max_x, max_y) = grown.bounds();

        assert_close(min_x, 100.0 - distance, 1e-3);
        assert_close(min_y, 200.0 - distance, 1e-3);
        assert_close(max_x, 100.0 + width + distance, 1e-3);
        assert_close(max_y, 200.0 + height + distance, 1e-3);
    }

    #[test]
    fn unclip_leaves_a_degenerate_quad_alone_rather_than_producing_nan() {
        let point = PointF::new(5.0, 5.0);
        let degenerate = Quad::new([point, point, point, point]);
        let result = degenerate.unclip(1.5);
        for corner in &result.corners {
            assert!(corner.x.is_finite(), "unclip produced a non-finite x");
            assert!(corner.y.is_finite(), "unclip produced a non-finite y");
        }
        assert_eq!(result, degenerate);
    }

    #[test]
    fn scaled_maps_a_box_from_the_resized_map_back_to_source_pixels() {
        // The detector saw a 320x320 copy of a 640x480 frame.
        let in_map = rect(10.0, 20.0, 40.0, 8.0);
        let home = in_map.scaled(640.0 / 320.0, 480.0 / 320.0);
        let (min_x, min_y, max_x, max_y) = home.bounds();
        assert_close(min_x, 20.0, 1e-3);
        assert_close(min_y, 30.0, 1e-3);
        assert_close(max_x, 100.0, 1e-3);
        assert_close(max_y, 42.0, 1e-3);
    }

    #[test]
    fn clamped_keeps_an_unclipped_box_inside_the_frame() {
        let overhanging = rect(-5.0, -5.0, 30.0, 30.0);
        let inside = overhanging.clamped(20.0, 20.0);
        let (min_x, min_y, max_x, max_y) = inside.bounds();
        assert_close(min_x, 0.0, 1e-6);
        assert_close(min_y, 0.0, 1e-6);
        assert_close(max_x, 20.0, 1e-6);
        assert_close(max_y, 20.0, 1e-6);
    }

    #[test]
    fn convex_hull_drops_interior_points() {
        let points = vec![
            PointF::new(0.0, 0.0),
            PointF::new(10.0, 0.0),
            PointF::new(10.0, 10.0),
            PointF::new(0.0, 10.0),
            PointF::new(5.0, 5.0), // strictly inside
            PointF::new(3.0, 7.0), // strictly inside
        ];
        let hull = convex_hull(&points);
        assert_eq!(hull.len(), 4, "hull was {hull:?}");
        assert!(!hull.contains(&PointF::new(5.0, 5.0)));
    }

    #[test]
    fn convex_hull_of_collinear_points_is_degenerate_rather_than_a_sliver() {
        let points: Vec<PointF> = (0..5)
            .map(|i| PointF::new(i as f32, 2.0 * i as f32))
            .collect();
        assert!(convex_hull(&points).len() < 3);
    }

    #[test]
    fn min_area_rect_recovers_an_axis_aligned_box_exactly() {
        let points = vec![
            PointF::new(4.0, 2.0),
            PointF::new(14.0, 2.0),
            PointF::new(14.0, 8.0),
            PointF::new(4.0, 8.0),
            PointF::new(9.0, 5.0),
        ];
        let quad = min_area_rect(&points).unwrap();
        assert_close(quad.area(), 60.0, 1e-2);
        let (min_x, min_y, max_x, max_y) = quad.bounds();
        assert_close(min_x, 4.0, 1e-2);
        assert_close(min_y, 2.0, 1e-2);
        assert_close(max_x, 14.0, 1e-2);
        assert_close(max_y, 8.0, 1e-2);
    }

    #[test]
    fn min_area_rect_beats_the_bounding_box_on_a_rotated_one() {
        // A 20x6 rectangle rotated 30 degrees. Its axis-aligned bounding box is
        // much bigger than 120; the minimum-area rectangle must find 120.
        let (width, height) = (20.0_f32, 6.0_f32);
        let angle = std::f32::consts::FRAC_PI_6;
        let (sin, cos) = angle.sin_cos();
        let corners: Vec<PointF> = [(0.0, 0.0), (width, 0.0), (width, height), (0.0, height)]
            .into_iter()
            .map(|(x, y): (f32, f32)| {
                PointF::new(
                    x.mul_add(cos, -(y * sin)) + 50.0,
                    x.mul_add(sin, y * cos) + 50.0,
                )
            })
            .collect();

        let quad = min_area_rect(&corners).unwrap();
        assert_close(quad.area(), width * height, 0.5);

        let (min_x, min_y, max_x, max_y) = quad.bounds();
        let bounding_box_area = (max_x - min_x) * (max_y - min_y);
        assert!(
            bounding_box_area > quad.area() * 1.2,
            "the test is only meaningful if the box is genuinely rotated: \
             bounding box {bounding_box_area}, min-area {}",
            quad.area()
        );
    }

    #[test]
    fn is_rectangular_accepts_a_rectangle_at_any_rotation() {
        // A true rectangle projected onto a rotated basis is what min_area_rect
        // returns; the tolerance exists for exactly this floating-point case.
        for degrees in [0.0_f32, 7.0, 30.0, 45.0, 89.0] {
            let (sin, cos) = degrees.to_radians().sin_cos();
            let corners: Vec<PointF> = [(0.0, 0.0), (30.0, 0.0), (30.0, 9.0), (0.0, 9.0)]
                .into_iter()
                .map(|(x, y): (f32, f32)| {
                    PointF::new(x.mul_add(cos, -(y * sin)), x.mul_add(sin, y * cos))
                })
                .collect();
            let quad = Quad::new([corners[0], corners[1], corners[2], corners[3]]);
            assert!(
                quad.is_rectangular(),
                "rejected a rectangle at {degrees} degrees"
            );
        }
    }

    #[test]
    fn is_rectangular_rejects_the_kite_that_breaks_unclip() {
        // The shape whose offsets come out non-uniform. This is the precondition
        // `unclip`'s debug_assert fires on, and the reason it exists.
        let kite = Quad::new([
            PointF::new(20.0, 0.0),
            PointF::new(40.0, 20.0),
            PointF::new(20.0, 30.0),
            PointF::new(0.0, 20.0),
        ]);
        assert!(!kite.is_rectangular(), "accepted a kite as a rectangle");
    }

    #[test]
    fn min_area_rect_always_returns_something_unclip_may_be_called_on() {
        // The structural reason the precondition holds in production: the only
        // caller of unclip passes min_area_rect's output, and min_area_rect
        // builds corners from extremes in an orthonormal basis. Checked over a
        // deliberately awkward spread rather than asserted.
        let clouds: Vec<Vec<PointF>> = vec![
            vec![
                PointF::new(0.0, 0.0),
                PointF::new(10.0, 3.0),
                PointF::new(4.0, 9.0),
                PointF::new(7.0, 1.0),
            ],
            vec![
                PointF::new(-5.0, 2.0),
                PointF::new(12.0, -3.0),
                PointF::new(6.0, 14.0),
                PointF::new(0.0, 8.0),
                PointF::new(3.0, 3.0),
            ],
            (0..12)
                .map(|i| {
                    let angle = i as f32 * 0.5;
                    PointF::new(angle.cos() * 20.0, angle.sin() * 6.0)
                })
                .collect(),
        ];
        for (index, cloud) in clouds.iter().enumerate() {
            let quad = min_area_rect(cloud).unwrap();
            assert!(
                quad.is_rectangular(),
                "min_area_rect returned a non-rectangle for cloud {index}: {quad:?}"
            );
        }
    }

    #[test]
    fn min_area_rect_refuses_a_degenerate_set() {
        assert!(min_area_rect(&[]).is_none());
        assert!(min_area_rect(&[PointF::new(1.0, 1.0)]).is_none());
        let collinear: Vec<PointF> = (0..4).map(|i| PointF::new(i as f32, i as f32)).collect();
        assert!(min_area_rect(&collinear).is_none());
    }
}
