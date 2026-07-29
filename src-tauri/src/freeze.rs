//! The frozen screen: full-monitor stills held while PLACEMENT is frozen
//! (roadmap task 1.9d, [ADR-0026]).
//!
//! # What this is for
//!
//! Some things on screen do not wait to be selected — a video frame, a
//! notification sliding away, a hover state that dies the moment you reach for
//! the mouse. Freeze-on-demand captures the monitors to stills and shows those,
//! so the moment can be selected at leisure ([ADR-0014] §4).
//!
//! # The semantics, all decided rather than inferred
//!
//! [ADR-0026] settles what [ADR-0014] §4 left open, and each of these is a
//! decision this module implements rather than a choice made here:
//!
//! * **`Ctrl+Space` toggles frozen↔live, and only in PLACEMENT.** This is not a
//!   system-wide screen freeze.
//! * **The default is live, and it resets to live on every entry to PLACEMENT**
//!   ([`thaw`] is called on the way in). Freezing is always something the user
//!   asked for during *this* visit — which is what keeps ADR-0014's promise that
//!   the desktop never freezes while you place an area.
//! * **The toggle always fires**, whatever type is armed or none. Freezing is
//!   only *useful* for types that consume pixels at creation, but usefulness is
//!   not a gate: a key that silently does nothing is worse than one that does
//!   something harmless.
//! * **Freezing changes no area type's behaviour.** It is a view state. A
//!   Screenshot area arms, drags, releases and copies identically either way.
//! * **Each freeze re-captures** ([ADR-0014] §4), so toggling off and on gives
//!   the current moment rather than the first one.
//!
//! # Why there is no freshness bound here, unlike `precapture`
//!
//! [`crate::precapture`] bounds its held frame's age, because the user is
//! selecting on **live** pixels while a stale frame waits to be cropped — the
//! image and the screen can silently disagree. Frozen is the opposite case:
//! the user is selecting *on the still itself*, so the pixels they see are by
//! construction the pixels they get, however long they take. [ADR-0022] §5 names
//! this — the frozen source "carries no staleness question at all" — and it is
//! the reason this module has no clock in it.
//!
//! **That is a real invariant and not a convenience.** If a future change ever
//! displays something other than what [`crop`] serves, this reasoning collapses
//! and a bound has to come back.
//!
//! # Why the crop is not implemented here
//!
//! It is [`RgbaBitmap::crop_screen`], shared with `precapture`. 1B's exit gate
//! requires every path that can produce a Screenshot's pixels to produce
//! identical results for the same rectangle, and the cheapest way to hold that
//! is for the paths to be the same code. See that function's docs.
//!
//! [ADR-0014]: the private planning repo's
//! `DECISIONS/ADR-0014-capture-and-render-over-live-content.md`
//! [ADR-0022]: the private planning repo's
//! `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`
//! [ADR-0026]: the private planning repo's
//! `DECISIONS/ADR-0026-freeze-on-demand-trigger.md`

use std::sync::{Mutex, PoisonError};

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

/// One monitor's still, and where on the virtual desktop it came from.
///
/// The rectangle is not decoration: a bitmap does not know its own position, and
/// without it a screen-space crop cannot be computed at all.
struct Still {
    rect: Rect,
    bitmap: RgbaBitmap,
}

/// The stills currently displayed, one per frozen monitor. Empty means live.
///
/// **Emptiness is the state**, rather than a separate `bool` that could disagree
/// with it. A frozen screen with no stills is not a state this feature has: if
/// every capture failed there is nothing to show, and continuing to report
/// "frozen" would leave the user looking at a live desktop the app believes is
/// frozen — the two-flags-one-fact defect that keeps showing up in this project's
/// findings ledger.
static STILLS: Mutex<Vec<Still>> = Mutex::new(Vec::new());

fn stills() -> std::sync::MutexGuard<'static, Vec<Still>> {
    STILLS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the screen is currently frozen.
pub(crate) fn is_frozen() -> bool {
    !stills().is_empty()
}

