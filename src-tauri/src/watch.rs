//! The host half of watch areas (roadmap `1.58`, `ADR-0048`): reading each
//! watching area's region when its screen changes and raising UP-TAKE's own notification
//! card (`ADR-0054`, `crate::notice`) when what it shows changes, or stops
//! changing.
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
//! - **Local.** A reading is reduced to the average brightness of each 4 by
//!   4 pixel square of its region (`uptake_core::watch::CELL`, roadmap
//!   `1.61`), which is the region's greyscale picture at a quarter of its
//!   size: about 130,000 values for an area the size of a 1080p monitor. The
//!   detector keeps up to two such readings, the last still one and the last
//!   one. Nothing is written,
//!   logged or sent; the notification says which moment it was, never what
//!   was read. ⚠️ **What is held in between is larger than a reading:** while
//!   any area reads, a capture session on each monitor under one keeps that
//!   monitor's newest frame as a GPU texture (`uptake_capture::warm`). A
//!   reading copies only its area's part on the GPU, averaged down to a
//!   small picture or, where UP-TAKE's own drawing is left out of it, whole,
//!   and reads back just that; the copy is dropped with the reading. Only
//!   the areas' parts are read; the whole frame is what the session holds. `ADR-0048` forecloses a whole-screen *watcher*,
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
//! [`mark_on_screen`]). The mark sits in the area's top left corner (top
//! right until 2026-11-10, the maintainer's choice), and an
//! area may hang off the desktop or into a gap between monitors; the capture
//! would still read the part that is on screen while the mark is drawn
//! nowhere, which is `ADR-0048`'s first *Revisit if* (review of `#154`,
//! round 3). Checked against the area's top left corner, which holds the
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
//! The page measures each watching mark once it is laid out (not once it is
//! painted: the host cannot see a paint), counting as covering everything it
//! draws after the mark and a freeze's hidden monitors. An area is read only
//! while the page's latest word, at most [`CONFIRMATION_LIFETIME`] old and
//! measured after the last thing the host sent that changes what the page
//! draws (it echoes that event's generation, [`emit_redraw`]), says its mark
//! is seen at the bounds and in the overlay state being read. That closes:
//!
//! - the host ahead of the page, both ways: a mark appearing (switching
//!   watching on, a sticky area resuming, a move ending; round 7) and a mark
//!   going (a menu, a hover's controls or another area drawn over it; rounds
//!   20 and 21);
//! - anything the page draws over the mark (rounds 14 and 20);
//! - the mark clipped away on a small area, at any scaling (rounds 17, 18);
//! - the mark reaching past the host's corner in a long language (round 19);
//! - a freeze's or another capture's step-aside, which reaches the page as a
//!   hide that voids the word, after which the page leaves those marks out
//!   (round 5).
//!
//! ⚠️ **What stays open is timing**, and remains `I-475`'s. The page's word,
//! the host's check before a reading and the frame the reading uses are three
//! moments:
//!
//! - a change between the check and the frame read back, a few milliseconds,
//!   is not seen by the check;
//! - the frame is a held session's newest, which can be up to
//!   `warm::WATCH_FRAME_INTERVAL` (about 33 ms) older than the
//!   check, and can
//!   come from between the page's layout and its paint (rounds 12 and 20). A
//!   watcher made or started again waits one pass before its first reading
//!   ([`Watcher`]), which keeps such a frame out of a baseline, not out of
//!   every reading.
//!
//! ⚠️ **And what the page cannot see**, also `I-475`'s: a window Windows draws
//! above UP-TAKE's overlay (a notification toast, this feature's own among
//! them; the Start menu; another always-on-top program) can cover the mark
//! while the page, which measures only its own drawing, still says it is
//! seen (review of `#154`, round 24). Not probed on a real screen.
//!
//! ⚠️ **With *Show UP-TAKE in screen recordings* on, UP-TAKE's own drawing
//! is in every reading, and only the border and the mark are left out.**
//! The largest other part, Placement's darkened edges, comes and goes with
//! the overlay state, so every watcher starts again from a fresh baseline
//! when the state changes (review of `#154`, round 2). The area's name label
//! comes and goes with its name, so a rename starts its watcher again too
//! ([`Target::label`], `I-480`). What is left: a menu, a selection box or
//! another area's flash drawn over a watched region while the user works
//! there can count as a change. A notification card or the settings panel
//! over one does not: UP-TAKE lists where its own windows are and a reading
//! leaves their cells out of the comparison ([`unknown_under`], `I-480`).
//! The setting is off by default.
//!
//! # Cost
//!
//! **Event-driven, the maintainer's design of 2026-11-05** (UP-TAKE roadmap
//! `1.58`, *as responsive as possible without impacting UP-TAKE and PC
//! performance*). Capture sessions are held on the monitors under watching
//! areas (`uptake_capture::warm::watch_hold`), at most about 30 frames a
//! second (`warm::WATCH_FRAME_INTERVAL`), each reporting where it changed. A
//! frame whose dirty regions touch a watch area wakes the thread
//! (`warm::on_watch_frame`); any other frame wakes nobody. A reading copies
//! only the area's own part on the GPU, never the whole monitor: usually
//! averaged down there to a small picture (`warm::watch_capture_small`), and
//! read whole (`warm::watch_capture_region`) when UP-TAKE's own drawing is
//! painted out of it, when the area spans monitors, or when the small read
//! fails. After each pass
//! the thread rests in proportion to what the pass cost, so it spends at most
//! [`BUDGET`] of one core however busy the screen ([`rest_after`]).
//!
//! - **A still screen sends no frames and costs nothing**; the thread looks
//!   again every [`IDLE`] in case a change was missed.
//! - **Measured, 2026-11-10, with 4-pixel cells** (roadmap `1.61`; the
//!   maintainer's private planning workspace, `Projects/UP-TAKE/evidence/
//!   1.61-watch-dot-cost/`, which holds the runs, the stand-in dot and the
//!   harness, `1.58-watch-events/watch_events.rs` there, which runs this
//!   loop without the app; not in this repository; the watcher's harness on a still,
//!   dark backdrop over the area, 30 s a case): one 600 by 400 area, 0.05 %
//!   to 0.25 % of a core while nothing changes (no reads at all; the spread
//!   is the harness's own idle passes); an area as large as a monitor,
//!   0.61 % on the full read. **The
//!   watching dot matters more than the read:** a dot animated all the time
//!   woke its area about 12 times a second even with UP-TAKE kept out of
//!   captures, and cost 1.7 % to 3.1 % (over `quality-bars.md` §1's 2 %
//!   target per live area, under its 5 % hard fail); one short pulse every
//!   5 s cost 0.35 % to 0.8 %, which is why the dot pulses and does not
//!   breathe. **Not measured yet: the whole watcher, reads included, on a
//!   busy screen held at 30 frames a second for a long time** (`I-474`). The
//!   capture sessions' own share at 30 frames a second, without reads, is
//!   measured ([`BUDGET`]'s note).
//! - Measured before `1.61`, with at most 16 by 16 cells
//!   (`evidence/1.58-watch-events/`, 2026-11-05): an ordinary desktop sent 3
//!   to 11 frames a second on the primary monitor, about 2 to 3 % of the
//!   screen each; 0.34 % of a core for one area, 0.43 % for five, 1.69 % for
//!   an area as large as a monitor, and 2.49 % to 2.59 % for that area read
//!   whole. Those figures are for the old grid and are kept as history.
//! - Earlier routes, measured in workspace `evidence/1.58-watch-cost/`: held
//!   sessions read whole every two seconds, 0.13 % of a core for one area
//!   and for five on a still screen; a capture built per reading, 7.3 % for
//!   one area. That second route is now only the fallback for a monitor WGC
//!   cannot hold, which the governor paces by its cost.
//!
//! The sessions are let go whenever nothing can be read (switched off,
//! paused, overlay hidden).
//!
//! # The thread
//!
//! One thread, started the first time any area is switched to watching and
//! never stopped. While no watch area is switched on it waits on a condition
//! variable and costs nothing; [`wake`] is what moves it. While one is
//! switched on but none can be read, it looks once per [`LOOK`]: a paused
//! sticky area follows its window again, or the overlay comes back, with
//! nothing to wake the thread, so it has to look. Round 1 of the review of
//! `#154` found it asleep for good after a pause. While a change is settling
//! it reads every [`SETTLE_POLL`], frames or not, since the calm that ends a
//! change sends no frames.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use uptake_core::area::{Area, AreaId, AreaStore, AreaType};
use uptake_core::geometry::Rect;
use uptake_core::watch::{Detector, Event, EventGate, Fingerprint, Tuning, Watch, leave_out};

