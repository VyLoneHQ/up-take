//! Overlay window lifecycle: sizing it over the whole virtual desktop,
//! showing, hiding, and keeping it fitted while the display configuration
//! changes underneath it.
//!
//! The window itself is declared in `tauri.conf.json` and created hidden at
//! startup — showing it is then a reposition + `show()`, cheap enough for the
//! < 100 ms hotkey-to-visible budget (quality-bars.md §1). Creating the window
//! on demand would not be.
//!
//! Geometry decisions live in `uptake_core::geometry`; this module only maps
//! Tauri's monitor reports into core types and talks to the OS.

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};
use uptake_core::area::{AreaStore, AreaType};
use uptake_core::geometry::{Monitor, Point, Rect, Size, virtual_desktop_bounds};

use crate::click_through;
use crate::overlay_state::{Event, OverlayState, next};
use crate::placement;

/// Label of the overlay window as declared in `tauri.conf.json`.
pub const WINDOW_LABEL: &str = "overlay";

/// Resizes the overlay to cover the entire virtual desktop and shows it.
///
/// Bounds are recomputed on every call rather than cached, which covers
/// display changes that happen while the app sits hidden in the tray. The
/// other half of M-6 — a display change while the overlay is *visible* — is
/// [`sync_bounds`]'s job, driven by `display_watch` and the window-event hook
/// in `lib.rs`.
pub fn show(app: &AppHandle) -> Result<(), String> {
    let window = overlay_window(app)?;
    apply_bounds(&window, desired_bounds(&window)?)?;
    // Known baseline before anything is visible: **click-through** (ADR-0014).
    // The overlay must never degrade the live content it sits over, so it
    // ignores the cursor whenever it is visible — in `Placement` the mouse hook
    // (`placement`) supplies the drag, and in `Living` clicks belong to the apps
    // underneath. The poll re-asserts this within one frame; setting it here too
    // means the first visible frame is already click-through rather than
    // stealing a click before the poll's first tick.
    window
        .set_ignore_cursor_events(true)
        .map_err(|e| format!("Could not set overlay click-through: {e}"))?;
    window
        .show()
        .map_err(|e| format!("Could not show the overlay: {e}"))?;
    // Focus so keyboard input reaches the overlay even though it is
    // click-through: `WS_EX_TRANSPARENT` affects only mouse hit-testing, so a
    // focused click-through window still receives `Esc` (M-11 keyboard-only).
    // The hook swallows placement clicks, so focus is not stolen mid-placement;
    // and the global hotkey re-focuses from anywhere (F-13) as the guaranteed
    // fallback if it ever is.
    window
        .set_focus()
        .map_err(|e| format!("Could not focus the overlay: {e}"))?;
    // Re-anchor the stored regions to the origin just applied. [`sync_bounds`]
    // cannot do this for us: it returns early while the overlay is hidden, and
    // the reposition above happens *before* `show()`, so the `Moved` event it
    // raises finds an invisible window and does nothing.
    //
    // Without this, a display change between a hide and the next show leaves
    // every region anchored to the old origin. The frontend does not rescue it
    // — it re-reports on resize only, and a rearrangement that moves the
    // virtual-desktop origin without changing its size resizes nothing.
    click_through::reconvert_regions(app);
    click_through::activate(app);
    Ok(())
}

/// Hides the overlay. The window stays alive so the next `show` is instant.
pub fn hide(app: &AppHandle) -> Result<(), String> {
    // Stop the poll first: quality-bars.md §1 requires zero poll activity
    // while the overlay is hidden. The poll thread resets the window to
    // interactive as it parks.
    click_through::deactivate(app);
    overlay_window(app)?
        .hide()
        .map_err(|e| format!("Could not hide the overlay: {e}"))?;
    // Opt-in, debug-only, off unless UPTAKE_DEV_RESHOW is set: brings the
    // overlay back from a spawned thread so a display change can be made in
    // between. See dev_harness.rs for why that thread is the point.
    #[cfg(debug_assertions)]
    crate::dev_harness::schedule_reshow(app);
    Ok(())
}

