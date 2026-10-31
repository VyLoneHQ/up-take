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
//! - **Local.** A reading is reduced to at most 256 brightness averages
//!   (a region under 16 pixels on a side keeps one per pixel on that side,
//!   which is its greyscale picture), and the detector keeps two such
//!   summaries, the last still one and the last one. Nothing is written,
//!   logged or sent; the notification says which moment it was, never what
//!   was read. ⚠️ **What is held in between is larger than a reading:** while
//!   any area reads, a capture session on each monitor under one keeps that
//!   monitor's newest frame as a GPU texture (`uptake_capture::warm`), and
//!   each pass copies it to memory, cuts the areas out and drops the copy at
//!   the end of the pass. Only the areas' parts are read; the whole frame is
//!   what the session holds. `ADR-0048` forecloses a whole-screen *watcher*,
//!   which this is not, but it is the reason a held frame stays in memory and
//!   is never written anywhere.
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
//! round 3). Checked against the area's top right corner, which holds the
//! mark because the page clips the mark to the area (rounds 15 and 16).
//!
//! **Each of these is asked again right before each capture**, not once a
//! pass, so an overlay hidden or a move started partway through a pass stops
//! the readings that follow it.
//!
//! **An area being moved or resized does not read**, because the page draws
//! it as a preview without its mark, and its readings start again from a
//! fresh baseline when the gesture ends.
//!
//! **The page confirms the mark** ([`confirm`], the founder's choice of
//! 2026-10-31). Every check above asks the host's own state, and rounds 2 to
//! 19 of the review of `#154` each found another way for the mark to be
//! unseen while those checks passed (UP-TAKE backlog `I-475`). `ADR-0048`'s
//! *Revisit if* names the fix for the class: the reader enforces the mark.
//! The page measures each watching mark once it is laid out and painted, and
//! an area is read only while the page's latest word, at most
//! [`CONFIRMATION_LIFETIME`] old, says its mark is seen at the bounds and in
//! the overlay state being read. That closes:
//!
//! - the host ahead of the page: switching watching on, a sticky area
//!   resuming, a move or resize ending (round 7);
//! - another area drawn over the mark (round 14);
//! - the mark clipped away on a small area, at any scaling (rounds 17, 18);
//! - the mark reaching past the host's corner in a long language (round 19).
//!
//! ⚠️ **Two routes stay open**, and remain `I-475`'s:
//!
//! - With *Show UP-TAKE in screen recordings* on, a freeze or another
//!   area's capture steps UP-TAKE's drawing aside for a few hundred
//!   milliseconds (`freeze::STEP_ASIDE`), after the page's word and without
//!   the page's knowledge; a reading can land inside that gap (round 5).
//! - A reading uses a held session's newest frame, which can be up to
//!   `warm::WATCH_FRAME_INTERVAL` (one second) older than the page's word; a
//!   frame from just before the mark was painted can be the one read (round
//!   12). A watcher made or started again waits one pass before its first
//!   reading ([`Watcher`]), which keeps such a frame out of a baseline, not
//!   out of every reading.
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
//! **Readings come from capture sessions held on the monitors under watching
//! areas** (`uptake_capture::warm::watch_hold`), each sent at most one frame
//! a second; a pass reads each monitor's frame once and cuts every area out
//! of it (`warm::compose`). The founder's choice, 2026-10-31 (UP-TAKE backlog
//! `I-474`), after the measurement in workspace `evidence/1.58-watch-cost/`:
//!
//! - **This route: 0.13 % of a core for one area and for five**, every
//!   [`INTERVAL`], on a still screen.
//! - The first route, a capture built per reading, cost 7.3 % for one area
//!   and 16.5 % for five, against `quality-bars.md` section 1's 2 % per live
//!   area (hard fail 5 %).
//! - ⚠️ **A busy screen under a held session is not measured for this
//!   route.** `ADR-0026` measured an unthrottled one at +1.33 points with
//!   video playing; these are throttled to one frame a second.
//!
//! The sessions are let go whenever nothing can be read (switched off,
//! paused, overlay hidden). For about a third of a second after they are
//! taken, and on a monitor WGC cannot hold, a reading falls back to one
//! capture of its own (`output.rs`, `capture_for_watch`), at the old cost.
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
use std::time::{Duration, Instant};

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
/// mark's text at 11 CSS pixels, in either language, at up to about 230 %
/// scaling. ⚠️ **Width binds first**: the German mark is about 99 to 114
/// CSS pixels wide (Windows font engines, not WebView2), so from about 230 %
/// it is wider than 240, and from about 300 % taller than 40. The page clips
/// the mark to the area, not to this corner, so on a wide area the dot can
/// then lie outside the corner [`mark_on_screen`] checks (review of `#154`,
/// round 19). Named among `I-475`'s routes in the module docs.
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

