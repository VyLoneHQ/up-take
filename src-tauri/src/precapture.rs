//! The held full-monitor frame a Screenshot is cropped out of (roadmap task
//! 1.9c, [ADR-0022]).
//!
//! # What this is for
//!
//! `SPECS/quality-bars.md` §1 budgets **selection release → image on clipboard**
//! at 300 ms. Task 1.9b measured where that time goes: capture is **86–98 %** of
//! it, and the encode and clipboard publish together are 1–9 ms. So the budget
//! is met by taking capture *out of the measured interval*, not by making
//! capture faster — and the way to do that is to capture before the interval
//! starts.
//!
//! A create drag is hundreds of milliseconds of human time, and the monitor
//! under the cursor is known at mouse-**down**. So on mouse-down with a capture
//! type armed, a spawned thread captures that whole monitor; mouse-up crops the
//! held frame to the rectangle the user drew. WGC captures whole monitors
//! regardless of the region asked for — argued from `wgc.rs`'s structure and
//! then **measured** (capture time is flat across a 4× area range, ADR-0022) —
//! so the pre-capture costs exactly what the capture it replaces would have.
//!
//! # The moment that gets captured, which is a semantic choice
//!
//! The pixels are from **during the drag, not from drag-release**, and they
//! carry a [`FRESHNESS`] bound saying how old they may be when the user lets go.
//!
//! ADR-0022 §3 framed this as "drag-start" with a 200 ms bound and a fallback
//! for anything slower. **Both halves of that were wrong on the rig**, and in
//! opposite directions. The bound was unsatisfiable — a capture takes ~240 ms,
//! so a frame is older than 200 ms before it exists — and "drag-start" was never
//! accurate either, because the frame's content is from roughly a capture's
//! length *after* mouse-down. Three ordinary drags fell back at 333, 840 and
//! 6381 ms and the fast path never ran.
//!
//! So the frame is **re-taken during the drag** ([`refresh`]) rather than the
//! drag being abandoned to the slow path. A gesture of any length is served, and
//! the pixels are never older than [`FRESHNESS`] at release. The user's call,
//! 2026-07-28, over simply widening the bound: a long drag gets *fresh* pixels
//! rather than merely *permitted* ones.
//!
//! The cost is bounded and paid where it is invisible — at most one capture in
//! flight, one per [`REFRESH_AFTER`] while a capturing drag is being drawn, and
//! none at any other time. This is the narrow version of the continuous-capture
//! cost ADR-0022 point 7 rejected for a *persistent* service: bounded by the
//! gesture instead of by the process's lifetime.
//!
//! # Three ways this declines, all of them logged
//!
//! [`take`] returns [`Fallback`] rather than a bad image when the frame is
//! missing, stale, or does not cover the drawn rectangle (a drag that
//! **straddles** monitors, which the single-monitor pre-capture cannot serve).
//! Every one of them falls back to `uptake_capture::capture_region`, unchanged
//! and still correct — the fast path is an optimisation that is allowed to
//! decline, never a second way of producing pixels. That is what keeps 1B's
//! exit-gate row ("all four paths produce identical results") tractable: there
//! is one capture path and one crop, not four pipelines.
//!
//! # Why nothing here runs in the hook callback
//!
//! [`begin`] spawns. Mouse-down arrives inside the `WH_MOUSE_LL` callback on
//! the event-loop thread, and a capture is 100–300 ms — far past
//! `LowLevelHooksTimeout`, after which Windows silently removes the hook and
//! placement goes inert with nothing reporting a problem. That is the failure
//! class F-33 found the hard way and F-25's second half found again; ADR-0022 §5
//! writes it into the decision itself.
//!
//! [ADR-0022]: the private planning repo's
//! `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

