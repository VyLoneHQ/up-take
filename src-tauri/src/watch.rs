//! The host half of watch areas (roadmap `1.58`, `ADR-0048`): reading each
//! watching area's region on a timer and raising a Windows notification when
//! what it shows changes, or stops changing.
//!
//! The rule for *what counts* as a change is `uptake_core::watch`, which needs
//! no screen and is tested there. This module takes the readings and says so.
//!
//! # When it reads, and when it does not
//!
//! `ADR-0048` admits an area that reads its own region repeatedly on four
//! conditions, and three of them are held here:
//!
//! - **Switched on per area, off by default.** An area is read only when
//!   `Area::is_watching` says so: a watch area, switched on, and drawn. Every
//!   area starts `Off` (`uptake_core::area`).
//! - **Visibly marked while it reads.** The page draws the *watching* mark
//!   from the same `Area::is_watching`, sent in the area payload, so the two
//!   answer one question once. Nothing is read while the overlay is hidden,
//!   because the mark is hidden with it.
//! - **Local.** A reading is reduced to a few hundred brightness averages
//!   and the picture dropped; the detector keeps two such summaries, the
//!   last still one and the last one. Nothing is written, logged or sent;
//!   the notification says which moment it was, never what was read.
//!
//! The fourth, *the user placed it*, is true of every area.
//!
//! **The mark stays on screen while a reading is taken.** The capture does not
//! step UP-TAKE's drawing aside, unlike every other capture (`output.rs`,
//! `capture_for_watch`): the area's border and mark are left in the picture
//! and left out of the comparison ([`own_drawing`]).
//!
//! **An area whose mark is not wholly on a monitor does not read** ([`mark`],
//! [`mark_on_screen`]). The mark sits in the area's top right corner, and an
//! area may hang off the desktop or into a gap between monitors; the capture
//! would still read the part that is on screen while the mark is drawn
//! nowhere, which is `ADR-0048`'s first *Revisit if* (review of `#154`,
//! round 3). Checked against the corner left out for the mark, which is
//! larger than the mark, so it errs on the side of reading less.
//!
//! **Each of these is asked again right before each capture**, not once a
//! pass, so an overlay hidden or a move started partway through a pass stops
//! the readings that follow it.
//!
//! **An area being moved or resized does not read**, because the page draws
//! it as a preview without its mark, and its readings start again from a
//! fresh baseline when the gesture ends.
//!
//! ⚠️ **Known gap, left to its own change on the founder's decision
//! (2026-10-31, `I-475`).** With *Show UP-TAKE in screen recordings* on, a
//! freeze or another area's capture steps UP-TAKE's drawing aside for a
//! few hundred milliseconds (`freeze::STEP_ASIDE`), and a reading can land
//! inside that gap while the mark is hidden (review of `#154`, round 5).
//! Each check above closes one route by asking the page's state; rounds 2
//! to 5 each found another. `ADR-0048`'s *Revisit if* names the fix that
//! closes the class: the reader enforces the mark itself rather than
//! trusting what the page should be drawing. That is `I-475`, not this
//! module's checks.
//!
//! ⚠️ **With *Show UP-TAKE in screen recordings* on, UP-TAKE's own drawing
//! is in every reading, and only the border and the mark are left out.**
//! The largest other part, Placement's darkened edges, comes and goes with
//! the overlay state, so every watcher starts again from a fresh baseline
//! when the state changes (review of `#154`, round 2). What is left: a
//! menu, a selection box or another area's flash drawn over a watched
//! region while the user works there can count as a change. The setting
//! is off by default.
//!
//! # Cost
//!
//! ⚠️ **Not measured yet, and the interval is a first value.** Each reading is
//! one `uptake_capture::capture_region`, which builds a capture session per
//! call (about 100 to 350 ms, mostly waiting, per that crate's notes), every
//! [`INTERVAL`] for each watching area. Roadmap `1.58` requires the rate and
//! its cost to be measured against `quality-bars.md` section 1 before the
//! default is chosen; that measurement is owed.
//!
//! # The thread
//!
//! One thread, started the first time any area is switched to watching and
//! never stopped. While no watch area is switched on it waits on a condition
//! variable and costs nothing; [`wake`] is what moves it. While one is
//! switched on, it looks once per [`INTERVAL`], whether or not that area can
//! be read now: a paused sticky area follows its window again, or the overlay
//! comes back, with nothing to wake the thread, so it has to look. Round 1 of
//! the review of `#154` found it asleep for good after a pause.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use uptake_core::area::{AreaId, AreaStore, AreaType};
use uptake_core::geometry::Rect;
use uptake_core::watch::{Detector, Event, Fingerprint, Watch, leave_out};

