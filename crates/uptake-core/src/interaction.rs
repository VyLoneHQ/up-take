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
        fn a_move_is_reversible(
            bounds in any_area(),
            dx in -2000i32..2000,
            dy in -2000i32..2000,
        ) {
            prop_assert_eq!(move_by(move_by(bounds, dx, dy), -dx, -dy), bounds);
        }
    }
}
