//! What part of an area a pointer grabs, and how dragging that part moves the
//! area's bounds (roadmap task 1.6).
//!
//! This is the geometry half of area lifecycle — deliberately pure, so the
//! rules that decide "you grabbed the south-east corner, not the body" are unit
//! tested rather than discovered on a rig. The Win32 half (which button, when,
//! and swallowing it) lives in `placement`, and the two meet at
//! [`handle_at`].
//!
//! # Why an area's own chrome is hit-tested here rather than in the WebView
//!
//! The overlay is **click-through whenever it is visible** (ADR-0014), so the
//! WebView never receives a mouse event and a close button rendered as a DOM
//! element could never be clicked. Every control an area appears to have is
//! therefore a *rectangle Rust knows about*, hit-tested against the coordinates
//! the placement mouse hook reports; the WebView only draws it. Anything that
//! looks interactive on screen must have a match here or it is decoration
//! pretending to be a control.
//!
//! # Sizes are physical pixels, and adapt to small areas
//!
//! The bands below are physical device pixels, which is the space the hook
//! reports in. They are also *capped by the area's own size*: a fixed 8 px
//! resize band around a 20 px-tall area would leave no body to grab it by, so
//! every band shrinks with the area rather than swallowing it. This is why the
//! spans are functions and not constants.

use crate::geometry::{Point, Rect};

/// The smallest an area may be on either axis, in physical pixels.
///
/// This is the "minimum size policy" the area model deferred to task 1.6. It
/// exists for one concrete reason: **an area the user cannot grab is an area
/// the user cannot dismiss.** Below roughly this span there is no room for a
/// close control, a resize band and a draggable body at once, so a smaller area
/// would be a permanent fixture of the screen — the same failure the
/// empty-rectangle rejection in `AreaStore::create` prevents, only reached by a
/// slightly longer drag.
///
/// A resize clamps to this rather than refusing, so the area stops shrinking
/// under the cursor instead of the drag appearing to break.
pub const MIN_AREA_SPAN: u32 = 24;

/// How wide the grab band along an area's edge is, before adapting to size.
const RESIZE_BAND: u32 = 8;

/// How large the close control is, before adapting to size.
const CLOSE_SPAN: u32 = 18;

/// Which part of an area a point falls on.
///
/// Order of precedence is decided in [`handle_at`], not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Handle {
    /// The close control: dismisses the area (PRODUCT-VISION §4.3 — closing one
    /// area is a deliberate gesture with its own control, never `Esc`).
    Close,
    /// An edge or corner: drag to resize from that side.
    Resize(Resize),
    /// Anywhere else inside: drag to move the whole area.
    Body,
}

/// Which edge or corner a resize is anchored to. The *other* side stays put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resize {
    /// Top edge.
    North,
    /// Bottom edge.
    South,
    /// Right edge.
    East,
    /// Left edge.
    West,
    /// Top-right corner.
    NorthEast,
    /// Top-left corner.
    NorthWest,
    /// Bottom-right corner.
    SouthEast,
    /// Bottom-left corner.
    SouthWest,
}

impl Resize {
    /// Whether this resize moves the given edge.
    const fn moves_north(self) -> bool {
        matches!(self, Self::North | Self::NorthEast | Self::NorthWest)
    }
    const fn moves_south(self) -> bool {
        matches!(self, Self::South | Self::SouthEast | Self::SouthWest)
    }
    const fn moves_west(self) -> bool {
        matches!(self, Self::West | Self::NorthWest | Self::SouthWest)
    }
    const fn moves_east(self) -> bool {
        matches!(self, Self::East | Self::NorthEast | Self::SouthEast)
    }
}

/// The close control's rectangle: a square inside the area's top-right corner.
///
/// Shrinks with the area so it is never larger than a quarter of it — a control
/// that covers the thing it belongs to is not a control.
#[must_use]
pub fn close_control(bounds: Rect) -> Rect {
    let span = CLOSE_SPAN
        .min(bounds.size.width / 2)
        .min(bounds.size.height / 2)
        .max(1);
    Rect::new(
        // `right()` is exclusive, so the control's left edge is `span` back from
        // it. i64 throughout: an area can sit at a negative virtual-desktop
        // coordinate, and its right edge can exceed i32 only after saturation.
        clamp_to_i32(bounds.right() - i64::from(span)),
        bounds.origin.y,
        span,
        span,
    )
}