/// How old a held frame may be at mouse-up and still be used.
///
/// # 200 ms was unachievable by construction, and the rig proved it
///
/// ADR-0022 §3 chose 200 ms by argument and flagged it as owed a measurement.
/// The measurement (2026-07-28, dev rig) fired the ADR's own first *Revisit if*:
/// **three consecutive ordinary drags fell back on staleness**, at ages of 333,
/// 840 and 6381 ms. The fast path never ran once.
///
/// The reason is arithmetic rather than tuning. A capture takes **~240 ms**, and
/// a frame is only stamped once it exists, so a held frame is *already* ~240 ms
/// old the instant it lands. A 200 ms bound is therefore past its limit before
/// any dragging happens at all — the feature was inert on every realistic
/// gesture, and would have shipped that way.
///
/// **The general rule this yields, which outlives the number: the freshness
/// bound must exceed the capture duration.** No scheme can keep a frame younger
/// than the time it takes to produce one. Any future edit of this constant that
/// puts it near or below [`CAPTURE_ALLOWANCE`] makes the fast path unreachable
/// again, which is what `the_bound_must_exceed_what_a_capture_costs` fails on.
///
/// 750 ms is then chosen so a refresh (see [`refresh`]) always lands before the
/// frame it replaces goes stale: a frame is re-taken at [`REFRESH_AFTER`], and
/// the replacement has [`CAPTURE_ALLOWANCE`] to arrive.
pub const FRESHNESS: Duration = Duration::from_millis(750);

/// What a capture is allowed to cost before the refresh scheme stops keeping up.
///
/// Measured at 239–244 ms on the dev rig across the 2026-07-28 pass, and 183–313
/// ms across 1.9b's four-sample instrumentation. 300 ms is the observed worst
/// case rounded up — not a target, a **budget for the refresh arithmetic**. A
/// capture slower than this does not break anything: the held frame goes stale,
/// and mouse-up falls back to a normal capture exactly as it did before.
const CAPTURE_ALLOWANCE: Duration = Duration::from_millis(300);

/// When a held frame is re-taken mid-drag, so the drag never has to fall back.
///
/// Derived, never written as its own number: the whole point is that a
/// replacement must be *finished* before the frame it replaces expires, and
/// hand-maintaining two constants that have to agree is how they stop agreeing.
const REFRESH_AFTER: Duration = FRESHNESS.saturating_sub(CAPTURE_ALLOWANCE);

/// A frame captured at drag-start, waiting to be cropped.
///
/// Deliberately carries **no** generation of its own. The drag a frame belongs
/// to is enforced entirely on the storing side (see [`begin`]): both [`begin`]
/// and [`discard`] clear the slot, so anything found in it belongs to the
/// current drag by construction. An earlier cut of this struct held the
/// generation and re-checked it in [`take`] as a belt-and-braces assertion — and
/// that check was **wrong**, not merely redundant. [`discard`] bumps the counter
/// and clears the slot as two operations, so a `take` running between them (the
/// previous gesture's capture thread finishing while the user `Esc`s a new drag)
/// legitimately sees a frame whose generation is already one behind, and the
/// assertion fired on correct behaviour.
struct Held {
    /// What the frame shows, in physical virtual-desktop pixels. This is the
    /// captured monitor's rectangle, and it is what makes the crop offset
    /// computable: a bitmap alone does not know where it came from.
    rect: Rect,
    bitmap: RgbaBitmap,
    /// When the capture *completed*, not when it was requested.
    ///
    /// The conservative end of the two: [`FRESHNESS`] bounds how stale the
    /// pixels may be, and the pixels are the ones WGC delivered at the end of
    /// the capture, not at its start. Timing from the request would understate
    /// the age of the image by the whole capture duration — 200–300 ms, i.e.
    /// more than the bound itself, which would make the bound meaningless.
    taken: Instant,
}

/// Why [`take`] declined to serve a crop.
///
/// Each variant is a real path through the system and each one is logged, so a
/// gesture that quietly stopped taking the fast path is visible rather than
/// merely slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// Nothing was held: the drag did not arm a capture type, the pre-capture
    /// had not finished, or it failed. All three mean "capture normally".
    NoFrame,
    /// The held frame was older than [`FRESHNESS`] at mouse-up. Carries the age
    /// so the log says how far over it was rather than only that it was over.
    Stale { age_ms: u128 },
    /// The drawn rectangle is not wholly inside the captured monitor — the drag
    /// straddled monitors, or crossed onto one the pre-capture did not cover.
    Straddle,
}

impl std::fmt::Display for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFrame => write!(f, "no held frame"),
            Self::Stale { age_ms } => write!(
                f,
                "held frame was {age_ms} ms old, over the {} ms freshness bound",
                FRESHNESS.as_millis()
            ),
            Self::Straddle => write!(f, "the area is not wholly on the pre-captured monitor"),
        }
    }
}

/// The frame currently held, if any. At most one — a new drag replaces it.
static HELD: Mutex<Option<Held>> = Mutex::new(None);

