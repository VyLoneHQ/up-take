//! The overlay poll: asserts the window's one input state and paces the
//! placement pump (roadmap tasks 1.2/1.6c).
//!
//! The overlay window is **never interactive**
//! ([ADR-0016](../../../Projects/UP-TAKE/DECISIONS/ADR-0016-living-input-via-the-global-hook.md)):
//! `WS_EX_TRANSPARENT` stays applied in every visible state, because an
//! interactive window overlapping hardware-accelerated video is the single
//! state that degrades it (ADR-0014), and per-area input arrives through the
//! global mouse hook instead (`crate::placement`). This module once toggled
//! click-through per cursor position against frontend-reported regions; that
//! machinery — the region store, the CSS→physical conversion at the IPC
//! boundary, the re-anchoring on display changes — was deleted with ADR-0016,
//! not disabled. If a future task needs the window to take input anywhere, it
//! is re-opening that ADR, not flipping a flag.
//!
//! What remains is the poll thread itself, which has two jobs:
//!
//! - **Assert click-through.** `overlay::show` sets it before the window is
//!   visible; the poll re-asserts it (cheaply, cached) so no code path — a
//!   future `set_focus` quirk, an external `SetWindowLong` — can leave the
//!   window interactive for more than one frame.
//! - **Pace [`crate::placement::pump`]** at ~60 Hz while the overlay is
//!   visible: the live gesture rectangle, the cursor shape, the hover
//!   highlights, and the hook health check. The mouse hook only writes
//!   atomics; the poll is where the per-frame work happens, which is what
//!   keeps the hook's callback fast enough that Windows does not remove it.
//!
//! Budget (quality-bars.md §1): the poll runs **only while the overlay is
//! visible** — it parks on a condvar whenever the overlay is hidden, so a
//! hidden overlay costs zero ticks and the idle-CPU budget (< 0.5 %) is met by
//! construction, not by measurement. Task 1.2 measured the visible poll at
//! 0.63 % of one core.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};

use crate::overlay::overlay_window;

/// A per-process high-resolution timer, for pacing finer than the system tick.
///
/// # Why this exists
///
/// The poll paced on `Condvar::wait_timeout`, which rounds up to the system
/// timer granularity — ~15.6 ms, so **~63 Hz whatever it asked for**. Measured
/// across 36 gestures on the dev rig: every one landed at 62–63 Hz against a
/// 250 Hz request. Any faster pacing needs a different waiting primitive, not a
/// smaller number.
///
/// `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` (Win10 1803+, well under the build
/// 19041 floor in `MASTER-PLAN.md` §4.1) gives sub-millisecond resolution **for
/// this object only**. That is the whole reason to prefer it over
/// `timeBeginPeriod`, which raises the timer resolution *globally* and with it
/// the power draw of every process on the machine — something an always-on tray
/// app has no business doing to a laptop on battery.
///
/// Used **only while a gesture is live**, so the idle path keeps its condvar and
/// stays promptly interruptible by `deactivate`. Losing that promptness for up
/// to one 4 ms tick during a drag costs nothing.
struct HighResTimer(HANDLE);

// SAFETY: a waitable timer handle is a kernel object, valid across threads; this
// one is created and waited on by the poll thread alone.
unsafe impl Send for HighResTimer {}

