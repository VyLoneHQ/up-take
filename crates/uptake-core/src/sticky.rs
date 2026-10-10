//! Sticky areas: where an area sits on the window it follows (roadmap `1.47`,
//! ADR-0049 decision 3).
//!
//! A sticky area is anchored to a window instead of to the screen. This module
//! is the arithmetic of that and nothing else: given the area and the window
//! at the moment they were joined, it records an [`Anchor`], and given the
//! window's rectangle later it says where the area belongs now. Finding the
//! window, hearing that it moved and noticing that it closed are the host's
//! job (`src-tauri/src/sticky.rs`), because they are Windows calls and this
//! crate has none.
//!
//! # The rule
//!
//! The founder's decision of 2026-10-09: when the window is resized, the area
//! keeps its own size and its distance from the window's **nearest corner or
//! edge**. Read per axis, that is one of three things:
//!
//! - the area sits in the first third of the window, so it keeps its distance
//!   from the left (or top) edge;
//! - it sits in the last third, so it keeps its distance from the right (or
//!   bottom) edge;
//! - it sits in the middle third, so it keeps its distance from the middle.
//!
//! Two side thirds make a corner. One side third and one middle third make an
//! edge: an area in the top middle rides the top edge and stays centred on it.
//! An area in the very middle of the window has no corner or edge to prefer by
//! thirds, so it takes the edge its own border is closest to.
//!
//! The thirds are measured at the area's centre. That is this module's reading
//! of "nearest", and it is the part to change if the founder meant another.
//!
//! # What does not change the anchor
//!
//! Moving the window changes nothing, and resizing it does not re-pick the
//! thirds. The anchor is taken when the area is made sticky and again when the
//! user moves or resizes the area by hand, so a window that is shrunk and then
//! grown back puts the area exactly where it was.

use serde::{Deserialize, Serialize};

use crate::geometry::{Rect, Size};

/// Whether an area follows a window, and whether it can right now.
///
/// This lives on the [`Area`](crate::area::Area) so the page and the menu read
/// it with everything else the area is. The host owns the transitions: only it
/// knows whether the window still exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sticky {
    /// Anchored to the screen, as every area is when it is created.
    #[default]
    Free,
    /// Anchored to a window that is on screen. The area moves with it.
    Following,
    /// Anchored to a window that cannot be followed right now: it is
    /// minimised, hidden, or closed. The area waits where it was and is shown
    /// as paused. It follows again by itself when the window comes back.
    Paused,
}

impl Sticky {
    /// Whether the area is anchored to a window at all, followed or waiting.
    #[must_use]
    pub const fn is_sticky(self) -> bool {
        !matches!(self, Self::Free)
    }
}

/// Which part of the window one axis of the area keeps its distance from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The left edge, or the top edge.
    Near,
    /// The middle of the window along this axis.
    Centre,
    /// The right edge, or the bottom edge.
    Far,
}

/// One axis of an [`Anchor`]: the part of the window, and the distance from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Axis {
    side: Side,
    /// Physical pixels at the anchor's own scale. For [`Side::Near`] it is the
    /// area's start minus the window's start. For [`Side::Far`] it is the
    /// window's end minus the area's end. For [`Side::Centre`] it is **twice**
    /// the area's centre minus twice the window's centre: doubled so that a
    /// centre that falls on a half pixel is still a whole number, and the area
    /// goes back to exactly where it was.
    offset: i64,
}

/// Where an area sits on a window: the nearest corner or edge, the distance
/// from it, and the area's own size.
///
/// Take one with [`Anchor::of`] and ask it for the area's rectangle with
/// [`Anchor::place`]. For the same window the answer is the rectangle the
/// anchor was taken from, exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Anchor {
    horizontal: Axis,
    vertical: Axis,
    size: Size,
}

/// A span along one axis: where it starts and how long it is.
#[derive(Clone, Copy)]
struct Span {
    start: i64,
    length: i64,
}