/// The page's latest word on which watching marks it can see (roadmap
/// `1.58`, `ADR-0048`'s *Revisit if*: the mark enforced by the reader).
///
/// The page measures each watching area's mark once laid out and painted: its
/// dot wholly inside the badge, on a monitor, under no other area
/// (`src/lib/mark-check.ts`). It sends the areas whose mark is seen, with the
/// bounds it drew them at and the overlay state it drew in, after every change
/// and every second. An area is read only while the latest word names it at
/// its present bounds, in the present state, and is no older than
/// [`CONFIRMATION_LIFETIME`]: a page that stopped answering stops the reading.
///
/// **What it closes** (UP-TAKE backlog `I-475`): the host ahead of the page
/// (round 7), the mark covered by another area (round 14), the mark clipped
/// away on a small area or at high scaling (rounds 17 and 18), and a mark
/// wider than the host's corner (round 19), since the page measures the mark
/// it drew. **What it does not**: a freeze stepping the drawing aside after
/// the page's word (round 5), and a held frame older than the word (round 12).
struct Confirmation {
    state: OverlayState,
    marks: Vec<(u64, Rect)>,
    at: Instant,
}

static CONFIRMED: Mutex<Option<Confirmation>> = Mutex::new(None);

/// How long a confirmation stands. The page sends one every second while any
/// mark is seen, so three missed in a row stop the reading.
pub const CONFIRMATION_LIFETIME: Duration = Duration::from_secs(3);

/// Records the page's latest confirmation ([`Confirmation`]).
pub fn confirm(state: OverlayState, marks: Vec<(u64, Rect)>) {
    *CONFIRMED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Confirmation {
        state,
        marks,
        at: Instant::now(),
    });
}

/// Whether the latest confirmation, as of `now`, says `target`'s mark is seen.
fn mark_seen(target: Target, state: OverlayState, now: Instant) -> bool {
    CONFIRMED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|word| seen_by(word, target, state, now))
}