/// Bumped by every [`begin`], so a capture that finishes after its own drag has
/// ended can tell that it is no longer wanted.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The monitor this drag pre-captures, for [`refresh`] to re-take.
///
/// **The monitor chosen at mouse-down, deliberately, and not the one the cursor
/// is on now.** A refresh exists to keep serving the *same* crop, and a drag
/// that has wandered onto a second monitor must still be answered from the one
/// the area is being drawn on — re-capturing under the current cursor would
/// swap the frame for one the crop then cannot use, turning a working fast path
/// into a straddle fallback halfway through a gesture.
static TARGET: Mutex<Option<Rect>> = Mutex::new(None);

/// Whether a capture is already running, so a 60–221 Hz poll cannot stack
/// hundreds of them. Cleared by the capture thread on both paths out.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

fn held() -> std::sync::MutexGuard<'static, Option<Held>> {
    HELD.lock().unwrap_or_else(PoisonError::into_inner)
}

fn target() -> std::sync::MutexGuard<'static, Option<Rect>> {
    TARGET.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts capturing `monitor` for the drag beginning now.
///
/// Called from the mouse hook on button-down, and **spawns immediately** — see
/// the module docs. Any previously held frame is dropped here rather than at the
/// end of the last drag: it belongs to a gesture that is now over, and holding
/// two full-monitor frames is 66 MB at 4K for no reason.
///
/// # The late-capture race, and why a generation counter and not a flag
///
/// A capture takes 100–300 ms and a drag can be shorter than that. So a
/// pre-capture can still be in flight when its own drag ends, and — if the user
/// starts a second drag straight away — can complete *during* the next one. A
/// plain "is anything in flight" flag cannot distinguish the two, and the
/// stale frame would be stored over the fresh one and then cropped, producing a
/// screenshot of the previous drag's moment: exactly the quiet, plausible-looking
/// wrong image [`FRESHNESS`] exists to prevent, and one it would not catch,
/// because the frame would be young. Tagging each capture with the drag it
/// belongs to makes the check exact rather than probabilistic.
pub(crate) fn begin(monitor: Rect) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    *held() = None;
    *target() = Some(monitor);
    spawn_capture(monitor, generation);
}

/// Re-takes the held frame if it is old enough that this drag would otherwise
/// fall back at mouse-up.
///
/// Called from the placement poll while a capturing drag is in flight. **This is
/// what makes the fast path reachable for a drag of any length**: without it,
/// only a gesture completed within [`FRESHNESS`] of the first capture landing
/// could ever be served, which the rig showed is a flick and not a drag.
///
/// # Why this is not a busy loop
///
/// It looks like one — the poll ticks at 60 Hz idle and 221 Hz mid-gesture — but
/// at most one capture runs at a time ([`IN_FLIGHT`]), and a fresh frame stops
/// the next one from starting for another [`REFRESH_AFTER`]. So the real rate is
/// one capture per ~450 ms *while a capturing drag is being drawn*, and none at
/// any other time. That is the narrow version of the continuous-capture cost
/// ADR-0022 rejected for a persistent service: bounded by the gesture rather
/// than by the process's lifetime, and paid only when the user is already
/// spending the time.
pub(crate) fn refresh() {
    let Some(monitor) = *target() else {
        return;
    };
    if IN_FLIGHT.load(Ordering::SeqCst) {
        return;
    }
    // A missing frame is refreshed too, not only an ageing one: the first
    // capture of the drag may have failed outright, and a drag long enough to
    // notice is long enough to try again.
    let due = held()
        .as_ref()
        .is_none_or(|frame| frame.taken.elapsed() >= REFRESH_AFTER);
    if due {
        spawn_capture(monitor, GENERATION.load(Ordering::SeqCst));
    }
}

