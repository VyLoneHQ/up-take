//! Rust-side subscription to Windows display-configuration changes
//! (roadmap task 1.3, quality-bars.md §3 M-6).
//!
//! tao surfaces no event when a monitor is added, removed, rearranged or
//! changes resolution: it handles `WM_DPICHANGED` (and then only by rescaling
//! the window — see `overlay::sync_bounds` for why that is wrong for the
//! overlay) and ignores `WM_DISPLAYCHANGE` entirely. Without this module a
//! *visible* overlay keeps stale bounds until the next hide/show cycle, and a
//! monitor rearrangement that moves the virtual-desktop origin without
//! changing its size leaves every click-through region anchored to the old
//! origin with no event firing anywhere.
//!
//! The overlay window's wndproc is therefore subclassed via comctl32
//! subclassing — the supported composition mechanism; tao subclasses the same
//! window and the chain composes — and `WM_DISPLAYCHANGE`, which Windows
//! broadcasts to every top-level window after any display-configuration
//! change, triggers a bounds re-sync.

use std::panic::{AssertUnwindSafe, catch_unwind};

use tauri::AppHandle;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{WM_DISPLAYCHANGE, WM_NCDESTROY};

use crate::overlay;

/// Distinguishes this registration from any other subclass on the window.
const SUBCLASS_ID: usize = 0x5550_544b; // "UPTK"

/// Installs the display-change hook on the overlay window.
///
/// Must run on the thread that owns the window — `SetWindowSubclass` is
/// thread-affine. Tauri's `setup` runs there, and `lib.rs` calls this from
/// `setup`.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let window = overlay::overlay_window(app)?;
    let hwnd: HWND = window
        .hwnd()
        .map_err(|e| format!("Could not get the overlay window handle: {e}"))?
        .0;
    // Freed in the subclass proc on WM_NCDESTROY — the last message a window
    // ever receives, so nothing can observe the handle after the free.
    let app = Box::into_raw(Box::new(app.clone()));
    let installed =
        unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, app as usize) };
    if installed == 0 {
        drop(unsafe { Box::from_raw(app) });
        return Err("Could not subclass the overlay window for display-change events.".into());
    }
    Ok(())
}

/// Runs in the window's message loop; `data` is the `Box<AppHandle>` leaked by
/// [`install`]. Everything is forwarded down the chain via `DefSubclassProc` —
/// this proc only *observes* `WM_DISPLAYCHANGE`, it does not consume it.
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_DISPLAYCHANGE => {
            let app = unsafe { &*(data as *const AppHandle) };
            // A panic must not cross the FFI boundary: since Rust 1.81 an
            // unwind out of an `extern "system"` fn aborts the process, and a
            // dead tray app is a lost session (architecture §5).
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                if let Err(error) = overlay::sync_bounds(app) {
                    eprintln!("display-watch: could not re-sync the overlay: {error}");
                }
            }));
            if outcome.is_err() {
                eprintln!("display-watch: panic while handling a display change");
            }
        }
        WM_NCDESTROY => unsafe {
            RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
            drop(Box::from_raw(data as *mut AppHandle));
        },
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