/// Captures `monitors` and holds the stills, replacing any already held.
///
/// Returns the number of monitors actually captured, which is **not** always
/// `monitors.len()`: a monitor WGC and GDI both decline is skipped rather than
/// failing the whole freeze, because freezing three of four screens is more
/// useful than freezing none. The count is what the caller logs.
///
/// # Threading
///
/// **Blocks for roughly one capture per monitor** — 183–313 ms each on the dev
/// rig, so approaching a second on a four-monitor desktop. It must not run on
/// the event-loop thread or inside the `WH_MOUSE_LL` callback; the caller
/// spawns. That is the same constraint `precapture` documents at length and the
/// failure class F-33 found the hard way.
///
/// The overlay is permanently excluded from capture ([ADR-0019]), so a freeze
/// never captures UP-TAKE's own chrome and re-freezing cannot compound it.
///
/// [ADR-0019]: the private planning repo's
/// `DECISIONS/ADR-0019-overlay-excluded-from-capture.md`
pub(crate) fn freeze(monitors: &[Rect]) -> usize {
    let captured: Vec<Still> = monitors
        .iter()
        .filter_map(|monitor| match uptake_capture::capture_region(*monitor) {
            Ok(shot) => Some(Still {
                // What the capture crate reports it took, never what was asked
                // for: it clamps to the virtual desktop, and trusting the
                // request would offset every crop by the clamp distance.
                rect: shot.rect,
                bitmap: shot.bitmap,
            }),
            Err(error) => {
                eprintln!("freeze: could not capture {monitor:?}: {error}");
                None
            }
        })
        .collect();
    let count = captured.len();
    *stills() = captured;
    count
}

/// Returns to live, dropping every still.
///
/// Called on the toggle's way out **and on every entry to PLACEMENT**, which is
/// what makes ADR-0026's "reset to live" true rather than merely intended.
pub(crate) fn thaw() {
    stills().clear();
}

/// The pixels for `bounds`, cropped out of the frozen still that contains it.
///
/// `None` when the screen is live, or when `bounds` does not lie wholly inside
/// any single still — a rectangle straddling two monitors, which is the same
/// case `precapture` calls a straddle and answers the same way: fall back to an
/// ordinary capture rather than return a short image.
///
/// **Does not consume the still.** `precapture::take` consumes, because a held
/// frame belongs to one drag and reusing it would silently serve stale pixels.
/// Frozen is the opposite: the still is what the user is *looking at*, and it
/// stays until they unfreeze. Consuming it here would mean the second drag on a
/// frozen screen quietly captured live pixels while the display still showed the
/// freeze — the exact see-one-thing-get-another defect this feature exists to
/// prevent.
pub(crate) fn crop(bounds: Rect) -> Option<RgbaBitmap> {
    stills()
        .iter()
        .find_map(|still| still.bitmap.crop_screen(still.rect.origin, bounds))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "architecture §5 bans unwrap outside tests; inside them a failed \
              setup should abort the test loudly"
)]
mod tests {
    use uptake_core::geometry::Size;

    use super::*;

    /// A bitmap whose every pixel encodes its own coordinates, so a crop taken
    /// from the wrong offset produces different bytes rather than
    /// coincidentally-equal ones.
    ///
    /// A flat fill would let an off-by-anything crop pass every assertion here —
    /// the same weakness the 1.9c review found in the full-size-crop test, where
    /// the coordinate pattern is what makes an axis swap fail.
    fn patterned(size: Size) -> RgbaBitmap {
        let mut bitmap = RgbaBitmap::transparent(size).unwrap();
        let pixels = bitmap.pixels_mut();
        for y in 0..size.height {
            for x in 0..size.width {
                let at = ((y * size.width + x) * 4) as usize;
                pixels[at] = (x % 251) as u8;
                pixels[at + 1] = (y % 251) as u8;
                pixels[at + 2] = ((x ^ y) % 251) as u8;
                pixels[at + 3] = 255;
            }
        }
        bitmap
    }

    /// Installs stills directly, standing in for completed captures — the tests
    /// below are about the decision and the arithmetic, neither of which needs a
    /// desktop.
    fn hold(stills_to_set: Vec<(Rect, RgbaBitmap)>) {
        *stills() = stills_to_set
            .into_iter()
            .map(|(rect, bitmap)| Still { rect, bitmap })
            .collect();
    }

    #[test]
    fn live_until_something_is_frozen() {
        let _guard = crate::precapture::frame_store_guard();
        thaw();
        assert!(!is_frozen());
        assert!(crop(Rect::new(0, 0, 10, 10)).is_none());
    }

