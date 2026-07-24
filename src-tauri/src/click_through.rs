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

use crate::overlay::overlay_window;

/// Target cadence: ~60 fps. The effective rate depends on the Windows timer
/// resolution (`Sleep` granularity is ~16 ms only when some process holds the
/// resolution at 1 ms), which is acceptable — the requirement that matters is
/// the CPU budget plus gesture feedback that feels instant, and even a
/// worst-case ~30 Hz tick keeps the selection box within ~35 ms of the mouse.
const FRAME: Duration = Duration::from_millis(16);

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
        loop {
            tick(app, &state);
            // Drive the placement pump at the poll's cadence: the live gesture
            // rectangle, the cursor shape, the hover highlights, and the hook
            // health check. The mouse hook only writes atomics, so pacing the
            // work here caps it at ~60 Hz however fast the mouse reports — see
            // `placement::pump`.
            crate::placement::pump(app, &mut pump);

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

        // Deactivated: the overlay is hidden. Leave the window click-through —
        // its only state (ADR-0016) — so nothing ever observes it interactive.
        if let Ok(window) = overlay_window(app)
            && let Err(error) = window.set_ignore_cursor_events(true)
        {
            eprintln!("click-through: could not reset on hide: {error}");
        }
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