impl Span {
    const fn end(self) -> i64 {
        self.start + self.length
    }

    /// Twice the centre, so it stays a whole number.
    const fn centre_doubled(self) -> i64 {
        2 * self.start + self.length
    }
}

fn horizontal(rect: Rect) -> Span {
    Span {
        start: i64::from(rect.origin.x),
        length: i64::from(rect.size.width),
    }
}

fn vertical(rect: Rect) -> Span {
    Span {
        start: i64::from(rect.origin.y),
        length: i64::from(rect.size.height),
    }
}

/// Which third of the window the area's centre is in.
fn third(area: Span, window: Span) -> Side {
    // Doubled throughout, like the centre itself. A window with no length has
    // no thirds, and the near edge is as good as any.
    let position = area.centre_doubled() - 2 * window.start;
    let whole = 2 * window.length;
    if whole <= 0 || 3 * position < whole {
        Side::Near
    } else if 3 * position > 2 * whole {
        Side::Far
    } else {
        Side::Centre
    }
}

fn offset(side: Side, area: Span, window: Span) -> i64 {
    match side {
        Side::Near => area.start - window.start,
        Side::Far => window.end() - area.end(),
        Side::Centre => area.centre_doubled() - window.centre_doubled(),
    }
}

/// The side whose window edge the area's own border is closer to.
fn closer_side(area: Span, window: Span) -> (Side, i64) {
    let near = (area.start - window.start).abs();
    let far = (window.end() - area.end()).abs();
    if near <= far {
        (Side::Near, near)
    } else {
        (Side::Far, far)
    }
}

impl Anchor {
    /// Takes the anchor of `area` on `window`, both in physical pixels in
    /// virtual-desktop space.
    ///
    /// The area does not have to be inside the window. One that hangs off it,
    /// or sits beside it, is anchored to the corner or edge it is nearest, and
    /// its distance is simply negative.
    #[must_use]
    pub fn of(area: Rect, window: Rect) -> Self {
        let across = (horizontal(area), horizontal(window));
        let down = (vertical(area), vertical(window));
        let mut sides = (third(across.0, across.1), third(down.0, down.1));
        if sides == (Side::Centre, Side::Centre) {
            // The very middle. Take the one edge the area's border is closest
            // to, and stay centred along it. A tie goes to the top or bottom.
            let (side_across, gap_across) = closer_side(across.0, across.1);
            let (side_down, gap_down) = closer_side(down.0, down.1);
            if gap_down <= gap_across {
                sides.1 = side_down;
            } else {
                sides.0 = side_across;
            }
        }
        Self {
            horizontal: Axis {
                side: sides.0,
                offset: offset(sides.0, across.0, across.1),
            },
            vertical: Axis {
                side: sides.1,
                offset: offset(sides.1, down.0, down.1),
            },
            size: area.size,
        }
    }

    /// The part of the window the area keeps its distance from, as
    /// `(horizontal, vertical)`.
    #[must_use]
    pub const fn sides(self) -> (Side, Side) {
        (self.horizontal.side, self.vertical.side)
    }

    /// Where the area belongs on `window` as it is now.
    ///
    /// `factor` is how much larger the window's contents are drawn than when
    /// the anchor was taken: the window's scale now divided by its scale then.
    /// It is `1.0` unless the window moved to a monitor with another scale, in
    /// which case what the area sat over is drawn `factor` times larger and
    /// `factor` times further from the window's edges, and so is the area.
    /// Anything that is not a positive, finite number is read as `1.0`.
    ///
    /// The area keeps its size when the window is resized. It is never made
    /// smaller than one pixel a side, and its position is held inside the
    /// range a coordinate can express.
    #[must_use]
    pub fn place(self, window: Rect, factor: f64) -> Rect {
        let factor = if factor.is_finite() && factor > 0.0 {
            factor
        } else {
            1.0
        };
        let width = scale(i64::from(self.size.width), factor).max(1);
        let height = scale(i64::from(self.size.height), factor).max(1);
        let x = start(self.horizontal, horizontal(window), width, factor);
        let y = start(self.vertical, vertical(window), height, factor);
        Rect::new(saturate(x), saturate(y), extent(width), extent(height))
    }
}

