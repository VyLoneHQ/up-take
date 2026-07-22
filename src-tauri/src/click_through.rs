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

/// The interactive regions in both coordinate spaces, kept under one lock so
/// a reader can never see a CSS set paired with someone else's conversion.
struct RegionSet {
    /// What the frontend last reported, verbatim. Kept because it survives a
    /// window *move*: the WebView does not reflow, so the CSS rects stay valid
    /// while their physical conversion goes stale — see [`reconvert_regions`].
    ///
    /// It does **not** survive a scale change: the CSS viewport is resized, so
    /// a centred element's CSS coordinates genuinely move and only a fresh
    /// report can fix them. That report does arrive — but by a longer route
    /// than it looks; see [`reconvert_regions`] for the actual chain, which is
    /// load-bearing and not obvious.
    css: Vec<CssRect>,
    /// The scale factor `css` was measured in, as reported by the WebView's
    /// `devicePixelRatio`. Stored rather than re-read from the window, because
    /// these two can disagree — see [`overlay_set_interactive_regions`].
    scale: f64,
    /// The physical virtual-desktop conversion the poll hit-tests against.
    physical: Vec<Rect>,
}

impl RegionSet {
    /// The empty set: no interactive regions, so the whole window is
    /// click-through.
    ///
    /// The starting state, and the state to fall back to whenever a report
    /// cannot be trusted — see [`ClickThrough::regions`] for why empty now means
    /// fully click-through (ADR-0014) rather than interactive.
    fn empty() -> Self {
        Self {
            css: Vec::new(),
            scale: 1.0,
            physical: Vec::new(),
        }
    }
}

/// Shared click-through state, managed via `app.manage`.
pub struct ClickThrough {
    /// Interactive regions. While the poll is active, the window takes input
    /// inside them and ignores cursor events everywhere else.
    ///
    /// **Empty means "the whole window is click-through"** (ADR-0014): with no
    /// interactive area under the cursor, every click belongs to the app
    /// underneath, and the overlay must never degrade the live content it sits
    /// over. This is the resting `Living` shape, and the `Placement` shape too —
    /// there the mouse hook (`crate::placement`) supplies the drag, not this
    /// poll. Interactive areas (task 1.6c) add carve-outs where the window takes
    /// input; until one is reported there are none, so the overlay is fully
    /// click-through whenever it is visible.
    regions: Mutex<RegionSet>,
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
            regions: Mutex::new(RegionSet::empty()),
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

