//! UP-TAKE's own notification card (`ADR-0054`).
//!
//! A watch area that notices a change says so in a small card drawn by
//! UP-TAKE, at the top centre of the monitor the area is on. Not a Windows
//! toast: at the first real-screen test (2026-11-03) Windows accepted every
//! toast and showed none, and toasts are off on some PCs altogether. The card
//! shows whatever Windows' notification settings are, and follows UP-TAKE's
//! own *Show notifications* setting instead.
//!
//! # The window
//!
//! One window, labelled [`LABEL`], made the first time a card is shown and
//! reused after: transparent, without a frame, always on top, out of the
//! taskbar, and **never focused**. It takes a click (to close the card) but
//! never the keyboard, so a card appearing while the user types does not take
//! their keystrokes. It follows the overlay's capture policy (`ADR-0019`): with
//! *Show UP-TAKE in screen recordings* off it is excluded from capture, which
//! also keeps it out of a watch area's own readings.
//!
//! # The card
//!
//! The page (`src/routes/notice/+page.svelte`) draws it, fades it out after
//! six seconds unless the pointer is over it, and closes it on a click; either
//! way it calls [`notice_dismiss`], which hides the window. The text says which
//! moment it was and never what an area read.

use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use uptake_core::geometry::{Monitor, Rect};

/// The notification window's label. `capabilities/notice.json` grants it.
pub const LABEL: &str = "notice";

/// The event that hands the page a card to show.
const NOTICE_EVENT: &str = "notice://show";

/// The card's size in logical pixels. The page lays the card out inside it.
const SIZE: (f64, f64) = (380.0, 84.0);

/// How far below the monitor's top edge the card sits, in logical pixels.
const TOP_MARGIN: f64 = 16.0;

/// One card: what it says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoticePayload {
    /// The card's title line.
    pub title: String,
    /// The card's text.
    pub body: String,
    /// Counts up with every card, so the page restarts its fade for a new card
    /// with the same words.
    pub serial: u64,
}

/// The card on show, for a page that loaded after the event was sent.
static CURRENT: Mutex<Option<NoticePayload>> = Mutex::new(None);

/// Where the card goes: centred across `monitor`, [`TOP_MARGIN`] below its
/// top edge, sized for its scale. Physical pixels, as `(x, y, width, height)`.
#[must_use]
pub fn placement(monitor: &Monitor) -> (i32, i32, u32, u32) {
    let scale = if monitor.scale_factor > 0.0 {
        monitor.scale_factor
    } else {
        1.0
    };
    // Rounded and clamped so a monitor narrower than the card still gets a
    // card that starts on it.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let width = ((SIZE.0 * scale).round() as u32).min(monitor.bounds.size.width.max(1));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let height = (SIZE.1 * scale).round() as u32;
    #[allow(clippy::cast_possible_wrap)]
    let x = monitor.bounds.origin.x + ((monitor.bounds.size.width - width) / 2) as i32;
    #[allow(clippy::cast_possible_truncation)]
    let y = monitor.bounds.origin.y + (TOP_MARGIN * scale).round() as i32;
    (x, y, width, height)
}

/// The monitor a card for `bounds` goes on: the one holding most of the
/// area, else the first. `None` with no monitor at all.
#[must_use]
pub fn monitor_for(bounds: Rect, monitors: &[Monitor]) -> Option<Monitor> {
    monitors
        .iter()
        .max_by_key(|monitor| {
            monitor.bounds.intersection(bounds).map_or(0, |shared| {
                u64::from(shared.size.width) * u64::from(shared.size.height)
            })
        })
        .copied()
}

/// Shows a card for an area at `bounds`, unless the user switched UP-TAKE's
/// notifications off. Never blocks the caller on the page.
pub fn show(app: &AppHandle, bounds: Rect, title: &str, body: &str) {
    if !crate::settings::current().notifications {
        return;
    }
    let Some(monitor) = monitor_for(bounds, &crate::overlay::monitors_cached()) else {
        return;
    };
    let payload = {
        let mut current = CURRENT.lock().unwrap_or_else(PoisonError::into_inner);
        let serial = current.as_ref().map_or(1, |card| card.serial + 1);
        let card = NoticePayload {
            title: title.to_string(),
            body: body.to_string(),
            serial,
        };
        *current = Some(card.clone());
        card
    };
    // Window work belongs on the main thread; the watcher calls from its own.
    let app_for_main = app.clone();
    let scheduled = app.run_on_main_thread(move || {
        if let Err(error) = present(&app_for_main, &monitor, &payload) {
            crate::diagnostics::trouble("notice: the card could not be shown", &error);
        }
    });
    if let Err(error) = scheduled {
        crate::diagnostics::trouble("notice: the card could not be scheduled", &error);
    }
}

