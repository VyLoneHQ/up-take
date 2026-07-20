//! Geometry in **physical pixels, virtual-desktop space**.
//!
//! Every type in this module obeys the crate-level rule: coordinates are
//! physical device pixels positioned in the Windows virtual desktop, where a
//! monitor left of (or above) the primary has negative coordinates. CSS/logical
//! coordinates from the WebView enter this space through [`css_to_physical`]
//! and leave it through [`physical_to_css`] — nowhere else.
//!
//! Arithmetic policy: rectangle edges are computed in `i64` so that
//! `origin + size` can never overflow, and spans that would not fit in `u32`
//! saturate rather than panic. Real virtual desktops sit well inside these
//! limits (Windows caps the virtual screen far below ±2³¹), so saturation is a
//! defensive posture, not an expected code path.

use serde::{Deserialize, Serialize};

/// A point in physical pixels, virtual-desktop space.
///
/// Coordinates can be negative: a monitor positioned left of the primary
/// starts at `x < 0`, one above it at `y < 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Point {
    /// Horizontal position in physical pixels.
    pub x: i32,
    /// Vertical position in physical pixels.
    pub y: i32,
}

impl Point {
    /// Creates a point at `(x, y)`.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A size in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
}

impl Size {
    /// Creates a size of `width × height`.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// True when either dimension is zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// An axis-aligned rectangle in physical pixels, virtual-desktop space.
///
/// Edges are half-open: the rectangle contains its origin but not its right or
/// bottom edge, so two monitors that share an edge do not "contain" the same
/// pixel column twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    /// Top-left corner.
    pub origin: Point,
    /// Extent to the right of and below the origin.
    pub size: Size,
}

impl Rect {
    /// Creates a rectangle from its top-left corner and size.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            origin: Point::new(x, y),
            size: Size::new(width, height),
        }
    }

    /// Right edge (exclusive). `i64` because `origin.x + width` can exceed
    /// `i32::MAX`.
    #[must_use]
    pub fn right(self) -> i64 {
        i64::from(self.origin.x) + i64::from(self.size.width)
    }

    /// Bottom edge (exclusive). `i64` for the same reason as [`Rect::right`].
    #[must_use]
    pub fn bottom(self) -> i64 {
        i64::from(self.origin.y) + i64::from(self.size.height)
    }

    /// The rectangle spanned by two corner points, in either order.
    ///
    /// This is the normalization a drag selection goes through: dragging from
    /// bottom-right to top-left yields the same rectangle as the reverse drag.
    /// A span wider than `u32::MAX` saturates (see module docs).
    #[must_use]
    pub fn from_corner_points(a: Point, b: Point) -> Self {
        Self {
            origin: Point::new(a.x.min(b.x), a.y.min(b.y)),
            size: Size::new(
                saturating_span(i64::from(a.x), i64::from(b.x)),
                saturating_span(i64::from(a.y), i64::from(b.y)),
            ),
        }
    }

    /// True when `point` lies inside the rectangle (half-open edges).
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.origin.x
            && i64::from(point.x) < self.right()
            && point.y >= self.origin.y
            && i64::from(point.y) < self.bottom()
    }

    /// The smallest rectangle containing both `self` and `other`.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        let x = self.origin.x.min(other.origin.x);
        let y = self.origin.y.min(other.origin.y);
        Self {
            origin: Point::new(x, y),
            size: Size::new(
                saturating_len(i64::from(x), self.right().max(other.right())),
                saturating_len(i64::from(y), self.bottom().max(other.bottom())),
            ),
        }
    }

    /// The overlap of `self` and `other`, or `None` when they do not overlap.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.origin.x.max(other.origin.x);
        let y = self.origin.y.max(other.origin.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if i64::from(x) >= right || i64::from(y) >= bottom {
            return None;
        }
        Some(Self {
            origin: Point::new(x, y),
            size: Size::new(
                saturating_len(i64::from(x), right),
                saturating_len(i64::from(y), bottom),
            ),
        })
    }
}

// A per-monitor type carrying `scale_factor` alongside `bounds` will be needed
// by task 1.3 (per-monitor-DPI correctness). It is deliberately absent until
// then: nothing constructs it today, and an unused type invites callers to
// reach for a scale factor outside the single sanctioned conversion below.