use crate::overlay_state::OverlayState;

use crate::strings::{Text, text};

/// **The watcher is event-driven** (the maintainer's design of 2026-11-05: *as
/// responsive as possible without impacting UP-TAKE and PC performance*). It
/// sleeps until a frame whose dirty regions touch a watch area arrives
/// (`uptake_capture::warm::on_watch_frame`), so a change is seen within about a
/// frame, and a still screen costs nothing. Until then it read every half
/// second, and before 2026-11-03 every two seconds.
///
/// With nothing to wake it, it still looks again after this long: a frame the
/// capture lost, or a change its dirty regions missed, is read then, or after
/// the governor's rest when a pass costs more ([`rest_after`]).
pub const IDLE: Duration = Duration::from_secs(2);

/// While a change is settling, how often it reads with no frame arriving: the
/// calm that ends a change is only seen by reading through it, so a *stopped
/// changing* comes this long after the calm it waits for, or after the
/// governor's rest when a pass costs more than about a millisecond
/// ([`rest_after`]).
pub const SETTLE_POLL: Duration = Duration::from_millis(50);

/// One is switched on and none can be read now (paused, the overlay hidden):
/// how soon it looks again, since nothing wakes the thread when that changes.
pub const LOOK: Duration = Duration::from_millis(500);

/// With nothing ringing sooner, how long until a watcher made or started
/// again reads, and until one whose monitor has no frame held yet, or whose
/// mark the page has not confirmed, is looked at again. A frame or the page's
/// word can end the wait earlier; the bound that keeps a fresh baseline newer
/// than the reason it started again is the governor's one-frame floor
/// ([`Watcher::fresh`], [`rest_after`]), not this.
pub const FRESH_WAIT: Duration = Duration::from_millis(66);

const _: () = assert!(
    FRESH_WAIT.as_nanos() >= 2 * uptake_capture::warm::WATCH_FRAME_INTERVAL.as_nanos(),
    "the wait with nothing ringing spans at least two frames"
);

/// The share of one core the watcher's own thread may spend: `quality-bars.md`
/// §1's 2 % per live area, for all of them together. After each pass it rests
/// until its own time is no more than this share ([`rest_after`]). The held
/// sessions' frames cost on top of it, outside the governor: about 0.3 to
/// 0.4 % of a core measured at 30 frames a second, the sessions alone and no
/// reads (the private planning workspace's `evidence/1.58-watch-events/`;
/// the whole watcher on such a screen is not measured, `I-474`).
pub const BUDGET: f64 = 0.02;

/// How long to rest after a pass that took `spent`: long enough that the pass
/// was at most [`BUDGET`] of the time, and never less than a frame. Measured
/// on the wall clock, which counts waits on the GPU as spent: it errs on the
/// side of reading less.
#[must_use]
pub fn rest_after(spent: Duration) -> Duration {
    let share = spent.mul_f64(1.0 / BUDGET - 1.0);
    share.max(uptake_capture::warm::WATCH_FRAME_INTERVAL)
}

/// How wide a band along each edge of the area is left out of a reading, in
/// physical pixels: the border, 1.5 CSS pixels, at up to 250 % scaling.
const EDGE: u32 = 4;

/// The corner left out for the *watching* mark, in physical pixels: a dot
/// of 6 CSS pixels, 1 in from the corner, so 7 CSS pixels each way, at up to
/// about 340 % scaling. Until 2026-11-10 the mark was the word *watching*
/// beside the dot, and this corner was 240 by 40 to hold it; the maintainer's
/// rig pass found an area as small as two letters could not show the word,
/// so its mark was never seen and it never read. The page clips the mark to
/// the area and measures the mark it drew ([`confirm`]), so a scaling past
/// this corner is caught there (review of `#154`, round 19).
const MARK: (u32, u32) = (24, 24);

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

/// Rung by a frame that touched a watch area (`warm::on_watch_frame`), on the
/// capture's own thread: it only signals.
fn ring() {
    let mut bell = bell();
    bell.rung = true;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Target {
    id: AreaId,
    bounds: Rect,
    watch: Watch,
    /// The name UP-TAKE draws inside the area, as [`label_of`] keeps it. With
    /// *Show UP-TAKE in screen recordings* on, that label is in the reading,
    /// so a new name is new pixels: the watcher starts again from a fresh
    /// baseline rather than reporting its own label as a change (`I-480`).
    label: u64,
    /// How it reads and reports (roadmap `1.65`). A change of this alone
    /// keeps the watcher's baseline: it changes nothing in the reading.
    tuning: Tuning,
}

/// What [`Target::label`] keeps of an area's name: enough to tell a new name
/// from the old one.
fn label_of(name: Option<&str>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    hasher.finish()
}

/// The target an area is read as.
fn target_of(store: &AreaStore, area: &Area) -> Target {
    Target {
        id: area.id,
        bounds: area.bounds,
        watch: area.watch,
        label: label_of(store.name(area.id)),
        tuning: area.tuning,
    }
}

/// What the store says now: the areas to read, and whether any watch area is
/// switched on at all, readable or not. Takes the store lock for the copy and
/// nothing else.
fn look(app: &AppHandle) -> (Vec<Target>, bool) {
    let store = app.state::<Mutex<AreaStore>>();
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    let switched_on = store
        .iter()
        .any(|area| area.kind == AreaType::Watch && !area.held);
    let targets = store
        .iter()
        .filter(|area| area.is_watching())
        .map(|area| target_of(&store, area))
        .collect();
    (targets, switched_on)
}

/// Whether `target` is still what the store holds and still reads: switched
/// on, unmoved, not renamed, not paused. Checked right before its capture.
fn still_watching(app: &AppHandle, target: Target) -> bool {
    let store = app.state::<Mutex<AreaStore>>();
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    store
        .get(target.id)
        .is_some_and(|area| area.is_watching() && target_of(&store, area) == target)
}

fn overlay_state(app: &AppHandle) -> OverlayState {
    let state = app.state::<Mutex<OverlayState>>();
    *state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The page's latest word on which watching marks it can see (roadmap
/// `1.58`, `ADR-0048`'s *Revisit if*: the mark enforced by the reader).
///
/// The page measures each watching area's mark once laid out: its
/// dot wholly inside the badge, on a monitor, under no other area
/// (`src/lib/mark-check.ts`). It sends the areas whose mark is seen, with the
/// bounds it drew them at and the overlay state it drew in, after every change
/// and every second. An area is read only while the latest word names it at
/// its present bounds, in the present state, and is no older than
/// [`CONFIRMATION_LIFETIME`]: a page that stopped answering stops the reading.
///
/// What it closes and what stays open (timing) is in the module docs, under
/// *The page confirms the mark*.
struct Confirmation {
    state: OverlayState,
    marks: Vec<(u64, Rect)>,
    /// The generation the page had drawn when it measured ([`emit_redraw`]).
    generation: u64,
    at: Instant,
}

static CONFIRMED: Mutex<Option<Confirmation>> = Mutex::new(None);

/// How many times the host has sent the page something that changes what it
/// draws ([`emit_redraw`]). The page echoes the number it has drawn, and a
/// word counts only while it echoes this one.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The event that tells the page which generation it has just been sent.
pub const DRAWN_EVENT: &str = "overlay://drawn";

/// The payload of [`DRAWN_EVENT`].
#[derive(Clone, serde::Serialize)]
struct DrawnPayload {
    generation: u64,
}

/// Sends the page an event that can change what it draws (areas, state, a
/// menu, hover, a pin, the coach, a freeze's hide), and then the generation
/// that event begins.
///
/// **Exact, not timed** (review of `#154`, round 21, which found round 20's
/// 250 ms settle assumed a word in flight arrives within it). The generation
/// moves before the event, so every word the page measured before it echoes
/// an older number and stops counting at once. The page learns the new number
/// only from [`DRAWN_EVENT`], sent after the event, so the page has handled
/// the change, and Svelte has applied it, before it can echo the number.
///
/// **On the main thread, always** (round 23). An emit made on the main thread
/// reaches the page at once; one made on any other thread is queued behind
/// the event loop (`tauri-runtime-wry`'s `send_user_message`, and this
/// repository's `overlay::emit_areas` doc), so a worker's earlier redraw
/// could reach the page after the main thread's later one, under any lock. So
/// the whole redraw, generation included, runs on the main thread:
/// `run_on_main_thread` runs it there at once when called there, and queues
/// it otherwise. Redraws then take their numbers in the order the page
/// receives them, and a word that echoes the current generation describes
/// the page after the latest change.
///
/// From another thread this returns before the event is sent, and an emit
/// that fails there is not reported to the caller: a page left one
/// generation behind reads nothing, which errs on the side of reading less.
pub fn emit_redraw<S: serde::Serialize + Clone + Send + 'static>(
    app: &AppHandle,
    event: &'static str,
    payload: S,
) -> tauri::Result<()> {
    let on_main = app.clone();
    app.run_on_main_thread(move || {
        in_order(
            || {
                let _ = on_main.emit(event, payload);
            },
            |generation| {
                let _ = on_main.emit(DRAWN_EVENT, DrawnPayload { generation });
            },
        );
    })
}

/// Held across a whole [`in_order`], so two redraws cannot interleave (round
/// 22). Every redraw runs on the main thread ([`emit_redraw`]), which already
/// serialises them; this keeps [`in_order`] correct for any caller, and its
/// test. No Rust code listens to these events, so nothing sent under it calls
/// back into it.
static REDRAWING: Mutex<()> = Mutex::new(());

/// [`emit_redraw`]'s order, apart so it is tested without a window: the
/// generation moves, then the event is sent, then the new generation is, all
/// under [`REDRAWING`].
fn in_order<T>(event: impl FnOnce() -> T, drawn: impl FnOnce(u64)) -> T {
    let _one_at_a_time = REDRAWING.lock().unwrap_or_else(PoisonError::into_inner);
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let sent = event();
    drawn(generation);
    sent
}

/// How long a confirmation stands. The page sends one every second while any
/// mark is seen, so three missed in a row stop the reading.
pub const CONFIRMATION_LIFETIME: Duration = Duration::from_secs(3);

/// Records the page's latest confirmation ([`Confirmation`]).
pub fn confirm(state: OverlayState, marks: Vec<(u64, Rect)>, generation: u64) {
    *CONFIRMED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Confirmation {
        state,
        marks,
        generation,
        at: Instant::now(),
    });
    // A mark just seen is an area that can read now: wake the watcher rather
    // than leave it to its next look (review of `#162`, round 1).
    ring();
}

