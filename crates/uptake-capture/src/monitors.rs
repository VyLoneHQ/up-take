//! Monitor enumeration: the live `HMONITOR` handles and virtual-desktop
//! bounds the capture plan is built against.
//!
//! Deliberately not shared with the Tauri app's own monitor enumeration
//! (`overlay::monitors`): that one reports through tao and carries scale
//! factors for DPI decisions; capture needs the raw `HMONITOR` to hand to WGC
//! and nothing else. The two describe the same hardware through the same OS
//! tables.
//!
//! **Coordinates are physical pixels only in a per-monitor-DPI-aware
//! process.** The UP-TAKE app is one (tao opts in). A standalone binary using
//! this crate must opt in itself — see `examples/grab.rs` — or Windows serves
//! it DPI-virtualized coordinates and every rectangle is subtly wrong.

use uptake_core::geometry::Rect;
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};

use crate::error::CaptureError;
use crate::plan::MonitorInfo;

/// Enumerates the monitors of the virtual desktop, with their `HMONITOR`
/// handles (as `isize`) and bounds in physical virtual-desktop pixels.
///
/// An empty result is returned as such — the planner turns it into
/// [`CaptureError::NoMonitors`] — so this function fails only when
/// `EnumDisplayMonitors` itself does.
pub(crate) fn enumerate() -> Result<Vec<MonitorInfo>, CaptureError> {
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    // SAFETY: the callback only runs during this call, so the pointer to
    // `monitors` it receives outlives every use of it.
    let ok = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(push_monitor),
            std::ptr::from_mut(&mut monitors) as LPARAM,
        )
    };
    if ok == 0 {
        return Err(CaptureError::Enumeration);
    }
    Ok(monitors)
}

/// `EnumDisplayMonitors` callback: appends one monitor to the `Vec` behind
/// `lparam`.
unsafe extern "system" fn push_monitor(
    handle: HMONITOR,
    _hdc: HDC,
    rect: *mut RECT,
    lparam: LPARAM,
) -> i32 {
    // SAFETY: `lparam` is the pointer `enumerate` passed one call up, and
    // `rect` is a valid monitor rectangle for the duration of the callback.
    let monitors = unsafe { &mut *(lparam as *mut Vec<MonitorInfo>) };
    let rect = unsafe { &*rect };
    // Spans are computed in i64 and clamp to zero if the OS ever reports an
    // inverted rectangle; a zero-sized monitor simply never intersects a
    // region, which is the correct way for a nonsense rect to not exist.
    let width = u32::try_from(i64::from(rect.right) - i64::from(rect.left)).unwrap_or(0);
    let height = u32::try_from(i64::from(rect.bottom) - i64::from(rect.top)).unwrap_or(0);
    monitors.push(MonitorInfo {
        handle: handle as isize,
        bounds: Rect::new(rect.left, rect.top, width, height),
    });
    1
}