/// The bounding rectangle of the whole virtual desktop: the union of all
/// monitor bounds. `None` when no monitor is reported — callers must surface
/// that to the user rather than guessing a rectangle.
pub fn virtual_desktop_bounds<I>(monitor_bounds: I) -> Option<Rect>
where
    I: IntoIterator<Item = Rect>,
{
    monitor_bounds.into_iter().reduce(Rect::union)
}

/// Converts a WebView CSS/logical coordinate to physical pixels in
/// virtual-desktop space.
///
/// **This is the single sanctioned conversion point** (architecture §3.1):
/// every CSS coordinate crossing the IPC boundary goes through here, and no
/// other code multiplies by a scale factor. `window_origin` is the overlay
/// window's top-left in physical virtual-desktop coordinates; `scale_factor`
/// is the window's current scale factor as reported by the OS.
///
/// Results are clamped to the `i32` range. Infinities clamp to the range ends;
/// a `NaN` input (which cannot be clamped meaningfully) yields `window_origin`,
/// i.e. a zero offset. Neither panics.
#[must_use]
pub fn css_to_physical(css_x: f64, css_y: f64, scale_factor: f64, window_origin: Point) -> Point {
    Point::new(
        add_scaled(window_origin.x, css_x, scale_factor),
        add_scaled(window_origin.y, css_y, scale_factor),
    )
}

/// Inverse of [`css_to_physical`]: physical virtual-desktop pixels to WebView
/// CSS coordinates. Same boundary, opposite direction — used when the backend
/// hands geometry (e.g. monitor rectangles) to the frontend for rendering.
#[must_use]
pub fn physical_to_css(point: Point, scale_factor: f64, window_origin: Point) -> (f64, f64) {
    (
        (f64::from(point.x) - f64::from(window_origin.x)) / scale_factor,
        (f64::from(point.y) - f64::from(window_origin.y)) / scale_factor,
    )
}