/// Where the area starts along one axis.
fn start(axis: Axis, window: Span, length: i64, factor: f64) -> i64 {
    let distance = scale(axis.offset, factor);
    match axis.side {
        Side::Near => window.start + distance,
        Side::Far => window.end() - distance - length,
        Side::Centre => (window.centre_doubled() + distance - length).div_euclid(2),
    }
}

/// `value * factor`, rounded to the nearest whole pixel. Exact at `1.0`.
///
/// The casts are safe: pixel counts are far inside what an `f64` holds exactly,
/// and the result is clamped before it becomes an integer again. The exact
/// comparison is meant, because `1.0` is the common case and must not round.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::float_cmp
)]
fn scale(value: i64, factor: f64) -> i64 {
    if factor == 1.0 {
        return value;
    }
    let scaled = (value as f64 * factor).round();
    scaled.clamp(-FAR, FAR) as i64
}

/// Further than any desktop reaches, and small enough to add without overflow.
const FAR: f64 = 1.0e12;

fn saturate(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// A length as a `u32`. The caller has already made it at least 1.
fn extent(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use proptest::prelude::*;

    use super::{Anchor, Side, Sticky};
    use crate::geometry::Rect;

    const WINDOW: Rect = Rect::new(100, 200, 900, 600);

    #[test]
    fn only_a_free_area_is_not_sticky() {
        assert!(!Sticky::Free.is_sticky());
        assert!(Sticky::Following.is_sticky());
        assert!(Sticky::Paused.is_sticky());
        assert_eq!(Sticky::default(), Sticky::Free);
    }

    #[test]
    fn each_third_of_the_window_picks_its_own_corner_or_edge() {
        // A 60 x 60 area centred in each ninth of a 900 x 600 window.
        let columns = [(150, Side::Near), (450, Side::Centre), (750, Side::Far)];
        let rows = [(100, Side::Near), (300, Side::Centre), (500, Side::Far)];
        for (centre_x, across) in columns {
            for (centre_y, down) in rows {
                let area = Rect::new(100 + centre_x - 30, 200 + centre_y - 30, 60, 60);
                let sides = Anchor::of(area, WINDOW).sides();
                if (across, down) == (Side::Centre, Side::Centre) {
                    // The very middle is not a corner or an edge. The window
                    // is wider than tall, so the top and bottom are closer.
                    assert_eq!(sides, (Side::Centre, Side::Near));
                } else {
                    assert_eq!(sides, (across, down), "centre {centre_x}, {centre_y}");
                }
            }
        }
    }

    #[test]
    fn the_thirds_change_exactly_at_a_third_and_two_thirds() {
        // A 900 px wide window has its thirds at 300 and 600. An area whose
        // centre is ON a boundary is in the middle third; half a pixel outside
        // it is in the side third. Every area here sits in the top third, so
        // the "very middle" rule never comes into it.
        let across = |x: i32, width: u32| {
            Anchor::of(Rect::new(100 + x, 210, width, 20), WINDOW)
                .sides()
                .0
        };
        assert_eq!(across(299, 1), Side::Near, "centre at 299.5");
        assert_eq!(across(299, 2), Side::Centre, "centre at 300");
        assert_eq!(across(599, 2), Side::Centre, "centre at 600");
        assert_eq!(across(600, 1), Side::Far, "centre at 600.5");
        // And down: a 600 px tall window has its thirds at 200 and 400.
        let down = |y: i32, height: u32| {
            Anchor::of(Rect::new(110, 200 + y, 20, height), WINDOW)
                .sides()
                .1
        };
        assert_eq!(down(199, 1), Side::Near, "centre at 199.5");
        assert_eq!(down(199, 2), Side::Centre, "centre at 200");
        assert_eq!(down(399, 2), Side::Centre, "centre at 400");
        assert_eq!(down(400, 1), Side::Far, "centre at 400.5");
    }

    #[test]
    fn a_tie_in_the_very_middle_goes_to_the_top_or_bottom() {
        // 100 px from all four edges of the window, so no edge is closer.
        let area = Rect::new(200, 300, 700, 400);
        assert_eq!(Anchor::of(area, WINDOW).sides(), (Side::Centre, Side::Near));
        // One pixel closer to the left edge than to the top, and the left
        // wins: the tie rule decides ties and nothing else.
        let nearer_left = Rect::new(199, 300, 702, 400);
        assert_eq!(
            Anchor::of(nearer_left, WINDOW).sides(),
            (Side::Near, Side::Centre)
        );
    }

    #[test]
    fn a_centred_area_rounds_the_same_way_left_of_the_primary_monitor() {
        // Centred on the top edge of a window whose width then becomes odd,
        // so the centre falls on a half pixel. The area goes to the pixel on
        // the left of it, on a monitor at negative coordinates exactly as on
        // one at positive coordinates. Rounding toward zero would put it one
        // pixel further right on the negative side only.
        let odd = |x: i32| {
            let window = Rect::new(x, 0, 900, 600);
            let area = Rect::new(x + 400, 10, 100, 30);
            let placed = Anchor::of(area, window).place(Rect::new(x, 0, 901, 600), 1.0);
            placed.origin.x - x
        };
        assert_eq!(odd(1000), 400);
        assert_eq!(odd(-1000), 400);
        assert_eq!(odd(-450), 400, "a window that straddles zero");
    }

    #[test]
    fn the_very_middle_takes_the_edge_its_border_is_closest_to() {
        // Middle third both ways, and its right border 40 px from the right
        // edge while top and bottom are 150 px away.
        let wide = Rect::new(100 + 320, 200 + 150, 540, 300);
        assert_eq!(Anchor::of(wide, WINDOW).sides(), (Side::Far, Side::Centre));
        // The same shape turned: its bottom border is the close one.
        let tall = Rect::new(100 + 350, 200 + 160, 200, 420);
        assert_eq!(Anchor::of(tall, WINDOW).sides(), (Side::Centre, Side::Far));
    }

    #[test]
    fn a_resize_keeps_the_distance_from_the_anchored_corner() {
        let grown = Rect::new(100, 200, 1300, 900);
        // Top left: nothing moves when the right and bottom edges do.
        let top_left = Rect::new(120, 230, 80, 40);
        assert_eq!(Anchor::of(top_left, WINDOW).place(grown, 1.0), top_left);
        // Bottom right: 20 px from the right edge and 30 px from the bottom,
        // before and after.
        let bottom_right = Rect::new(100 + 900 - 20 - 80, 200 + 600 - 30 - 40, 80, 40);
        let placed = Anchor::of(bottom_right, WINDOW).place(grown, 1.0);
        assert_eq!(
            placed,
            Rect::new(100 + 1300 - 20 - 80, 200 + 900 - 30 - 40, 80, 40)
        );
    }

    #[test]
    fn an_area_on_the_top_edge_stays_centred_on_it() {
        // Centred across, 10 px under the top edge.
        let area = Rect::new(100 + 450 - 50, 210, 100, 30);
        let anchor = Anchor::of(area, WINDOW);
        assert_eq!(anchor.sides(), (Side::Centre, Side::Near));
        let grown = Rect::new(100, 200, 1300, 900);
        assert_eq!(
            anchor.place(grown, 1.0),
            Rect::new(100 + 650 - 50, 210, 100, 30)
        );
    }

    #[test]
    fn an_area_beside_the_window_is_anchored_with_a_negative_distance() {
        // 50 px left of the window and 20 px above it.
        let area = Rect::new(10, 150, 40, 30);
        let anchor = Anchor::of(area, WINDOW);
        assert_eq!(anchor.sides(), (Side::Near, Side::Near));
        let moved = Rect::new(-2000, 40, 900, 600);
        assert_eq!(anchor.place(moved, 1.0), Rect::new(-2090, -10, 40, 30));
    }

    #[test]
    fn another_scale_moves_and_sizes_the_area_with_the_window_contents() {
        // 40 px in and 60 px down on a 100 % monitor. On a 150 % monitor the
        // same button is 60 px in, 90 px down and half again as large.
        let area = Rect::new(140, 260, 200, 80);
        let anchor = Anchor::of(area, WINDOW);
        let scaled_window = Rect::new(3000, 0, 1350, 900);
        assert_eq!(
            anchor.place(scaled_window, 1.5),
            Rect::new(3060, 90, 300, 120)
        );
    }

    #[test]
    fn a_scaled_distance_rounds_to_the_nearest_pixel() {
        // 10 px in and 10 px down, 30 x 30, moved to a monitor at 125 %. The
        // distances become 12.5 and the sides 37.5: halves, which round up.
        // Every other scaled case in this file is an exact multiple, so
        // rounding down passed all of them.
        let area = Rect::new(110, 210, 30, 30);
        let placed = Anchor::of(area, WINDOW).place(WINDOW, 1.25);
        assert_eq!(placed, Rect::new(113, 213, 38, 38));
        // And 0.9: 9.0 and 27.0 exactly, then 0.84: 8.4 rounds down, 25.2 too.
        let placed = Anchor::of(area, WINDOW).place(WINDOW, 0.84);
        assert_eq!(placed, Rect::new(108, 208, 25, 25));
    }

    #[test]
    fn a_factor_that_is_not_a_size_is_read_as_one() {
        let area = Rect::new(140, 260, 200, 80);
        let anchor = Anchor::of(area, WINDOW);
        for factor in [0.0, -2.0, f64::NAN, f64::INFINITY] {
            assert_eq!(anchor.place(WINDOW, factor), area, "factor {factor}");
        }
    }

    #[test]
    fn a_window_with_no_size_still_yields_an_area() {
        let nothing = Rect::new(500, 500, 0, 0);
        let area = Rect::new(480, 470, 40, 30);
        let anchor = Anchor::of(area, nothing);
        assert_eq!(anchor.place(nothing, 1.0), area);
        // A tiny factor cannot make the area vanish.
        let placed = anchor.place(nothing, 0.000_1);
        assert_eq!((placed.size.width, placed.size.height), (1, 1));
    }

    fn doubled_centre_x(rect: Rect) -> i64 {
        2 * i64::from(rect.origin.x) + i64::from(rect.size.width)
    }

    fn doubled_centre_y(rect: Rect) -> i64 {
        2 * i64::from(rect.origin.y) + i64::from(rect.size.height)
    }

    fn rect() -> impl Strategy<Value = Rect> {
        (
            -20_000i32..20_000,
            -20_000i32..20_000,
            0u32..8_000,
            0u32..8_000,
        )
            .prop_map(|(x, y, width, height)| Rect::new(x, y, width, height))
    }

    fn area() -> impl Strategy<Value = Rect> {
        (
            -20_000i32..20_000,
            -20_000i32..20_000,
            1u32..8_000,
            1u32..8_000,
        )
            .prop_map(|(x, y, width, height)| Rect::new(x, y, width, height))
    }

    proptest! {
        /// The anchor of an area, asked about the same window, is the area.
        #[test]
        fn the_same_window_gives_the_same_area(area in area(), window in rect()) {
            prop_assert_eq!(Anchor::of(area, window).place(window, 1.0), area);
        }

        /// Moving the window moves the area by exactly as much, whatever the
        /// anchor. This is the whole of following a drag.
        #[test]
        fn a_moved_window_carries_the_area_with_it(
            area in area(),
            window in rect(),
            dx in -5_000i32..5_000,
            dy in -5_000i32..5_000,
        ) {
            let moved = Rect::new(
                window.origin.x + dx,
                window.origin.y + dy,
                window.size.width,
                window.size.height,
            );
            let expected = Rect::new(
                area.origin.x + dx,
                area.origin.y + dy,
                area.size.width,
                area.size.height,
            );
            prop_assert_eq!(Anchor::of(area, window).place(moved, 1.0), expected);
        }

        /// A resize never changes the area's size, and a window resized and
        /// put back puts the area back.
        #[test]
        fn a_resize_keeps_the_size_and_is_undone_by_its_opposite(
            area in area(),
            window in rect(),
            width in 0u32..8_000,
            height in 0u32..8_000,
        ) {
            let anchor = Anchor::of(area, window);
            let resized = Rect::new(window.origin.x, window.origin.y, width, height);
            prop_assert_eq!(anchor.place(resized, 1.0).size, area.size);
            prop_assert_eq!(anchor.place(window, 1.0), area);
        }

        /// A side anchor keeps its exact distance from its own edge.
        #[test]
        fn a_side_anchor_keeps_its_distance_from_its_edge(
            area in area(),
            window in rect(),
            width in 0u32..8_000,
            height in 0u32..8_000,
        ) {
            let anchor = Anchor::of(area, window);
            let resized = Rect::new(window.origin.x, window.origin.y, width, height);
            let placed = anchor.place(resized, 1.0);
            match anchor.sides().0 {
                Side::Near => prop_assert_eq!(
                    placed.origin.x - resized.origin.x,
                    area.origin.x - window.origin.x
                ),
                Side::Far => prop_assert_eq!(
                    resized.right() - placed.right(),
                    window.right() - area.right()
                ),
                // A centred area keeps its distance from the middle to within
                // the half pixel a whole-pixel position can be off by, and
                // always to the same side.
                Side::Centre => {
                    let before = doubled_centre_x(area) - doubled_centre_x(window);
                    let after = doubled_centre_x(placed) - doubled_centre_x(resized);
                    prop_assert!(before - after == 0 || before - after == 1, "{before} {after}");
                }
            }
            match anchor.sides().1 {
                Side::Near => prop_assert_eq!(
                    placed.origin.y - resized.origin.y,
                    area.origin.y - window.origin.y
                ),
                Side::Far => prop_assert_eq!(
                    resized.bottom() - placed.bottom(),
                    window.bottom() - area.bottom()
                ),
                Side::Centre => {
                    let before = doubled_centre_y(area) - doubled_centre_y(window);
                    let after = doubled_centre_y(placed) - doubled_centre_y(resized);
                    prop_assert!(before - after == 0 || before - after == 1, "{before} {after}");
                }
            }
        }

        /// Where the window is on the desktop never changes where the area
        /// sits on it, for a resized window too. This is what holds a centred
        /// area to one rounding on both sides of zero.
        #[test]
        fn a_resized_window_places_the_area_the_same_wherever_it_is(
            area in area(),
            window in rect(),
            width in 0u32..8_000,
            height in 0u32..8_000,
            dx in -30_000i32..30_000,
            dy in -30_000i32..30_000,
        ) {
            let anchor = Anchor::of(area, window);
            let here = Rect::new(window.origin.x, window.origin.y, width, height);
            let there = Rect::new(window.origin.x + dx, window.origin.y + dy, width, height);
            let placed_here = anchor.place(here, 1.0);
            let placed_there = anchor.place(there, 1.0);
            prop_assert_eq!(placed_there.origin.x - placed_here.origin.x, dx);
            prop_assert_eq!(placed_there.origin.y - placed_here.origin.y, dy);
            prop_assert_eq!(placed_there.size, placed_here.size);
        }

        /// No window and no factor can produce an empty area, which the store
        /// would refuse and the area would then stop following.
        #[test]
        fn the_area_is_never_empty(
            area in area(),
            window in rect(),
            later in rect(),
            factor in 0.01f64..16.0,
        ) {
            let placed = Anchor::of(area, window).place(later, factor);
            prop_assert!(!placed.size.is_empty());
        }
    }
}