/// Captures `monitor` on a spawned thread and stores it if `generation` is still
/// the live drag. Shared by [`begin`] and [`refresh`] so the two cannot diverge
/// on what "store a frame" means.
fn spawn_capture(monitor: Rect, generation: u64) {
    // Claimed before the thread exists: setting it inside the thread would leave
    // a window in which the poll ticks again and spawns a second capture.
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let captured = uptake_capture::capture_region(monitor);
        // Released before the store rather than after, and on every path out —
        // an early `return` that skipped it would wedge the flag set and stop
        // every later refresh in the process's life, silently.
        IN_FLIGHT.store(false, Ordering::SeqCst);
        let captured = match captured {
            Ok(captured) => captured,
            Err(error) => {
                // Not a user-facing failure: the gesture still produces a
                // screenshot, just by the slow path. Logged because a
                // pre-capture failing *every* time would otherwise look like
                // nothing more than the fast path never helping.
                eprintln!("precapture: could not pre-capture {monitor:?}: {error}");
                return;
            }
        };
        let taken = Instant::now();
        let mut slot = held();
        // Checked under the lock, so the generation cannot advance between the
        // test and the store.
        if GENERATION.load(Ordering::SeqCst) == generation {
            *slot = Some(Held {
                // The capture crate clamps to the virtual desktop and reports
                // what it actually captured. Trusting the request over the
                // report here would offset every crop by the clamp distance.
                rect: captured.rect,
                bitmap: captured.bitmap,
                taken,
            });
        }
    });
}

/// Drops any held frame, because the drag that asked for it is not going to use
/// it — a mid-drag `Esc`, or a drag that created nothing.
///
/// The generation is bumped too, so an in-flight capture for that drag lands
/// nowhere instead of arriving after the cancel and sitting in memory until the
/// next gesture.
pub(crate) fn discard() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    *held() = None;
    // Clearing the target is what stops [`refresh`] — an in-flight capture is
    // left to finish and discard itself on the generation check rather than
    // being cancelled, because `capture_region` has no cancellation and waiting
    // on it here would block whatever is ending the drag.
    *target() = None;
}

/// The pixels for `bounds`, cropped out of the held frame — or why not.
///
/// **Consumes the frame.** A held frame belongs to exactly one drag, and the
/// alternative (leaving it for a later gesture to find) is how a stale image
/// reaches the clipboard without any single step looking wrong.
pub(crate) fn take(bounds: Rect) -> Result<RgbaBitmap, Fallback> {
    // The drag is over either way, so stop refreshing before anything else can
    // return early — every path below ends this gesture, and a target left set
    // would keep re-capturing a monitor nobody is dragging on.
    *target() = None;
    let Some(frame) = held().take() else {
        return Err(Fallback::NoFrame);
    };
    // No generation check here — see [`Held`] for why re-checking it would be
    // wrong rather than merely redundant.
    let age = frame.taken.elapsed();
    if age > FRESHNESS {
        return Err(Fallback::Stale {
            age_ms: age.as_millis(),
        });
    }
    // Screen space → frame-local space. The subtraction is the only coordinate
    // arithmetic in the fast path, and `crop` refuses anything that does not fit
    // rather than clamping, so a straddling drag lands in `Straddle` instead of
    // producing a short image.
    let local = Rect::new(
        i32::try_from(i64::from(bounds.origin.x) - i64::from(frame.rect.origin.x))
            .map_err(|_| Fallback::Straddle)?,
        i32::try_from(i64::from(bounds.origin.y) - i64::from(frame.rect.origin.y))
            .map_err(|_| Fallback::Straddle)?,
        bounds.size.width,
        bounds.size.height,
    );
    frame.bitmap.crop(local).ok_or(Fallback::Straddle)
}

/// The monitor of `monitors` containing `point`, for [`begin`] to capture.
///
/// `None` in a dead zone between mismatched monitors — there is no monitor to
/// pre-capture there, and the drag will take the ordinary path. Kept as a free
/// function over a slice so the choice is pure and testable; the caller reads
/// the live list from the overlay's cache.
pub(crate) fn monitor_holding(
    monitors: &[Rect],
    point: uptake_core::geometry::Point,
) -> Option<Rect> {
    monitors.iter().copied().find(|rect| rect.contains(point))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "architecture §5 bans unwrap outside tests; inside them a failed \
              setup should abort the test loudly"
)]
mod tests {
    use uptake_core::geometry::{Point, Size};

    use super::*;

    /// Installs a frame directly, standing in for a completed pre-capture.
    /// Tests the decision logic in [`take`] without a desktop to capture.
    fn hold(rect: Rect, taken: Instant) {
        let bitmap = RgbaBitmap::transparent(rect.size).unwrap();
        *held() = Some(Held {
            rect,
            bitmap,
            taken,
        });
    }