/// Whether the latest confirmation, as of `now`, says `target`'s mark is seen.
fn mark_seen(target: Target, state: OverlayState, now: Instant) -> bool {
    let current = GENERATION.load(Ordering::SeqCst);
    CONFIRMED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|word| seen_by(word, target, state, now, current))
}

/// [`mark_seen`]'s rule, apart so it is tested without the shared store.
fn seen_by(
    word: &Confirmation,
    target: Target,
    state: OverlayState,
    now: Instant,
    current: u64,
) -> bool {
    word.state == state
        && word.generation == current
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
    /// is hidden): look again in [`LOOK`], because nothing wakes the
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
    /// pass.
    failing: bool,
    /// Made, or started again, in this pass: it reads from the next pass on.
    ///
    /// A reading comes from a held session's newest frame, which can be up to
    /// [`uptake_capture::warm::WATCH_FRAME_INTERVAL`] older than the checks
    /// made before it. A baseline taken in the pass that started a watcher
    /// again (the overlay state changed, the setting flipped, a move ended)
    /// could still show what came before, and the next reading would report
    /// that as a change (review of `#154`, round 12). Its next pass comes
    /// at least one frame interval later, since the governor always rests
    /// that long ([`rest_after`]'s floor, which its test holds), so the frame
    /// read is newer than the reason it started again; and [`FRESH_WAIT`]
    /// later when nothing rings sooner and the governor's rest is shorter
    /// (review of `#159`, round 1; restated on the review of `#162`, rounds 1
    /// and 5).
    ///
    /// ⚠️ **Assumed, not measured:** that WGC, held at
    /// `WATCH_FRAME_INTERVAL`, delivers a frame within that interval of a
    /// change on screen. Nothing here probes frame age under the throttle
    /// (round 13); UP-TAKE backlog `I-474` carries it.
    fresh: bool,
    /// The size of the picture its baseline came from. A reading can come by
    /// another route (the small picture averaged on the GPU, the area's own
    /// rectangle, a capture of its own), and the routes' pictures differ in
    /// size and so in every cell; a reading of another size re-anchors the
    /// detector ([`Detector::rebase_at`]) rather than reporting the route as a
    /// change, and a change under way still ends with *stopped changing*
    /// (review of `#162`, rounds 5 and 6). A change made in the very reading
    /// that switched route cannot be told from the switch and is not
    /// reported.
    picture: Option<uptake_core::geometry::Size>,
    /// The area's tuning applied to its events: the minimum gap, then which
    /// become a card (roadmap `1.65`).
    gate: EventGate,
    /// When it last read, for its sampling rate ([`Tuning::sample_wait`]).
    read_at: Option<Duration>,
}

/// The parts of a reading that are UP-TAKE's own drawing, in the reading's
/// pixels: a band along each edge for the border, and the top left corner
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
    [
        Rect::new(dx, dy, width, EDGE),
        Rect::new(dx, bottom - edge, width, EDGE),
        Rect::new(dx, dy, EDGE, height),
        Rect::new(right - edge, dy, EDGE, height),
        Rect::new(dx, dy, MARK.0.min(width), MARK.1.min(height)),
    ]
}

/// Whether UP-TAKE's own drawing is in captures (*Show UP-TAKE in screen
/// recordings*, `ADR-0019`).
fn in_captures() -> bool {
    crate::settings::current().show_in_screen_recordings
}

/// What a reading leaves out. Nothing while UP-TAKE is kept out of captures,
/// which is the default (`ADR-0019`): its drawing is not in the reading, and
/// leaving the corner out anyway cost every area its mark's corner.
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

/// The parts of a reading that lie under UP-TAKE's own windows, in the
/// reading's pixels: `windows` in desktop pixels, cut to `taken`, the
/// rectangle the capture returned. With *Show UP-TAKE in screen recordings*
/// on, a notification card or the settings panel over a watch area is in its
/// reading; those parts are
/// unknown to the comparison rather than a change, or a card could raise the
/// next card (`I-480`, review of `#160`, round 1).
fn unknown_under(windows: &[Rect], taken: Rect) -> Vec<Rect> {
    windows
        .iter()
        .filter_map(|window| window.intersection(taken))
        .map(|part| {
            Rect::new(
                part.origin.x - taken.origin.x,
                part.origin.y - taken.origin.y,
                part.size.width,
                part.size.height,
            )
        })
        .collect()
}

