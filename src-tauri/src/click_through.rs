//! Per-region click-through (roadmap task 1.2).
//!
//! Windows only supports click-through per *window* (`WS_EX_TRANSPARENT`), not
//! per region, so the overlay polls the cursor at ~60 fps while visible and
//! toggles `set_ignore_cursor_events` as the cursor crosses region boundaries:
//! inside an interactive region the window takes input, everywhere else clicks
//! fall through to whatever is underneath.
//!
//! The poll lives on the Rust side by necessity, not preference: the moment the
//! window ignores cursor events the WebView stops receiving mouse moves, so a
//! JS `mousemove` listener could observe the cursor *leaving* an interactive
//! region but never re-entering one.
//!
//! Budget (quality-bars.md §1): poll CPU < 3 % of one core, and the poll runs
//! **only while the overlay is visible**. The poll thread parks on a condvar
//! whenever the overlay is hidden, so a hidden overlay costs zero ticks — the
//! idle-CPU budget (< 0.5 %) is met by construction, not by measurement.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tauri::{AppHandle, Manager, State, WebviewWindow};
use uptake_core::geometry::{CssRect, Point, Rect, point_in_any};

use crate::overlay::overlay_window;

/// Target cadence: ~60 fps. The effective rate depends on the Windows timer
/// resolution (`Sleep` granularity is ~16 ms only when some process holds the
/// resolution at 1 ms), which is acceptable — the requirement that matters is
/// the CPU budget plus region transitions that feel instant, and even a
/// worst-case ~30 Hz tick keeps the transition under 35 ms.
const FRAME: Duration = Duration::from_millis(16);

/// Shared click-through state, managed via `app.manage`.
pub struct ClickThrough {
    /// Interactive regions in physical virtual-desktop pixels. While the poll
    /// is active, the window takes input inside them and ignores cursor events
    /// everywhere else.
    ///
    /// **Empty means "keep the whole window interactive"**, not "everything
    /// passes through": until the frontend has successfully reported its
    /// regions, click-through would strand the user — a click anywhere would
    /// fall through, focus the app underneath, and take Esc (the dismiss path)
    /// with it. Fail interactive, never fail click-through.
    regions: Mutex<Vec<Rect>>,
    /// Whether the poll should run. Guarded by `signal` so `activate` /
    /// `deactivate` can wake the poll thread promptly instead of leaving it in
    /// a stale 16 ms sleep.
    active: Mutex<bool>,
    signal: Condvar,
    /// The last state pushed to the OS, or `None` when that is unknown and the
    /// next tick must push unconditionally.
    ///
    /// This lives on the state rather than the poll thread's stack because
    /// [`activate`] has to invalidate it: `overlay::show` resets the window to
    /// interactive underneath a running poll, so a cached `Some(true)` would
    /// suppress the very push that restores click-through.
    applied: Mutex<Option<bool>>,
}

impl ClickThrough {
    /// Creates the state with no regions and the poll inactive.
    pub fn new() -> Self {
        Self {
            regions: Mutex::new(Vec::new()),
            active: Mutex::new(false),
            signal: Condvar::new(),
            applied: Mutex::new(None),
        }
    }
}

/// Starts the poll. Called by `overlay::show` after the window is visible.
///
/// Invalidates the applied-state cache first, because `show` has just reset the
/// window to interactive. Showing an *already visible* overlay — which the
/// global hotkey (task 1.4) makes reachable — would otherwise leave the poll
/// believing it had already applied click-through, so it would skip the push
/// and the overlay would swallow clicks across the whole virtual desktop until
/// the cursor next crossed a region boundary.
pub fn activate(app: &AppHandle) {
    let state = app.state::<ClickThrough>();
    *lock(&state.applied) = None;
    *lock(&state.active) = true;
    state.signal.notify_all();
}

/// Stops the poll. Called by `overlay::hide`; the poll thread resets the
/// window to interactive on its way into the parked state.
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

        loop {
            tick(app, &state);

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

        // Deactivated: leave the (now hidden) window interactive so the next
        // show starts from the known fail-safe state.
        if let Ok(window) = overlay_window(app)
            && let Err(error) = window.set_ignore_cursor_events(false)
        {
            eprintln!("click-through: could not reset on hide: {error}");
        }
    }
}

/// One poll step: read the cursor, decide, and push the decision to the OS
/// only when it differs from what is already applied — `WS_EX_TRANSPARENT`
/// does not need refreshing 60 times a second.
fn tick(app: &AppHandle, state: &ClickThrough) {
    let desired = desired_ignore(app, state);
    let mut applied = lock(&state.applied);
    if *applied == Some(desired) {
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
    match window.set_ignore_cursor_events(desired) {
        Ok(()) => *applied = Some(desired),
        Err(error) => eprintln!("click-through: could not apply state: {error}"),
    }
}

/// Whether the window should currently ignore cursor events.
///
/// Fail safe throughout: an unreadable cursor position, a non-finite
/// coordinate, or an empty region set all answer "no" — a wrongly-interactive
/// overlay still dismisses with Esc, while a wrongly click-through one lets a
/// click fall through, hand focus to the app underneath, and take the Esc
/// dismiss path with it.
fn desired_ignore(app: &AppHandle, state: &ClickThrough) -> bool {
    let Ok(position) = app.cursor_position() else {
        return false;
    };
    let Some(cursor) = Point::from_physical_f64(position.x, position.y) else {
        return false;
    };
    let regions = lock(&state.regions);
    if regions.is_empty() {
        return false;
    }
    !point_in_any(&regions, cursor)
}

/// Replaces the set of interactive regions.
///
/// Regions arrive in CSS pixels relative to the overlay's viewport (what
/// `getBoundingClientRect` reports) and are converted to physical
/// virtual-desktop pixels here, at the IPC boundary, through the sanctioned
/// conversion (architecture §3.1). The scale factor and window origin are read
/// at call time, so a report that races a monitor-layout change is stale for
/// at most one report cycle — the frontend re-reports on every window resize.
#[tauri::command]
pub fn overlay_set_interactive_regions(
    window: WebviewWindow,
    state: State<'_, ClickThrough>,
    regions: Vec<CssRect>,
) -> Result<(), String> {
    let scale_factor = window
        .scale_factor()
        .map_err(|e| format!("Could not read the overlay scale factor: {e}"))?;
    // Inner, not outer: CSS coordinates are relative to the *client* area, so
    // the origin they are offset by must be the client area's. The two are
    // equal today only because the overlay is `decorations: false`; using
    // `outer_position` would silently offset every region by the title-bar
    // height the moment that changed.
    let position = window
        .inner_position()
        .map_err(|e| format!("Could not read the overlay position: {e}"))?;
    let origin = Point::new(position.x, position.y);
    let physical: Vec<Rect> = regions
        .into_iter()
        .map(|region| region.to_physical(scale_factor, origin))
        .collect();
    *lock(&state.regions) = physical;
    Ok(())
}

/// Locks a mutex, treating poisoning as recoverable: the data under these
/// mutexes (a region list, a bool) is valid after any panic that could poison
/// them, and the no-panic rule (architecture §5) forbids unwrap here anyway.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