        // Reset per show cycle: makes the drag→idle clearing emit in
        // `pump_selection` fire once when a placement drag ends, not every tick.
        let mut was_dragging = false;
        loop {
            tick(app, &state);
            // Publish the live placement selection rectangle at the poll's
            // cadence. The mouse hook only writes atomics, so pacing the emit
            // here caps it at ~60 Hz however fast the mouse reports — see
            // `placement::pump_selection`.
            crate::placement::pump_selection(app, &mut was_dragging);

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

        // Deactivated: the overlay is hidden. Leave the window click-through,
        // the visible baseline (ADR-0014), so nothing ever observes it interactive.
        if let Ok(window) = overlay_window(app)
            && let Err(error) = window.set_ignore_cursor_events(true)
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
/// ADR-0014 inverts the old fail-safe: the overlay is **click-through whenever
/// visible** and takes input only where an interactive area sits under the
/// cursor. So the answer is "yes, ignore" (click-through) unless the cursor is
/// provably inside a reported region. Every unresolved case — no regions, an
/// unreadable cursor, a non-finite coordinate — fails toward click-through: a
/// lost click reaches the app underneath, which is the safe direction here, and
/// `Esc` still works from the overlay's keyboard focus regardless.
fn desired_ignore(app: &AppHandle, state: &ClickThrough) -> bool {
    // Investigation toggle (dev only): force click-through even where an
    // interactive area would otherwise take input. Redundant while no regions
    // are reported (the default is already click-through), but kept for testing
    // the per-area routing task 1.6c adds. See `dev_harness::force_click_through`.
    #[cfg(debug_assertions)]
    if crate::dev_harness::force_click_through() {
        return true;
    }
    let regions = lock(&state.regions);
    // The common case this session: no interactive areas, so fully click-through
    // — and it costs no cursor read.
    if regions.physical.is_empty() {
        return true;
    }
    let Ok(position) = app.cursor_position() else {
        return true;
    };
    let Some(cursor) = Point::from_physical_f64(position.x, position.y) else {
        return true;
    };
    !point_in_any(&regions.physical, cursor)
}

/// Replaces the set of interactive regions.
///
/// Regions arrive in CSS pixels relative to the overlay's viewport (what
/// `getBoundingClientRect` reports) and are converted to physical
/// virtual-desktop pixels here, at the IPC boundary, through the sanctioned
/// conversion (architecture §3.1).
///
/// **`scale` is supplied by the caller and is not optional.** It used to be
/// read from the window via `WebviewWindow::scale_factor`, on the assumption
/// that tao's per-window scale factor equals the one the WebView laid out in.
/// That assumption is false in a case the overlay hits routinely: the window is
/// created on the primary monitor, then resized to span a virtual desktop where
/// Windows assigns it a different DPI, and the two values diverge with nothing
/// detecting it. Measured on the dev rig with a 100 %-scaled primary and a
/// 125 % secondary — the WebView reported a pill at CSS x 2085.06 (a viewport
/// of 4448 = 5560/1.25) while the window reported scale 1.0, converting it to
/// physical x 724 instead of 1245. The hint pill was drawn 556 px from its own
/// hit box and could not be clicked.
///
/// The WebView's `devicePixelRatio` is by definition the factor its rects were
/// measured in, so travelling together makes the pair self-consistent. The
/// window's own value is now only a cross-check, logged on disagreement.
#[tauri::command]
pub fn overlay_set_interactive_regions(
    window: WebviewWindow,
    state: State<'_, ClickThrough>,
    regions: Vec<CssRect>,
    scale: f64,
) -> Result<(), String> {
    let scale = match checked_scale(scale) {
        Ok(scale) => scale,
        Err(error) => {
            // Clear to the empty (fully click-through) set rather than keep a
            // stale one: a bad scale means the stored boxes may be wrong, and
            // hit-testing wrong boxes would take input where it should pass
            // through. Nothing retries — the frontend logs the rejection and the
            // next report only comes on a `resize`.
            *lock(&state.regions) = RegionSet::empty();
            return Err(error);
        }
    };
    if let Ok(window_scale) = window.scale_factor()
        // Exact inequality on purpose: both sides are display scale factors off
        // Windows' ladder (1.0, 1.25, 1.5, …), all exactly representable, and
        // this only decides whether to log. A tolerance here would be a
        // tolerance in name only — `f64::EPSILON` in particular is an absolute
        // threshold sized for values near 1.0, not a scale-invariant one.
        && window_scale != scale
    {
        // Not an error: the WebView's value is the authoritative one here. It
        // is logged because a persistent disagreement is the fingerprint of the
        // bug above, and silence is how it went unnoticed the first time.
        eprintln!(
            "click-through: WebView scale {scale} disagrees with the window's {window_scale}; using the WebView's"
        );
    }
    let physical = convert_regions(&window, &regions, scale)?;
    *lock(&state.regions) = RegionSet {
        css: regions,
        scale,
        physical,
    };
    Ok(())
}

/// Rejects a scale factor that cannot describe a real display.
///
/// The WebView is the least-trusted component in the process (architecture §4),
/// and this value is now load-bearing for every hit test. A non-finite or
/// non-positive scale would send every region to the virtual-desktop origin;
/// `f64::clamp` propagates `NaN`, so the geometry layer cannot catch it either.
///
/// The caller clears the region set on rejection rather than keeping the
/// previous one. A stale set would leave the window taking input over boxes that
/// may no longer be where the areas are, capturing clicks the user meant for the
/// app underneath. Clearing to empty makes the overlay fully click-through until
/// a good report arrives — the safe direction under ADR-0014.
fn checked_scale(scale: f64) -> Result<f64, String> {
    if scale.is_finite() && scale > 0.0 && scale <= 16.0 {
        Ok(scale)
    } else {
        Err(format!("Implausible scale factor reported: {scale}"))
    }
}

/// Re-derives the physical regions from the stored CSS regions with the
/// window's *current* scale factor and origin.
///
/// This is the stale-origin fix (M-6 family): rearranging monitors can move
/// the virtual-desktop origin without resizing anything, so no resize event
/// reaches the frontend and it never re-reports — yet every stored physical
/// rect is anchored to the old origin. The CSS rects are still valid (the
/// WebView layout did not change), so re-running the conversion with fresh
/// inputs is exact. After a *resize*, the CSS rects themselves may be stale
/// for a frame or two until the frontend's resize listener re-reports; the
/// interim conversion is wrong-but-bounded, and the report that follows
/// replaces it wholesale.
///
/// Called on the main thread (window events, `overlay::sync_bounds`). The
/// lock is held across the window getter deliberately: it is a direct Win32
/// read, and dropping the lock would let a fresh frontend report interleave
/// and be overwritten with a conversion of older CSS data.
///
/// Re-converts with the **stored** scale, not the window's current one. The
/// stored scale is the one `css` was measured in, so the pair stays
/// self-consistent; if the scale has genuinely changed, the CSS rects are stale
/// too and only a fresh frontend report can fix them.
///
/// **How that fresh report actually arrives — verified against tao, because the
/// obvious answer is wrong.** A DPI change does *not* by itself resize the CSS
/// viewport: tao's `WM_DPICHANGED` handler rescales the window's physical size
/// to preserve its *logical* size (`tao-0.35.3`
/// `platform_impl/windows/event_loop.rs`, `old.to_logical(old_sf).to_physical(new_sf)`),
/// so physical size and `devicePixelRatio` move by the same ratio, CSS size is
/// unchanged, and the WebView fires no `resize`. What produces the re-report is
/// the correction *after* it: `ScaleFactorChanged` routes to
/// [`crate::overlay::sync_bounds`] (see the hook in `lib.rs`), which writes the
/// overlay back to the full virtual desktop — and *that* changes the CSS size
/// and fires `resize`.
///
/// So the precondition for this function's stored-scale strategy is not "a
/// scale change fires resize" but **"the overlay is always re-fitted to a
/// display-derived rect after a scale change"**. If overlay UI ever stops being
/// sized to the virtual desktop — task 1.6 is expected to move it per-monitor
/// (friction F-13) — check that a scale change still ends in a frontend report,
/// or the scale-mismatch bug returns with these comments still denying it.
pub fn reconvert_regions(app: &AppHandle) {
    let Ok(window) = overlay_window(app) else {
        return;
    };
    let state = app.state::<ClickThrough>();
    let mut regions = lock(&state.regions);
    if regions.css.is_empty() {
        return;
    }
    match convert_regions(&window, &regions.css, regions.scale) {
        Ok(physical) => regions.physical = physical,
        Err(error) => {
            // Fail click-through: clearing the physical set passes every click
            // to the app underneath rather than hit-testing boxes it could not
            // rebuild (ADR-0014).
            regions.physical.clear();
            eprintln!("click-through: could not re-convert regions: {error}");
        }
    }
}

/// Converts frontend-reported CSS rects into physical virtual-desktop rects
/// using the supplied scale and the window's origin as it is *right now*.
///
/// The origin is read here rather than passed in because it is genuinely the
/// window's property and the frontend cannot know it — unlike the scale, which
/// the frontend is the authority on.
fn convert_regions(
    window: &WebviewWindow,
    regions: &[CssRect],
    scale_factor: f64,
) -> Result<Vec<Rect>, String> {
    // Inner, not outer: CSS coordinates are relative to the *client* area, so
    // the origin they are offset by must be the client area's. The two are
    // equal today only because the overlay is `decorations: false`; using
    // `outer_position` would silently offset every region by the title-bar
    // height the moment that changed.
    let position = window
        .inner_position()
        .map_err(|e| format!("Could not read the overlay position: {e}"))?;
    let origin = Point::new(position.x, position.y);
    // Both sides of the IPC boundary printing their own numbers is what made
    // the scale mismatch visible; neither is wrong alone. Off unless the
    // harness is enabled — see dev_harness.rs.
    #[cfg(debug_assertions)]
    if let Ok(size) = window.inner_size() {
        crate::dev_harness::log_conversion(scale_factor, origin.x, origin.y, size.width);
    }
    Ok(regions
        .iter()
        .map(|region| region.to_physical(scale_factor, origin))
        .collect())
}

/// Locks a mutex, treating poisoning as recoverable: the data under these
/// mutexes (a region list, a bool) is valid after any panic that could poison
/// them, and the no-panic rule (architecture §5) forbids unwrap here anyway.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_display_scales_are_accepted() {
        // 100 %, 125 %, 150 %, 175 %, 200 %, 350 % — Windows' own ladder.
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 3.5] {
            assert_eq!(checked_scale(scale), Ok(scale));
        }
    }

    #[test]
    fn non_finite_scales_are_rejected() {
        // The one that matters: `f64::clamp` propagates NaN and `NaN as i32`
        // saturates to 0, so a NaN reaching the geometry layer would silently
        // move every region to the virtual-desktop origin.
        for scale in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(checked_scale(scale).is_err());
        }
    }

    #[test]
    fn non_positive_and_absurd_scales_are_rejected() {
        for scale in [0.0, -1.0, -1.25, 16.001, 1e9] {
            assert!(checked_scale(scale).is_err());
        }
    }

    #[test]
    fn the_empty_set_means_fully_click_through() {
        // Pins what the rejection path in `overlay_set_interactive_regions`
        // relies on: `RegionSet::empty` must leave nothing to hit-test, because
        // `desired_ignore` reads an empty `physical` as "click-through
        // everywhere" (ADR-0014). A future field that defaulted to something
        // non-empty would silently make the overlay take input with no area to
        // justify it.
        let empty = RegionSet::empty();
        assert!(empty.physical.is_empty());
        assert!(empty.css.is_empty());
    }
}
