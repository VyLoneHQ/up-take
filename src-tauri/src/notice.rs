//! UP-TAKE's own notification cards (`ADR-0054`).
//!
//! A watch area that notices a change says so in a card drawn by UP-TAKE, at
//! the top centre of the monitor the area is on. Not a Windows toast: at the
//! first real-screen test (2026-11-03) Windows accepted every toast and showed
//! none, and toasts are off on some PCs altogether. The cards show whatever
//! Windows' notification settings are, and follow UP-TAKE's own *Show
//! notifications* setting instead.
//!
//! # The stack
//!
//! The page (`src/routes/notice/+page.svelte`, its rules in
//! `src/lib/notice.ts`) keeps the cards as a stack, the founder's design of
//! 2026-11-03: a card for a new kind (another area, or another moment) goes on
//! top and pushes the others down; the same kind again while its card shows
//! counts up on that card and restarts its lifetime, where it is; each card
//! fades after its own six seconds, and the cards below move up into the gap.
//! [`NoticePayload::kind`] is what "the same kind" means.
//!
//! # The windows
//!
//! One window per monitor, labelled [`label_for`], made for its monitor's first
//! card and reused after: transparent, without a frame, always on top, out of
//! the taskbar, and **never focused**. It takes a click (to close a card) but
//! never the keyboard, so a card appearing while the user types does not take
//! their keystrokes. It follows the overlay's capture policy (`ADR-0019`): with
//! *Show UP-TAKE in screen recordings* off it is excluded from capture, which
//! also keeps it out of a watch area's own readings.
//!
//! The page sizes its window to its stack through [`notice_resize`]: at once
//! when the stack grows, and only after an exit animation when it shrinks, so
//! no card is clipped while it moves. A height of zero hides the window.
//!
//! # Handing cards over
//!
//! Cards wait in a queue per window until its page takes them
//! ([`notice_take`]); the event only says there is something to take. So a
//! card sent before the page has loaded is not lost, and no card is shown
//! twice. The text says which moment it was and never what an area read.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use uptake_core::geometry::{Monitor, Rect};

/// Every notification window's label starts with this;
/// `capabilities/notice.json` grants the pattern.
pub const LABEL_PREFIX: &str = "notice-";

/// The event that tells a page it has cards to take.
const NOTICE_EVENT: &str = "notice://ready";

/// The window's width in logical pixels; the page lays the stack out inside it.
const WIDTH: f64 = 392.0;

/// How far below the monitor's top edge the stack starts, in logical pixels.
const TOP_MARGIN: f64 = 12.0;

/// How many cards wait for a page at most. Far more than it shows (the page
/// keeps five); a page that never loads must not grow this without bound.
const QUEUE_LIMIT: usize = 32;

/// One card to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoticePayload {
    /// What makes two cards "the same": the area and the moment. A card of a
    /// kind already on show counts up there instead of adding a card.
    pub kind: String,
    /// The card's title line.
    pub title: String,
    /// The card's text.
    pub body: String,
}

/// Cards waiting for their window's page, by window label.
static QUEUES: Mutex<Option<HashMap<String, Vec<NoticePayload>>>> = Mutex::new(None);

/// The window label for the monitor at `bounds`: one window, one stack, per
/// monitor. From the monitor's position, which is what tells two monitors
/// apart in the virtual desktop. Letters and digits only (`n` for a negative
/// coordinate), as a window label must be.
#[must_use]
pub fn label_for(bounds: Rect) -> String {
    let part = |value: i32| {
        if value < 0 {
            format!("n{}", value.unsigned_abs())
        } else {
            value.to_string()
        }
    };
    format!(
        "{LABEL_PREFIX}{}x{}",
        part(bounds.origin.x),
        part(bounds.origin.y)
    )
}

/// The card's kind for an area and a moment.
#[must_use]
pub fn kind_of(area: u64, moment: &str) -> String {
    format!("{area}:{moment}")
}

