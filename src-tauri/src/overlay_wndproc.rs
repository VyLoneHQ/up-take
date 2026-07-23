//! The overlay window's own wndproc: the window messages tao does not handle
//! and the overlay cannot do without.
//!
//! Subclassed via comctl32 subclassing — the supported composition mechanism;
//! tao subclasses the same window and the chain composes. Two messages, for two
//! unrelated reasons.
//!
//! # `WM_DISPLAYCHANGE` — display-configuration changes (task 1.3, M-6)
//!
//! tao surfaces no event when a monitor is added, removed, rearranged or
//! changes resolution: it handles `WM_DPICHANGED` (and then only by rescaling
//! the window — see `overlay::sync_bounds` for why that is wrong for the
//! overlay) and ignores `WM_DISPLAYCHANGE` entirely. Without it a *visible*
//! overlay keeps stale bounds until the next hide/show cycle, and a monitor
//! rearrangement that moves the virtual-desktop origin without changing its size
//! leaves every click-through region anchored to the old origin with no event
//! firing anywhere. Observed, not consumed: it is forwarded down the chain.
//!
//! # `WM_SYSCOMMAND`/`SC_KEYMENU` — refusing to enter menu mode
//!
//! Pressing and releasing `Alt` on its own makes Windows send the focused window
//! `SC_KEYMENU`, and `DefWindowProc` answers it by entering a **modal menu
//! loop** on that thread. The overlay is `decorations: false` with no menu bar
//! and no system menu, so there is nothing for that loop to show — but the main
//! thread is now inside a nested loop, and the main thread is what services the
//! placement mouse hook. A `WH_MOUSE_LL` callback that cannot run promptly is
//! *silently removed* by Windows, so a bare `Alt` press left the overlay in
//! Placement with dead input until the user toggled the state twice.
//!
//! `Alt` is not an incidental key here either: holding it is how a drag opts out
//! of edge snapping (`interaction::SNAP_DISTANCE`), so releasing it is the
//! ordinary end of a precise placement. This message is therefore **consumed**,
//! not forwarded — the documented way for a window with no menu to decline menu
//! mode.

use std::panic::{AssertUnwindSafe, catch_unwind};

use tauri::AppHandle;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SC_KEYMENU, WM_DISPLAYCHANGE, WM_NCDESTROY, WM_SYSCOMMAND,
};

use crate::overlay;

/// Distinguishes this registration from any other subclass on the window.
const SUBCLASS_ID: usize = 0x5550_544b; // "UPTK"

/// Installs the overlay's wndproc subclass.
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
/// [`install`]. `WM_DISPLAYCHANGE` is observed and forwarded; `SC_KEYMENU` is
/// the one message consumed, and the module docs say why.
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    data: usize,
) -> LRESULT {
    // Consumed rather than forwarded: returning 0 without reaching
    // `DefWindowProc` is how a window with no menu declines menu mode. The low
    // four bits of `wparam` are reserved by the system, hence the mask.
    if message == WM_SYSCOMMAND && (wparam & 0xFFF0) == SC_KEYMENU as usize {
        return 0;
    }
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
