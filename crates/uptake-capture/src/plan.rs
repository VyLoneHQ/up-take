//! Pure capture planning: which monitors a region touches, what to crop from
//! each, and where each crop lands in the output bitmap.
//!
//! Everything here is plain arithmetic on `uptake-core` geometry — no Windows
//! calls — so the region↔monitor math that decides *what gets captured* is
//! testable without a desktop, which is exactly the split quality-bars.md §2
//! prescribes for this crate ("test the decision logic rather than the capture
//! itself").
//!
//! Coordinates follow the project rule: physical pixels, virtual-desktop
//! space. A shot's `source` is the one deliberate exception — it is
//! monitor-local (relative to that monitor's top-left), because that is the
//! space a WGC frame of the monitor is addressed in.

use uptake_core::geometry::{Rect, virtual_desktop_bounds};

use crate::error::CaptureError;

/// One monitor as the planner sees it: its Win32 handle (an `HMONITOR` cast to
/// `isize`, `0` never valid) and its bounds in physical virtual-desktop pixels.
///
/// # `#[non_exhaustive]`, and what it does and does not buy
///
/// This was `pub(crate)` until `I-31` made it part of the public surface, and
/// going public gave up something that had been free: the only constructor was
/// `monitors::push_monitor`, so the `0 is never a valid handle` line above was
/// enforced by there being nowhere else to build one.
///
/// `#[non_exhaustive]` restores **half** of that, and it is worth being exact
/// about which half. It stops an outside crate assembling one from a struct
/// literal, so [`new`][Self::new] is the only way in and a field added later is
/// not a breaking change. It does **not** validate the handle: `new(0, …)`
/// compiles and is accepted. Nothing public consumes a `MonitorInfo` today —
/// [`crate::enumerate_monitors`] only returns them — so a bad handle has no path
/// into this crate, and a validating constructor would be a guard against a
/// caller that does not exist. Raised by the independent review of `I-31`;
/// recorded rather than over-solved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MonitorInfo {
    /// The `HMONITOR` cast to `isize`, so the value is `Send` and carries no
    /// pointer semantics until the capture thread turns it back into a handle.
    pub handle: isize,
    /// The monitor's rectangle in physical virtual-desktop pixels.
    pub bounds: Rect,
}

impl MonitorInfo {
    /// Creates a monitor description from a handle and its bounds.
    ///
    /// The one way to build one from outside this crate, which is what
    /// `#[non_exhaustive]` above is for. Test fixtures are the expected caller:
    /// real values come from [`crate::enumerate_monitors`].
    #[must_use]
    pub const fn new(handle: isize, bounds: Rect) -> Self {
        Self { handle, bounds }
    }
}

/// One monitor's contribution to a capture: crop `source` (monitor-local, in
/// that monitor's frame) and place it at `dest` in the output bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shot {
    /// The monitor to capture.
    pub monitor: MonitorInfo,
    /// Monitor-local offset of the crop's top-left inside the monitor frame.
    pub source_x: u32,
    /// Monitor-local offset of the crop's top-left inside the monitor frame.
    pub source_y: u32,
    /// Output-local offset where the crop's top-left lands.
    pub dest_x: u32,
    /// Output-local offset where the crop's top-left lands.
    pub dest_y: u32,
    /// The crop's dimensions — never empty.
    pub size: uptake_core::geometry::Size,
}

/// The full plan for one capture call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapturePlan {
    /// The rectangle the output bitmap represents: the requested region
    /// clamped to the virtual desktop's bounding rectangle.
    pub output: Rect,
    /// Per-monitor shots. Non-empty; pixels of `output` covered by no shot
    /// (dead zones) stay transparent in the result.
    pub shots: Vec<Shot>,
}

/// Plans a capture of `region` across `monitors`.
///
/// The output rectangle is `region ∩ bounding-box(monitors)` — clamping to
/// the desktop bounds what the bitmap allocation can cost, while keeping every
/// on-screen pixel the caller asked for. Errors:
///
/// - [`CaptureError::EmptyRegion`] — `region` has no area
/// - [`CaptureError::NoMonitors`] — `monitors` is empty
/// - [`CaptureError::Offscreen`] — `region` overlaps no monitor (including
///   the dead-zone case: inside the bounding box, on no monitor)
///
/// Windows does not normally report overlapping monitors; if it ever does,
/// later shots overwrite earlier ones where they overlap, which is at worst
/// the same pixels twice.
pub(crate) fn plan(region: Rect, monitors: &[MonitorInfo]) -> Result<CapturePlan, CaptureError> {
    if region.size.is_empty() {
        return Err(CaptureError::EmptyRegion);
    }
    let desktop = virtual_desktop_bounds(monitors.iter().map(|m| m.bounds))
        .ok_or(CaptureError::NoMonitors)?;
    let output = region
        .intersection(desktop)
        .ok_or(CaptureError::Offscreen)?;

    let shots: Vec<Shot> = monitors
        .iter()
        .filter_map(|&monitor| {
            let visible = output.intersection(monitor.bounds)?;
            Some(Shot {
                monitor,
                source_x: offset(visible.origin.x, monitor.bounds.origin.x),
                source_y: offset(visible.origin.y, monitor.bounds.origin.y),
                dest_x: offset(visible.origin.x, output.origin.x),
                dest_y: offset(visible.origin.y, output.origin.y),
                size: visible.size,
            })
        })
        .collect();

    if shots.is_empty() {
        return Err(CaptureError::Offscreen);
    }
    Ok(CapturePlan { output, shots })
}