/// Re-fits a *visible* overlay to the virtual desktop and refreshes the
/// click-through regions. This is M-6 while the overlay is up: a monitor
/// hot-plugged, unplugged, rearranged, or changing resolution or DPI.
///
/// Idempotent and self-converging: bounds are only written when they differ
/// from what the window already has, so the `Moved`/`Resized` events its own
/// writes raise come back here, find nothing left to fix, and stop. That
/// convergence is also what heals tao's `WM_DPICHANGED` handling — tao
/// rescales the window's physical size to preserve its *logical* size, which
/// is right for a normal window and wrong for one that must cover the virtual
/// desktop physically.
///
/// Regions are re-converted even when the bounds did not change: a scale-factor
/// change alone invalidates the CSS→physical conversion without moving the
/// window (see `click_through::reconvert_regions`).
///
/// A hidden overlay is left alone — [`show`] recomputes bounds anyway, and
/// resizing a hidden window would spend cycles on state the next `show`
/// discards.
pub fn sync_bounds(app: &AppHandle) -> Result<(), String> {
    let window = overlay_window(app)?;
    if !window
        .is_visible()
        .map_err(|e| format!("Could not read overlay visibility: {e}"))?
    {
        return Ok(());
    }
    let desired = desired_bounds(&window)?;
    if needs_write(current_bounds(&window)?, desired) {
        apply_bounds(&window, desired)?;
    }
    click_through::reconvert_regions(app);
    Ok(())
}

/// Whether [`sync_bounds`] must write the window's bounds.
///
/// Extracted and test-pinned because the whole sync ↔ window-event cycle
/// terminates on this returning `false` once the bounds agree: `apply_bounds`
/// raises `Moved`/`Resized`, which route straight back into `sync_bounds`. A
/// version of this that ever answers `true` for equal rectangles is not a
/// cosmetic bug — it is an unbounded `SetWindowPos` loop.
fn needs_write(current: Rect, desired: Rect) -> bool {
    current != desired
}

/// The rectangle the overlay must occupy: the whole virtual desktop.
fn desired_bounds(window: &WebviewWindow) -> Result<Rect, String> {
    virtual_desktop_bounds(monitors(window)?.iter().map(|monitor| monitor.bounds))
        .ok_or_else(|| "No display detected — the overlay needs at least one monitor.".to_string())
}

/// The window's current rectangle. Inner, not outer, to match the origin the
/// click-through regions are anchored to.
///
/// The two coincide only while the overlay is **both** `decorations: false`
/// **and** `shadow: false` in `tauri.conf.json`. Both halves matter: tao treats
/// an undecorated window *with* shadows as having hidden offsets and inflates
/// `set_inner_size` by the window/client delta (`window_state.rs`
/// `undecorated_with_shadows`, applied in `window.rs` `set_inner_size`). Turn
/// shadows on and this function's rectangle can never equal the one
/// [`apply_bounds`] writes, so [`needs_write`] answers `true` forever and every
/// correction raises the event that triggers the next one — a self-sustaining
/// `SetWindowPos` loop, not a few pixels of drift. Compare against the same
/// coordinate family the writes use before changing either flag.
fn current_bounds(window: &WebviewWindow) -> Result<Rect, String> {
    let position = window
        .inner_position()
        .map_err(|e| format!("Could not read the overlay position: {e}"))?;
    let size = window
        .inner_size()
        .map_err(|e| format!("Could not read the overlay size: {e}"))?;
    Ok(Rect {
        origin: Point::new(position.x, position.y),
        size: Size::new(size.width, size.height),
    })
}

/// The overlay's current origin, for debug instrumentation only.
///
/// `None` rather than an error when the window cannot be read: a diagnostic
/// that can fail a caller is a diagnostic that changes behaviour.
#[cfg(debug_assertions)]
pub fn current_origin(app: &AppHandle) -> Option<(i32, i32)> {
    let position = overlay_window(app).ok()?.inner_position().ok()?;
    Some((position.x, position.y))
}

fn apply_bounds(window: &WebviewWindow, bounds: Rect) -> Result<(), String> {
    window
        .set_position(PhysicalPosition::new(bounds.origin.x, bounds.origin.y))
        .map_err(|e| format!("Could not position the overlay: {e}"))?;
    window
        .set_size(PhysicalSize::new(bounds.size.width, bounds.size.height))
        .map_err(|e| format!("Could not size the overlay: {e}"))
}

/// Snapshot of the current monitors as core types — the single place Tauri's
/// monitor reports become [`Monitor`] values.
///
/// Tauri already reports physical pixels here, so this is a type mapping, not
/// a coordinate-space conversion — the only sanctioned CSS↔physical conversion
/// lives in `uptake_core::geometry`, and it uses the *window's* scale factor,
/// never these per-monitor ones (see the `Monitor` docs for what they are for).
fn monitors(window: &WebviewWindow) -> Result<Vec<Monitor>, String> {
    let monitors = window
        .available_monitors()
        .map_err(|e| format!("Could not enumerate monitors: {e}"))?;
    Ok(monitors
        .iter()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            Monitor::new(
                Rect {
                    origin: Point::new(position.x, position.y),
                    size: Size::new(size.width, size.height),
                },
                monitor.scale_factor(),
            )
        })
        .collect())
}

pub(crate) fn overlay_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    app.get_webview_window(WINDOW_LABEL)
        .ok_or_else(|| format!("Window '{WINDOW_LABEL}' does not exist — check tauri.conf.json."))
}