/// [`mark_seen`]'s rule, apart so it is tested without the shared store.
fn seen_by(word: &Confirmation, target: Target, state: OverlayState, now: Instant) -> bool {
    word.state == state
        && now.saturating_duration_since(word.at) <= CONFIRMATION_LIFETIME
        && word
            .marks
            .iter()
            .any(|&(id, bounds)| id == target.id.get() && bounds == target.bounds)
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
    /// Made, or started again, in this pass: it reads from the next pass on.
    ///
    /// A reading comes from a held session's newest frame, which can be up to
    /// [`uptake_capture::warm::WATCH_FRAME_INTERVAL`] older than the checks
    /// made before it. A baseline taken in the pass that started a watcher
    /// again (the overlay state changed, the setting flipped, a move ended)
    /// could still show what came before, and the next reading would report
    /// that as a change (review of `#154`, round 12). One [`INTERVAL`] later
    /// the frame should be newer than the reason it started again.
    ///
    /// ⚠️ **Assumed, not measured:** that WGC, held at
    /// `WATCH_FRAME_INTERVAL`, delivers a frame within that interval of a
    /// change on screen. Nothing here probes frame age under the throttle
    /// (round 13); UP-TAKE backlog `I-474` carries it.
    fresh: bool,
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

/// Whether UP-TAKE's own drawing is in captures (*Show UP-TAKE in screen
/// recordings*, `ADR-0019`).
fn in_captures() -> bool {
    crate::settings::current().show_in_screen_recordings
}

/// What a reading leaves out. Nothing while UP-TAKE is kept out of captures,
/// which is the default (`ADR-0019`): its drawing is not in the reading, and
/// leaving the corner out anyway cost every area its top right corner.
/// With *Show UP-TAKE in screen recordings* on, [`own_drawing`], except the
/// mark's corner when that would leave nothing to read: an area no wider
/// than the corner and no taller than it read solid black and could never
/// notify while it said *watching* (review of `#154`, round 8). The mark is
/// still, so leaving it in costs no false change.
fn left_out(bounds: Rect, taken: Rect, overlay_in_captures: bool) -> Vec<Rect> {
    if !overlay_in_captures {
        return Vec::new();
    }
    let parts = own_drawing(bounds, taken);
    // Decided on the picture actually taken, which is `bounds` clamped to
    // the desktop: an area hanging off an edge keeps only part of itself
    // (review of `#154`, round 10, after round 8 decided it on `bounds`).
    let picture = Rect::new(0, 0, taken.size.width, taken.size.height);
    let inside = Rect::new(
        bounds.origin.x - taken.origin.x + i32::try_from(EDGE).unwrap_or(0),
        bounds.origin.y - taken.origin.y + i32::try_from(EDGE).unwrap_or(0),
        bounds.size.width.saturating_sub(2 * EDGE),
        bounds.size.height.saturating_sub(2 * EDGE),
    )
    .intersection(picture);
    match inside {
        // Nothing inside the border is on screen: leave it all in, since the
        // border is as still as the mark.
        None => Vec::new(),
        Some(inside) if parts[4].intersection(inside) == Some(inside) => parts[..4].to_vec(),
        Some(_) => parts.to_vec(),
    }
}

/// Where the *watching* mark can be, in desktop pixels: the area's top right
/// corner, [`MARK`] in size or the area's own size when that is smaller.
///
/// The host's own first check, before the page's word ([`confirm`]): an area
/// whose corner is off every monitor is not read, whatever the page says. It
/// is not the whole of the check, and cannot be (rounds 15, 16 and 19): the
/// page clips the mark to the area (`+page.svelte`, `.watch-badge`, pinned by
/// `watch-badge-css.test.ts`) and measures the mark it drew.
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

/// What the reading loop asks of the app, apart so the loop itself can be
/// tested with a stand-in (UP-TAKE backlog `I-473`: rounds 6 to 10 of the
/// review of `#154` each found a defect in this loop's glue, which no test
/// held).
trait Host {
    /// The areas to read, and whether any watch area is switched on.
    fn look(&mut self) -> (Vec<Target>, bool);
    /// The overlay state, and whether UP-TAKE's drawing is in captures.
    fn state(&mut self) -> (OverlayState, bool);
    /// The area a move or resize is under way on, if any.
    fn in_gesture(&mut self) -> Option<AreaId>;
    /// Whether `target` is still what the store holds and still reads.
    fn still_watching(&mut self, target: Target) -> bool;
    /// Whether the page's latest confirmation says `target`'s mark is seen,
    /// at these bounds and in `state` ([`confirm`]).
    fn mark_seen(&mut self, target: Target, state: OverlayState) -> bool;
    /// Every monitor's rectangle.
    fn monitors(&mut self) -> Vec<Rect>;
    /// Keeps capture sessions held on the monitors under `regions`, and
    /// starts a pass: frames read in the last pass are not reused.
    fn hold(&mut self, regions: &[Rect]);
    /// Lets every held session go: nothing will be read for a while.
    fn release(&mut self);
    /// The pass is over: drop what it read.
    fn end_pass(&mut self);
    /// One reading of `bounds`.
    fn capture(&mut self, bounds: Rect) -> Result<uptake_capture::CapturedRegion, String>;
    /// A reading failed; logged once until one succeeds.
    fn trouble(&mut self, error: &str);
    /// Raises the notification for `event`.
    fn notify(&mut self, event: Event);
}

/// The app itself, and the monitor frames read in this pass.
struct AppHost<'a> {
    app: &'a AppHandle,
    /// Each monitor's frame, read once a pass however many areas it holds.
    frames: Vec<uptake_capture::CapturedRegion>,
}