/// `a - b` as a `u32`, for offsets that are non-negative by construction (an
/// intersection's origin never precedes its operands' origins).
///
/// Computed in `i64` so no intermediate overflows; a negative result — which
/// would mean the intersection arithmetic itself is broken — clamps to `0`
/// rather than panicking, per the workspace's no-panic posture.
fn offset(a: i32, b: i32) -> u32 {
    u32::try_from(i64::from(a) - i64::from(b)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use proptest::prelude::*;
    use uptake_core::geometry::{Point, Size};

    use super::*;

    /// The 4-monitor dev rig, mirroring `uptake-core`'s geometry fixture:
    /// 2560×1440 @ 125 % primary, 1920×1080 right of it, another above it, and
    /// a 1080×1920 portrait left of it (with a dead zone above the portrait).
    fn dev_rig() -> Vec<MonitorInfo> {
        vec![
            MonitorInfo {
                handle: 1,
                bounds: Rect::new(0, 0, 2560, 1440),
            },
            MonitorInfo {
                handle: 2,
                bounds: Rect::new(2560, 0, 1920, 1080),
            },
            MonitorInfo {
                handle: 3,
                bounds: Rect::new(0, -1080, 1920, 1080),
            },
            MonitorInfo {
                handle: 4,
                bounds: Rect::new(-1080, -267, 1080, 1920),
            },
        ]
    }

    #[test]
    fn a_region_inside_one_monitor_yields_one_full_shot() {
        let plan = plan(Rect::new(100, 200, 640, 480), &dev_rig()).unwrap();
        assert_eq!(plan.output, Rect::new(100, 200, 640, 480));
        assert_eq!(plan.shots.len(), 1);
        let shot = plan.shots[0];
        assert_eq!(shot.monitor.handle, 1);
        assert_eq!((shot.source_x, shot.source_y), (100, 200));
        assert_eq!((shot.dest_x, shot.dest_y), (0, 0));
        assert_eq!(shot.size, Size::new(640, 480));
    }

    #[test]
    fn a_region_straddling_two_monitors_splits_at_the_shared_edge() {
        // 200 px on the primary, 300 px on the monitor to its right.
        let plan = plan(Rect::new(2360, 100, 500, 400), &dev_rig()).unwrap();
        assert_eq!(plan.shots.len(), 2);
        let on_primary = plan.shots.iter().find(|s| s.monitor.handle == 1).unwrap();
        let on_right = plan.shots.iter().find(|s| s.monitor.handle == 2).unwrap();

        assert_eq!((on_primary.source_x, on_primary.source_y), (2360, 100));
        assert_eq!((on_primary.dest_x, on_primary.dest_y), (0, 0));
        assert_eq!(on_primary.size, Size::new(200, 400));

        // The right-hand monitor starts at x = 2560, so its crop starts at its
        // own left edge and lands 200 px into the output.
        assert_eq!((on_right.source_x, on_right.source_y), (0, 100));
        assert_eq!((on_right.dest_x, on_right.dest_y), (200, 0));
        assert_eq!(on_right.size, Size::new(300, 400));
    }

    #[test]
    fn negative_coordinate_monitors_plan_like_any_other() {
        // Entirely on the portrait monitor left of the primary (M-4 territory).
        let plan = plan(Rect::new(-1000, 0, 400, 300), &dev_rig()).unwrap();
        assert_eq!(plan.shots.len(), 1);
        let shot = plan.shots[0];
        assert_eq!(shot.monitor.handle, 4);
        // (-1000) - (-1080) = 80 into the monitor, 267 below its top.
        assert_eq!((shot.source_x, shot.source_y), (80, 267));
        assert_eq!(shot.size, Size::new(400, 300));
    }

    #[test]
    fn a_partially_offscreen_region_is_clamped_to_the_desktop() {
        // Extends 500 px past the right-hand monitor's right edge (4480).
        let plan = plan(Rect::new(4200, 100, 780, 200), &dev_rig()).unwrap();
        assert_eq!(plan.output, Rect::new(4200, 100, 280, 200));
        assert_eq!(plan.shots.len(), 1);
        assert_eq!(plan.shots[0].size, Size::new(280, 200));
    }

    #[test]
    fn empty_no_monitor_and_offscreen_inputs_each_get_their_own_error() {
        let rig = dev_rig();
        assert!(matches!(
            plan(Rect::new(0, 0, 0, 100), &rig),
            Err(CaptureError::EmptyRegion)
        ));
        assert!(matches!(
            plan(Rect::new(0, 0, 100, 100), &[]),
            Err(CaptureError::NoMonitors)
        ));
        // Far outside the virtual desktop.
        assert!(matches!(
            plan(Rect::new(100_000, 0, 100, 100), &rig),
            Err(CaptureError::Offscreen)
        ));
        // Inside the desktop's bounding box but in the dead zone above the
        // portrait monitor — clamping alone would happily emit an all-blank
        // bitmap here; the planner must refuse instead.
        assert!(matches!(
            plan(Rect::new(-600, -1050, 50, 50), &rig),
            Err(CaptureError::Offscreen)
        ));
    }

    fn small_rect() -> impl Strategy<Value = Rect> {
        (-4000i32..4000, -4000i32..4000, 1u32..2000, 1u32..2000)
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    proptest! {
        /// Every shot stays inside its monitor, inside the output, and agrees
        /// with itself: the source crop fits the monitor frame and the dest
        /// placement fits the output bitmap.
        #[test]
        fn shots_are_consistent_with_monitor_and_output(region in small_rect()) {
            let rig = dev_rig();
            if let Ok(plan) = plan(region, &rig) {
                // The output is the region clamped to the desktop.
                let desktop =
                    virtual_desktop_bounds(rig.iter().map(|m| m.bounds)).unwrap();
                prop_assert_eq!(plan.output, region.intersection(desktop).unwrap());

                for shot in &plan.shots {
                    prop_assert!(!shot.size.is_empty());
                    // Source crop fits inside the monitor frame.
                    let m = shot.monitor.bounds.size;
                    prop_assert!(u64::from(shot.source_x) + u64::from(shot.size.width) <= u64::from(m.width));
                    prop_assert!(u64::from(shot.source_y) + u64::from(shot.size.height) <= u64::from(m.height));
                    // Dest placement fits inside the output bitmap.
                    let o = plan.output.size;
                    prop_assert!(u64::from(shot.dest_x) + u64::from(shot.size.width) <= u64::from(o.width));
                    prop_assert!(u64::from(shot.dest_y) + u64::from(shot.size.height) <= u64::from(o.height));
                    // Source and dest describe the same virtual-desktop rect.
                    let via_monitor = Point::new(
                        shot.monitor.bounds.origin.x + i32::try_from(shot.source_x).unwrap(),
                        shot.monitor.bounds.origin.y + i32::try_from(shot.source_y).unwrap(),
                    );
                    let via_output = Point::new(
                        plan.output.origin.x + i32::try_from(shot.dest_x).unwrap(),
                        plan.output.origin.y + i32::try_from(shot.dest_y).unwrap(),
                    );
                    prop_assert_eq!(via_monitor, via_output);
                }
            }
        }

        /// Every output pixel that lies on some monitor is covered by exactly
        /// one shot (the rig's monitors do not overlap), and dead-zone pixels
        /// by none. Sampled at the output's corners, centre, and edge
        /// midpoints rather than exhaustively.
        #[test]
        fn probe_points_are_covered_exactly_when_on_a_monitor(region in small_rect()) {
            let rig = dev_rig();
            if let Ok(plan) = plan(region, &rig) {
                let o = plan.output;
                let (w, h) = (
                    i32::try_from(o.size.width).unwrap(),
                    i32::try_from(o.size.height).unwrap(),
                );
                let probes = [
                    (0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1),
                    (w / 2, h / 2), (w / 2, 0), (0, h / 2),
                ];
                for (px, py) in probes {
                    let p = Point::new(o.origin.x + px, o.origin.y + py);
                    let on_monitor = rig.iter().any(|m| m.bounds.contains(p));
                    let covering = plan
                        .shots
                        .iter()
                        .filter(|s| {
                            let sx = i64::from(px) - i64::from(s.dest_x);
                            let sy = i64::from(py) - i64::from(s.dest_y);
                            (0..i64::from(s.size.width)).contains(&sx)
                                && (0..i64::from(s.size.height)).contains(&sy)
                        })
                        .count();
                    prop_assert_eq!(covering, usize::from(on_monitor));
                }
            }
        }
    }
}