impl HighResTimer {
    /// Creates the timer, or `None` if the OS refuses — in which case the caller
    /// keeps its previous pacing rather than failing.
    fn new() -> Option<Self> {
        // SAFETY: all-null arguments are the documented form for an unnamed,
        // auto-reset timer; the returned handle is checked before use.
        let handle = unsafe {
            CreateWaitableTimerExW(
                std::ptr::null(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        (!handle.is_null()).then_some(Self(handle))
    }

    /// Waits `duration`, returning whether the wait actually happened — a `false`
    /// tells the caller to fall back rather than spin.
    fn wait(&self, duration: Duration) -> bool {
        // A negative 100 ns due time means "relative to now", which is the only
        // form that makes sense for pacing; an absolute time would drift with
        // the wall clock.
        let due = -(i64::try_from(duration.as_nanos() / 100).unwrap_or(i64::MAX >> 1));
        // SAFETY: `self.0` is a live timer handle owned by this struct; both
        // calls take it by value and touch nothing else of ours.
        unsafe {
            if SetWaitableTimer(self.0, &raw const due, 0, None, std::ptr::null(), 0) == 0 {
                return false;
            }
            WaitForSingleObject(self.0, u32::MAX) == WAIT_OBJECT_0
        }
    }
}

impl Drop for HighResTimer {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateWaitableTimerExW` and is closed
        // exactly once, here.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Target cadence: ~60 fps. The effective rate depends on the Windows timer
/// resolution (`Sleep` granularity is ~16 ms only when some process holds the
/// resolution at 1 ms), which is acceptable — the requirement that matters is
/// the CPU budget plus gesture feedback that feels instant, and even a
/// worst-case ~30 Hz tick keeps the selection box within ~35 ms of the mouse.
const FRAME: Duration = Duration::from_millis(16);

/// The pacing asked for while a gesture is live (~250 Hz).
///
/// # An experiment, not a settled value
///
/// A drag looks stepped on a high-refresh display and the poll's ~60 Hz is the
/// obvious suspect — but the chain is hook → atomic → poll tick → IPC emit →
/// Svelte reactivity → WebView paint, and the poll is only one term in it.
/// Raising the rate blind would spend CPU on a guess (F-39's lesson from earlier
/// today: do not act on an inferred split). So it is raised **only while a
/// gesture is live**, and the *achieved* interval is measured — which separates
/// two questions that look like one: did the rate actually change, and did that
/// make it smoother.
///
/// **It may well not change.** `Condvar::wait_timeout` is subject to the system
/// timer resolution, ~15.6 ms by default on Windows, so a 4 ms request can round
/// up to a whole tick and deliver nothing. Going finer needs `timeBeginPeriod`
/// (a **global** setting that raises power draw for every process on the
/// machine) or a high-resolution waitable timer. If the measurement shows the
/// interval pinned near 16 ms, that is the answer, and the fix is a timer
/// change rather than a smaller constant here.
///
/// The cost is bounded by construction: this applies only while a mouse button
/// is down, so it cannot touch the idle or resting-overlay CPU budgets.
///
/// # ⚠️ Measured 2026-07-26 — inert on a condvar, delivered on the timer
///
/// Across **36 gestures** on the dev rig the achieved rate was **62–63 Hz**,
/// every time, against the 250 Hz asked for here. The predicted clamp was real:
/// `Condvar::wait_timeout` rounds up to the system timer granularity (~15.6 ms
/// ⇒ ~64 Hz), so while this constant was paced on the condvar it was
/// indistinguishable from [`FRAME`] and always had been.
///
/// **The smoothness improvement reported from the build that introduced it was
/// therefore not caused by it.** Nothing else in that build touched rendering,
/// so the improvement was expectation rather than effect — recorded because a
/// change that appears to work while provably doing nothing is worse than one
/// that plainly fails.
///
/// It is **not** inert now: [`HighResTimer`] paces the gesture path instead
/// (see the wait in [`poll_loop`]), and the achieved rate measured **221 Hz**.
/// Whether *that* is what makes a drag feel smooth is a separate question the
/// rate alone cannot answer, which is why the emit→painted probe exists —
/// mean 3.7–4.3 ms, worst 7.7 ms over ~1200 samples, now a `quality-bars.md`
/// §1 row.
const FRAME_GESTURE: Duration = Duration::from_millis(4);

/// Shared poll state, managed via `app.manage`.
pub struct ClickThrough {
    /// Whether the poll should run. Guarded by `signal` so `activate` /
    /// `deactivate` can wake the poll thread promptly instead of leaving it in
    /// a stale 16 ms sleep.
    active: Mutex<bool>,
    signal: Condvar,
    /// Whether click-through has been pushed to the OS this show cycle, or
    /// `None` when that is unknown and the next tick must push
    /// unconditionally.
    ///
    /// This lives on the state rather than the poll thread's stack because
    /// [`activate`] has to invalidate it: `overlay::show` runs window calls
    /// underneath a possibly-running poll, and a stale `Some(true)` would
    /// suppress the re-assert that makes the baseline self-healing.
    applied: Mutex<Option<bool>>,
}

impl ClickThrough {
    /// Creates the state with the poll inactive.
    pub fn new() -> Self {
        Self {
            active: Mutex::new(false),
            signal: Condvar::new(),
            applied: Mutex::new(None),
        }
    }
}

/// Starts the poll. Called by `overlay::show` after the window is visible.
pub fn activate(app: &AppHandle) {
    let state = app.state::<ClickThrough>();
    *lock(&state.applied) = None;
    *lock(&state.active) = true;
    state.signal.notify_all();
}

/// Stops the poll. Called by `overlay::hide`; the poll thread re-asserts
/// click-through on its way into the parked state.
pub fn deactivate(app: &AppHandle) {
    let state = app.state::<ClickThrough>();
    *lock(&state.active) = false;
    state.signal.notify_all();
}

/// Spawns the single long-lived poll thread. Called once at setup, after
/// `ClickThrough` is managed; the thread parks until [`activate`].
///
/// One persistent thread instead of a spawn-per-show: a thread that is
/// starting up while `hide` and a second `show` race each other can miss the
/// stop flag and leave two pollers running. A single thread has no such
/// lifecycle to get wrong.
pub fn spawn_poll_thread(app: AppHandle) {
    std::thread::spawn(move || poll_loop(&app));
}

fn poll_loop(app: &AppHandle) -> ! {
    let state = app.state::<ClickThrough>();
    // Created once for the thread's lifetime. `None` means the OS refused it,
    // in which case pacing simply stays where it was — a missing optimisation,
    // not a failure.
    let high_res = HighResTimer::new();
    // Not gated on `UPTAKE_DEV_PACING`: this one reports a real degradation of
    // the shipping path rather than an instrumentation number, and it prints at
    // most once per process, only when the OS refused the timer.
    #[cfg(debug_assertions)]
    if high_res.is_none() {
        eprintln!("poll: no high-resolution timer available — gesture pacing stays at ~63 Hz");
    }
    loop {
        // Park while the overlay is hidden. Zero wakeups until `activate`.
        drop(
            state
                .signal
                .wait_while(lock(&state.active), |active| !*active)
                .unwrap_or_else(PoisonError::into_inner),
        );

        // Reset per show cycle: the pump's edge-triggered emits (gesture ended,
        // cursor shape changed, hover moved) compare against this, and a fresh
        // show starts from "nothing applied yet" rather than from whatever the
        // last cycle left behind.
        let mut pump = crate::placement::PumpState::default();
        // Measures what the OS actually delivered during a gesture, so a request
        // the timer resolution silently refused is visible instead of assumed.
        //
        // Debug builds only, and inside those only while `UPTAKE_DEV_PACING` is
        // set — so a release build carries neither the state nor the branch, and
        // an ordinary dev build does not count ticks it will never print.
        #[cfg(debug_assertions)]
        let mut gesture: Option<(u32, std::time::Instant)> = None;
        loop {
            tick(app, &state);
            // Drive the placement pump at the poll's cadence: the live gesture
            // rectangle, the cursor shape, the hover highlights, and the hook
            // health check. The mouse hook only writes atomics, so pacing the
            // work here caps it at ~60 Hz however fast the mouse reports — see
            // `placement::pump`.
            crate::placement::pump(app, &mut pump);

            // A live gesture is the only thing on screen that tracks the mouse
            // continuously, so it is the only thing that can look stepped.
            let dragging = crate::placement::is_dragging();
            // A live gesture is the only thing on screen that tracks the mouse
            // continuously, so it is the only thing that can look stepped.
            //
            // Opt-in via `UPTAKE_DEV_PACING` (see `dev_harness`): a line per
            // gesture is a line per drag, and the probe that feeds the second
            // line adds IPC to the path it measures — so the default dev build
            // is both silent and unweighted.
            #[cfg(debug_assertions)]
            if crate::dev_harness::pacing_enabled() {
                match (dragging, gesture) {
                    (true, None) => gesture = Some((0, std::time::Instant::now())),
                    (true, Some((ref mut ticks, _))) => *ticks = ticks.saturating_add(1),
                    (false, Some((ticks, start))) => {
                        // Reported per gesture rather than per tick: one line at
                        // the end says what the OS delivered, where a per-tick
                        // line would itself perturb what is being measured.
                        if ticks > 0 {
                            report_gesture(ticks, start);
                        }
                        gesture = None;
                    }
                    (false, None) => {}
                }
            }

            // A live gesture paces on the high-resolution timer, which is the
            // only way the requested rate is actually delivered. The lock is
            // dropped first: this wait is deliberately *not* interruptible by
            // `deactivate`, and holding the mutex across it would block the
            // event-loop thread trying to set it.
            if dragging && let Some(timer) = high_res.as_ref() {
                let still_active = *lock(&state.active);
                if !still_active {
                    break;
                }
                if timer.wait(FRAME_GESTURE) {
                    continue;
                }
                // The timer refused; fall through to the condvar rather than
                // spinning on a wait that is not happening.
            }

            // Pace to FRAME, but let deactivate cut the sleep short.
            let guard = lock(&state.active);
            if !*guard {
                break;
            }
            let (guard, _timeout) = state
                .signal
                .wait_timeout(guard, FRAME)
                .unwrap_or_else(PoisonError::into_inner);
            if !*guard {
                break;
            }
        }

        // A gesture still in hand when the loop exits is REPORTED, not dropped.
        //
        // **What this covers is the ABANDONMENT path, and NOT the create path.**
        // The first version of this comment said a create gesture ends by
        // deactivating the overlay, and that is false: `AreaType::after_create`
        // returns `StayInPlacement` for all seven types (ADR-0023), the state
        // machine keeps `Placement` on `AreaCreated`, and `overlay.rs` reaches
        // `deactivate` from `OverlayState::Hidden` alone. It was false at
        // `3cbeb1e` too — the exact commit of the rig pass it was offered to
        // explain — so it could never have been the cause of nine drags
        // producing zero lines. Caught in independent review, by reading the
        // call chain rather than by measuring.
        //
        // **On the create path this flush is unreachable**, and the ordering is
        // why: `WM_LBUTTONUP` clears `DRAGGING` *before* `finish_gesture`, and
        // `is_dragging` is read at the top of the iteration, so the next
        // iteration takes the `(false, Some(..))` arm and reports normally.
        //
        // **The path it does cover is real.** `OverlayState::Hidden` runs
        // `placement::exit` (which clears `DRAGGING`) and then `hide`, both on
        // the event-loop thread while this thread sleeps in `wait_timeout`. This
        // thread wakes, sees `!active`, and breaks **without re-reading
        // `is_dragging`** — so a drag abandoned by `Esc` or by toggling the
        // overlay away loses its gesture entirely. That is a genuine drop and
        // this is a genuine fix; it is simply not `I-11`'s cause.
        //
        // **`I-11` therefore remains undiagnosed**, and the honest next suspect
        // is the one its row lists first: `pacing_enabled()` returning false
        // because the variable never reached the process. `announce_pacing` is
        // what settles that, and it is this change's actual deliverable.
        //
        // The line is labelled so a reader can tell a flushed gesture from an
        // ordinary one — a completed drag and one whose overlay vanished
        // underneath it are different events (`UT-F-46`).
        #[cfg(debug_assertions)]
        if let Some((ticks, start)) = gesture.take()
            && ticks > 0
        {
            eprintln!("poll: gesture below was cut short by deactivate");
            report_gesture(ticks, start);
        }

        // Deactivated: the overlay is hidden. Leave the window click-through —
        // its only state (ADR-0016) — so nothing ever observes it interactive.
        if let Ok(window) = overlay_window(app)
            && let Err(error) = window.set_ignore_cursor_events(true)
        {
            eprintln!("click-through: could not reset on hide: {error}");
        }
    }
}

/// Prints what one completed gesture actually delivered: the achieved poll rate,
/// and the emit→painted round trip that the rate does not describe.
///
/// The two are independent and only the second is what "laggy" means — a rate
/// says how often we *tried* to put a frame on screen, not how long each attempt
/// took to get there. Reporting the rate alone is how a change that provably did
/// nothing was once read as an improvement (see [`FRAME_GESTURE`]).
#[cfg(debug_assertions)]
fn report_gesture(ticks: u32, start: std::time::Instant) {
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!(
        "poll: gesture ran {ticks} ticks in {:.0} ms — {:.0} Hz achieved (asked for {:.0} Hz)",
        elapsed * 1000.0,
        f64::from(ticks) / elapsed,
        1.0 / FRAME_GESTURE.as_secs_f64(),
    );
    match crate::placement::take_latency_summary() {
        Some((n, mean, worst)) => eprintln!(
            "poll: emit→painted over {n} samples — mean {mean:.1} ms, worst {worst:.1} ms"
        ),
        None => eprintln!(
            "poll: emit→painted — no samples returned (the frontend echo is not arriving)"
        ),
    }
}

/// One poll step: re-assert click-through if it is not known to be applied —
/// `WS_EX_TRANSPARENT` does not need refreshing 60 times a second, so the
/// cached state makes this free on every tick but the first.
fn tick(app: &AppHandle, state: &ClickThrough) {
    let mut applied = lock(&state.applied);
    if *applied == Some(true) {
        return;
    }
    let Ok(window) = overlay_window(app) else {
        return;
    };
    // Honest about what this records: on Windows tao posts the flag change to
    // the event-loop thread and returns `Ok` unconditionally, so `applied` is a
    // *requested* state, not a confirmed one, and the `Err` arm is unreachable.
    // It is kept for the platforms where the call is fallible; there, leaving
    // `applied` unchanged makes the next tick retry.
    match window.set_ignore_cursor_events(true) {
        Ok(()) => *applied = Some(true),
        Err(error) => eprintln!("click-through: could not apply state: {error}"),
    }
}

/// Locks a mutex, treating poisoning as recoverable: the data under these
/// mutexes (two flags) is valid after any panic that could poison them, and
/// the no-panic rule (architecture §5) forbids unwrap here anyway.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