// ---------------------------------------------------------------------------
// The three-state interaction model (ADR-0012).
//
// `overlay_state` decides *what* the next state is (pure, tested there); this
// section performs the effect — showing or hiding the window, driving the
// click-through poll through `show`/`hide`, and emitting the state to the
// frontend so it can render the focus indicator.
// ---------------------------------------------------------------------------

/// The Tauri event the overlay frontend listens on for state changes.
const STATE_EVENT: &str = "overlay://state";

/// The focus-indicator geometry sent to the frontend.
///
/// Monitor rects are **physical virtual-desktop pixels**; the frontend converts
/// them to CSS with its own `devicePixelRatio` (ADR-0011 — the WebView is the
/// authority on its scale) and the `origin` reported here. Rust deliberately
/// does not pre-convert: doing so would reintroduce the scale-mismatch bug
/// ADR-0011 exists to prevent.
#[derive(Serialize, Clone)]
struct StatePayload {
    /// `"hidden"`, `"placement"`, or `"living"`.
    state: &'static str,
    /// The overlay's virtual-desktop origin (its inner top-left), physical px.
    origin: (i32, i32),
    /// Each monitor's bounds in physical virtual-desktop px. Empty unless the
    /// state draws per-monitor chrome (Placement).
    monitors: Vec<(i32, i32, u32, u32)>,
}

const fn state_name(state: OverlayState) -> &'static str {
    match state {
        OverlayState::Hidden => "hidden",
        OverlayState::Placement => "placement",
        OverlayState::Living => "living",
    }
}

/// Summons the overlay into Placement — the tray, a single-instance relaunch,
/// and the debug startup all enter here. Idempotent: summoning an
/// already-visible overlay re-shows and re-focuses it.
pub fn summon(app: &AppHandle) {
    drive(app, Event::Summon);
}

/// Toggles input focus between UP-TAKE and the real screen — the global hotkey.
pub fn toggle(app: &AppHandle) {
    drive(app, Event::Toggle);
}

/// Handles `Esc` from the overlay.
///
/// A drag in progress is cancelled first and the state is left unchanged
/// (ADR-0012: mid-drag `Esc` = cancel); otherwise `Esc` backs out of Placement.
/// The distinction is read from the placement hook rather than tracked here,
/// because the hook is the only thing that knows a drag is live.
pub fn escape(app: &AppHandle) {
    if placement::is_dragging() {
        placement::cancel_drag();
        drive(app, Event::Escape { mid_drag: true });
    } else {
        drive(app, Event::Escape { mid_drag: false });
    }
}

/// Applies an event to the current state and performs the resulting effect.
///
/// The state lock is held only long enough to read-and-update it, then dropped
/// before the window/IPC work in [`apply`], which does not need it — holding a
/// mutex across a Win32 call would widen the critical section for nothing.
fn drive(app: &AppHandle, event: Event) {
    let target = {
        let cell = app.state::<Mutex<OverlayState>>();
        let mut guard = lock(&cell);
        let target = next(*guard, event, has_areas(app));
        *guard = target;
        target
    };
    if let Err(error) = apply(app, target) {
        eprintln!("overlay: could not apply state {target:?}: {error}");
    }
}

/// Performs a state's effect: show or hide the window (which also (de)activates
/// the click-through poll), toggle the placement input layer, and emit the new
/// state and area set to the frontend.
///
/// `placement::enter`/`exit` own the mouse hook and the crosshair cursor, which
/// belong to Placement alone. `exit` is called on the way into Living and Hidden
/// (and is idempotent) so the hook and the global cursor override never outlive
/// the one state that uses them.
fn apply(app: &AppHandle, state: OverlayState) -> Result<(), String> {
    match state {
        OverlayState::Hidden => {
            // Emit first so the frontend clears its indicator, then hide.
            emit_state(app, state)?;
            placement::exit(app);
            hide(app)
        }
        OverlayState::Placement => {
            show(app)?;
            placement::enter(app);
            emit_state(app, state)?;
            emit_areas(app)
        }
        OverlayState::Living => {
            show(app)?;
            placement::exit(app);
            emit_state(app, state)?;
            emit_areas(app)
        }
    }
}