impl Host for AppHost<'_> {
    fn look(&mut self) -> (Vec<Target>, bool) {
        look(self.app)
    }
    fn hold(&mut self, regions: &[Rect]) {
        self.frames.clear();
        let _ = uptake_capture::warm::watch_hold(regions);
    }
    fn release(&mut self) {
        self.frames.clear();
        uptake_capture::warm::watch_release();
    }
    fn end_pass(&mut self) {
        self.frames.clear();
    }
    fn state(&mut self) -> (OverlayState, bool) {
        (overlay_state(self.app), in_captures())
    }
    fn in_gesture(&mut self) -> Option<AreaId> {
        crate::placement::area_in_gesture()
    }
    fn still_watching(&mut self, target: Target) -> bool {
        still_watching(self.app, target)
    }
    fn mark_seen(&mut self, target: Target, state: OverlayState) -> bool {
        mark_seen(target, state, Instant::now())
    }
    fn monitors(&mut self) -> Vec<Rect> {
        crate::overlay::monitor_rects()
    }
    fn capture(&mut self, bounds: Rect) -> Result<uptake_capture::CapturedRegion, String> {
        // From the held sessions: each monitor's frame read once a pass, the
        // area cut out of it. A monitor whose session is still warming up
        // (about a third of a second after `hold`), or could not be held,
        // falls back to one capture of its own for this reading.
        for monitor in crate::overlay::monitor_rects() {
            if monitor.intersection(bounds).is_none()
                || self
                    .frames
                    .iter()
                    .any(|frame| frame.rect.intersection(monitor) == Some(monitor))
            {
                continue;
            }
            match uptake_capture::warm::watch_capture_monitor(monitor) {
                Some(frame) => self.frames.push(frame),
                None => return crate::output::capture_for_watch(bounds),
            }
        }
        uptake_capture::warm::compose(bounds, &self.frames, &crate::overlay::monitor_rects())
            .map_or_else(|| crate::output::capture_for_watch(bounds), Ok)
    }
    fn trouble(&mut self, error: &str) {
        crate::diagnostics::trouble(
            "watch: a reading could not be taken (said once until one is)",
            &error,
        );
    }
    fn notify(&mut self, event: Event) {
        notify(self.app, event);
    }
}

/// What the loop remembers between passes.
#[derive(Default)]
struct Memory {
    /// Each area's watcher, with the target it was made for: an area that
    /// moved, was resized or was given another choice starts again from a
    /// fresh baseline rather than reporting its own move as a change.
    watchers: HashMap<AreaId, Watcher>,
    /// The overlay state and the setting at the last pass.
    last_state: Option<(OverlayState, bool)>,
}

fn run(app: &AppHandle) {
    let mut host = AppHost {
        app,
        frames: Vec::new(),
    };
    let mut memory = Memory::default();
    loop {
        let timeout = pass(&mut host, &mut memory);
        wait(timeout);
    }
}

/// One pass over the watching areas. Returns how long to wait before the
/// next: `None` means until [`wake`] rings.
fn pass(host: &mut impl Host, memory: &mut Memory) -> Option<Duration> {
    let watchers = &mut memory.watchers;
    let (targets, switched_on) = host.look();
    // What UP-TAKE draws over the screen, and whether a reading sees it
    // and leaves it out: a change of either changes every reading
    // without the screen changing, so every watcher starts again
    // (the overlay state, round 2; the setting, round 9).
    let state = host.state();
    if memory.last_state != Some(state) {
        watchers.clear();
        memory.last_state = Some(state);
    }
    match next(
        switched_on,
        !targets.is_empty(),
        state.0 != OverlayState::Hidden,
    ) {
        Next::Sleep => {
            watchers.clear();
            host.release();
            return None;
        }
        Next::Look => {
            watchers.clear();
            host.release();
            return Some(INTERVAL);
        }
        Next::Read => {}
    }
    host.hold(
        &targets
            .iter()
            .map(|target| target.bounds)
            .collect::<Vec<_>>(),
    );
    let moving: Option<AreaId> = host.in_gesture();
    watchers.retain(|id, _| Some(*id) != moving && targets.iter().any(|target| target.id == *id));
    for target in targets {
        // Asked again right before each capture, not only once a pass: a
        // pass reads its areas one after another, each capture taking up
        // to a few hundred milliseconds, and the overlay can be hidden, a
        // move started or watching switched off between two of them
        // (review of `#154`, round 4). A capture already under way cannot
        // be stopped; this keeps the window to that one.
        let now = host.state();
        let gesture_now = host.in_gesture();
        let still = host.still_watching(target);
        let monitors = host.monitors();
        // The mark on a monitor by the host's geometry, AND seen by the page's
        // latest confirmation for these bounds in this overlay state.
        let shown =
            mark_on_screen(mark(target.bounds), &monitors) && host.mark_seen(target, state.0);
        match gate(target.id, now == state, [moving, gesture_now], still, shown) {
            Gate::Stop => break,
            Gate::Skip => {
                // Not read, and read again from a fresh baseline.
                watchers.remove(&target.id);
                continue;
            }
            Gate::Read => {}
        }
        let watcher = watchers.entry(target.id).or_insert_with(|| Watcher {
            target,
            detector: Detector::new(),
            failing: false,
            fresh: true,
        });
        if watcher.target != target {
            watcher.target = target;
            watcher.detector = Detector::new();
            watcher.fresh = true;
        }
        if watcher.fresh {
            watcher.fresh = false;
            continue;
        }
        let reading = match host.capture(target.bounds) {
            Ok(mut captured) => {
                watcher.failing = false;
                leave_out(
                    &mut captured.bitmap,
                    &left_out(target.bounds, captured.rect, state.1),
                );
                Fingerprint::of(&captured.bitmap)
            }
            Err(error) => {
                if !watcher.failing {
                    host.trouble(&error);
                }
                watcher.failing = true;
                None
            }
        };
        if let Some(event) = reading.and_then(|reading| watcher.detector.observe(reading))
            && target.watch.notifies(event)
        {
            host.notify(event);
        }
    }
    host.end_pass();
    Some(INTERVAL)
}