use crate::overlay_state::OverlayState;

use crate::strings::{Text, text};

/// How often a watching area reads its region. A first value; see the module
/// docs' *Cost*.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// How wide a band along each edge of the area is left out of a reading, in
/// physical pixels: the border, 1.5 CSS pixels, at up to 250 % scaling.
const EDGE: u32 = 4;

/// The corner left out for the *watching* mark, in physical pixels: the
/// mark's text at 11 CSS pixels, in either language, at up to 200 % scaling,
/// with room to spare.
const MARK: (u32, u32) = (240, 40);

/// Whether the thread exists, and whether it has been woken since it last
/// looked.
struct Bell {
    started: bool,
    rung: bool,
}

static BELL: Mutex<Bell> = Mutex::new(Bell {
    started: false,
    rung: false,
});
static RINGING: Condvar = Condvar::new();

fn bell() -> std::sync::MutexGuard<'static, Bell> {
    BELL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Tells the watcher that what it should watch may have changed, starting it
/// the first time. Called when an area's [`Watch`] is set.
pub fn wake(app: &AppHandle) {
    let mut bell = bell();
    bell.rung = true;
    if !bell.started {
        let app = app.clone();
        match std::thread::Builder::new()
            .name("uptake-watch".into())
            .spawn(move || run(&app))
        {
            Ok(_) => bell.started = true,
            Err(error) => crate::diagnostics::trouble(
                "watch: could not start the watcher, so no watch area will notify",
                &error,
            ),
        }
    }
    RINGING.notify_one();
}

/// Waits until [`wake`] rings, or `timeout` passes when there is one.
fn wait(timeout: Option<Duration>) {
    let mut bell = bell();
    match timeout {
        None => {
            while !bell.rung {
                bell = RINGING.wait(bell).unwrap_or_else(PoisonError::into_inner);
            }
        }
        Some(timeout) if !bell.rung => {
            bell = RINGING
                .wait_timeout(bell, timeout)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Some(_) => {}
    }
    bell.rung = false;
}

/// One area to read: where, and what for.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    id: AreaId,
    bounds: Rect,
    watch: Watch,
}

/// What the store says now: the areas to read, and whether any watch area is
/// switched on at all, readable or not. Takes the store lock for the copy and
/// nothing else.
fn look(app: &AppHandle) -> (Vec<Target>, bool) {
    let store = app.state::<Mutex<AreaStore>>();
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    let switched_on = store
        .iter()
        .any(|area| area.kind == AreaType::Watch && area.watch.is_on());
    let targets = store
        .iter()
        .filter(|area| area.is_watching())
        .map(|area| Target {
            id: area.id,
            bounds: area.bounds,
            watch: area.watch,
        })
        .collect();
    (targets, switched_on)
}

/// Whether `target` is still what the store holds and still reads: switched
/// on, unmoved, not paused. Checked right before its capture.
fn still_watching(app: &AppHandle, target: Target) -> bool {
    let store = app.state::<Mutex<AreaStore>>();
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    store.get(target.id).is_some_and(|area| {
        area.is_watching() && area.bounds == target.bounds && area.watch == target.watch
    })
}

fn overlay_state(app: &AppHandle) -> OverlayState {
    let state = app.state::<Mutex<OverlayState>>();
    *state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the thread does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// No watch area is switched on: wait for [`wake`], however long.
    Sleep,
    /// One is switched on and none can be read now (paused, or the overlay
    /// is hidden): look again in [`INTERVAL`], because nothing wakes the
    /// thread when that changes.
    Look,
    /// Read the readable ones.
    Read,
}

/// The thread's one decision, apart so it can be tested. Round 1 of the
/// review of `#154` found the thread asleep for good after a pause, and
/// round 2 found the fix held by no test.
const fn next(switched_on: bool, readable: bool, showing: bool) -> Next {
    if !switched_on {
        Next::Sleep
    } else if readable && showing {
        Next::Read
    } else {
        Next::Look
    }
}

/// What one area's watcher remembers between readings.
struct Watcher {
    target: Target,
    detector: Detector,
    /// Whether the last reading failed, so a failure that lasts (an area left
    /// on a monitor that was unplugged) is logged once and not every
    /// [`INTERVAL`].
    failing: bool,
}