    #[test]
    fn thawing_returns_to_live() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        assert!(is_frozen());
        thaw();
        assert!(!is_frozen());
    }

    #[test]
    fn crops_from_the_still_holding_the_rectangle() {
        let _guard = crate::precapture::frame_store_guard();
        let frame = patterned(Size::new(64, 48));
        // A monitor at a negative origin, which is the case that has produced
        // real defects in this project (F-15) rather than a tidy 0,0 one.
        hold(vec![(Rect::new(-1920, -200, 64, 48), frame)]);
        let cropped = crop(Rect::new(-1910, -190, 8, 6)).unwrap();
        assert_eq!(cropped.size(), Size::new(8, 6));
        // Top-left pixel of the crop is (10, 10) of the frame, by the pattern.
        assert_eq!(&cropped.pixels()[0..3], &[10, 10, 10 ^ 10]);
    }

    #[test]
    fn declines_a_rectangle_that_straddles_two_stills() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![
            (Rect::new(0, 0, 64, 48), patterned(Size::new(64, 48))),
            (Rect::new(64, 0, 64, 48), patterned(Size::new(64, 48))),
        ]);
        // Spans the seam: wholly inside neither, and clamping to either would
        // hand back half a screenshot.
        assert!(crop(Rect::new(60, 10, 10, 10)).is_none());
        // ...while a rectangle wholly inside the second still is served.
        assert!(crop(Rect::new(70, 10, 10, 10)).is_some());
    }

    #[test]
    fn a_crop_does_not_consume_the_still() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        let first = crop(Rect::new(4, 4, 16, 16)).unwrap();
        let second = crop(Rect::new(4, 4, 16, 16)).unwrap();
        assert_eq!(first.pixels(), second.pixels());
        assert!(is_frozen(), "the still must survive being cropped");
    }

    /// **1B exit-gate row 2, the half a unit test can carry.**
    ///
    /// The gate requires the paths that can produce a Screenshot's pixels to
    /// produce identical results for the same rectangle. This asserts that the
    /// frozen path and the held-pre-capture path, given the same frame at the
    /// same position, return **byte-identical** pixels for the same screen
    /// rectangle.
    ///
    /// **It drives both real entry points**, `freeze::crop` and
    /// `precapture::take`, rather than re-implementing either. An earlier cut of
    /// this test called `crop_screen` directly for the held side — which, now
    /// that both paths share that function, reduced to asserting a function
    /// equals itself and would have passed with `freeze::crop` cropping from
    /// entirely the wrong origin. Same shape as the sweep defect in backlog
    /// I-1, caught here by re-reading rather than by the suite.
    ///
    /// **What this does not prove, stated so nobody reads it as more than it
    /// is:** that two *real* captures of the same screen agree. Both sides are
    /// fed the same synthetic frame, so this is the *transformation* half of
    /// the gate row. The capture half needs hardware and belongs to 1.9d's rig
    /// pass.
    #[test]
    fn frozen_and_held_crops_are_byte_identical() {
        let _guard = crate::precapture::frame_store_guard();
        let monitor = Rect::new(-1920, -200, 64, 48);
        let wanted = Rect::new(-1900, -180, 12, 9);
        // Built twice rather than cloned: `patterned` is deterministic, so the
        // two frames are byte-identical inputs, and the test does not need
        // `RgbaBitmap: Clone` to exist for its own convenience.
        hold(vec![(monitor, patterned(Size::new(64, 48)))]);
        crate::precapture::install_for_test(monitor, patterned(Size::new(64, 48)));

        let from_frozen = crop(wanted).unwrap();
        let from_held = crate::precapture::take(wanted).unwrap();

        assert_eq!(from_frozen.size(), from_held.size());
        assert_eq!(
            from_frozen.pixels(),
            from_held.pixels(),
            "the frozen and held crop paths diverged for one rectangle"
        );
    }

    #[test]
    fn a_still_at_the_origin_still_needs_the_subtraction() {
        let _guard = crate::precapture::frame_store_guard();
        // Guards the degenerate case that would pass even if the subtraction
        // were dropped entirely: with the monitor at 0,0 screen space and frame
        // space coincide. Paired with the negative-origin test above, dropping
        // the subtraction fails one of the two.
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        let cropped = crop(Rect::new(10, 10, 8, 6)).unwrap();
        assert_eq!(&cropped.pixels()[0..3], &[10, 10, 10 ^ 10]);
    }
}
