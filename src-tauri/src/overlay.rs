//! Overlay window lifecycle: sizing it over the whole virtual desktop,
//! showing and hiding it.
//!
//! The window itself is declared in `tauri.conf.json` and created hidden at
//! startup — showing it is then a reposition + `show()`, cheap enough for the
//! < 100 ms hotkey-to-visible budget (quality-bars.md §1). Creating the window
//! on demand would not be.
//!
//! Geometry decisions live in `uptake_core::geometry`; this module only maps
//! Tauri's monitor reports into core types and talks to the OS.

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};
use uptake_core::geometry::{Point, Rect, Size, virtual_desktop_bounds};

/// Label of the overlay window as declared in `tauri.conf.json`.
pub const WINDOW_LABEL: &str = "overlay";

/// Resizes the overlay to cover the entire virtual desktop and shows it.
///
/// Bounds are recomputed on every call rather than cached: monitors can be
/// plugged, unplugged or rearranged while the app sits in the tray, and stale
/// bounds are exactly the M-6 failure (quality-bars.md §3).
pub fn show(app: &AppHandle) -> Result<(), String> {
    let window = overlay_window(app)?;
    let monitors = window
        .available_monitors()
        .map_err(|e| format!("Could not enumerate monitors: {e}"))?;
    let bounds = virtual_desktop_bounds(monitors.iter().map(monitor_bounds))
        .ok_or("No display detected — the overlay needs at least one monitor.")?;

    window
        .set_position(PhysicalPosition::new(bounds.origin.x, bounds.origin.y))
        .map_err(|e| format!("Could not position the overlay: {e}"))?;
    window
        .set_size(PhysicalSize::new(bounds.size.width, bounds.size.height))
        .map_err(|e| format!("Could not size the overlay: {e}"))?;
    window
        .show()
        .map_err(|e| format!("Could not show the overlay: {e}"))?;
    // Focus so keyboard input (Esc to dismiss — M-11 keyboard-only operation)
    // reaches the overlay immediately.
    window
        .set_focus()
        .map_err(|e| format!("Could not focus the overlay: {e}"))
}

/// Hides the overlay. The window stays alive so the next `show` is instant.
pub fn hide(app: &AppHandle) -> Result<(), String> {
    overlay_window(app)?
        .hide()
        .map_err(|e| format!("Could not hide the overlay: {e}"))
}

/// Maps a Tauri monitor into core virtual-desktop geometry.
///
/// Tauri already reports physical pixels here, so this is a type mapping, not
/// a coordinate-space conversion — the only sanctioned CSS↔physical conversion
/// lives in `uptake_core::geometry`.
fn monitor_bounds(monitor: &tauri::Monitor) -> Rect {
    let position = monitor.position();
    let size = monitor.size();
    Rect {
        origin: Point::new(position.x, position.y),
        size: Size::new(size.width, size.height),
    }
}

fn overlay_window(app: &AppHandle) -> Result<WebviewWindow, String> {
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