/// Where the *watching* mark can be, in desktop pixels: the area's top left
/// corner, [`MARK`] in size or the area's own size when that is smaller.
///
/// The host's own first check, before the page's word ([`confirm`]): an area
/// whose corner is off every monitor is not read, whatever the page says. It
/// is not the whole of the check, and cannot be (rounds 15, 16 and 19): the
/// page clips the mark to the area (`+page.svelte`, `.watch-badge`, pinned by
/// `watch-badge-css.test.ts`) and measures the mark it drew.
fn mark(bounds: Rect) -> Rect {
    Rect::new(
        bounds.origin.x,
        bounds.origin.y,
        MARK.0.min(bounds.size.width),
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
    /// One reading of `bounds`, or `None` while a monitor it covers has no
    /// frame held yet (nothing to read this time). `whole`: the reading needs
    /// the full picture, since UP-TAKE's own drawing is in captures and is
    /// painted out of it; otherwise it may be a small picture averaged down
    /// on the GPU, which is all a fingerprint needs.
    fn capture(
        &mut self,
        bounds: Rect,
        whole: bool,
    ) -> Result<Option<uptake_capture::CapturedRegion>, String>;
    /// The time a reading is made at, on a clock that only moves forward, for
    /// the detector's calm (`Detector::observe_at`). Asked once a pass.
    fn now(&mut self) -> Duration;
    /// Where UP-TAKE's own windows outside the overlay are, or were a moment
    /// ago (`crate::notice::own_windows`), desktop pixels.
    fn own_windows(&mut self) -> Vec<Rect>;
    /// A reading failed; logged once until one succeeds.
    fn trouble(&mut self, error: &str);
    /// Raises the notification for `event`, for the area `id` at `bounds`.
    fn notify(&mut self, event: Event, id: AreaId, bounds: Rect);
}

/// How an area was read from a held session, for [`Troubles`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Route {
    /// Averaged down on the GPU (`warm::watch_capture_small`).
    Small,
    /// Its own rectangle from the GPU (`warm::watch_capture_region`).
    Region,
    /// A monitor it covers cannot be held: read by a capture of its own.
    Unheld,
}

/// Which areas are failing on which route, so a lasting fault is logged once
/// for that area and route, and again only after that same read has
/// succeeded (review of `#162`, rounds 1 to 3: first nothing was logged, then
/// one flag for every area and route let any success re-arm it, and a fault
/// confined to one route logged on every reading).
#[derive(Debug, Default)]
struct Troubles {
    failing: std::collections::HashSet<(Rect, Route)>,
}

impl Troubles {
    /// The read of `bounds` by `route` failed: whether to log it.
    fn failed(&mut self, bounds: Rect, route: Route) -> bool {
        self.failing.insert((bounds, route))
    }

    /// The read of `bounds` by `route` worked.
    fn worked(&mut self, bounds: Rect, route: Route) {
        self.failing.remove(&(bounds, route));
    }
}

/// The app itself.
struct AppHost<'a> {
    app: &'a AppHandle,
    /// The origin of [`Host::now`].
    started: Instant,
    /// Reads that are failing, logged once each.
    troubles: Troubles,
}

impl AppHost<'_> {
    fn failed(&mut self, bounds: Rect, route: Route, error: &str) {
        if self.troubles.failed(bounds, route) {
            let what = match route {
                Route::Unheld => {
                    "watch: a monitor this area covers cannot be held, so it is read by a capture of its own (said once)"
                }
                Route::Small | Route::Region => {
                    "watch: reading this area from a held capture session failed; reading it another way (said once until that read works again)"
                }
            };
            crate::diagnostics::trouble(what, &error);
        }
    }
}

impl Host for AppHost<'_> {
    fn look(&mut self) -> (Vec<Target>, bool) {
        look(self.app)
    }
    fn hold(&mut self, regions: &[Rect]) {
        let _ = uptake_capture::warm::watch_hold(regions);
    }
    fn release(&mut self) {
        uptake_capture::warm::watch_release();
    }
    fn end_pass(&mut self) {}
    fn now(&mut self) -> Duration {
        self.started.elapsed()
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
    fn capture(
        &mut self,
        bounds: Rect,
        whole: bool,
    ) -> Result<Option<uptake_capture::CapturedRegion>, String> {
        let desktop = crate::overlay::monitor_rects();
        // Averaged down on the GPU and read back at one texel per 4-pixel
        // fingerprint cell (`1.61`): when nothing has to be painted out and
        // the area is on one monitor.
        if !whole {
            match uptake_capture::warm::watch_capture_small(bounds, &desktop) {
                Ok(small) => {
                    self.troubles.worked(bounds, Route::Small);
                    return Ok(small);
                }
                // Spanning monitors is not a fault: the region read below.
                Err(error) if error == uptake_capture::warm::NOT_ON_ONE_MONITOR => {}
                // Nor is a monitor that cannot be held: said once below.
                Err(error) if error == uptake_capture::warm::UNSERVABLE => {}
                Err(error) => self.failed(bounds, Route::Small, &error),
            }
        }
        // Otherwise only the area's own rectangle, copied on the GPU from the held
        // sessions and read back alone. A monitor whose session is still
        // warming up has nothing to read yet; one that cannot be held at all
        // falls back to a capture of its own, which the governor then paces
        // by what it costs.
        match uptake_capture::warm::watch_capture_region(bounds, &desktop) {
            Ok(read) => {
                self.troubles.worked(bounds, Route::Region);
                Ok(read)
            }
            Err(error) => {
                let route = if error == uptake_capture::warm::UNSERVABLE {
                    Route::Unheld
                } else {
                    Route::Region
                };
                self.failed(bounds, route, &error);
                crate::output::capture_for_watch(bounds).map(Some)
            }
        }
    }
    fn own_windows(&mut self) -> Vec<Rect> {
        crate::notice::own_windows()
    }
    fn trouble(&mut self, error: &str) {
        crate::diagnostics::trouble(
            "watch: a reading could not be taken (said once until one is)",
            &error,
        );
    }
    fn notify(&mut self, event: Event, id: AreaId, bounds: Rect) {
        notify(self.app, event, id, bounds);
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
        started: Instant::now(),
        troubles: Troubles::default(),
    };
    let mut memory = Memory::default();
    uptake_capture::warm::on_watch_frame(ring);
    loop {
        let began = Instant::now();
        let timeout = pass(&mut host, &mut memory);
        // The governor: rest in proportion to what the pass cost, and let the
        // frames that arrive meanwhile ring once for all of them.
        let rest = rest_after(began.elapsed());
        std::thread::sleep(rest);
        wait(timeout.map(|timeout| timeout.saturating_sub(rest)));
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
            return Some(LOOK);
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
    let at = host.now();
    // What the next wait must be at most: a watcher waiting for its first
    // frame, or a change settling, cannot wait for the next event.
    let mut soon: Option<Duration> = None;
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
                // Not read, and read again from a fresh baseline: soon, since
                // what it waits for (the page's word on its mark, a move
                // ending) rings nothing reliably, and a change made in the
                // wait would become its baseline (review of `#162`, round 1).
                watchers.remove(&target.id);
                soon = Some(FRESH_WAIT);
                continue;
            }
            Gate::Read => {}
        }
        let watcher = watchers.entry(target.id).or_insert_with(|| Watcher {
            target,
            detector: Detector::for_watch(target.watch),
            failing: false,
            fresh: true,
            picture: None,
            gate: EventGate::default(),
            read_at: None,
        });
        if watcher.target != target {
            // Only the tuning changed: nothing in the reading did, so the
            // watcher keeps its baseline and its gate.
            let tuning_only = Target {
                tuning: target.tuning,
                ..watcher.target
            } == target;
            watcher.target = target;
            if !tuning_only {
                watcher.detector = Detector::for_watch(target.watch);
                watcher.gate = EventGate::default();
                watcher.fresh = true;
            }
        }
        if watcher.fresh {
            watcher.fresh = false;
            soon = Some(FRESH_WAIT);
            continue;
        }
        // Its sampling rate (roadmap `1.65`): not read again until its step
        // has passed, and woken then whatever rings.
        if let Some(wait) = target.tuning.sample_wait(watcher.read_at, at) {
            soon = Some(soon.map_or(wait, |sooner| sooner.min(wait)));
            continue;
        }
        watcher.read_at = Some(at);
        let reading = match host.capture(target.bounds, state.1) {
            Ok(None) => {
                // No frame held yet for a monitor it covers: try again soon.
                soon = Some(FRESH_WAIT);
                continue;
            }
            Ok(Some(mut captured)) => {
                watcher.failing = false;
                leave_out(
                    &mut captured.bitmap,
                    &left_out(target.bounds, captured.rect, state.1),
                );
                // Only while UP-TAKE is in captures: with the setting off a
                // card is not in the picture and what is under it is read as
                // it is, so a change there is reported at once (review of
                // `#160`, round 3; round 2 hid it always, which kept such a
                // change from ever showing). A card on screen when the setting
                // is turned off is given the new policy at once
                // (`notice::apply_capture_policy`).
                let hidden = if state.1 {
                    unknown_under(&host.own_windows(), captured.rect)
                } else {
                    Vec::new()
                };
                let size = uptake_core::geometry::Size::new(
                    captured.bitmap.width(),
                    captured.bitmap.height(),
                );
                let switched = watcher.picture.is_some_and(|was| was != size);
                watcher.picture = Some(size);
                // The region's own size: the GPU route reads it averaged down
                // to one texel per cell, and the cells are the region's.
                let fingerprint =
                    Fingerprint::of_region(&captured.bitmap, captured.rect.size, &hidden);
                if switched {
                    // Another route: re-anchored, not compared (round 6).
                    if let Some(fingerprint) = fingerprint {
                        watcher.detector.rebase_at(fingerprint, at);
                    }
                    None
                } else {
                    fingerprint
                }
            }
            Err(error) => {
                if !watcher.failing {
                    host.trouble(&error);
                }
                watcher.failing = true;
                None
            }
        };
        // The area's tolerance (roadmap `1.61`) and, for *Notify when it
        // stops changing*, its calm, as they are now: the panel may have
        // moved them since the last reading.
        watcher.detector.tune(target.watch, target.tuning);
        if let Some(event) = reading.and_then(|reading| watcher.detector.observe_at(reading, at))
            && target.watch.notifies(event)
            && watcher.gate.admit(target.tuning, at).notifies
        {
            host.notify(event, target.id, target.bounds);
        }
        if watcher.detector.is_settling() {
            soon = Some(soon.map_or(SETTLE_POLL, |wait| wait.min(SETTLE_POLL)));
        }
    }
    host.end_pass();
    Some(soon.unwrap_or(IDLE))
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