/// The parts of a reading that are UP-TAKE's own drawing, in the reading's
/// pixels: a band along each edge for the border, and the top right corner
/// for the *watching* mark. `bounds` is the area, `taken` the rectangle the
/// capture returned, which is `bounds` clamped to the desktop.
fn own_drawing(bounds: Rect, taken: Rect) -> [Rect; 5] {
    let (dx, dy) = (
        bounds.origin.x - taken.origin.x,
        bounds.origin.y - taken.origin.y,
    );
    let (width, height) = (bounds.size.width, bounds.size.height);
    let right = dx + i32::try_from(width).unwrap_or(i32::MAX);
    let bottom = dy + i32::try_from(height).unwrap_or(i32::MAX);
    let edge = i32::try_from(EDGE).unwrap_or(0);
    let mark_width = i32::try_from(MARK.0.min(width)).unwrap_or(0);
    [
        Rect::new(dx, dy, width, EDGE),
        Rect::new(dx, bottom - edge, width, EDGE),
        Rect::new(dx, dy, EDGE, height),
        Rect::new(right - edge, dy, EDGE, height),
        Rect::new(
            right - mark_width,
            dy,
            MARK.0.min(width),
            MARK.1.min(height),
        ),
    ]
}

/// Where the *watching* mark can be, in desktop pixels: the top right corner
/// of `bounds` that [`own_drawing`] leaves out.
fn mark(bounds: Rect) -> Rect {
    let width = MARK.0.min(bounds.size.width);
    let right = bounds.origin.x + i32::try_from(bounds.size.width).unwrap_or(i32::MAX);
    Rect::new(
        right - i32::try_from(width).unwrap_or(0),
        bounds.origin.y,
        width,
        MARK.1.min(bounds.size.height),
    )
}

/// Whether the whole of `mark` is on one monitor, so the user can see it.
fn mark_on_screen(mark: Rect, monitors: &[Rect]) -> bool {
    monitors
        .iter()
        .any(|monitor| mark.intersection(*monitor) == Some(mark))
}

fn run(app: &AppHandle) {
    // Each area's watcher, with the target it was made for: an area that
    // moved, was resized or was given another choice starts again from a fresh
    // baseline rather than reporting its own move as a change.
    let mut watchers: HashMap<AreaId, Watcher> = HashMap::new();
    let mut last_state = None;
    loop {
        let (targets, switched_on) = look(app);
        let state = overlay_state(app);
        // A change of overlay state changes what UP-TAKE draws over the
        // screen, which a reading sees when the overlay is in captures.
        if last_state != Some(state) {
            watchers.clear();
            last_state = Some(state);
        }
        match next(
            switched_on,
            !targets.is_empty(),
            state != OverlayState::Hidden,
        ) {
            Next::Sleep => {
                watchers.clear();
                wait(None);
                continue;
            }
            Next::Look => {
                watchers.clear();
                wait(Some(INTERVAL));
                continue;
            }
            Next::Read => {}
        }
        let moving: Option<AreaId> = crate::placement::area_in_gesture();
        watchers
            .retain(|id, _| Some(*id) != moving && targets.iter().any(|target| target.id == *id));
        let monitors = crate::overlay::monitor_rects();
        for target in targets {
            // Asked again right before each capture, not only once a pass: a
            // pass reads its areas one after another, each capture taking up
            // to a few hundred milliseconds, and the overlay can be hidden, a
            // move started or watching switched off between two of them
            // (review of `#154`, round 4). A capture already under way cannot
            // be stopped; this keeps the window to that one.
            if overlay_state(app) != state {
                break;
            }
            let moving = moving.or_else(crate::placement::area_in_gesture);
            if Some(target.id) == moving
                || !still_watching(app, target)
                || !mark_on_screen(mark(target.bounds), &monitors)
            {
                // Not read, and read again from a fresh baseline.
                watchers.remove(&target.id);
                continue;
            }
            let watcher = watchers.entry(target.id).or_insert_with(|| Watcher {
                target,
                detector: Detector::new(),
                failing: false,
            });
            if watcher.target != target {
                watcher.target = target;
                watcher.detector = Detector::new();
            }
            let reading = match crate::output::capture_for_watch(target.bounds) {
                Ok(mut captured) => {
                    watcher.failing = false;
                    leave_out(
                        &mut captured.bitmap,
                        &own_drawing(target.bounds, captured.rect),
                    );
                    Fingerprint::of(&captured.bitmap)
                }
                Err(error) => {
                    if !watcher.failing {
                        crate::diagnostics::trouble(
                            "watch: a reading could not be taken (said once until one is)",
                            &error,
                        );
                    }
                    watcher.failing = true;
                    None
                }
            };
            if let Some(event) = reading.and_then(|reading| watcher.detector.observe(reading))
                && target.watch.notifies(event)
            {
                notify(app, event);
            }
        }
        wait(Some(INTERVAL));
    }
}