/// Which part of `bounds` the point grabs, or `None` if the point is outside
/// the area entirely.
///
/// # Precedence, and the one affordance it costs
///
/// The close control wins over the north-east resize corner they share. Losing
/// one of eight resize handles to an 18 px square is the cheaper trade: the
/// other seven still resize the same rectangle, while a close control that
/// competed with a resize band would be a dismiss gesture that sometimes
/// silently resizes instead — and dismissing is the gesture with no undo.
#[must_use]
pub fn handle_at(bounds: Rect, point: Point) -> Option<Handle> {
    if !bounds.contains(point) {
        return None;
    }
    if close_control(bounds).contains(point) {
        return Some(Handle::Close);
    }
    let band = i64::from(
        RESIZE_BAND
            .min(bounds.size.width / 3)
            .min(bounds.size.height / 3)
            .max(1),
    );
    let (x, y) = (i64::from(point.x), i64::from(point.y));
    let north = y - i64::from(bounds.origin.y) < band;
    let west = x - i64::from(bounds.origin.x) < band;
    // `right()`/`bottom()` are exclusive, so the last pixel *in* the area is one
    // less: without the `- 1` the band along the far edges would be one pixel
    // narrower than the band along the near ones.
    let south = bounds.bottom() - 1 - y < band;
    let east = bounds.right() - 1 - x < band;
    let resize = match (north, south, east, west) {
        // A corner takes precedence over either edge that forms it: at the
        // meeting point the user means the corner.
        (true, _, true, _) => Some(Resize::NorthEast),
        (true, _, _, true) => Some(Resize::NorthWest),
        (_, true, true, _) => Some(Resize::SouthEast),
        (_, true, _, true) => Some(Resize::SouthWest),
        (true, _, _, _) => Some(Resize::North),
        (_, true, _, _) => Some(Resize::South),
        (_, _, true, _) => Some(Resize::East),
        (_, _, _, true) => Some(Resize::West),
        _ => None,
    };
    Some(resize.map_or(Handle::Body, Handle::Resize))
}

/// Moves an area by a delta, keeping its size exactly.
///
/// Nothing constrains the result to the visible desktop: an area may legitimately
/// sit on a monitor at negative coordinates, and clamping to the current
/// arrangement would fight the display-change handling that already re-fits the
/// overlay when monitors move.
#[must_use]
pub fn move_by(bounds: Rect, dx: i32, dy: i32) -> Rect {
    Rect::new(
        clamp_to_i32(i64::from(bounds.origin.x) + i64::from(dx)),
        clamp_to_i32(i64::from(bounds.origin.y) + i64::from(dy)),
        bounds.size.width,
        bounds.size.height,
    )
}

/// Applies a resize drag of `(dx, dy)` to `bounds`, moving only the edges the
/// [`Resize`] names.
///
/// The opposite edges are fixed, so dragging the west edge right *shrinks* the
/// area rather than moving it. Both axes clamp at [`MIN_AREA_SPAN`]: pushing an
/// edge past its opposite pins it there instead of inverting the rectangle,
/// which is what makes a fast drag through the far edge harmless.
#[must_use]
pub fn resize_by(bounds: Rect, resize: Resize, dx: i32, dy: i32) -> Rect {
    let min = i64::from(MIN_AREA_SPAN);
    let (mut left, mut top) = (i64::from(bounds.origin.x), i64::from(bounds.origin.y));
    let (mut right, mut bottom) = (bounds.right(), bounds.bottom());
    let (dx, dy) = (i64::from(dx), i64::from(dy));

    if resize.moves_west() {
        left = (left + dx).min(right - min);
    }
    if resize.moves_east() {
        right = (right + dx).max(left + min);
    }
    if resize.moves_north() {
        top = (top + dy).min(bottom - min);
    }
    if resize.moves_south() {
        bottom = (bottom + dy).max(top + min);
    }
    Rect::new(
        clamp_to_i32(left),
        clamp_to_i32(top),
        clamp_to_u32(right - left),
        clamp_to_u32(bottom - top),
    )
}

/// Whether a freshly dragged rectangle is big enough to become an area.
///
/// A drag shorter than [`MIN_AREA_SPAN`] on either axis reads as a click or a
/// slip of the hand, not as an intent to claim a sliver of screen — and the
/// sliver it would produce is one the user could not grab to remove. Paired with
/// `AreaStore::create`'s empty-rectangle rejection: this is the *policy*, that
/// is the *invariant*.
#[must_use]
pub fn is_placeable(bounds: Rect) -> bool {
    bounds.size.width >= MIN_AREA_SPAN && bounds.size.height >= MIN_AREA_SPAN
}

/// How close to a monitor edge an area's own edge must come before it snaps
/// flush, in physical pixels.
///
/// Small enough that deliberate placement a few pixels off an edge is still
/// possible, large enough that "put it against the edge" needs no precision.
pub const SNAP_DISTANCE: u32 = 12;

/// How much of an area must stay on a monitor, in physical pixels per axis.
pub const MIN_VISIBLE_SPAN: u32 = 48;