/// Emits the current state to the overlay frontend, with the monitor geometry
/// the focus indicator needs in Placement.
fn emit_state(app: &AppHandle, state: OverlayState) -> Result<(), String> {
    let window = overlay_window(app)?;
    // The real virtual-desktop origin travels with **every** state, not just
    // Placement. Living draws the persistent area borders and converts them to
    // CSS against this origin (ADR-0011); sending (0, 0) for Living was what made
    // the areas jump by the origin the moment Placement handed off to Living.
    let position = window
        .inner_position()
        .map_err(|e| format!("Could not read the overlay position: {e}"))?;
    let origin = (position.x, position.y);
    // The per-monitor focus frames are a Placement-only indicator; every other
    // state sends none.
    let monitors = if matches!(state, OverlayState::Placement) {
        monitors(&window)?
            .iter()
            .map(|m| {
                (
                    m.bounds.origin.x,
                    m.bounds.origin.y,
                    m.bounds.size.width,
                    m.bounds.size.height,
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    app.emit(
        STATE_EVENT,
        StatePayload {
            state: state_name(state),
            origin,
            monitors,
        },
    )
    .map_err(|e| format!("Could not emit overlay state: {e}"))
}

/// Whether any areas exist — read from the managed [`AreaStore`]. When it is
/// empty, `Living` collapses to `Hidden` (overlay_state), because a
/// click-through overlay with nothing on it is indistinguishable from hidden.
fn has_areas(app: &AppHandle) -> bool {
    let store = app.state::<Mutex<AreaStore>>();
    !lock(&store).is_empty()
}

/// The Tauri event carrying the current areas to the frontend, which draws each
/// as a persistent border. Physical rects; the frontend converts with its own
/// origin and `devicePixelRatio` (ADR-0011), exactly as it does the monitor
/// frames and the selection box.
const AREAS_EVENT: &str = "overlay://areas";

/// The area rectangles sent to the frontend.
#[derive(Serialize, Clone)]
struct AreasPayload {
    /// Each area's bounds in physical virtual-desktop px, bottom-first (paint
    /// order — later areas draw over earlier ones).
    areas: Vec<(i32, i32, u32, u32)>,
}

/// Emits the current area set. Called on entering a visible state, on the
/// frontend's mount request, and by the placement hook after it creates one.
pub(crate) fn emit_areas(app: &AppHandle) -> Result<(), String> {
    let store = app.state::<Mutex<AreaStore>>();
    let areas = lock(&store)
        .iter()
        .map(|area| {
            (
                area.bounds.origin.x,
                area.bounds.origin.y,
                area.bounds.size.width,
                area.bounds.size.height,
            )
        })
        .collect();
    app.emit(AREAS_EVENT, AreasPayload { areas })
        .map_err(|e| format!("Could not emit overlay areas: {e}"))
}

/// Creates a `Default` area at the given physical bounds, returning whether one
/// was created. `Default` is the only type task 1.6 ships (R-17); an empty
/// rectangle — a click or a drag that never moved — creates nothing, which
/// `AreaStore::create` enforces.
///
/// The placement hook calls this from the event-loop thread; it takes the store
/// lock only for the push.
pub(crate) fn create_default_area(
    app: &AppHandle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> bool {
    let bounds = Rect {
        origin: Point::new(x, y),
        size: Size::new(width, height),
    };
    let store = app.state::<Mutex<AreaStore>>();
    lock(&store).create(AreaType::Default, bounds).is_some()
}

/// Locks a mutex, treating poisoning as recoverable — the state under it is a
/// plain enum, valid after any panic, and architecture §5 forbids `unwrap`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// IPC surface: `Esc` from the overlay emits this intent.
#[tauri::command]
pub fn overlay_escape(app: AppHandle) {
    escape(&app);
}

/// IPC surface: the frontend requests the current state on mount.
///
/// A webview that loaded *after* the last transition — the debug startup show,
/// or a dev reload — would otherwise render no indicator and no areas until the
/// next change. This re-emits both the current state and the area set so the
/// overlay is correct immediately.
#[tauri::command]
pub fn overlay_request_state(app: AppHandle) -> Result<(), String> {
    let cell = app.state::<Mutex<OverlayState>>();
    let state = *lock(&cell);
    emit_state(&app, state)?;
    emit_areas(&app)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The termination property of the sync ↔ window-event cycle. Everything
    /// else in this module needs a real window; this one decision does not, and
    /// it is the one whose failure mode is an infinite loop.
    #[test]
    fn matching_bounds_are_never_rewritten() {
        let bounds = Rect::new(-1080, -1080, 5560, 2733);
        assert!(!needs_write(bounds, bounds));
    }

    #[test]
    fn a_moved_origin_is_rewritten_even_when_the_size_is_unchanged() {
        // The rearrangement case: same virtual-desktop size, new origin. No
        // resize event fires anywhere, so this comparison is the only thing
        // that notices.
        let before = Rect::new(-1080, -1080, 5560, 2733);
        let after = Rect::new(0, -1080, 5560, 2733);
        assert!(needs_write(before, after));
    }

    #[test]
    fn a_resized_desktop_is_rewritten() {
        let before = Rect::new(0, 0, 2560, 1440);
        let after = Rect::new(0, 0, 4480, 1440);
        assert!(needs_write(before, after));
    }
}