/// Raises the Windows notification for `event`. Says which moment it was and
/// never what the area read.
fn notify(app: &AppHandle, event: Event) {
    use tauri_plugin_notification::NotificationExt;
    let body = match event {
        Event::Changed => text(Text::WatchChanged),
        Event::Settled => text(Text::WatchSettled),
    };
    if let Err(error) = app
        .notification()
        .builder()
        .title(text(Text::WatchTitle))
        .body(body)
        .show()
    {
        crate::diagnostics::trouble("watch: the notification could not be shown", &error);
    }
}

#[cfg(test)]
mod tests {
    use super::{EDGE, MARK, Next, mark, mark_on_screen, next, own_drawing};

    /// The mark is on screen only when its whole corner is on one monitor:
    /// not past the desktop's right edge, not above its top, not across the
    /// gap where a shorter monitor ends.
    #[test]
    fn the_mark_must_be_wholly_on_one_monitor() {
        let left = Rect::new(0, 0, 1920, 1080);
        let right = Rect::new(1920, 0, 1280, 720);
        let monitors = [left, right];
        let inside = Rect::new(100, 100, 600, 400);
        assert_eq!(mark(inside), Rect::new(700 - 240, 100, MARK.0, MARK.1));
        assert!(mark_on_screen(mark(inside), &monitors));
        // Past the right edge of the rightmost monitor.
        assert!(!mark_on_screen(
            mark(Rect::new(2900, 100, 600, 400)),
            &monitors
        ));
        // Above the top of the desktop.
        assert!(!mark_on_screen(
            mark(Rect::new(100, -20, 600, 400)),
            &monitors
        ));
        // Below the shorter right monitor, beside the taller left one.
        assert!(!mark_on_screen(
            mark(Rect::new(1700, 900, 600, 100)),
            &monitors
        ));
        // Across the seam between two monitors: on neither wholly.
        assert!(!mark_on_screen(
            mark(Rect::new(1500, 100, 500, 300)),
            &monitors
        ));
        assert!(!mark_on_screen(mark(inside), &[]), "no monitors, no mark");
    }

    /// The thread sleeps without a timeout only while no watch area is
    /// switched on; a switched-on area it cannot read now is looked at again.
    #[test]
    fn it_sleeps_only_when_nothing_is_switched_on() {
        for readable in [false, true] {
            for showing in [false, true] {
                assert_eq!(next(false, readable, showing), Next::Sleep);
            }
        }
        assert_eq!(next(true, false, true), Next::Look, "a paused sticky area");
        assert_eq!(next(true, true, false), Next::Look, "the overlay hidden");
        assert_eq!(next(true, false, false), Next::Look);
        assert_eq!(next(true, true, true), Next::Read);
    }
    use uptake_core::geometry::Rect;

    /// The parts left out sit on the area's own edges and its top right
    /// corner, in the reading's pixels.
    #[test]
    fn own_drawing_is_the_border_and_the_marks_corner() {
        let bounds = Rect::new(100, 50, 600, 400);
        let parts = own_drawing(bounds, bounds);
        assert_eq!(parts[0], Rect::new(0, 0, 600, EDGE));
        assert_eq!(parts[1], Rect::new(0, 400 - 4, 600, EDGE));
        assert_eq!(parts[2], Rect::new(0, 0, EDGE, 400));
        assert_eq!(parts[3], Rect::new(600 - 4, 0, EDGE, 400));
        assert_eq!(parts[4], Rect::new(600 - 240, 0, MARK.0, MARK.1));
    }

    /// An area hanging off the desktop's left edge: the capture starts 30
    /// pixels into it, so every part moves left by 30.
    #[test]
    fn own_drawing_follows_a_clamped_capture() {
        let bounds = Rect::new(-30, 0, 300, 200);
        let taken = Rect::new(0, 0, 270, 200);
        let parts = own_drawing(bounds, taken);
        assert_eq!(parts[0], Rect::new(-30, 0, 300, EDGE));
        assert_eq!(parts[4], Rect::new(270 - 240, 0, MARK.0, MARK.1));
    }

    /// An area smaller than the mark's corner leaves out what it has.
    #[test]
    fn a_small_area_leaves_out_no_more_than_itself() {
        let bounds = Rect::new(0, 0, 100, 30);
        assert_eq!(own_drawing(bounds, bounds)[4], Rect::new(0, 0, 100, 30));
    }
}