/// Shows UP-TAKE's own card for `event` (`ADR-0054`), on the monitor the
/// user's setting picks for the area at `bounds` (`notice::card_monitor`). Says which moment it was and never what the area read.
///
/// Not a Windows toast any more: at the first real-screen test Windows
/// accepted every toast and showed none.
fn notify(app: &AppHandle, event: Event, id: AreaId, bounds: Rect) {
    let (moment, label) = match event {
        Event::Changed => ("changed", text(Text::NoticeChanged)),
        Event::Settled => ("settled", text(Text::NoticeSettled)),
    };
    let name = {
        let store = app.state::<Mutex<AreaStore>>();
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        store.name(id).map(str::to_string)
    };
    crate::notice::show(
        app,
        crate::notice::Raised {
            area: id.get(),
            bounds,
            name,
            area_type: text(Text::NoticeTypeWatch),
            tone: "watch",
            moment,
            event: label,
        },
    );
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
            /// One of UP-TAKE's own windows on screen, and the grey it shows,
            /// desktop pixels; listed by `own_windows`.
            card: Option<(Rect, u8)>,
            /// The clock `now` gives: one reading step a pass.
            clock: Duration,
            /// How many more captures find no frame held yet.
            unready: usize,
            /// Whether each capture asked for the full picture.
            whole: Vec<bool>,
            /// The size of the picture captures return, when not the area's.
            picture: Option<Size>,
            /// A small part of the screen showing another grey, desktop
            /// pixels: a change to some of an area and not all of it.
            patch: Option<(Rect, u8)>,
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
                    card: None,
                    clock: Duration::ZERO,
                    unready: 0,
                    whole: Vec::new(),
                    picture: None,
                    patch: None,
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
            fn now(&mut self) -> Duration {
                self.clock += uptake_core::watch::READING_STEP;
                self.clock
            }
            fn capture(
                &mut self,
                bounds: Rect,
                whole: bool,
            ) -> Result<Option<CapturedRegion>, String> {
                self.captured.push(bounds);
                self.whole.push(whole);
                if self.unready > 0 {
                    self.unready -= 1;
                    return Ok(None);
                }
                if let Some(state) = self.state_after_capture.take() {
                    self.state = state;
                }
                let rect = bounds
                    .intersection(self.monitor)
                    .ok_or_else(|| "off screen".to_owned())?;
                let mut pixels = [self.level, self.level, self.level, 255]
                    .repeat(usize::try_from(rect.size.width * rect.size.height).unwrap_or(0));
                if let Some((patch, grey)) = self.patch
                    && let Some(part) = patch.intersection(rect)
                {
                    let width = i32::try_from(rect.size.width).unwrap_or(0);
                    for y in
                        part.origin.y..part.origin.y + i32::try_from(part.size.height).unwrap_or(0)
                    {
                        for x in part.origin.x
                            ..part.origin.x + i32::try_from(part.size.width).unwrap_or(0)
                        {
                            let at = usize::try_from(
                                ((y - rect.origin.y) * width + (x - rect.origin.x)) * 4,
                            )
                            .unwrap_or(0);
                            if let Some(pixel) = pixels.get_mut(at..at + 3) {
                                pixel.copy_from_slice(&[grey, grey, grey]);
                            }
                        }
                    }
                }
                // The card, drawn into the picture as a capture would show it:
                // only while the setting puts UP-TAKE in captures.
                if self.state.1
                    && let Some((card, grey)) = self.card
                    && let Some(part) = card.intersection(rect)
                {
                    for y in
                        part.origin.y..part.origin.y + i32::try_from(part.size.height).unwrap_or(0)
                    {
                        for x in part.origin.x
                            ..part.origin.x + i32::try_from(part.size.width).unwrap_or(0)
                        {
                            let at = usize::try_from(
                                ((y - rect.origin.y) * i32::try_from(rect.size.width).unwrap_or(0)
                                    + (x - rect.origin.x))
                                    * 4,
                            )
                            .unwrap_or(0);
                            if let Some(pixel) = pixels.get_mut(at..at + 3) {
                                pixel.copy_from_slice(&[grey, grey, grey]);
                            }
                        }
                    }
                }
                // Another route's picture: another size, the same grey.
                if let Some(size) = self.picture {
                    let pixels = [self.level, self.level, self.level, 255]
                        .repeat(usize::try_from(size.width * size.height).unwrap_or(0));
                    let bitmap =
                        RgbaBitmap::from_pixels(size, pixels).ok_or_else(|| "bitmap".to_owned())?;
                    return Ok(Some(CapturedRegion { rect, bitmap }));
                }
                let bitmap =
                    RgbaBitmap::from_pixels(Size::new(rect.size.width, rect.size.height), pixels)
                        .ok_or_else(|| "bitmap".to_owned())?;
                Ok(Some(CapturedRegion { rect, bitmap }))
            }
            fn own_windows(&mut self) -> Vec<Rect> {
                self.card.map(|(card, _)| card).into_iter().collect()
            }
            fn trouble(&mut self, _error: &str) {}
            fn notify(&mut self, event: Event, _id: AreaId, _bounds: Rect) {
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
                        label: crate::watch::label_of(None),
                        tuning: uptake_core::watch::Tuning::default(),
                    })
                })
                .collect()
        }

        /// The founder's rig, 2026-11-03 (`I-480`): with *Show UP-TAKE in
        /// screen recordings* on, renaming a watch area told him it changed,
        /// because its new label is new pixels in the reading. A renamed area
        /// is a new target, read from a fresh baseline: the label is not a
        /// change, and a real change after it still is.
        #[test]
        fn renaming_a_watch_area_is_not_a_change_of_its_screen() {
            let mut store = AreaStore::new();
            let bounds = Rect::new(100, 100, 600, 400);
            let Some(id) = store.create(AreaType::Watch, bounds) else {
                panic!("an area");
            };
            let target = |store: &AreaStore| {
                store
                    .get(id)
                    .map(|area| crate::watch::target_of(store, area))
                    .unwrap_or_else(|| panic!("the area"))
            };
            let before = target(&store);
            assert!(store.set_name(id, "Download"));
            let renamed = target(&store);
            assert_ne!(before, renamed, "a new name is a new target");
            assert!(!store.set_name(id, "Download"), "unchanged");
            assert_eq!(renamed, target(&store), "the same name is not");

            let mut fake = Fake::new(vec![before]);
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            // The rename: the label's pixels arrive with the new target.
            fake.targets = vec![renamed];
            fake.level = 20;
            run(&mut fake, &mut memory, 4);
            assert!(fake.notified.is_empty(), "its own label is no change");
            fake.level = 200;
            run(&mut fake, &mut memory, 1);
            assert_eq!(fake.notified, vec![Event::Changed], "still watching");
        }

        fn run(fake: &mut Fake, memory: &mut Memory, passes: usize) {
            for _ in 0..passes {
                let _ = pass(fake, memory);
            }
        }

        /// Review of `#160`, round 1 (`I-480`): with *Show UP-TAKE in screen
        /// recordings* on, a card over a watch area is in its reading. Its
        /// coming, changing and going is no change, so a card cannot raise the
        /// next card. Round 2: the same with the setting off, since a card
        /// placed before the setting was turned off can still be in the
        /// picture.
        #[test]
        fn a_card_over_a_watch_area_is_no_change_of_its_screen() {
            let card = Rect::new(300, 100, 200, 60);
            for in_captures in [true, false] {
                let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
                fake.state = (OverlayState::Living, in_captures);
                let mut memory = Memory::default();
                run(&mut fake, &mut memory, 2);
                for grey in [30, 120, 250] {
                    fake.card = Some((card, grey));
                    run(&mut fake, &mut memory, 2);
                }
                fake.card = None;
                run(&mut fake, &mut memory, 4);
                assert!(fake.notified.is_empty(), "{in_captures}: its own card");
                fake.level = 20;
                run(&mut fake, &mut memory, 1);
                assert_eq!(fake.notified, vec![Event::Changed], "{in_captures}");
            }
        }

        /// Review of `#160`, round 3: with the setting off (the default) the
        /// card is not in the picture, so what is under it is still read, and
        /// a change there is reported at once.
        #[test]
        fn with_up_take_out_of_captures_a_change_under_a_card_is_seen() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            fake.card = Some((Rect::new(100, 100, 600, 400), 250));
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            assert_eq!(fake.notified, vec![Event::Changed]);
        }

        /// The event-driven loop (2026-11-05): after a pass it waits for the
        /// next frame, at most [`IDLE`]; [`FRESH_WAIT`] while a watcher waits
        /// for its first frame; [`SETTLE_POLL`] while a change settles, since
        /// the calm sends no frames; [`LOOK`] while nothing can be read.
        #[test]
        fn each_pass_says_how_long_the_next_may_wait() {
            use super::super::{FRESH_WAIT, IDLE, LOOK, SETTLE_POLL};
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            assert_eq!(pass(&mut fake, &mut memory), Some(FRESH_WAIT), "new");
            fake.unready = 1;
            assert_eq!(
                pass(&mut fake, &mut memory),
                Some(FRESH_WAIT),
                "no frame yet"
            );
            assert_eq!(pass(&mut fake, &mut memory), Some(IDLE), "baseline, calm");
            fake.level = 20;
            assert_eq!(pass(&mut fake, &mut memory), Some(SETTLE_POLL), "changed");
            assert_eq!(fake.notified, vec![Event::Changed]);
            // The fake clock steps half a second a pass, longer than the
            // re-arm: the next pass ends the change.
            assert_eq!(pass(&mut fake, &mut memory), Some(IDLE), "settled");
            fake.seen = false;
            fake.still = false;
            fake.targets.clear();
            assert_eq!(pass(&mut fake, &mut memory), None, "nothing switched on");
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            fake.state = (OverlayState::Hidden, false);
            assert_eq!(
                pass(&mut fake, &mut Memory::default()),
                Some(LOOK),
                "hidden"
            );
        }

        /// A reading asks for the full picture exactly when UP-TAKE's own
        /// drawing is in captures, since it is painted out of the full-size
        /// picture first; otherwise the small one averaged down on the GPU
        /// will do (review of `#162`, round 1: no test held the routing).
        #[test]
        fn a_reading_asks_for_the_full_picture_only_when_drawing_is_left_out() {
            for in_captures in [false, true] {
                let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
                fake.state = (OverlayState::Living, in_captures);
                let mut memory = Memory::default();
                run(&mut fake, &mut memory, 3);
                assert!(!fake.whole.is_empty(), "read");
                assert!(
                    fake.whole.iter().all(|&whole| whole == in_captures),
                    "{in_captures}"
                );
            }
        }

        /// Review of `#162`, round 1: an area skipped by its gate (its mark not
        /// yet confirmed, say) is looked at again soon, not after the idle
        /// wait, or a change made meanwhile becomes its baseline.
        #[test]
        fn an_area_waiting_for_its_mark_is_looked_at_again_soon() {
            use super::super::FRESH_WAIT;
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            fake.seen = false;
            assert_eq!(pass(&mut fake, &mut Memory::default()), Some(FRESH_WAIT));
        }

        /// Review of `#162`, round 3: a failing read is logged once for its
        /// area and route, not again until that same read works; another
        /// area's success, or another route's, does not re-arm it.
        #[test]
        fn a_failing_read_is_logged_once_per_area_and_route() {
            use super::super::{Route, Troubles};
            let (a, b) = (Rect::new(0, 0, 10, 10), Rect::new(50, 0, 10, 10));
            let mut troubles = Troubles::default();
            assert!(troubles.failed(a, Route::Small), "first: logged");
            troubles.worked(a, Route::Region);
            troubles.worked(b, Route::Small);
            assert!(!troubles.failed(a, Route::Small), "not re-armed by others");
            assert!(troubles.failed(a, Route::Region), "another route: its own");
            troubles.worked(a, Route::Small);
            assert!(
                troubles.failed(a, Route::Small),
                "after it worked: logged again"
            );
            assert!(troubles.failed(b, Route::Unheld));
            assert!(!troubles.failed(b, Route::Unheld));
        }

        /// Review of `#162`, round 6: a route switch while a change settles
        /// still ends with *stopped changing*.
        #[test]
        fn a_route_switch_keeps_a_settling_change() {
            let mut fake = Fake::new(targets_for(
                &[Rect::new(100, 100, 600, 400)],
                Watch::Settles,
            ));
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            fake.picture = Some(Size::new(38, 25));
            let passes = 4 * usize::try_from(Watch::Settles.settle_readings()).unwrap_or(0);
            run(&mut fake, &mut memory, passes);
            assert_eq!(fake.notified, vec![Event::Settled]);
        }

        /// Review of `#162`, round 5: a reading by another route gives a
        /// picture of another size, every cell different on a still screen;
        /// the watcher starts again from it instead of reporting a change.
        #[test]
        fn a_reading_by_another_route_is_no_change() {
            let mut fake = Fake::new(targets(&[Rect::new(100, 100, 600, 400)]));
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 3);
            fake.picture = Some(Size::new(38, 25));
            fake.level = 120;
            run(&mut fake, &mut memory, 3);
            fake.picture = None;
            fake.level = 200;
            run(&mut fake, &mut memory, 3);
            assert!(fake.notified.is_empty(), "{:?}", fake.notified);
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            assert_eq!(fake.notified, vec![Event::Changed], "still watching");
        }

        /// The governor: a pass that took `t` is followed by a rest of 49 `t`,
        /// so the watcher spends at most 2 % of a core, and never less than a
        /// frame.
        #[test]
        fn the_watcher_rests_in_proportion_to_what_it_spent() {
            use super::super::{BUDGET, rest_after};
            let frame = uptake_capture::warm::WATCH_FRAME_INTERVAL;
            assert_eq!(rest_after(Duration::ZERO), frame);
            assert_eq!(rest_after(Duration::from_micros(100)), frame);
            let spent = Duration::from_millis(10);
            let rest = rest_after(spent);
            assert_eq!(rest, Duration::from_millis(490));
            let share = spent.as_secs_f64() / (spent + rest).as_secs_f64();
            assert!((share - BUDGET).abs() < 1e-9);
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
                generation: 7,
                at,
            };
            assert!(seen_by(&word, one, OverlayState::Living, at, 7));
            assert!(
                !seen_by(&word, other, OverlayState::Living, at, 7),
                "not named"
            );
            assert!(
                !seen_by(&word, one, OverlayState::Placement, at, 7),
                "another state"
            );
            let moved = Target {
                bounds: Rect::new(110, 100, 600, 400),
                ..one
            };
            assert!(
                !seen_by(&word, moved, OverlayState::Living, at, 7),
                "other bounds"
            );
            let late = at + CONFIRMATION_LIFETIME + Duration::from_millis(1);
            assert!(
                !seen_by(&word, one, OverlayState::Living, late, 7),
                "too old"
            );
            assert!(seen_by(
                &word,
                one,
                OverlayState::Living,
                at + CONFIRMATION_LIFETIME,
                7
            ));
            // Round 21: a word measured before the latest redraw echoes an
            // older generation, however late it arrives.
            assert!(
                !seen_by(&word, one, OverlayState::Living, at, 8),
                "measured before the latest redraw"
            );
        }

        /// Rounds 20, 21, 24 and 25: every host message that can change what
        /// the page draws goes through `emit_redraw`, which moves the
        /// generation. The events are every `*_EVENT` constant in the crate
        /// but those named as not redrawing, each with its reason, so a new
        /// event is held here until someone says why it does not redraw. The
        /// scan reads every source file but this one, for every way of
        /// emitting, naming an event by its constant or by its text.
        #[test]
        fn every_message_that_redraws_the_page_moves_the_generation() {
            const NOT_REDRAWING: [(&str, &str); 6] = [
                (
                    "FREEZE_REVEAL_EVENT",
                    "only shows more; a mark it reveals is confirmed after",
                ),
                (
                    "ACTIVE_MONITOR_EVENT",
                    "moves the armed badge, drawn before every area",
                ),
                (
                    "CHANGED_EVENT",
                    "settings: area border and wash alpha, never the mark",
                ),
                ("DRAWN_EVENT", "the generation itself, sent by emit_redraw"),
                (
                    "NOTICE_EVENT",
                    "the notice card's own window (ADR-0054), not the overlay page",
                ),
                (
                    "PANEL_EVENT",
                    "the settings panel's own window (1.60), not the overlay page",
                ),
            ];
            const SENDS: [&str; 4] = [".emit(", "emit_to(", "emit_filter(", "Emitter::emit("];
            let floor = |text: &str, mut at: usize| {
                while !text.is_char_boundary(at) {
                    at -= 1;
                }
                at
            };
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
            let Ok(listing) = std::fs::read_dir(&dir) else {
                panic!("{} is readable", dir.display())
            };
            let mut sources: Vec<(String, String)> = Vec::new();
            for path in listing.flatten().map(|entry| entry.path()) {
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned();
                let Ok(source) = std::fs::read_to_string(&path) else {
                    panic!("{} is readable", path.display())
                };
                sources.push((name, source));
            }
            assert!(
                sources.len() > 10,
                "read {} files; the scan has stopped seeing the crate",
                sources.len()
            );
            // Every `const NAME_EVENT: &str = "text";` in the crate.
            let mut declared: Vec<(String, String)> = Vec::new();
            for (_, source) in &sources {
                for line in source.lines() {
                    let line = line.trim_start().trim_start_matches("pub ");
                    let Some(rest) = line.strip_prefix("const ") else {
                        continue;
                    };
                    let Some((name, value)) = rest.split_once(": &str = ") else {
                        continue;
                    };
                    if name.ends_with("_EVENT") {
                        declared.push((name.to_owned(), value.trim_end_matches(';').to_owned()));
                    }
                }
            }
            for (exempt, _) in NOT_REDRAWING {
                assert!(
                    declared.iter().any(|(name, _)| name == exempt),
                    "{exempt} is exempt and no longer declared"
                );
            }
            let events: Vec<(String, String)> = declared
                .into_iter()
                .filter(|(name, _)| !NOT_REDRAWING.iter().any(|(exempt, _)| exempt == name))
                .collect();
            assert!(
                events.len() >= 11,
                "found {} redrawing events",
                events.len()
            );
            let mut through = 0;
            for (name, source) in sources.iter().filter(|(name, _)| name != "watch.rs") {
                for send in SENDS {
                    for (at, _) in source.match_indices(send) {
                        let end = floor(source, (at + 200).min(source.len()));
                        let after = &source[at..end];
                        assert!(
                            !events.iter().any(|(constant, text)| {
                                after.contains(constant.as_str()) || after.contains(text.as_str())
                            }),
                            "{name}: `{send}` at byte {at} sends a redrawing event past emit_redraw"
                        );
                    }
                }
                through += source.matches("crate::watch::emit_redraw(").count();
            }
            assert!(
                through >= 14,
                "found {through}; the scan has stopped seeing them"
            );
        }

        /// Round 23: a redraw's order is the page's only if every redraw runs
        /// on the main thread, where an emit reaches the page at once; one
        /// made elsewhere is queued and can arrive after a later one. No
        /// window here to run it on, so the source is held instead.
        #[test]
        fn a_redraw_runs_on_the_main_thread() {
            let source = include_str!("watch.rs");
            let Some(start) = source.find("pub fn emit_redraw<") else {
                panic!("emit_redraw is where it was");
            };
            let Some(length) = source[start..].find(
                "
}
",
            ) else {
                panic!("emit_redraw ends");
            };
            let body = &source[start..start + length];
            assert!(body.contains("app.run_on_main_thread(move ||"), "{body}");
            assert!(body.contains("in_order("), "{body}");
        }

        /// Serialises the tests that move the process-wide generation, so one
        /// cannot move it under another's assertions.
        static GENERATION_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

        /// Round 21: the generation moves before the event, so a word measured
        /// before it no longer counts, and is announced only after it, so the
        /// page cannot echo it before it has the change.
        #[test]
        fn the_generation_moves_before_the_event_and_is_told_after() {
            use super::super::{GENERATION, in_order};
            use std::sync::atomic::Ordering;
            let _serial = GENERATION_TESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let before = GENERATION.load(Ordering::SeqCst);
            let mut steps = Vec::new();
            let during = in_order(
                || GENERATION.load(Ordering::SeqCst),
                |told| steps.push(told),
            );
            assert!(during > before, "moved before the event was sent");
            assert_eq!(steps, vec![during], "told after, the same number");
        }

        /// Round 22: redraws from several threads at once still reach the
        /// page as event, then its number, one redraw at a time, with numbers
        /// rising, and the last number the page hears is the host's.
        #[test]
        fn redraws_from_many_threads_reach_the_page_in_order() {
            use super::super::{GENERATION, in_order};
            use std::sync::atomic::Ordering;
            use std::sync::{Arc, Mutex};
            #[derive(Debug, PartialEq)]
            enum Sent {
                Event(u64),
                Drawn(u64),
            }
            let _serial = GENERATION_TESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let window = Arc::new(Mutex::new(Vec::new()));
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let window = Arc::clone(&window);
                    std::thread::spawn(move || {
                        for _ in 0..500 {
                            in_order(
                                || {
                                    let now = GENERATION.load(Ordering::SeqCst);
                                    window
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .push(Sent::Event(now));
                                },
                                |told| {
                                    window
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .push(Sent::Drawn(told));
                                },
                            );
                        }
                    })
                })
                .collect();
            for thread in threads {
                assert!(thread.join().is_ok());
            }
            let sent = window
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut last = 0;
            for pair in sent.chunks(2) {
                let [Sent::Event(during), Sent::Drawn(told)] = pair else {
                    panic!("a redraw was split by another: {pair:?}");
                };
                assert_eq!(during, told, "the event and its number belong together");
                assert!(*told > last, "numbers reach the page rising");
                last = *told;
            }
            assert_eq!(
                last,
                GENERATION.load(Ordering::SeqCst),
                "the page ends on the host's"
            );
        }

        /// The page reads `generation` from the drawn event (`DrawnPayload` in
        /// `overlay-state.ts`), and nothing else in this module goes to it.
        #[test]
        fn the_drawn_payload_is_the_key_the_page_reads() {
            use crate::payload_keys::{assert_keys, assert_payload_coverage};
            assert_keys(
                "DrawnPayload",
                &super::super::DrawnPayload { generation: 3 },
                &["generation"],
            );
            assert_payload_coverage("watch.rs", include_str!("watch.rs"), &["DrawnPayload"], &[]);
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
        /// though part of the area is on screen. The corner is the top left
        /// since 2026-11-10, so the area hangs off the desktop's left edge.
        #[test]
        fn an_area_whose_mark_is_off_screen_is_not_read() {
            let mut fake = Fake::new(targets(&[Rect::new(-300, 100, 600, 400)]));
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
            // One reading more than the settle count, since a pass may land
            // on the reading that only starts the still run.
            let settle = usize::try_from(Watch::Settles.settle_readings()).unwrap_or(usize::MAX);
            run(&mut fake, &mut memory, settle + 1);
            assert_eq!(fake.notified, vec![Event::Settled]);
        }

        /// Roadmap `1.65`: an area with a sampling rate of one reading a
        /// second reads every other pass at the fake's half-second clock,
        /// and one set to no notifications reports a change to no card.
        #[test]
        fn an_areas_tuning_spaces_its_readings_and_silences_its_cards() {
            let mut list = targets(&[Rect::new(100, 100, 600, 400)]);
            list[0].tuning.sample_ms = 1_000;
            let mut fake = Fake::new(list.clone());
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 1); // fresh: no reading
            run(&mut fake, &mut memory, 10);
            assert_eq!(fake.captured.len(), 5, "one reading a second");

            list[0].tuning = uptake_core::watch::Tuning {
                notify: uptake_core::watch::Notify::Never,
                ..uptake_core::watch::Tuning::default()
            };
            let mut fake = Fake::new(list);
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            assert!(fake.notified.is_empty(), "no card");
        }

        /// The maintainer's rig, 2026-11-10: *Notify when it stops changing*
        /// said nothing. A bar grows across a 600 by 400 area for five
        /// seconds and stops: one notification, that it settled.
        #[test]
        fn a_bar_that_fills_and_stops_says_it_settled() {
            let list = targets_for(&[Rect::new(100, 100, 600, 400)], Watch::Settles);
            let mut fake = Fake::new(list);
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            for step in 1..=10_u32 {
                fake.patch = Some((Rect::new(120, 280, 40 * step, 20), 20));
                run(&mut fake, &mut memory, 1);
            }
            assert!(fake.notified.is_empty(), "nothing while it moves");
            let settle = usize::try_from(Watch::Settles.settle_readings()).unwrap_or(usize::MAX);
            run(&mut fake, &mut memory, settle + 2);
            assert_eq!(fake.notified, vec![Event::Settled]);
        }

        /// The maintainer, 2026-11-10: *Notify when it stops changing* follows
        /// the area's own setting. With a calm of one second, a change that
        /// stops says so after two still readings at the fake's half-second
        /// clock, not after the six seconds it used to wait.
        #[test]
        fn stops_changing_waits_the_areas_own_calm() {
            let mut list = targets_for(&[Rect::new(100, 100, 600, 400)], Watch::Settles);
            list[0].tuning.settle_ms = 1_000;
            let mut fake = Fake::new(list);
            let mut memory = Memory::default();
            run(&mut fake, &mut memory, 2);
            fake.level = 20;
            run(&mut fake, &mut memory, 1);
            assert!(fake.notified.is_empty(), "nothing while it changes");
            run(&mut fake, &mut memory, 3);
            assert_eq!(fake.notified, vec![Event::Settled]);
        }

        /// Roadmap `1.61`: the host reads with the area's own tolerance. A
        /// 16 by 16 change on a 600 by 400 area (16 of its 15,000 4-pixel
        /// cells, about 0.1 % of it) is a change at *any change*, and none at
        /// one percent of the area.
        #[test]
        fn an_areas_tolerance_decides_how_much_change_counts() {
            let small_change = |share_permille: u16| {
                let mut list = targets(&[Rect::new(100, 100, 600, 400)]);
                list[0].tuning.share_permille = share_permille;
                let mut fake = Fake::new(list);
                let mut memory = Memory::default();
                run(&mut fake, &mut memory, 2);
                fake.patch = Some((Rect::new(300, 300, 16, 16), 20));
                run(&mut fake, &mut memory, 1);
                fake.notified
            };
            assert_eq!(small_change(0), vec![Event::Changed], "any change");
            assert!(small_change(10).is_empty(), "one percent of the area");
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
    /// them, a small area keeps something to read: one whose inside the mark's
    /// corner would cover leaves the corner in.
    #[test]
    fn a_reading_always_keeps_something_to_compare() {
        let small = Rect::new(0, 0, 20, 16);
        assert!(left_out(small, small, false).is_empty(), "the default");
        let parts = left_out(small, small, true);
        assert_eq!(parts.len(), 4, "the border only, not the corner");
        let Some(mut bitmap) = uptake_core::bitmap::RgbaBitmap::from_pixels(
            uptake_core::geometry::Size::new(20, 16),
            [255_u8; 4].repeat(20 * 16),
        ) else {
            panic!("a 20 by 16 bitmap");
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

    /// The mark is on screen only when its whole corner, the area's top left,
    /// is on one monitor: not past the desktop's right edge, not above its
    /// top, not across the gap where a shorter monitor ends.
    #[test]
    fn the_mark_must_be_wholly_on_one_monitor() {
        let left = Rect::new(0, 0, 1920, 1080);
        let right = Rect::new(1920, 0, 1280, 720);
        let monitors = [left, right];
        let inside = Rect::new(100, 100, 600, 400);
        assert_eq!(mark(inside), Rect::new(100, 100, MARK.0, MARK.1));
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
            mark(Rect::new(3190, 100, 600, 400)),
            &monitors
        ));
        // Above the top of the desktop.
        assert!(!mark_on_screen(
            mark(Rect::new(100, -20, 600, 400)),
            &monitors
        ));
        // Below the shorter right monitor, beside the taller left one.
        assert!(!mark_on_screen(
            mark(Rect::new(2000, 900, 600, 100)),
            &monitors
        ));
        // Across the seam between two monitors: on neither wholly.
        assert!(!mark_on_screen(
            mark(Rect::new(1910, 100, 300, 300)),
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

    /// The parts left out sit on the area's own edges and its top left
    /// corner, in the reading's pixels.
    #[test]
    fn own_drawing_is_the_border_and_the_marks_corner() {
        let bounds = Rect::new(100, 50, 600, 400);
        let parts = own_drawing(bounds, bounds);
        assert_eq!(parts[0], Rect::new(0, 0, 600, EDGE));
        assert_eq!(parts[1], Rect::new(0, 400 - 4, 600, EDGE));
        assert_eq!(parts[2], Rect::new(0, 0, EDGE, 400));
        assert_eq!(parts[3], Rect::new(600 - 4, 0, EDGE, 400));
        assert_eq!(parts[4], Rect::new(0, 0, MARK.0, MARK.1));
    }

    /// An area hanging off the desktop's left edge: the capture starts 30
    /// pixels into it, so every part moves left by 30.
    #[test]
    fn own_drawing_follows_a_clamped_capture() {
        let bounds = Rect::new(-30, 0, 300, 200);
        let taken = Rect::new(0, 0, 270, 200);
        let parts = own_drawing(bounds, taken);
        assert_eq!(parts[0], Rect::new(-30, 0, 300, EDGE));
        assert_eq!(parts[4], Rect::new(-30, 0, MARK.0, MARK.1));
    }

    /// An area smaller than the mark's corner leaves out what it has.
    #[test]
    fn a_small_area_leaves_out_no_more_than_itself() {
        let bounds = Rect::new(0, 0, 20, 16);
        assert_eq!(own_drawing(bounds, bounds)[4], Rect::new(0, 0, 20, 16));
    }
}