/// The monitor an area belongs to: the one it overlaps most, or — when it
/// overlaps none, which is what happens in the gap between mismatched monitors —
/// the one whose centre is nearest.
///
/// Never `None` for a non-empty monitor list, because every caller here needs
/// *some* monitor to reason against and "no answer" would mean leaving an area
/// wherever it landed.
fn host_monitor(bounds: Rect, monitors: &[Rect]) -> Option<Rect> {
    let overlapping = monitors
        .iter()
        .filter_map(|monitor| {
            let overlap = bounds.intersection(*monitor)?;
            Some((
                u64::from(overlap.size.width) * u64::from(overlap.size.height),
                *monitor,
            ))
        })
        .max_by_key(|(area, _)| *area);
    if let Some((_, monitor)) = overlapping {
        return Some(monitor);
    }
    monitors
        .iter()
        .min_by_key(|monitor| centre_distance_squared(bounds, **monitor))
        .copied()
}

/// Squared distance between two rectangles' centres, in i128 so that two
/// far-apart virtual-desktop coordinates cannot overflow the comparison.
fn centre_distance_squared(a: Rect, b: Rect) -> i128 {
    let ax = i128::from(a.origin.x) * 2 + i128::from(a.size.width);
    let ay = i128::from(a.origin.y) * 2 + i128::from(a.size.height);
    let bx = i128::from(b.origin.x) * 2 + i128::from(b.size.width);
    let by = i128::from(b.origin.y) * 2 + i128::from(b.size.height);
    (ax - bx).pow(2) + (ay - by).pow(2)
}

/// Snaps a moved area flush to its monitor's edges when it comes within
/// [`SNAP_DISTANCE`], preserving its size exactly.
///
/// Each axis snaps independently and to at most one edge — the nearer of the
/// two, so an area narrower than the monitor cannot be pulled toward both.
#[must_use]
pub fn snap_move(bounds: Rect, monitors: &[Rect]) -> Rect {
    let Some(monitor) = host_monitor(bounds, monitors) else {
        return bounds;
    };
    let snap = i64::from(SNAP_DISTANCE);
    let dx = nearest_snap(
        i64::from(bounds.origin.x) - i64::from(monitor.origin.x),
        bounds.right() - monitor.right(),
        snap,
    );
    let dy = nearest_snap(
        i64::from(bounds.origin.y) - i64::from(monitor.origin.y),
        bounds.bottom() - monitor.bottom(),
        snap,
    );
    Rect::new(
        clamp_to_i32(i64::from(bounds.origin.x) - dx),
        clamp_to_i32(i64::from(bounds.origin.y) - dy),
        bounds.size.width,
        bounds.size.height,
    )
}

/// The smaller of two edge offsets, if either is within `snap`; `0` otherwise.
fn nearest_snap(near: i64, far: i64, snap: i64) -> i64 {
    let near_hit = near.abs() <= snap;
    let far_hit = far.abs() <= snap;
    match (near_hit, far_hit) {
        (true, true) => {
            if near.abs() <= far.abs() {
                near
            } else {
                far
            }
        }
        (true, false) => near,
        (false, true) => far,
        (false, false) => 0,
    }
}

/// Snaps the edges a resize actually moved flush to the monitor, leaving the
/// fixed edges alone.
///
/// A snap that would take the area below [`MIN_AREA_SPAN`] is dropped rather
/// than clamped: clamping would move the edge somewhere the user did not drag it
/// *and* somewhere it is not flush, which is the worst of both.
#[must_use]
pub fn snap_resize(bounds: Rect, resize: Resize, monitors: &[Rect]) -> Rect {
    let Some(monitor) = host_monitor(bounds, monitors) else {
        return bounds;
    };
    let snap = i64::from(SNAP_DISTANCE);
    let (mut left, mut top) = (i64::from(bounds.origin.x), i64::from(bounds.origin.y));
    let (mut right, mut bottom) = (bounds.right(), bounds.bottom());

    if resize.moves_west() && (left - i64::from(monitor.origin.x)).abs() <= snap {
        left = i64::from(monitor.origin.x);
    }
    if resize.moves_east() && (right - monitor.right()).abs() <= snap {
        right = monitor.right();
    }
    if resize.moves_north() && (top - i64::from(monitor.origin.y)).abs() <= snap {
        top = i64::from(monitor.origin.y);
    }
    if resize.moves_south() && (bottom - monitor.bottom()).abs() <= snap {
        bottom = monitor.bottom();
    }
    let min = i64::from(MIN_AREA_SPAN);
    if right - left < min || bottom - top < min {
        return bounds;
    }
    Rect::new(
        clamp_to_i32(left),
        clamp_to_i32(top),
        clamp_to_u32(right - left),
        clamp_to_u32(bottom - top),
    )
}