/// Where the window goes for a stack `height_logical` tall: centred across
/// `monitor`, [`TOP_MARGIN`] below its top edge, sized for its scale. Physical
/// pixels, as `(x, y, width, height)`. The height is clamped to the monitor.
#[must_use]
pub fn placement(monitor: &Monitor, height_logical: f64) -> (i32, i32, u32, u32) {
    let scale = if monitor.scale_factor > 0.0 {
        monitor.scale_factor
    } else {
        1.0
    };
    let bounds = monitor.bounds;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let width = ((WIDTH * scale).round() as u32).min(bounds.size.width.max(1));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let height = ((height_logical.max(0.0) * scale).round() as u32).min(bounds.size.height);
    #[allow(clippy::cast_possible_wrap)]
    let x = bounds.origin.x + ((bounds.size.width - width) / 2) as i32;
    #[allow(clippy::cast_possible_truncation)]
    let y = bounds.origin.y + (TOP_MARGIN * scale).round() as i32;
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

/// Adds `card` to `label`'s queue, dropping the oldest past [`QUEUE_LIMIT`].
fn enqueue(label: &str, card: NoticePayload) {
    let mut queues = QUEUES.lock().unwrap_or_else(PoisonError::into_inner);
    let queue = queues
        .get_or_insert_with(HashMap::new)
        .entry(label.to_string())
        .or_default();
    queue.push(card);
    if queue.len() > QUEUE_LIMIT {
        let excess = queue.len() - QUEUE_LIMIT;
        queue.drain(..excess);
    }
}

/// Shows a card for the area `area` at `bounds`, unless the user switched
/// UP-TAKE's notifications off. Never blocks the caller on the page.
pub fn show(app: &AppHandle, area: u64, bounds: Rect, moment: &str, title: &str, body: &str) {
    if !crate::settings::current().notifications {
        return;
    }
    let Some(monitor) = monitor_for(bounds, &crate::overlay::monitors_cached()) else {
        return;
    };
    let label = label_for(monitor.bounds);
    enqueue(
        &label,
        NoticePayload {
            kind: kind_of(area, moment),
            title: title.to_string(),
            body: body.to_string(),
        },
    );
    // Window work belongs on the main thread; the watcher calls from its own.
    let app_for_main = app.clone();
    let scheduled = app.run_on_main_thread(move || {
        if let Err(error) = wake_page(&app_for_main, &label, &monitor) {
            crate::diagnostics::trouble("notice: the card could not be shown", &error);
        }
    });
    if let Err(error) = scheduled {
        crate::diagnostics::trouble("notice: the card could not be scheduled", &error);
    }
}

/// Makes `label`'s window if it is not there yet (hidden: its page shows it
/// once it has sized itself), and tells its page there are cards to take.
fn wake_page(app: &AppHandle, label: &str, monitor: &Monitor) -> Result<(), String> {
    let window = match app.get_webview_window(label) {
        Some(window) => window,
        None => {
            let window = WebviewWindowBuilder::new(app, label, WebviewUrl::App("notice/".into()))
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
                .map_err(|error| format!("could not make the notice window: {error}"))?;
            place(&window, monitor, 0.0)?;
            window
        }
    };
    window
        .emit(NOTICE_EVENT, ())
        .map_err(|error| format!("could not wake the notice page: {error}"))
}

/// Sizes and places `window` for a stack `height` logical pixels tall, shows
/// it without focus, or hides it at zero.
fn place(window: &tauri::WebviewWindow, monitor: &Monitor, height: f64) -> Result<(), String> {
    let (x, y, width, physical_height) = placement(monitor, height);
    if physical_height == 0 {
        return window
            .hide()
            .map_err(|error| format!("could not hide the notice window: {error}"));
    }
    window
        .set_size(tauri::PhysicalSize::new(width, physical_height))
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
    Ok(())
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

/// The monitor a notice window belongs to, from its label.
fn monitor_of(label: &str) -> Option<Monitor> {
    crate::overlay::monitors_cached()
        .into_iter()
        .find(|monitor| label_for(monitor.bounds) == label)
}

/// IPC surface: the cards waiting for the calling window's page, oldest
/// first. Taking them empties the queue.
#[tauri::command]
#[must_use]
pub fn notice_take(window: tauri::WebviewWindow) -> Vec<NoticePayload> {
    QUEUES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        .and_then(|queues| queues.remove(window.label()))
        .unwrap_or_default()
}

/// IPC surface: the page's stack is `height` logical pixels tall now; size the
/// window to it, or hide it at zero.
#[tauri::command]
pub fn notice_resize(window: tauri::WebviewWindow, height: f64) -> Result<(), String> {
    if !window.label().starts_with(LABEL_PREFIX) {
        return Err("not a notice window".to_string());
    }
    let Some(monitor) = monitor_of(window.label()) else {
        // Its monitor is gone: nothing to show it on.
        return window
            .hide()
            .map_err(|error| format!("could not hide the notice window: {error}"));
    };
    place(&window, &monitor, height)
}

#[cfg(test)]
mod tests {
    use super::{
        LABEL_PREFIX, NoticePayload, QUEUE_LIMIT, kind_of, label_for, monitor_for, placement,
    };
    use crate::payload_keys::{assert_keys, assert_payload_coverage};
    use uptake_core::geometry::{Monitor, Rect};

    fn monitor(x: i32, y: i32, width: u32, height: u32, scale: f64) -> Monitor {
        Monitor::new(Rect::new(x, y, width, height), scale)
    }

    /// The cards cross IPC to their page, so their keys are pinned (`I-67`).
    #[test]
    fn a_card_reaches_its_page_under_the_keys_it_reads() {
        assert_keys(
            "NoticePayload",
            &NoticePayload {
                kind: kind_of(7, "changed"),
                title: "UP-TAKE".to_string(),
                body: "A watch area changed.".to_string(),
            },
            &["kind", "title", "body"],
        );
        assert_payload_coverage(
            "notice.rs",
            include_str!("notice.rs"),
            &["NoticePayload"],
            &[],
        );
    }

    /// Its own windows, never the overlay's or the settings window: the
    /// overlay's window hooks key on the label.
    #[test]
    fn the_windows_are_not_the_overlay() {
        let label = label_for(Rect::new(0, 0, 10, 10));
        assert!(label.starts_with(LABEL_PREFIX));
        assert_ne!(label, crate::overlay::WINDOW_LABEL);
        assert_ne!(label, crate::settings_window::WINDOW_LABEL);
    }

    /// One window per monitor, told apart by position, in characters a window
    /// label accepts.
    #[test]
    fn each_monitor_gets_its_own_window() {
        assert_eq!(label_for(Rect::new(0, 0, 1920, 1080)), "notice-0x0");
        assert_eq!(
            label_for(Rect::new(-2560, -200, 2560, 1440)),
            "notice-n2560xn200"
        );
        assert_ne!(
            label_for(Rect::new(1920, 0, 1920, 1080)),
            label_for(Rect::new(0, 0, 1920, 1080))
        );
        assert!(
            label_for(Rect::new(-1, 3, 1, 1))
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        );
    }

    /// The same area and moment is the same kind; anything else is not.
    #[test]
    fn a_kind_is_the_area_and_the_moment() {
        assert_eq!(kind_of(3, "changed"), kind_of(3, "changed"));
        assert_ne!(kind_of(3, "changed"), kind_of(4, "changed"));
        assert_ne!(kind_of(3, "changed"), kind_of(3, "settled"));
    }

    /// `ADR-0054` decision 2: top centre of the monitor, sized for its scale.
    #[test]
    fn the_stack_sits_at_the_top_centre_of_its_monitor() {
        assert_eq!(
            placement(&monitor(0, 0, 1920, 1080, 1.0), 100.0),
            (764, 12, 392, 100)
        );
        // A monitor left of the primary, at 150 %.
        assert_eq!(
            placement(&monitor(-2560, 0, 2560, 1440, 1.5), 100.0),
            (-2560 + (2560 - 588) / 2, 18, 588, 150)
        );
        // Narrower than the stack, and a stack taller than the monitor.
        assert_eq!(
            placement(&monitor(100, 0, 300, 400, 1.0), 900.0),
            (100, 12, 300, 400)
        );
        // Zero hides.
        assert_eq!(placement(&monitor(0, 0, 1920, 1080, 1.0), 0.0).3, 0);
    }

    /// The cards go on the monitor that holds most of the area.
    #[test]
    fn a_card_follows_its_area_to_its_monitor() {
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
        assert_eq!(
            monitor_for(Rect::new(-30, 0, 100, 10), &monitors),
            Some(right)
        );
        assert_eq!(monitor_for(Rect::new(0, 0, 10, 10), &[]), None);
    }

    /// A page that never takes its cards cannot grow the queue without bound,
    /// and taking empties it in order.
    #[test]
    fn the_queue_is_bounded_and_keeps_the_newest() {
        let label = "notice-test-queue";
        for index in 0..QUEUE_LIMIT + 5 {
            super::enqueue(
                label,
                NoticePayload {
                    kind: index.to_string(),
                    title: String::new(),
                    body: String::new(),
                },
            );
        }
        let taken = super::QUEUES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .and_then(|queues| queues.remove(label))
            .unwrap_or_default();
        assert_eq!(taken.len(), QUEUE_LIMIT);
        assert_eq!(taken.first().map(|card| card.kind.as_str()), Some("5"));
        assert_eq!(
            taken.last().map(|card| card.kind.clone()),
            Some((QUEUE_LIMIT + 4).to_string())
        );
    }
}
