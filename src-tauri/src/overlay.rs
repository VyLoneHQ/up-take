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

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};
use uptake_core::geometry::{Monitor, Point, Rect, Size, virtual_desktop_bounds};

use crate::click_through;

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
    // Known baseline before anything is visible: interactive everywhere, so
    // Esc works from the first frame even if the click-through poll has not
    // ticked yet. The poll refines this within one frame.
    window
        .set_ignore_cursor_events(false)
        .map_err(|e| format!("Could not reset overlay click-through: {e}"))?;
    window
        .show()
        .map_err(|e| format!("Could not show the overlay: {e}"))?;
    // Focus so keyboard input (Esc to dismiss — M-11 keyboard-only operation)
    // reaches the overlay immediately.
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
        .map_err(|e| format!("Could not hide the overlay: {e}"))
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

/// IPC surface for the frontend; the global hotkey (task 1.4) will call
/// [`show`] directly.
#[tauri::command]
pub fn overlay_show(app: AppHandle) -> Result<(), String> {
    show(&app)
}

/// IPC surface for the frontend (Esc key emits this intent).
#[tauri::command]
pub fn overlay_hide(app: AppHandle) -> Result<(), String> {
    hide(&app)
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