/// What the loop does with one area right before its capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// The overlay state changed: end the pass.
    Stop,
    /// Leave this area unread and start it again from a fresh baseline.
    Skip,
    /// Take the reading.
    Read,
}

/// The decision before each capture, apart so it can be tested. `gestures`
/// holds the area under a move or resize at the start of the pass and the
/// one under a move or resize now; both are asked, because one user can
/// release one area and press another within a pass. Round 6 of the review
/// of `#154` found the second never asked once the first was set.
fn gate<Id: PartialEq + Copy>(
    id: Id,
    state_unchanged: bool,
    gestures: [Option<Id>; 2],
    still_watching: bool,
    mark_on_screen: bool,
) -> Gate {
    if !state_unchanged {
        Gate::Stop
    } else if gestures.contains(&Some(id)) || !still_watching || !mark_on_screen {
        Gate::Skip
    } else {
        Gate::Read
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
    use super::{EDGE, Gate, MARK, Next, gate, left_out, mark, mark_on_screen, next, own_drawing};

    /// The reading loop itself, against a stand-in for the app
    /// (UP-TAKE backlog `I-473`). Each test is one route a review of `#154`
    /// found; each goes red if its line in [`pass`] is undone.
    mod the_loop {
        use super::super::{
            CONFIRMATION_LIFETIME, Confirmation, Host, Memory, Target, pass, seen_by,
        };
        use crate::overlay_state::OverlayState;
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};
        use uptake_capture::CapturedRegion;
        use uptake_core::area::{AreaId, AreaStore, AreaType};
        use uptake_core::bitmap::RgbaBitmap;
        use uptake_core::geometry::{Rect, Size};
        use uptake_core::watch::{Event, Watch};

        struct Fake {
            targets: Vec<Target>,
            state: (OverlayState, bool),
            /// Applied to `state` right after the first capture of a pass.
            state_after_capture: Option<(OverlayState, bool)>,
            /// What `in_gesture` answers, in order; then `None`.
            gestures: VecDeque<Option<AreaId>>,
            still: bool,
            /// What the page's confirmation says.
            seen: bool,
            monitor: Rect,
            /// The grey level the screen shows.
            level: u8,
            captured: Vec<Rect>,
            notified: Vec<Event>,
            /// What the loop last asked to hold sessions for.
            held: Vec<Rect>,
            /// How many times it let them go.
            released: usize,
        }

        impl Fake {
            fn new(targets: Vec<Target>) -> Self {
                Self {
                    targets,
                    state: (OverlayState::Living, false),
                    state_after_capture: None,
                    gestures: VecDeque::new(),
                    still: true,
                    seen: true,
                    monitor: Rect::new(0, 0, 1920, 1080),
                    level: 200,
                    captured: Vec::new(),
                    notified: Vec::new(),
                    held: Vec::new(),
                    released: 0,
                }
            }
        }

        impl Host for Fake {
            fn look(&mut self) -> (Vec<Target>, bool) {
                (self.targets.clone(), !self.targets.is_empty())
            }
            fn state(&mut self) -> (OverlayState, bool) {
                self.state
            }
            fn in_gesture(&mut self) -> Option<AreaId> {
                self.gestures.pop_front().flatten()
            }
            fn still_watching(&mut self, _target: Target) -> bool {
                self.still
            }
            fn mark_seen(&mut self, _target: Target, _state: OverlayState) -> bool {
                self.seen
            }
            fn monitors(&mut self) -> Vec<Rect> {
                vec![self.monitor]
            }
            fn hold(&mut self, regions: &[Rect]) {
                self.held = regions.to_vec();
            }
            fn release(&mut self) {
                self.held.clear();
                self.released += 1;
            }
            fn end_pass(&mut self) {}
            fn capture(&mut self, bounds: Rect) -> Result<CapturedRegion, String> {
                self.captured.push(bounds);
                if let Some(state) = self.state_after_capture.take() {
                    self.state = state;
                }
                let rect = bounds
                    .intersection(self.monitor)
                    .ok_or_else(|| "off screen".to_owned())?;
                let pixels = [self.level, self.level, self.level, 255]
                    .repeat(usize::try_from(rect.size.width * rect.size.height).unwrap_or(0));
                let bitmap =
                    RgbaBitmap::from_pixels(Size::new(rect.size.width, rect.size.height), pixels)
                        .ok_or_else(|| "bitmap".to_owned())?;
                Ok(CapturedRegion { rect, bitmap })
            }
            fn trouble(&mut self, _error: &str) {}
            fn notify(&mut self, event: Event) {
                self.notified.push(event);
            }
        }

        fn targets(bounds: &[Rect]) -> Vec<Target> {
            targets_for(bounds, Watch::Changes)
        }

        fn targets_for(bounds: &[Rect], watch: Watch) -> Vec<Target> {
            let mut store = AreaStore::new();
            bounds
                .iter()
                .filter_map(|bounds| {
                    store.create(AreaType::Watch, *bounds).map(|id| Target {
                        id,
                        bounds: *bounds,
                        watch,
                    })
                })
                .collect()
        }

        fn run(fake: &mut Fake, memory: &mut Memory, passes: usize) {
            for _ in 0..passes {
                let _ = pass(fake, memory);
            }
        }

        /// The working case: a baseline, then a change of the screen notifies.
        #[test]
        fn a_change_of_the_screen_notifies() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            assert!(fake.notified.is_empty(), "still screen");
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            assert_eq!(fake.notified, vec![Event::Changed]);
        }

        /// Sessions are held for the areas being read, and let go as soon as
        /// nothing can be read: switched off, or the overlay hidden.
        #[test]
        fn sessions_are_held_only_while_reading() {
            let bounds = Rect::new(100, 100, 600, 400);
            let mut fake = Fake::new(targets(&[bounds]));
            let mut memory = Memory::default();
            let _ = pass(&mut fake, &mut memory);
            assert_eq!(fake.held, vec![bounds]);
            fake.state = (OverlayState::Hidden, false);
            let _ = pass(&mut fake, &mut memory);
            assert_eq!((fake.held.len(), fake.released), (0, 1), "hidden");
            fake.state = (OverlayState::Living, false);
            let _ = pass(&mut fake, &mut memory);
            fake.targets.clear();
            let _ = pass(&mut fake, &mut memory);
            assert_eq!((fake.held.len(), fake.released), (0, 2), "switched off");
        }

        /// Round 12: a watcher made in a pass does not read until the next,
        /// so its baseline is newer than whatever made it.
        #[test]
        fn a_new_watcher_reads_from_the_next_pass() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            let _ = pass(&mut fake, &mut memory);
            assert_eq!(fake.captured.len(), 0);
            let _ = pass(&mut fake, &mut memory);
            assert_eq!(fake.captured.len(), 1);
            fake.state = (OverlayState::Placement, false);
            let _ = pass(&mut fake, &mut memory);
            assert_eq!(fake.captured.len(), 1, "started again: waits a pass");
        }

        /// Round 14: an area that moved between two passes (a sticky area
        /// following its window, a move that ended) starts again: it waits a
        /// pass, then takes a new baseline, and its move is not a change.
        #[test]
        fn an_area_that_moved_starts_again() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            assert_eq!(fake.captured.len(), 1, "the baseline");
            fake.targets[0].bounds = Rect::new(300, 200, 600, 400);
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            assert_eq!(fake.captured.len(), 1, "waits a pass after the move");
            run(&mut fake, &mut memory, 3);
            assert_eq!(fake.notified.len(), 0, "{:?}", fake.notified);
            // And it reads again: a watcher that never forgot the old place
            // would start again every pass and never read (round 15).
            assert!(fake.captured.len() > 1, "{:?}", fake.captured);
        }

        /// The page's word names the area at its bounds, in the state being
        /// read, and is fresh; otherwise the mark is not seen.
        #[test]
        fn the_pages_word_must_match_and_be_fresh() {
            let list = targets(&[Rect::new(100, 100, 600, 400), Rect::new(800, 100, 600, 400)]);
            let (one, other) = (list[0], list[1]);
            let at = Instant::now();
            let word = Confirmation {
                state: OverlayState::Living,
                marks: vec![(one.id.get(), one.bounds)],
                at,
            };
            assert!(seen_by(&word, one, OverlayState::Living, at));
            assert!(
                !seen_by(&word, other, OverlayState::Living, at),
                "not named"
            );
            assert!(
                !seen_by(&word, one, OverlayState::Placement, at),
                "another state"
            );
            let moved = Target {
                bounds: Rect::new(110, 100, 600, 400),
                ..one
            };
            assert!(
                !seen_by(&word, moved, OverlayState::Living, at),
                "other bounds"
            );
            let late = at + CONFIRMATION_LIFETIME + Duration::from_millis(1);
            assert!(!seen_by(&word, one, OverlayState::Living, late), "too old");
            assert!(seen_by(
                &word,
                one,
                OverlayState::Living,
                at + CONFIRMATION_LIFETIME
            ));
        }

        /// An area whose mark the page has not confirmed is not read.
        #[test]
        fn an_area_whose_mark_is_not_seen_is_not_read() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            let _ = pass(&mut fake, &mut memory);
            fake.seen = false;
            run(&mut fake, &mut memory, 3);
            assert_eq!(fake.captured.len(), 0);
        }

        /// Nothing switched on: the loop sleeps until woken.
        #[test]
        fn nothing_switched_on_sleeps_until_woken() {
            let mut fake = Fake::new(Vec::new());
            assert_eq!(pass(&mut fake, &mut Memory::default()), None);
            assert!(fake.captured.is_empty());
        }

        /// Round 6: one area moving when the pass began, another pressed
        /// during it. The second is asked about fresh and not read.
        #[test]
        fn an_area_pressed_during_a_pass_is_not_read() {
            let list = targets(&[Rect::new(100, 100, 600, 400), Rect::new(800, 100, 600, 400)]);
            let (first, second) = (list[0].id, list[1].id);
            let mut fake = Fake::new(list);
            let mut memory = Memory::default();
            // A first pass makes both watchers; a watcher does not read in the
            // pass that made it, so without it this test could not fail
            // (review of `#154`, round 13).
            let _ = pass(&mut fake, &mut memory);
            // At the start of the pass `first` is moving; before the first
            // area, still `first`; before the second, `second` is pressed.
            fake.gestures = VecDeque::from([Some(first), Some(first), Some(second)]);
            let _ = pass(&mut fake, &mut memory);
            assert!(fake.captured.is_empty(), "{:?}", fake.captured);
        }

        /// Round 4: the overlay hidden partway through a pass stops the
        /// readings after it.
        #[test]
        fn a_state_change_during_a_pass_stops_it() {
            let mut fake = Fake::new(targets(&[
                Rect::new(100, 100, 600, 400),
                Rect::new(800, 100, 600, 400),
            ]));
            let mut memory = Memory::default();
            let _ = pass(&mut fake, &mut memory);
            fake.state_after_capture = Some((OverlayState::Hidden, false));
            let _ = pass(&mut fake, &mut memory);
            assert_eq!(fake.captured.len(), 1);
        }

        /// An area the store no longer has watching is not read.
        #[test]
        fn an_area_no_longer_watching_is_not_read() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            let _ = pass(&mut fake, &mut memory);
            fake.still = false;
            let _ = pass(&mut fake, &mut memory);
            assert!(fake.captured.is_empty());
        }

        /// Round 9: flipping *Show UP-TAKE in screen recordings* changes what
        /// a reading leaves out, which is not a change of the screen.
        #[test]
        fn flipping_show_in_recordings_is_not_a_change() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            fake.state = (OverlayState::Living, true);
            run(&mut fake, &mut memory, 5);
            assert!(fake.notified.is_empty(), "{:?}", fake.notified);
        }

        /// Round 3: an area whose mark corner is on no monitor is not read,
        /// though part of the area is on screen.
        #[test]
        fn an_area_whose_mark_is_off_screen_is_not_read() {
            let mut fake = Fake::new(targets(&[Rect::new(1700, 100, 600, 400)]));
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            assert_eq!(fake.captured.len(), 0);
        }

        /// An area set to notify when the screen settles stays quiet when it
        /// starts changing, and speaks when it settles.
        #[test]
        fn an_area_notifies_only_the_moment_it_was_set_to() {
            let list = targets_for(&[Rect::new(100, 100, 600, 400)], Watch::Settles);
            let mut fake = Fake::new(list);
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            assert_eq!(fake.notified.len(), 0);
            run(&mut fake, &mut memory, 4);
            assert_eq!(fake.notified, vec![Event::Settled]);
        }

        /// Round 10: an area hanging off the bottom of the desktop, with the
        /// overlay in captures, still sees a change.
        #[test]
        fn an_area_off_the_edge_still_notifies() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 1040, 200, 100)]));
            fake.state = (OverlayState::Living, true);
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            assert_eq!(fake.notified, vec![Event::Changed]);
        }
    }

    /// Nothing is left out while UP-TAKE is kept out of captures; with it in
    /// them, a small area keeps something to read.
    #[test]
    fn a_reading_always_keeps_something_to_compare() {
        let small = Rect::new(0, 0, 200, 30);
        assert!(left_out(small, small, false).is_empty(), "the default");
        let parts = left_out(small, small, true);
        assert_eq!(parts.len(), 4, "the border only, not the corner");
        let Some(mut bitmap) = uptake_core::bitmap::RgbaBitmap::from_pixels(
            uptake_core::geometry::Size::new(200, 30),
            [255_u8; 4].repeat(200 * 30),
        ) else {
            panic!("a 200 by 30 bitmap");
        };
        uptake_core::watch::leave_out(&mut bitmap, &parts);
        assert!(
            bitmap.pixels().chunks(4).any(|pixel| pixel[0] != 0),
            "something is left"
        );
        let large = Rect::new(0, 0, 600, 400);
        assert_eq!(left_out(large, large, true).len(), 5);
    }

    /// Round 10: an area hanging off the desktop is judged by the part that
    /// was captured, so it too keeps something to compare.
    #[test]
    fn an_area_off_the_edge_keeps_something_to_compare() {
        let monitor = Rect::new(0, 0, 1920, 1080);
        for bounds in [
            Rect::new(100, 1040, 200, 100),
            Rect::new(-360, 100, 600, 10),
        ] {
            let Some(taken) = bounds.intersection(monitor) else {
                panic!("partly on screen");
            };
            let (width, height) = (taken.size.width, taken.size.height);
            let Some(mut bitmap) = uptake_core::bitmap::RgbaBitmap::from_pixels(
                uptake_core::geometry::Size::new(width, height),
                [255_u8; 4].repeat(usize::try_from(width * height).unwrap_or(0)),
            ) else {
                panic!("a bitmap");
            };
            uptake_core::watch::leave_out(&mut bitmap, &left_out(bounds, taken, true));
            assert!(
                bitmap.pixels().chunks(4).any(|pixel| pixel[0] != 0),
                "{bounds:?} keeps something"
            );
        }
    }

    /// An area is read right before its capture only while the overlay state
    /// is the pass's, no move or resize is on it (at the start of the pass or
    /// now), it is still watching and its mark is on screen.
    #[test]
    fn each_capture_is_gated_on_every_condition() {
        let (a, b) = (1_u64, 2_u64);
        assert_eq!(gate(a, true, [None, None], true, true), Gate::Read);
        assert_eq!(gate(a, false, [None, None], true, true), Gate::Stop);
        assert_eq!(gate(a, true, [Some(a), None], true, true), Gate::Skip);
        // Round 6: another area was moving when the pass began, and this one
        // is pressed now.
        assert_eq!(gate(a, true, [Some(b), Some(a)], true, true), Gate::Skip);
        assert_eq!(gate(a, true, [Some(b), Some(b)], true, true), Gate::Read);
        assert_eq!(gate(a, true, [None, None], false, true), Gate::Skip);
        assert_eq!(gate(a, true, [None, None], true, false), Gate::Skip);
    }

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
        // Rounds 15 and 16: small areas near a monitor's left edge, whose
        // mark (clipped to the area by the page) is on screen, do read.
        assert!(mark_on_screen(mark(Rect::new(0, 100, 40, 400)), &monitors));
        assert!(mark_on_screen(
            mark(Rect::new(20, 100, 200, 100)),
            &monitors
        ));
        assert!(mark_on_screen(
            mark(Rect::new(1930, 100, 200, 100)),
            &monitors
        ));
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