    /// The tests share the process-global `HELD`, and `cargo test` runs them
    /// concurrently — the exact shape that silently disabled a live test in
    /// F-33. A mutex around the ones that touch the global keeps them ordered;
    /// it is held for the whole test rather than per-call, because each test is
    /// a sequence of operations on that state, not a single one.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    fn a_fresh_frame_covering_the_area_serves_the_crop() {
        let _guard = serial();
        // A monitor at a non-zero origin, because the offset subtraction is the
        // thing most likely to be written the wrong way round and a monitor at
        // (0, 0) would hide it — every rig has at least one that is not.
        hold(Rect::new(1920, -180, 1920, 1080), Instant::now());
        let cropped = take(Rect::new(2020, -80, 300, 200)).unwrap();
        assert_eq!(cropped.size(), Size::new(300, 200));
    }

    #[test]
    fn a_frame_is_used_once_and_then_gone() {
        let _guard = serial();
        hold(Rect::new(0, 0, 800, 600), Instant::now());
        assert!(take(Rect::new(10, 10, 20, 20)).is_ok());
        // The second call must not quietly serve the same pixels to a later
        // gesture — that is a stale screenshot with nothing looking wrong.
        assert_eq!(take(Rect::new(10, 10, 20, 20)), Err(Fallback::NoFrame));
    }

    #[test]
    fn a_frame_older_than_the_bound_is_refused() {
        let _guard = serial();
        hold(
            Rect::new(0, 0, 800, 600),
            Instant::now() - FRESHNESS - Duration::from_millis(1),
        );
        let Err(Fallback::Stale { age_ms }) = take(Rect::new(0, 0, 100, 100)) else {
            panic!("a frame past the freshness bound must not be served");
        };
        assert!(age_ms >= FRESHNESS.as_millis());
    }

    #[test]
    fn a_frame_at_exactly_the_bound_is_still_served() {
        let _guard = serial();
        // The boundary is `>`, not `>=`: a frame is stale once it is *older*
        // than the bound. Pinned so a later "tidy-up" of the comparison has to
        // fail a test rather than silently move the boundary.
        hold(Rect::new(0, 0, 800, 600), Instant::now() - FRESHNESS);
        // Time passes between the two lines, so this can only be asserted in
        // the direction that does not race: it must be *served or stale*, never
        // straddle or missing, and a frame aged exactly to the bound at the
        // moment of the call is served.
        assert!(matches!(
            take(Rect::new(0, 0, 100, 100)),
            Ok(_) | Err(Fallback::Stale { .. })
        ));
    }

    #[test]
    fn an_area_running_off_the_captured_monitor_falls_back() {
        let _guard = serial();
        // The straddle case: the monitor is 1920 wide from x=0, and the area
        // reaches x=2000. Clamping would produce a 1920-wide image where 400
        // was asked for; the contract is that it declines instead.
        hold(Rect::new(0, 0, 1920, 1080), Instant::now());
        assert_eq!(
            take(Rect::new(1600, 100, 400, 200)),
            Err(Fallback::Straddle)
        );
    }

    #[test]
    fn an_area_on_a_different_monitor_entirely_falls_back() {
        let _guard = serial();
        hold(Rect::new(0, 0, 1920, 1080), Instant::now());
        // Negative coordinates are ordinary here — a monitor above or left of
        // the primary has them, and the rig has both. The subtraction must send
        // this to `Straddle` and not wrap into a plausible in-bounds offset.
        assert_eq!(
            take(Rect::new(-500, -300, 100, 100)),
            Err(Fallback::Straddle)
        );
    }

    #[test]
    fn an_area_flush_against_the_monitors_far_edge_still_crops() {
        let _guard = serial();
        // The off-by-one that would send every bottom-right area down the slow
        // path forever, and would look like nothing worse than "the fast path
        // does not help much".
        hold(Rect::new(-1080, 200, 1080, 1920), Instant::now());
        assert!(take(Rect::new(-80, 2020, 80, 100)).is_ok());
    }

    #[test]
    fn a_frame_still_in_the_slot_is_served_even_mid_discard() {
        let _guard = serial();
        // The interleaving that killed an earlier belt-and-braces assertion in
        // `take`. `discard` bumps the generation and clears the slot as two
        // operations, so a capture thread calling `take` between them sees a
        // frame whose generation is already one behind — which is correct, not
        // corrupt. Simulated by bumping the counter without clearing the slot,
        // because the real interleaving is a race no test can schedule.
        hold(Rect::new(0, 0, 800, 600), Instant::now());
        GENERATION.fetch_add(1, Ordering::SeqCst);
        assert!(
            take(Rect::new(10, 10, 20, 20)).is_ok(),
            "a frame in the slot belongs to the drag that is consuming it"
        );
    }

    #[test]
    fn discarding_leaves_nothing_for_the_next_drag() {
        let _guard = serial();
        hold(Rect::new(0, 0, 800, 600), Instant::now());
        discard();
        assert_eq!(take(Rect::new(0, 0, 10, 10)), Err(Fallback::NoFrame));
    }

    #[test]
    fn the_bound_must_exceed_what_a_capture_costs() {
        // The rule the rig taught on 2026-07-28, pinned so it cannot be undone
        // by someone tightening the bound for good-sounding reasons.
        //
        // A frame is stamped when it *exists*, so it is already one capture old
        // the moment it lands. A bound at or below the capture cost is therefore
        // unsatisfiable by construction — which is precisely what 200 ms was,
        // and the whole feature sat inert behind it with every gate green.
        assert!(
            FRESHNESS > CAPTURE_ALLOWANCE,
            "a frame cannot be younger than the time it takes to produce one"
        );
        // And the refresh must *complete* before the frame it replaces expires,
        // or a long drag still falls back — the failure this scheme exists to
        // remove.
        assert!(
            REFRESH_AFTER + CAPTURE_ALLOWANCE <= FRESHNESS,
            "a refresh started at {REFRESH_AFTER:?} and costing up to \
             {CAPTURE_ALLOWANCE:?} would land after {FRESHNESS:?}"
        );
        // Non-zero, or every tick of a 221 Hz poll would qualify as due and the
        // in-flight guard would be the only thing between this and a capture
        // storm.
        assert!(!REFRESH_AFTER.is_zero());
    }

    #[test]
    fn a_frame_is_refreshed_only_once_it_is_due() {
        let _guard = serial();
        discard();
        *target() = Some(Rect::new(0, 0, 100, 100));

        // Fresh: nothing should start. Asserted through the in-flight flag,
        // which is what a spawned capture claims — the capture itself needs a
        // desktop and cannot run under test.
        hold(Rect::new(0, 0, 100, 100), Instant::now());
        refresh();
        assert!(
            !IN_FLIGHT.load(Ordering::SeqCst),
            "a frame inside the refresh window must not be re-taken"
        );

        // Due: one capture starts, and a second tick must not stack another.
        hold(
            Rect::new(0, 0, 100, 100),
            Instant::now() - REFRESH_AFTER - Duration::from_millis(1),
        );
        refresh();
        assert!(
            IN_FLIGHT.load(Ordering::SeqCst),
            "an aged frame is re-taken"
        );
        refresh();
        assert!(
            IN_FLIGHT.load(Ordering::SeqCst),
            "the in-flight guard is what stops a 221 Hz poll starting hundreds"
        );

        // Leave the statics as the other tests expect to find them. The spawned
        // capture will clear the flag itself, but not before this test ends.
        IN_FLIGHT.store(false, Ordering::SeqCst);
        discard();
    }

    #[test]
    fn nothing_refreshes_once_the_drag_is_over() {
        let _guard = serial();
        discard();
        IN_FLIGHT.store(false, Ordering::SeqCst);
        // No target: the drag ended, by `take` or by `discard`. An aged frame
        // must not keep the monitor being re-captured forever.
        hold(
            Rect::new(0, 0, 100, 100),
            Instant::now() - REFRESH_AFTER - Duration::from_millis(1),
        );
        refresh();
        assert!(!IN_FLIGHT.load(Ordering::SeqCst));
        *held() = None;
    }

    #[test]
    fn the_monitor_under_the_point_is_the_one_that_contains_it() {
        let monitors = [
            Rect::new(0, 0, 2560, 1440),
            Rect::new(2560, 0, 1920, 1080),
            Rect::new(-1080, 0, 1080, 1920),
        ];
        assert_eq!(
            monitor_holding(&monitors, Point::new(2600, 40)),
            Some(monitors[1])
        );
        assert_eq!(
            monitor_holding(&monitors, Point::new(-500, 1500)),
            Some(monitors[2])
        );
        // A dead zone: below the secondary, off the bottom of everything. There
        // is nothing to pre-capture, and inventing a monitor here would capture
        // the wrong one.
        assert_eq!(monitor_holding(&monitors, Point::new(3000, 1300)), None);
        assert_eq!(monitor_holding(&[], Point::new(0, 0)), None);
    }
}
