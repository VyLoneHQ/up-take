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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

/// One monitor's still: where it came from, the pixels a crop is cut out of,
/// and the PNG the WebView displays.
///
/// The rectangle is not decoration: a bitmap does not know its own position, and
/// without it a screen-space crop cannot be computed at all.
///
/// # Why both representations, and why the PNG is made now
///
/// The same reason [`crate::captures::CaptureStore`] holds both: a crop needs
/// raw RGBA and an `<img>` needs PNG, and neither cheaply produces the other.
/// Encoding happens **at freeze time**, on the thread that already spawned for
/// the captures, rather than inside the URI-scheme handler — that handler runs
/// on the WebView2 UI thread, and a full-monitor PNG encode there would stall
/// the very repaint it is feeding.
///
/// The cost is memory, and it is the largest this feature carries: a 1440p
/// monitor is ~14.7 MB raw plus its PNG, and a 4K one ~33 MB. Four monitors
/// frozen is therefore well past `quality-bars.md` §1's 80 MB idle-RAM row —
/// **which is why [`thaw`] runs on every state transition and not only on the
/// toggle.** Frozen is a transient state by construction; if it ever becomes a
/// resting one, this is the number that has to be revisited first.
struct Still {
    rect: Rect,
    bitmap: RgbaBitmap,
    png: Vec<u8>,
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

/// Bumped by every [`freeze`], and carried in each still's URL.
///
/// **Cache-busting, and it is load-bearing rather than tidy.** WebView2 caches
/// by URL, so a second freeze re-using `frozen-0.png` would redisplay the
/// *first* freeze's pixels — a still of a moment the user deliberately replaced,
/// with nothing on screen to say so. Exactly the defect the pin store's own
/// version counter exists for, and ADR-0014 §4's "each freeze re-captures the
/// current moment" is the promise it would quietly break.
static VERSION: AtomicU64 = AtomicU64::new(0);

fn stills() -> std::sync::MutexGuard<'static, Vec<Still>> {
    STILLS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The URL a frozen still is served at. The only thing that builds one.
///
/// Shares [`crate::captures::SCHEME`] rather than registering a second protocol:
/// one scheme means one handler, one `img-src` entry for whoever adds the CSP
/// this app still lacks, and one place the Windows `http://<scheme>.localhost`
/// form is got right (see [`crate::captures::pin_url`], where getting it wrong
/// cost a session).
///
/// The `frozen-` prefix is what keeps the two namespaces apart: an area's URL is
/// `<id>-<version>.png` and an id is a number, so no area can ever produce a
/// path that starts with `frozen-`.
#[must_use]
fn still_url(index: usize, version: u64) -> String {
    let path = format!("frozen-{index}-{version}.png");
    if cfg!(windows) {
        format!("http://{}.localhost/{path}", crate::captures::SCHEME)
    } else {
        format!("{}://localhost/{path}", crate::captures::SCHEME)
    }
}

/// The PNG for one frozen still, if `version` is still the live freeze.
///
/// A version mismatch is `None` rather than the current bytes, for the same
/// reason the pin store refuses one: the only way to ask for a stale version is
/// to hold a stale URL, and answering it with fresh pixels would hide a caching
/// bug instead of surfacing it.
pub(crate) fn still_png(index: usize, version: u64) -> Option<Vec<u8>> {
    if version != VERSION.load(Ordering::SeqCst) {
        return None;
    }
    stills().get(index).map(|still| still.png.clone())
}

/// Every frozen still as `(rect, url)`, for the frontend to lay out.
///
/// Physical virtual-desktop pixels, unconverted — the WebView owns its own
/// scale factor (ADR-0011), and pre-converting here is the exact mistake that
/// ADR made a rule about.
pub(crate) fn stills_for_display() -> Vec<(Rect, String)> {
    let version = VERSION.load(Ordering::SeqCst);
    stills()
        .iter()
        .enumerate()
        .map(|(index, still)| (still.rect, still_url(index, version)))
        .collect()
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
        .filter_map(|monitor| {
            let shot = match uptake_capture::capture_region(*monitor) {
                Ok(shot) => shot,
                Err(error) => {
                    eprintln!("freeze: could not capture {monitor:?}: {error}");
                    return None;
                }
            };
            // A still that cannot be encoded is dropped rather than kept, so
            // the display and the crop source cannot disagree about which
            // monitors are frozen. Keeping it would mean a monitor whose
            // pixels a drag would use but which shows live content — the
            // see-one-thing-get-another failure this feature exists to avoid.
            let png = match crate::output::encode_png(&shot.bitmap) {
                Ok(png) => png,
                Err(error) => {
                    eprintln!("freeze: could not encode {monitor:?}: {error}");
                    return None;
                }
            };
            Some(Still {
                // What the capture crate reports it took, never what was asked
                // for: it clamps to the virtual desktop, and trusting the
                // request would offset every crop by the clamp distance.
                rect: shot.rect,
                bitmap: shot.bitmap,
                png,
            })
        })
        .collect();
    let count = captured.len();
    // Bumped even when nothing was captured: a failed freeze must still
    // invalidate the previous freeze's URLs, or the WebView would happily
    // redisplay an older still for a freeze that produced none.
    VERSION.fetch_add(1, Ordering::SeqCst);
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
    clippy::expect_used,
    reason = "architecture §5 bans both outside tests; inside them a failed \
              setup should abort the test loudly. `expect` earns its place in \
              the URL tests: the path is derived from the URL rather than \
              written out, because a hard-coded prefix is exactly what let \
              F-38's round-trip test pass over an unresolvable URL"
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
            .map(|(rect, bitmap)| Still {
                rect,
                bitmap,
                // These tests are about the crop decision and the arithmetic,
                // neither of which reads the PNG. A real encode here would buy
                // nothing and make every test depend on the encoder.
                png: Vec::new(),
            })
            .collect();
    }

    /// The URL a still is served at must parse back to the same still through
    /// the **scheme handler's own parser**, not through a copy of it here.
    ///
    /// This is the F-38 lesson pinned as a test: `pin_url` had a round-trip
    /// test that passed while the URL was unresolvable, because it trimmed a
    /// hard-coded prefix and so checked the function against the same wrong
    /// assumption the function was built on. Driving `parse_frozen_path` is
    /// what makes this an independent check rather than a mirror.
    #[test]
    fn a_still_url_parses_back_through_the_scheme_handler() {
        let url = still_url(2, 7);
        let path = url
            .rsplit_once('/')
            .map(|(_, tail)| format!("/{tail}"))
            .expect("a still url always has a path");
        assert_eq!(crate::captures::parse_frozen_path(&path), Some((2, 7)));
    }

    #[test]
    fn a_still_url_is_not_mistaken_for_an_area_pin() {
        // The two namespaces share one scheme, so a frozen path reaching the
        // area parser would 404 as "missing capture" and send the next reader
        // looking in the wrong store entirely.
        let url = still_url(0, 1);
        let path = url
            .rsplit_once('/')
            .map(|(_, tail)| format!("/{tail}"))
            .expect("a still url always has a path");
        assert_eq!(crate::captures::parse_path(&path), None);
    }

    #[test]
    fn a_stale_version_is_refused_rather_than_answered_with_current_pixels() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![(Rect::new(0, 0, 8, 8), patterned(Size::new(8, 8)))]);
        let live = VERSION.load(Ordering::SeqCst);
        assert!(still_png(0, live.wrapping_sub(1)).is_none());
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