/// Makes or reuses the window, places it, shows it without taking focus, and
/// hands the page its card.
fn present(app: &AppHandle, monitor: &Monitor, payload: &NoticePayload) -> Result<(), String> {
    let window = match app.get_webview_window(LABEL) {
        Some(window) => window,
        None => WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("notice/".into()))
            .title("UP-TAKE")
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .focusable(false)
            .visible(false)
            .build()
            .map_err(|error| format!("could not make the notice window: {error}"))?,
    };
    let (x, y, width, height) = placement(monitor);
    window
        .set_size(tauri::PhysicalSize::new(width, height))
        .map_err(|error| format!("could not size the notice window: {error}"))?;
    window
        .set_position(tauri::PhysicalPosition::new(x, y))
        .map_err(|error| format!("could not place the notice window: {error}"))?;
    #[cfg(windows)]
    {
        let hwnd = window
            .hwnd()
            .map_err(|error| format!("could not get the notice window handle: {error}"))?
            .0;
        let show_in_recordings = crate::settings::current().show_in_screen_recordings;
        if let Err(error) = crate::overlay::write_affinity_to(hwnd, show_in_recordings) {
            crate::diagnostics::trouble("notice: capture policy not applied", &error);
        }
        show_without_focus(hwnd);
    }
    #[cfg(not(windows))]
    window
        .show()
        .map_err(|error| format!("could not show the notice window: {error}"))?;
    window
        .emit(NOTICE_EVENT, payload.clone())
        .map_err(|error| format!("could not hand the card to its page: {error}"))
}

/// Shows the window on top without activating it, so the program the user is
/// typing into keeps the keyboard. `WS_EX_NOACTIVATE` also keeps a click on
/// the card from activating it.
#[cfg(windows)]
fn show_without_focus(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };
    // SAFETY: `hwnd` is this process's live notice window, valid for the
    // duration of these calls, which only read and set its styles and z-order.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        #[allow(clippy::cast_possible_wrap)]
        let wanted = style | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST) as isize;
        if wanted != style {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
        }
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// IPC surface: the card on show, for a page that loaded after it was sent.
#[tauri::command]
#[must_use]
pub fn notice_current() -> Option<NoticePayload> {
    CURRENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// IPC surface: the page's card faded out or was clicked; hide the window.
/// A newer card than `serial` keeps it open.
#[tauri::command]
pub fn notice_dismiss(app: AppHandle, serial: u64) -> Result<(), String> {
    {
        let mut current = CURRENT.lock().unwrap_or_else(PoisonError::into_inner);
        if current.as_ref().is_some_and(|card| card.serial != serial) {
            return Ok(());
        }
        *current = None;
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        window
            .hide()
            .map_err(|error| format!("could not hide the notice window: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{NoticePayload, monitor_for, placement};
    use crate::payload_keys::{assert_keys, assert_payload_coverage};

    /// The card crosses IPC to its page, so its keys are pinned (`I-67`).
    #[test]
    fn the_card_reaches_its_page_under_the_keys_it_reads() {
        assert_keys(
            "NoticePayload",
            &NoticePayload {
                title: "UP-TAKE".to_string(),
                body: "A watch area changed.".to_string(),
                serial: 1,
            },
            &["title", "body", "serial"],
        );
        assert_payload_coverage(
            "notice.rs",
            include_str!("notice.rs"),
            &["NoticePayload"],
            &[],
        );
    }

    /// Its own window, never the overlay's: the overlay's window hooks key on
    /// the label (see `settings_window`'s same test).
    #[test]
    fn the_window_is_not_the_overlay() {
        assert_ne!(super::LABEL, crate::overlay::WINDOW_LABEL);
        assert_ne!(super::LABEL, crate::settings_window::WINDOW_LABEL);
    }
    use uptake_core::geometry::{Monitor, Rect};

    fn monitor(x: i32, y: i32, width: u32, height: u32, scale: f64) -> Monitor {
        Monitor::new(Rect::new(x, y, width, height), scale)
    }

    /// `ADR-0054` decision 2: top centre of the monitor, sized for its scale.
    #[test]
    fn the_card_sits_at_the_top_centre_of_its_monitor() {
        assert_eq!(
            placement(&monitor(0, 0, 1920, 1080, 1.0)),
            (770, 16, 380, 84)
        );
        // A monitor left of the primary, at 150 %.
        assert_eq!(
            placement(&monitor(-2560, 0, 2560, 1440, 1.5)),
            (-2560 + (2560 - 570) / 2, 24, 570, 126)
        );
        // Narrower than the card: the card is clamped to it and starts on it.
        assert_eq!(
            placement(&monitor(100, 0, 300, 400, 1.0)),
            (100, 16, 300, 84)
        );
    }

    /// The card goes on the monitor that holds most of the area.
    #[test]
    fn the_card_follows_the_area_to_its_monitor() {
        let left = monitor(-1920, 0, 1920, 1080, 1.0);
        let right = monitor(0, 0, 1920, 1080, 1.25);
        let monitors = [left, right];
        assert_eq!(
            monitor_for(Rect::new(100, 100, 50, 50), &monitors),
            Some(right)
        );
        assert_eq!(
            monitor_for(Rect::new(-500, 100, 50, 50), &monitors),
            Some(left)
        );
        // Straddling: the side holding more of it wins.
        assert_eq!(
            monitor_for(Rect::new(-30, 0, 100, 10), &monitors),
            Some(right)
        );
        assert_eq!(monitor_for(Rect::new(0, 0, 10, 10), &[]), None);
    }
}