/// Pushes an area back until it is reachable again, and returns it unchanged if
/// it already is.
///
/// # The guarantee
///
/// **An area can never be left somewhere the user cannot get to it.** An area
/// dragged past the edge of the screen, or into the gap between two mismatched
/// monitors, is not merely awkward: it still costs memory and compositing, it
/// cannot be moved back, and it cannot be dismissed — so it is permanent for the
/// session. That is a defect, not a rough edge, which is why this is enforced on
/// every commit rather than offered as a snapping nicety.
///
/// Two conditions, both against the area's host monitor:
///
/// 1. **The close control is fully on the monitor.** This is the load-bearing
///    one: it is what makes "always dismissable" true, and it implies the top
///    edge is on screen, which is also what Windows guarantees for a title bar.
/// 2. **At least [`MIN_VISIBLE_SPAN`] of the area is on the monitor on each
///    axis** (or all of it, for an area smaller than that). Condition 1 alone
///    would allow an area reduced to a sliver of its right-hand edge — reachable
///    in principle, useless in practice.
///
/// The area is only ever *translated*, never resized: a resize would silently
/// change something the user set.
#[must_use]
pub fn contain(bounds: Rect, monitors: &[Rect]) -> Rect {
    let Some(monitor) = host_monitor(bounds, monitors) else {
        return bounds;
    };
    // Visible-span first, then the close control, so that where the two
    // disagree — an area larger than the monitor — dismissability wins.
    let visible_x = i64::from(MIN_VISIBLE_SPAN.min(bounds.size.width));
    let visible_y = i64::from(MIN_VISIBLE_SPAN.min(bounds.size.height));
    let mut dx = axis_push(
        i64::from(bounds.origin.x),
        bounds.right(),
        i64::from(monitor.origin.x),
        monitor.right(),
        visible_x,
    );
    let mut dy = axis_push(
        i64::from(bounds.origin.y),
        bounds.bottom(),
        i64::from(monitor.origin.y),
        monitor.bottom(),
        visible_y,
    );

    let control = close_control(bounds);
    dx += axis_contain(
        i64::from(control.origin.x) + dx,
        control.right() + dx,
        i64::from(monitor.origin.x),
        monitor.right(),
    );
    dy += axis_contain(
        i64::from(control.origin.y) + dy,
        control.bottom() + dy,
        i64::from(monitor.origin.y),
        monitor.bottom(),
    );
    move_by(bounds, clamp_to_i32(dx), clamp_to_i32(dy))
}

/// The shift needed so at least `visible` of `[low, high)` overlaps
/// `[bound_low, bound_high)`. Zero when it already does.
fn axis_push(low: i64, high: i64, bound_low: i64, bound_high: i64, visible: i64) -> i64 {
    if high < bound_low + visible {
        return bound_low + visible - high;
    }
    if low > bound_high - visible {
        return bound_high - visible - low;
    }
    0
}

/// The shift needed so `[low, high)` lies wholly inside `[bound_low, bound_high)`.
/// Zero when it already does. When it cannot fit, the low edge wins — for the
/// close control that keeps its clickable corner on screen.
fn axis_contain(low: i64, high: i64, bound_low: i64, bound_high: i64) -> i64 {
    if high > bound_high {
        let shifted = bound_high - high;
        if low + shifted < bound_low {
            return bound_low - low;
        }
        return shifted;
    }
    if low < bound_low {
        return bound_low - low;
    }
    0
}

/// Everything a committed move or resize must satisfy: snap to the edges, then
/// guarantee reachability.
///
/// Snapping runs first on purpose. Doing it the other way round would let a snap
/// pull an area back off the edge that [`contain`] had just rescued it from,
/// which would make the guarantee conditional on the order two features happened
/// to run in.
#[must_use]
pub fn settle_move(bounds: Rect, monitors: &[Rect]) -> Rect {
    contain(snap_move(bounds, monitors), monitors)
}

/// [`settle_move`] for a resize: the snap follows the dragged edges.
#[must_use]
pub fn settle_resize(bounds: Rect, resize: Resize, monitors: &[Rect]) -> Rect {
    contain(snap_resize(bounds, resize, monitors), monitors)
}

/// Height of one area-menu row, physical pixels.
pub const MENU_ITEM_HEIGHT: u32 = 28;

/// Width of the area menu, physical pixels.
pub const MENU_WIDTH: u32 = 176;

/// Padding above and below the area menu's rows.
pub const MENU_PADDING: u32 = 5;