/// `origin + round(css × scale)`, clamped into `i32`.
///
/// `NaN` is handled before the clamp because `f64::clamp` propagates it, and
/// `NaN as i32` saturates to `0` — which would silently relocate the point to
/// the virtual-desktop origin rather than leaving it at `origin`.
#[allow(clippy::cast_possible_truncation)] // truncation is impossible after the clamp
fn add_scaled(origin: i32, css: f64, scale: f64) -> i32 {
    let value = f64::from(origin) + (css * scale).round();
    if value.is_nan() {
        return origin;
    }
    value.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Absolute distance between two coordinates, saturated to `u32`.
fn saturating_span(a: i64, b: i64) -> u32 {
    u32::try_from((a - b).abs()).unwrap_or(u32::MAX)
}

/// Length from `start` to `end_exclusive`, floored at 0 and saturated to `u32`.
fn saturating_len(start: i64, end_exclusive: i64) -> u32 {
    u32::try_from((end_exclusive - start).max(0)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use proptest::prelude::*;

    // --- Concrete cases for the two scenarios that actually fail in the wild
    //     (quality-bars.md §3: M-3 mixed DPI, M-4 monitor left of primary). ---

    #[test]
    fn m4_monitor_left_of_primary_yields_negative_origin() {
        let left = Rect::new(-2560, 0, 2560, 1440);
        let primary = Rect::new(0, 0, 3840, 2160);
        let bounds = virtual_desktop_bounds([left, primary]).unwrap();
        assert_eq!(bounds, Rect::new(-2560, 0, 6400, 2160));
        assert!(bounds.contains(Point::new(-2560, 0)));
        assert!(bounds.contains(Point::new(-1, 100)));
        assert!(!bounds.contains(Point::new(3840, 0)));
    }

    #[test]
    fn m3_css_conversion_with_scaled_window_and_negative_origin() {
        // Overlay origin on a monitor left of the primary; the window's scale
        // factor is 1.5 (Windows assigns the DPI of the monitor with the
        // largest window overlap).
        let origin = Point::new(-2560, 0);
        assert_eq!(
            css_to_physical(100.0, 50.0, 1.5, origin),
            Point::new(-2410, 75)
        );
        // CSS (0, 0) is exactly the window origin at any scale.
        assert_eq!(css_to_physical(0.0, 0.0, 1.5, origin), origin);
    }

    #[test]
    fn non_finite_conversion_inputs_degrade_predictably() {
        let origin = Point::new(-2560, 40);
        // A NaN axis yields a zero offset on *that axis only*, not a jump to
        // (0, 0) — `NaN as i32` saturates to 0 and would do exactly that.
        assert_eq!(
            css_to_physical(f64::NAN, 10.0, 1.5, origin),
            Point::new(-2560, 55)
        );
        // A NaN scale factor poisons both axes, so both hold at the origin.
        assert_eq!(css_to_physical(10.0, 10.0, f64::NAN, origin), origin);
        // Infinities clamp to the range ends rather than wrapping.
        assert_eq!(
            css_to_physical(f64::INFINITY, f64::NEG_INFINITY, 1.0, origin),
            Point::new(i32::MAX, i32::MIN)
        );
    }

    #[test]
    fn monitors_above_the_primary_extend_bounds_upward() {
        let above = Rect::new(0, -1080, 1920, 1080);
        let primary = Rect::new(0, 0, 1920, 1080);
        assert_eq!(
            virtual_desktop_bounds([above, primary]).unwrap(),
            Rect::new(0, -1080, 1920, 2160)
        );
    }

    #[test]
    fn empty_monitor_list_has_no_bounds() {
        assert_eq!(virtual_desktop_bounds([]), None);
    }

    #[test]
    fn adjacent_monitors_do_not_overlap() {
        let a = Rect::new(0, 0, 1920, 1080);
        let b = Rect::new(1920, 0, 1920, 1080);
        assert_eq!(a.intersection(b), None);
    }

    // --- Property tests. `WIN` bounds coordinates to a realistic Windows
    //     virtual-screen range; exact-value properties use it. No-panic
    //     properties run on the full i32 range. ---

    const WIN: i32 = 32_768;

    fn win_point() -> impl Strategy<Value = Point> {
        (-WIN..WIN, -WIN..WIN).prop_map(|(x, y)| Point::new(x, y))
    }

    fn win_rect() -> impl Strategy<Value = Rect> {
        (-WIN..WIN, -WIN..WIN, 1u32..8192, 1u32..8192)
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    proptest! {
        #[test]
        fn corner_points_are_order_independent(a in win_point(), b in win_point()) {
            prop_assert_eq!(
                Rect::from_corner_points(a, b),
                Rect::from_corner_points(b, a)
            );
        }

        #[test]
        fn corner_rect_origin_is_componentwise_min(a in win_point(), b in win_point()) {
            let r = Rect::from_corner_points(a, b);
            prop_assert_eq!(r.origin, Point::new(a.x.min(b.x), a.y.min(b.y)));
            prop_assert_eq!(r.right(), i64::from(a.x.max(b.x)));
            prop_assert_eq!(r.bottom(), i64::from(a.y.max(b.y)));
        }

        #[test]
        fn union_contains_both_rects(a in win_rect(), b in win_rect()) {
            let u = a.union(b);
            prop_assert!(u.contains(a.origin));
            prop_assert!(u.contains(b.origin));
            prop_assert!(u.right() >= a.right().max(b.right()));
            prop_assert!(u.bottom() >= a.bottom().max(b.bottom()));
        }

        #[test]
        fn intersection_lies_within_both(a in win_rect(), b in win_rect()) {
            if let Some(i) = a.intersection(b) {
                prop_assert!(a.contains(i.origin) && b.contains(i.origin));
                prop_assert!(i.right() <= a.right().min(b.right()));
                prop_assert!(i.bottom() <= a.bottom().min(b.bottom()));
                prop_assert!(!i.size.is_empty());
            }
        }

        #[test]
        fn intersection_is_commutative(a in win_rect(), b in win_rect()) {
            prop_assert_eq!(a.intersection(b), b.intersection(a));
        }

        #[test]
        fn css_round_trip_is_within_one_pixel(
            p in win_point(),
            origin in win_point(),
            scale in 0.5f64..4.0,
        ) {
            let (cx, cy) = physical_to_css(p, scale, origin);
            let back = css_to_physical(cx, cy, scale, origin);
            prop_assert!((i64::from(back.x) - i64::from(p.x)).abs() <= 1);
            prop_assert!((i64::from(back.y) - i64::from(p.y)).abs() <= 1);
        }

        #[test]
        fn no_panic_on_extreme_inputs(
            ax: i32, ay: i32, bx: i32, by: i32,
            css in proptest::num::f64::ANY,
            scale in proptest::num::f64::ANY,
        ) {
            let a = Point::new(ax, ay);
            let b = Point::new(bx, by);
            let r = Rect::from_corner_points(a, b);
            let _ = r.union(r);
            let _ = r.intersection(r);
            let _ = r.contains(b);
            let _ = css_to_physical(css, css, scale, a);
        }
    }
}