/// Where an area menu opened at `anchor` should sit so that it stays on the
/// monitor holding the cursor.
///
/// The menu opens with its top-left at the anchor, and **flips** rather than
/// slides when it would overflow: past the right edge it opens leftward, past
/// the bottom it opens upward. Flipping keeps the anchor on a corner of the
/// menu, so the item under the cursor is predictable; sliding would put a
/// different item there depending on how close to the edge you clicked.
///
/// `monitor` is the monitor under the cursor, never the whole virtual desktop.
/// That is F-13's rule: overlay chrome positioned against the desktop as a whole
/// can land in a dead zone between monitors, where the cursor cannot reach it.
#[must_use]
pub fn menu_bounds(anchor: Point, items: u32, monitor: Rect) -> Rect {
    let height = items * MENU_ITEM_HEIGHT + 2 * MENU_PADDING;
    let (width_i, height_i) = (i64::from(MENU_WIDTH), i64::from(height));
    let (ax, ay) = (i64::from(anchor.x), i64::from(anchor.y));

    let x = if ax + width_i > monitor.right() {
        ax - width_i
    } else {
        ax
    };
    let y = if ay + height_i > monitor.bottom() {
        ay - height_i
    } else {
        ay
    };
    // A menu taller or wider than the monitor cannot be fully placed; clamping
    // after the flip keeps its top-left on screen, which is the half the user
    // reads first.
    let x = x.clamp(
        i64::from(monitor.origin.x),
        (monitor.right() - width_i).max(i64::from(monitor.origin.x)),
    );
    let y = y.clamp(
        i64::from(monitor.origin.y),
        (monitor.bottom() - height_i).max(i64::from(monitor.origin.y)),
    );
    Rect::new(clamp_to_i32(x), clamp_to_i32(y), MENU_WIDTH, height)
}

/// The rectangle of the `index`-th row of a menu occupying `menu`.
#[must_use]
pub fn menu_item_bounds(menu: Rect, index: u32) -> Rect {
    Rect::new(
        menu.origin.x,
        clamp_to_i32(
            i64::from(menu.origin.y)
                + i64::from(MENU_PADDING)
                + i64::from(index * MENU_ITEM_HEIGHT),
        ),
        menu.size.width,
        MENU_ITEM_HEIGHT,
    )
}

fn clamp_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn clamp_to_u32(value: i64) -> u32 {
    value.clamp(0, i64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const ALL_RESIZES: [Resize; 8] = [
        Resize::North,
        Resize::South,
        Resize::East,
        Resize::West,
        Resize::NorthEast,
        Resize::NorthWest,
        Resize::SouthEast,
        Resize::SouthWest,
    ];

    fn area() -> Rect {
        Rect::new(100, 100, 200, 150)
    }

    #[test]
    fn a_point_outside_the_area_grabs_nothing() {
        assert_eq!(handle_at(area(), Point::new(99, 150)), None);
        assert_eq!(handle_at(area(), Point::new(300, 150)), None);
        assert_eq!(handle_at(area(), Point::new(150, 250)), None);
    }

    #[test]
    fn the_middle_of_an_area_is_the_body() {
        assert_eq!(handle_at(area(), Point::new(200, 175)), Some(Handle::Body));
    }

    #[test]
    fn each_edge_and_corner_grabs_its_own_resize() {
        let a = area();
        let cases = [
            (Point::new(200, 101), Resize::North),
            (Point::new(200, 248), Resize::South),
            (Point::new(298, 175), Resize::East),
            (Point::new(101, 175), Resize::West),
            (Point::new(101, 101), Resize::NorthWest),
            (Point::new(101, 248), Resize::SouthWest),
            (Point::new(298, 248), Resize::SouthEast),
        ];
        for (point, expected) in cases {
            assert_eq!(
                handle_at(a, point),
                Some(Handle::Resize(expected)),
                "at {point:?}"
            );
        }
    }

    #[test]
    fn the_close_control_wins_the_corner_it_shares_with_the_north_east_resize() {
        // Documented precedence, pinned: dismissing has no undo, so it must not
        // be the gesture that sometimes resizes by accident.
        let a = area();
        assert_eq!(handle_at(a, Point::new(298, 101)), Some(Handle::Close));
        assert!(close_control(a).contains(Point::new(298, 101)));
    }

    #[test]
    fn the_close_control_sits_inside_the_areas_top_right() {
        let control = close_control(area());
        assert_eq!(control.right(), area().right());
        assert_eq!(control.origin.y, area().origin.y);
        assert_eq!(control.size, control.size);
        assert!(control.size.width <= CLOSE_SPAN);
    }

    #[test]
    fn every_control_shrinks_rather_than_swallowing_a_minimum_sized_area() {
        // The reason the bands are functions of the area: at MIN_AREA_SPAN a
        // fixed 8 px band and an 18 px close control would together leave no
        // body at all, and an area with no body cannot be moved.
        let tiny = Rect::new(0, 0, MIN_AREA_SPAN, MIN_AREA_SPAN);
        let control = close_control(tiny);
        assert!(control.size.width <= tiny.size.width / 2);
        let centre = Point::new(
            tiny.size.width as i32 / 2,
            // Below the close control, clear of the north band.
            tiny.size.height as i32 / 2 + 2,
        );
        assert_eq!(handle_at(tiny, centre), Some(Handle::Body));
    }

    #[test]
    fn a_move_changes_the_origin_and_never_the_size() {
        let moved = move_by(area(), -250, 40);
        assert_eq!(moved.origin, Point::new(-150, 140));
        assert_eq!(moved.size, area().size);
    }

    #[test]
    fn resizing_moves_only_the_named_edges() {
        let a = area();
        // East: the left edge is fixed, the width grows.
        let east = resize_by(a, Resize::East, 50, 999);
        assert_eq!(east, Rect::new(100, 100, 250, 150));
        // West: the right edge is fixed, so dragging right shrinks it.
        let west = resize_by(a, Resize::West, 50, 999);
        assert_eq!(west, Rect::new(150, 100, 150, 150));
        // A corner moves both of its edges.
        let nw = resize_by(a, Resize::NorthWest, -20, -10);
        assert_eq!(nw, Rect::new(80, 90, 220, 160));
    }

    #[test]
    fn a_resize_dragged_through_the_opposite_edge_pins_at_the_minimum() {
        let a = area();
        for resize in ALL_RESIZES {
            // Deliberately far enough to invert the rectangle if unclamped.
            let big = resize_by(a, resize, -10_000, -10_000);
            assert!(big.size.width >= MIN_AREA_SPAN, "{resize:?} width");
            assert!(big.size.height >= MIN_AREA_SPAN, "{resize:?} height");
            let other = resize_by(a, resize, 10_000, 10_000);
            assert!(other.size.width >= MIN_AREA_SPAN, "{resize:?} width");
            assert!(other.size.height >= MIN_AREA_SPAN, "{resize:?} height");
        }
    }

    #[test]
    fn a_drag_too_small_to_grab_is_not_placeable() {
        assert!(is_placeable(Rect::new(0, 0, MIN_AREA_SPAN, MIN_AREA_SPAN)));
        assert!(!is_placeable(Rect::new(0, 0, MIN_AREA_SPAN - 1, 400)));
        assert!(!is_placeable(Rect::new(0, 0, 400, MIN_AREA_SPAN - 1)));
        assert!(!is_placeable(Rect::new(0, 0, 0, 0)));
    }

    /// The dev rig: a 2560×1440 primary, two 1920×1080, and a portrait monitor
    /// left of the primary at negative coordinates.
    fn rig() -> Vec<Rect> {
        vec![
            Rect::new(0, 0, 2560, 1440),
            Rect::new(2560, 0, 1920, 1080),
            Rect::new(0, -1080, 1920, 1080),
            Rect::new(-1080, -267, 1080, 1920),
        ]
    }

    /// Whether an area is reachable: its close control is fully on some monitor
    /// and enough of its body is visible to grab.
    ///
    /// Written independently of `contain` rather than by calling it, so the
    /// property below checks the *guarantee* instead of restating the code.
    fn is_reachable(bounds: Rect, monitors: &[Rect]) -> bool {
        let control = close_control(bounds);
        monitors.iter().any(|monitor| {
            let control_inside = monitor.intersection(control) == Some(control);
            let visible = monitor.intersection(bounds);
            let enough = visible.is_some_and(|overlap| {
                overlap.size.width >= MIN_VISIBLE_SPAN.min(bounds.size.width)
                    && overlap.size.height >= MIN_VISIBLE_SPAN.min(bounds.size.height)
            });
            control_inside && enough
        })
    }

    #[test]
    fn an_area_already_on_screen_is_left_exactly_where_it_is() {
        let bounds = Rect::new(500, 400, 300, 200);
        assert_eq!(contain(bounds, &rig()), bounds);
    }

    #[test]
    fn an_area_dragged_off_the_right_edge_is_pushed_back_into_reach() {
        // The failure the containment rule exists for: past the edge, the close
        // control goes with it and the area can never be dismissed again.
        let monitors = vec![Rect::new(0, 0, 1920, 1080)];
        let lost = Rect::new(1900, 500, 300, 200);
        assert!(!is_reachable(lost, &monitors));
        let settled = contain(lost, &monitors);
        assert!(is_reachable(settled, &monitors));
        assert_eq!(settled.size, lost.size, "containment must never resize");
    }

    #[test]
    fn an_area_dragged_above_the_top_edge_comes_back_down() {
        // The close control sits along the top edge, so "above the screen" is
        // the direction that loses it first — the same reason Windows will not
        // let a title bar go above the desktop.
        let monitors = vec![Rect::new(0, 0, 1920, 1080)];
        let lost = Rect::new(600, -190, 300, 200);
        assert!(!is_reachable(lost, &monitors));
        assert!(is_reachable(contain(lost, &monitors), &monitors));
    }

    #[test]
    fn an_area_left_in_the_gap_between_monitors_is_pulled_onto_one() {
        // The dev rig's portrait monitor spans y −267..1653 while the monitor
        // above the primary spans y −1080..0, so x < 0 above y = −267 is desktop
        // that no monitor covers. F-13 is the same class of dead zone.
        let stranded = Rect::new(-700, -800, 300, 200);
        assert!(!is_reachable(stranded, &rig()));
        assert!(is_reachable(contain(stranded, &rig()), &rig()));
    }

    #[test]
    fn an_area_near_a_monitor_edge_snaps_flush_to_it() {
        let monitors = vec![Rect::new(0, 0, 1920, 1080)];
        let near = Rect::new(7, 500, 300, 200);
        assert_eq!(snap_move(near, &monitors).origin.x, 0);
        // And the far edge snaps too, without changing the size.
        let near_right = Rect::new(1615, 500, 300, 200);
        let snapped = snap_move(near_right, &monitors);
        assert_eq!(snapped.right(), 1920);
        assert_eq!(snapped.size, near_right.size);
    }

    #[test]
    fn an_area_clear_of_every_edge_does_not_snap() {
        let monitors = vec![Rect::new(0, 0, 1920, 1080)];
        let free = Rect::new(400, 400, 300, 200);
        assert_eq!(snap_move(free, &monitors), free);
    }

    #[test]
    fn snapping_works_on_a_monitor_at_negative_coordinates() {
        // The portrait monitor starts at (−1080, −267): an implementation that
        // reasoned in absolute values or assumed a zero origin would snap to the
        // wrong screen entirely.
        let near = Rect::new(-1074, 200, 300, 200);
        assert_eq!(snap_move(near, &rig()).origin.x, -1080);
    }

    #[test]
    fn a_resize_snaps_only_the_edge_it_dragged() {
        let monitors = vec![Rect::new(0, 0, 1920, 1080)];
        // The west edge is near x = 0 and is the one being dragged.
        let bounds = Rect::new(6, 500, 300, 200);
        let snapped = snap_resize(bounds, Resize::West, &monitors);
        assert_eq!(snapped.origin.x, 0);
        assert_eq!(snapped.right(), bounds.right(), "the east edge is fixed");
        // Dragging the east edge leaves the near west edge alone.
        assert_eq!(
            snap_resize(bounds, Resize::East, &monitors).origin.x,
            bounds.origin.x
        );
    }

    #[test]
    fn a_snap_that_would_shrink_an_area_below_the_minimum_is_dropped() {
        let monitors = vec![Rect::new(0, 0, 1920, 1080)];
        // 26 px wide, its west edge 8 px from the monitor edge: snapping flush
        // would leave 18 px, under MIN_AREA_SPAN.
        let narrow = Rect::new(8, 500, 26, 200);
        assert_eq!(snap_resize(narrow, Resize::East, &monitors), narrow);
    }

    #[test]
    fn a_menu_with_room_opens_down_and_right_from_the_anchor() {
        let monitor = Rect::new(0, 0, 1920, 1080);
        let menu = menu_bounds(Point::new(400, 300), 4, monitor);
        assert_eq!(menu.origin, Point::new(400, 300));
        assert_eq!(menu.size.width, MENU_WIDTH);
        assert_eq!(menu.size.height, 4 * MENU_ITEM_HEIGHT + 2 * MENU_PADDING);
    }

    #[test]
    fn a_menu_near_an_edge_flips_instead_of_spilling_off_the_monitor() {
        let monitor = Rect::new(0, 0, 1920, 1080);
        let menu = menu_bounds(Point::new(1900, 1070), 4, monitor);
        assert!(menu.right() <= monitor.right());
        assert!(menu.bottom() <= monitor.bottom());
        assert!(menu.origin.x >= monitor.origin.x);
        assert!(menu.origin.y >= monitor.origin.y);
    }

    #[test]
    fn a_menu_stays_on_a_monitor_at_negative_coordinates() {
        // The portrait monitor left of the primary on the dev rig. A menu that
        // clamped to zero here would jump to another screen entirely.
        let monitor = Rect::new(-1080, -267, 1080, 1920);
        let menu = menu_bounds(Point::new(-1000, -200), 4, monitor);
        assert!(menu.origin.x >= monitor.origin.x);
        assert!(menu.origin.y >= monitor.origin.y);
        assert!(menu.right() <= monitor.right());
    }

    #[test]
    fn menu_rows_tile_the_menu_top_to_bottom_without_gaps() {
        let menu = menu_bounds(Point::new(0, 0), 4, Rect::new(0, 0, 1920, 1080));
        let mut previous = menu_item_bounds(menu, 0);
        assert_eq!(previous.origin.y, menu.origin.y + MENU_PADDING as i32);
        for index in 1..4 {
            let row = menu_item_bounds(menu, index);
            assert_eq!(row.origin.y as i64, previous.bottom());
            assert_eq!(row.size.width, menu.size.width);
            previous = row;
        }
        assert_eq!(previous.bottom(), menu.bottom() - i64::from(MENU_PADDING));
    }

    prop_compose! {
        fn any_area()(
            x in -3000i32..3000,
            y in -3000i32..3000,
            width in MIN_AREA_SPAN..1000,
            height in MIN_AREA_SPAN..1000,
        ) -> Rect {
            Rect::new(x, y, width, height)
        }
    }

    fn any_resize() -> impl Strategy<Value = Resize> {
        prop::sample::select(ALL_RESIZES.as_slice())
    }

    proptest! {
        #[test]
        fn every_point_inside_an_area_grabs_something(
            bounds in any_area(),
            fx in 0.0f64..1.0,
            fy in 0.0f64..1.0,
        ) {
            // No dead pixels: an area the cursor is over always offers a
            // gesture. A hole would read as "the overlay ignored me".
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let point = Point::new(
                bounds.origin.x + (f64::from(bounds.size.width - 1) * fx) as i32,
                bounds.origin.y + (f64::from(bounds.size.height - 1) * fy) as i32,
            );
            prop_assert!(bounds.contains(point));
            prop_assert!(handle_at(bounds, point).is_some());
        }

        #[test]
        fn a_resize_never_produces_an_area_too_small_to_grab(
            bounds in any_area(),
            resize in any_resize(),
            dx in -2000i32..2000,
            dy in -2000i32..2000,
        ) {
            // The invariant that keeps `MIN_AREA_SPAN`'s promise: however
            // violently the drag moves, the result stays grabbable — and
            // therefore dismissable.
            let resized = resize_by(bounds, resize, dx, dy);
            prop_assert!(resized.size.width >= MIN_AREA_SPAN);
            prop_assert!(resized.size.height >= MIN_AREA_SPAN);
            prop_assert!(is_placeable(resized));
        }

        #[test]
        fn a_resize_holds_the_edges_it_does_not_name(
            bounds in any_area(),
            resize in any_resize(),
            dx in -200i32..200,
            dy in -200i32..200,
        ) {
            let resized = resize_by(bounds, resize, dx, dy);
            if !resize.moves_west() {
                prop_assert_eq!(resized.origin.x, bounds.origin.x);
            }
            if !resize.moves_north() {
                prop_assert_eq!(resized.origin.y, bounds.origin.y);
            }
            if !resize.moves_east() {
                prop_assert_eq!(resized.right(), bounds.right());
            }
            if !resize.moves_south() {
                prop_assert_eq!(resized.bottom(), bounds.bottom());
            }
        }

        #[test]
        fn a_settled_move_can_always_be_reached_again(
            bounds in any_area(),
            dx in -8000i32..8000,
            dy in -8000i32..8000,
        ) {
            // The guarantee, as a property rather than as four hand-picked
            // cases: **however violently an area is dragged, it lands somewhere
            // the user can still grab and dismiss it.** An area that fails this
            // is permanent for the session — it cannot be moved back and it
            // cannot be closed, while still costing memory and compositing.
            let monitors = rig();
            let settled = settle_move(move_by(bounds, dx, dy), &monitors);
            prop_assert!(
                is_reachable(settled, &monitors),
                "unreachable after settling: {:?}",
                settled
            );
            prop_assert_eq!(settled.size, bounds.size, "a move must never resize");
        }

        #[test]
        fn a_settled_resize_can_always_be_reached_again(
            bounds in any_area(),
            resize in any_resize(),
            dx in -8000i32..8000,
            dy in -8000i32..8000,
        ) {
            let monitors = rig();
            let settled = settle_resize(resize_by(bounds, resize, dx, dy), resize, &monitors);
            prop_assert!(
                is_reachable(settled, &monitors),
                "unreachable after settling: {:?}",
                settled
            );
            prop_assert!(settled.size.width >= MIN_AREA_SPAN);
            prop_assert!(settled.size.height >= MIN_AREA_SPAN);
        }

        #[test]
        fn settling_an_already_reachable_area_leaves_it_alone(
            x in 200i32..2000,
            y in 200i32..1000,
            width in 100u32..400,
            height in 100u32..300,
        ) {
            // Well inside the primary monitor and clear of every edge, so
            // neither the snap nor the containment has anything to do. Without
            // this, a `contain` that always recentred would pass the property
            // above while making the feature unusable.
            let monitors = rig();
            let bounds = Rect::new(x, y, width, height);
            prop_assert_eq!(settle_move(bounds, &monitors), bounds);
        }

        #[test]
        fn a_move_is_reversible(
            bounds in any_area(),
            dx in -2000i32..2000,
            dy in -2000i32..2000,
        ) {
            prop_assert_eq!(move_by(move_by(bounds, dx, dy), -dx, -dy), bounds);
        }
    }
}
