//! The area settings panel (roadmap `1.60`, the founder's design of
//! 2026-11-03).
//!
//! Each area's own settings, in a small panel beside it: today its name, and
//! for a watch area what it tells the user about and whether it is paused;
//! later what a watch area looks for, its tolerance and its reactions
//! (`1.61` to `1.63`).
//!
//! # How it opens and closes
//!
//! - **Hover, while placing only:** resting the pointer on an area for
//!   [`HOVER_OPEN`] opens it. It closes once the pointer has been off both the
//!   area and the panel for [`HOVER_CLOSE`]. Not in Living: a watch area sits
//!   over something the user works in, and a panel appearing whenever the
//!   pointer crosses it would cover that work (agreed with the founder).
//! - **Pinned:** opened from the area menu's *Settings…*, or by a click into a
//!   hover panel. A pinned panel stays until `Esc`, a click elsewhere while
//!   placing, the area going, or placing ending.
//!
//! # Where it goes
//!
//! [`dock`]: outside the area on the side with the most room; an area with no
//! room around it (one filling a monitor) gets the panel inside, in the corner
//! nearest the pointer. So it always has a place.
//!
//! # The window
//!
//! Its own window, not part of the overlay: the placement hook hands a press to
//! any window above the overlay at that point (`placement::window_above`), so
//! the panel's controls are ordinary controls, a text field included. Shown
//! without taking the keyboard; a click into it does take it (to type a name),
//! and closing gives it back to the overlay while placing, or placing would go
//! deaf to `Esc` (the bug found with the cards, `notice::give_focus_back`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use uptake_core::area::{AreaId, AreaStore, AreaType};
use uptake_core::geometry::{Monitor, Point, Rect};
use uptake_core::watch::Watch;

/// The panel window's label. `capabilities/panel.json` grants it.
pub const LABEL: &str = "area-panel";

/// The event that tells the page its area changed and to read it again.
const PANEL_EVENT: &str = "panel://changed";

/// How long the pointer rests on an area before its panel opens.
pub const HOVER_OPEN: Duration = Duration::from_millis(400);

/// How long the pointer may be off both the area and its panel before an
/// unpinned panel closes.
pub const HOVER_CLOSE: Duration = Duration::from_millis(300);

/// The window's width in logical pixels, the page's 12px margin on each side
/// included (room for the shadow); its height is the page's.
const WIDTH: f64 = 300.0;

/// The gap between an area and a panel docked beside it, in logical pixels.
const GAP: f64 = 8.0;

/// The height used until the page has reported its own, in logical pixels.
const FIRST_HEIGHT: f64 = 160.0;

/// What the panel is showing.
#[derive(Debug, Clone, Copy)]
struct Open {
    area: AreaId,
    /// Where the pointer was when it opened, for [`dock`]'s inside case.
    pointer: Point,
    /// The page's height, logical pixels.
    height: f64,
    /// Stays until closed on purpose, not when the pointer leaves.
    pinned: bool,
    /// Where it was placed last, physical pixels.
    rect: Rect,
}

static OPEN: Mutex<Option<Open>> = Mutex::new(None);

/// Counts every hover change, so a delayed open or close fires only if
/// nothing moved on since it was scheduled.
static HOVER_TICKET: AtomicU64 = AtomicU64::new(0);

fn open_now() -> Option<Open> {
    *OPEN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where a panel `size` (physical) goes for an area at `area` on `monitor`:
/// beside the area on the side with the most room that fits it, else inside
/// the area in the corner nearest `pointer`. Always within the monitor.
#[must_use]
pub fn dock(area: Rect, monitor: Rect, size: (u32, u32), gap: i32, pointer: Point) -> Rect {
    let (width, height) = (to_i32(size.0), to_i32(size.1));
    let (m_left, m_top) = (monitor.origin.x, monitor.origin.y);
    let (m_right, m_bottom) = (
        m_left + to_i32(monitor.size.width),
        m_top + to_i32(monitor.size.height),
    );
    let (a_left, a_top) = (area.origin.x, area.origin.y);
    let (a_right, a_bottom) = (
        a_left + to_i32(area.size.width),
        a_top + to_i32(area.size.height),
    );
    let clamp_x = |x: i32| x.clamp(m_left, (m_right - width).max(m_left));
    let clamp_y = |y: i32| y.clamp(m_top, (m_bottom - height).max(m_top));
    // (room on that side, does it fit, where the panel goes)
    let sides = [
        (
            m_right - a_right - gap,
            width,
            (a_right + gap, clamp_y(a_top)),
        ),
        (
            a_left - m_left - gap,
            width,
            (a_left - gap - width, clamp_y(a_top)),
        ),
        (
            m_bottom - a_bottom - gap,
            height,
            (clamp_x(a_left), a_bottom + gap),
        ),
        (
            a_top - m_top - gap,
            height,
            (clamp_x(a_left), a_top - gap - height),
        ),
    ];
    let best = sides
        .iter()
        .filter(|(room, need, _)| room >= need)
        .max_by_key(|(room, need, _)| room - need);
    let (x, y) = match best {
        Some((_, _, at)) => *at,
        None => {
            // Inside, in the corner nearest the pointer.
            let mid_x = a_left + (a_right - a_left) / 2;
            let mid_y = a_top + (a_bottom - a_top) / 2;
            let x = if pointer.x < mid_x {
                a_left + gap
            } else {
                a_right - gap - width
            };
            let y = if pointer.y < mid_y {
                a_top + gap
            } else {
                a_bottom - gap - height
            };
            (clamp_x(x), clamp_y(y))
        }
    };
    Rect::new(x, y, size.0, size.1)
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// The panel's physical size on `monitor` for a page `height` logical pixels.
fn physical_size(monitor: &Monitor, height: f64) -> (u32, u32) {
    let scale = if monitor.scale_factor > 0.0 {
        monitor.scale_factor
    } else {
        1.0
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let size = (
        (WIDTH * scale).round() as u32,
        (height.max(1.0) * scale).round() as u32,
    );
    size
}

/// What the page shows for the open area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PanelView {
    /// The area's id.
    pub id: u64,
    /// Its type's wire name (`overlay::type_name`).
    pub kind: &'static str,
    /// Its name, if any.
    pub name: Option<String>,
    /// What a watch area tells the user about; `null` for another type.
    pub watch: Option<Watch>,
    /// Whether it is paused; `null` for a type with nothing to pause.
    pub held: Option<bool>,
}

fn view(store: &AreaStore, id: AreaId) -> Option<PanelView> {
    let area = store.get(id)?;
    Some(PanelView {
        id: id.get(),
        kind: crate::overlay::type_name(area.kind),
        name: store.name(id).map(str::to_string),
        watch: (area.kind == AreaType::Watch).then_some(area.watch),
        held: area.kind.keeps_working().then_some(area.held),
    })
}

fn area_by_raw(app: &AppHandle, raw: u64) -> Option<(AreaId, Rect)> {
    let store = app.state::<Mutex<AreaStore>>();
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    store
        .iter()
        .find(|area| area.id.get() == raw)
        .map(|area| (area.id, area.bounds))
}

/// Opens the panel for `area`, `pinned` or not, with the pointer at
/// `pointer`. Replaces any panel already open.
pub fn open(app: &AppHandle, area: AreaId, pinned: bool, pointer: Point) {
    let height = open_now().map_or(FIRST_HEIGHT, |open| open.height);
    *OPEN.lock().unwrap_or_else(PoisonError::into_inner) = Some(Open {
        area,
        pointer,
        height,
        pinned,
        rect: Rect::new(0, 0, 0, 0),
    });
    let app_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(error) = present(&app_main) {
            crate::diagnostics::trouble("panel: could not open", &error);
        }
    });
}

/// Closes the panel, and gives the keyboard back to the overlay while
/// placing. Does nothing when none is open.
pub fn close(app: &AppHandle) {
    if OPEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .is_none()
    {
        return;
    }
    let app_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = app_main.get_webview_window(LABEL) {
            let had_keyboard = window.is_focused().unwrap_or(false);
            hide_window(&window);
            if had_keyboard
                && crate::placement::is_placing()
                && let Some(overlay) = app_main.get_webview_window(crate::overlay::WINDOW_LABEL)
            {
                let _ = overlay.set_focus();
            }
        }
    });
}

/// Places and shows the window for the open panel, without taking the
/// keyboard, and tells the page to read its area.
fn present(app: &AppHandle) -> Result<(), String> {
    let Some(open) = open_now() else {
        return Ok(());
    };
    let Some(bounds) = app
        .state::<Mutex<AreaStore>>()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(open.area)
        .map(|area| area.bounds)
    else {
        close(app);
        return Ok(());
    };
    let Some(monitor) = crate::notice::monitor_for(bounds, &crate::overlay::monitors_cached())
    else {
        return Ok(());
    };
    let size = physical_size(&monitor, open.height);
    #[allow(clippy::cast_possible_truncation)]
    let gap = (GAP * monitor.scale_factor.max(1.0)).round() as i32;
    let rect = dock(bounds, monitor.bounds, size, gap, open.pointer);
    if let Some(current) = OPEN.lock().unwrap_or_else(PoisonError::into_inner).as_mut()
        && current.area == open.area
    {
        current.rect = rect;
    }
    let window = match app.get_webview_window(LABEL) {
        Some(window) => window,
        None => WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("panel/".into()))
            .title("UP-TAKE")
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false)
            .build()
            .map_err(|error| format!("could not make the panel window: {error}"))?,
    };
    window
        .set_size(tauri::PhysicalSize::new(rect.size.width, rect.size.height))
        .map_err(|error| format!("could not size the panel: {error}"))?;
    window
        .set_position(tauri::PhysicalPosition::new(rect.origin.x, rect.origin.y))
        .map_err(|error| format!("could not place the panel: {error}"))?;
    #[cfg(windows)]
    {
        let hwnd = window
            .hwnd()
            .map_err(|error| format!("could not get the panel window handle: {error}"))?
            .0;
        let show_in_recordings = crate::settings::current().show_in_screen_recordings;
        if let Err(error) = crate::overlay::write_affinity_to(hwnd, show_in_recordings) {
            crate::diagnostics::trouble("panel: capture policy not applied", &error);
        }
        show_on_top(hwnd);
    }
    #[cfg(not(windows))]
    window
        .show()
        .map_err(|error| format!("could not show the panel: {error}"))?;
    window
        .emit(PANEL_EVENT, ())
        .map_err(|error| format!("could not tell the panel page: {error}"))
}

/// Shows the window above the overlay without activating it. Unlike a card
/// it may be activated by a click, so its name field can take typing.
#[cfg(windows)]
fn show_on_top(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST,
    };
    // SAFETY: `hwnd` is this process's live panel window; these calls only
    // read and set its styles and z-order.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        #[allow(clippy::cast_possible_wrap)]
        let wanted = style | (WS_EX_TOOLWINDOW | WS_EX_TOPMOST) as isize;
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

/// Hides the window the way [`show_on_top`] showed it: with `ShowWindow`
/// itself. tao's own hide does nothing here, because tao hides a window only
/// when its own record says it is visible, and a window shown by a raw
/// `ShowWindow` is still hidden in that record (`I-478`, measured by a drill
/// against tao 0.37.1: the window stayed visible after `set_visible(false)`).
fn hide_window(window: &tauri::WebviewWindow) {
    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
        // SAFETY: `hwnd` is this process's live panel window; this only hides
        // it.
        unsafe { ShowWindow(hwnd.0, SW_HIDE) };
        return;
    }
    let _ = window.hide();
}

/// The areas changed: re-place the panel and have its page read its area
/// again, or close it if its area is gone. Called after every areas emit.
pub fn refresh(app: &AppHandle) {
    let Some(open) = open_now() else {
        return;
    };
    let gone = app
        .state::<Mutex<AreaStore>>()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(open.area)
        .is_none();
    if gone {
        close(app);
        return;
    }
    let app_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(error) = present(&app_main) {
            crate::diagnostics::trouble("panel: could not follow its area", &error);
        }
    });
}

/// The overlay changed state: a panel lives only while placing... unless it
/// is pinned, which the area menu also offers in Living.
pub fn on_state(app: &AppHandle, placing: bool) {
    if !placing && open_now().is_some_and(|open| !open.pinned) {
        close(app);
    }
}

/// The pointer's hover changed while placing: `hovered` is the area under it,
/// if any. Opens a panel after a rest, and watches an unpinned one for the
/// pointer leaving both the area and the panel.
pub fn on_hover(app: &AppHandle, hovered: Option<u64>, pointer: Point) {
    let ticket = HOVER_TICKET.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(raw) = hovered else {
        return;
    };
    // The pointer is on the panel, over whatever area lies under it: that is
    // using the panel, not asking for another one.
    if open_now().is_some_and(|open| open.rect.contains(pointer)) {
        return;
    }
    if open_now().is_some_and(|open| open.area.get() == raw) {
        return;
    }
    if open_now().is_some_and(|open| open.pinned) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(HOVER_OPEN);
        if HOVER_TICKET.load(Ordering::SeqCst) != ticket || !crate::placement::is_placing() {
            return;
        }
        if crate::placement::area_in_gesture().is_some() {
            return;
        }
        if open_now().is_some_and(|open| cursor().is_some_and(|point| open.rect.contains(point))) {
            return;
        }
        let Some((id, _)) = area_by_raw(&app, raw) else {
            return;
        };
        open(&app, id, false, pointer);
        watch_leaving(app, id);
    });
}

/// Closes an unpinned panel once the pointer has been off both its area and
/// the panel for [`HOVER_CLOSE`]. Ends when the panel closes, is pinned, or
/// shows another area.
fn watch_leaving(app: AppHandle, area: AreaId) {
    std::thread::spawn(move || {
        let step = Duration::from_millis(100);
        let mut away = Duration::ZERO;
        loop {
            std::thread::sleep(step);
            let Some(open) = open_now() else { return };
            if open.area != area || open.pinned {
                return;
            }
            let bounds = area_by_raw(&app, area.get()).map(|(_, bounds)| bounds);
            let inside = cursor().is_some_and(|point| {
                open.rect.contains(point) || bounds.is_some_and(|bounds| bounds.contains(point))
            });
            away = if inside { Duration::ZERO } else { away + step };
            if away >= HOVER_CLOSE {
                close(&app);
                return;
            }
        }
    });
}

/// The pointer, physical virtual-desktop pixels.
fn cursor() -> Option<Point> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut point = POINT { x: 0, y: 0 };
        // SAFETY: writes the cursor position into a local.
        let ok = unsafe { GetCursorPos(&mut point) };
        (ok != 0).then(|| Point::new(point.x, point.y))
    }
    #[cfg(not(windows))]
    None
}

/// Whether a press at `point` is the panel's: inside the open panel. Asked by
/// the placement hook before anything else, by position rather than z-order
/// (see the hook), so the panel's controls work wherever it sits in the stack.
#[must_use]
pub fn takes_press(point: Point) -> bool {
    press_is_the_panels(open_now().map(|open| open.rect), point)
}

/// [`takes_press`]'s rule, apart so it is tested.
#[must_use]
pub fn press_is_the_panels(panel: Option<Rect>, point: Point) -> bool {
    panel.is_some_and(|rect| rect.contains(point))
}

/// Puts the open panel back above the overlay, which rises above it whenever
/// it takes focus. Without the keyboard, so it takes nothing from placing.
pub fn raise(app: &AppHandle) {
    if open_now().is_none() {
        return;
    }
    #[cfg(windows)]
    if let Some(window) = app.get_webview_window(LABEL)
        && let Ok(hwnd) = window.hwnd()
    {
        show_on_top(hwnd.0);
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// A press while placing: one outside an open pinned panel closes it, the way
/// a press outside the menu closes the menu. Returns whether it closed one,
/// in which case the press does nothing else.
pub fn close_on_press(app: &AppHandle, point: Point) -> bool {
    match open_now() {
        Some(open) if !open.rect.contains(point) => {
            close(app);
            true
        }
        _ => false,
    }
}

fn with_open_area<T>(act: impl FnOnce(AreaId) -> T) -> Option<T> {
    open_now().map(|open| act(open.area))
}

fn changed(app: &AppHandle) -> Result<(), String> {
    crate::overlay::emit_areas(app)
}

/// IPC surface: what the panel shows, or `null` with none open.
#[tauri::command]
#[must_use]
pub fn panel_view(app: AppHandle) -> Option<PanelView> {
    let open = open_now()?;
    let store = app.state::<Mutex<AreaStore>>();
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    view(&store, open.area)
}

/// IPC surface: the page's height, logical pixels; re-places the panel.
#[tauri::command]
pub fn panel_resize(app: AppHandle, height: f64) {
    let resized = {
        let mut guard = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.as_mut() {
            Some(open) if (open.height - height).abs() >= 0.5 => {
                open.height = height;
                true
            }
            _ => false,
        }
    };
    if resized {
        let app_main = app.clone();
        let _ = app.run_on_main_thread(move || {
            let _ = present(&app_main);
        });
    }
}

/// IPC surface: the user clicked into the panel; it stays until closed.
#[tauri::command]
pub fn panel_pin() {
    if let Some(open) = OPEN.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        open.pinned = true;
    }
}

/// IPC surface: `Esc` in the panel, or its close control.
#[tauri::command]
pub fn panel_close(app: AppHandle) {
    close(&app);
}

/// IPC surface: names the open area, or clears its name when `name` is blank.
#[tauri::command]
pub fn panel_set_name(app: AppHandle, name: String) -> Result<(), String> {
    let renamed = with_open_area(|id| {
        let store = app.state::<Mutex<AreaStore>>();
        store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_name(id, &name)
    });
    if renamed == Some(true) {
        changed(&app)?;
    }
    Ok(())
}

/// IPC surface: what the open watch area tells the user about.
#[tauri::command]
pub fn panel_set_watch(app: AppHandle, watch: Watch) -> Result<(), String> {
    if with_open_area(|id| crate::overlay::set_area_watch(&app, id, watch)) == Some(true) {
        changed(&app)?;
    }
    Ok(())
}

/// IPC surface: pauses or resumes the open area (`ADR-0053`).
#[tauri::command]
pub fn panel_set_held(app: AppHandle, held: bool) -> Result<(), String> {
    if with_open_area(|id| crate::overlay::set_area_held(&app, id, held)) == Some(true) {
        changed(&app)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PanelView, dock};
    use crate::payload_keys::{assert_keys, assert_payload_coverage};
    use uptake_core::geometry::{Point, Rect};
    use uptake_core::watch::Watch;

    const MONITOR: Rect = Rect::new(0, 0, 1920, 1080);
    const SIZE: (u32, u32) = (280, 200);

    /// Beside the area, on the side with the most room that fits.
    #[test]
    fn the_panel_docks_on_the_roomiest_side() {
        let pointer = Point::new(0, 0);
        // Room on the right: to the right, top aligned.
        assert_eq!(
            dock(Rect::new(100, 100, 300, 200), MONITOR, SIZE, 8, pointer),
            Rect::new(408, 100, 280, 200)
        );
        // Against the right edge: to the left.
        assert_eq!(
            dock(Rect::new(1500, 100, 400, 200), MONITOR, SIZE, 8, pointer),
            Rect::new(1212, 100, 280, 200)
        );
        // Wide, near the top: below, its left edge clamped onto the monitor.
        assert_eq!(
            dock(Rect::new(0, 50, 1920, 300), MONITOR, SIZE, 8, pointer),
            Rect::new(0, 358, 280, 200)
        );
        // Beside it, but held on the monitor at the bottom.
        assert_eq!(
            dock(Rect::new(100, 1000, 300, 60), MONITOR, SIZE, 8, pointer),
            Rect::new(408, 880, 280, 200)
        );
    }

    /// No room around it: inside, in the corner nearest the pointer, so the
    /// panel always has a place (agreed with the founder 2026-11-03).
    #[test]
    fn an_area_filling_the_monitor_gets_the_panel_inside_by_the_pointer() {
        let full = Rect::new(0, 0, 1920, 1080);
        assert_eq!(
            dock(full, MONITOR, SIZE, 8, Point::new(10, 10)),
            Rect::new(8, 8, 280, 200)
        );
        assert_eq!(
            dock(full, MONITOR, SIZE, 8, Point::new(1900, 1000)),
            Rect::new(1920 - 8 - 280, 1080 - 8 - 200, 280, 200)
        );
        // A monitor not at the origin.
        let left = Rect::new(-1920, 0, 1920, 1080);
        assert_eq!(
            dock(left, left, SIZE, 8, Point::new(-10, 10)),
            Rect::new(-8 - 280, 8, 280, 200)
        );
    }

    /// The panel's view crosses IPC, so its keys are pinned (`I-67`).
    #[test]
    fn the_panel_reaches_its_page_under_the_keys_it_reads() {
        assert_keys(
            "PanelView",
            &PanelView {
                id: 1,
                kind: "watch",
                name: Some("Download watcher".to_string()),
                watch: Some(Watch::Changes),
                held: Some(false),
            },
            &["id", "kind", "name", "watch", "held"],
        );
        assert_payload_coverage("panel.rs", include_str!("panel.rs"), &["PanelView"], &[]);
    }

    /// The founder's rig, 2026-11-03: presses on the panel were swallowed. A
    /// press inside the open panel is the panel's; any other is not.
    #[test]
    fn a_press_inside_the_open_panel_is_the_panels() {
        use super::press_is_the_panels;
        let panel = Some(Rect::new(400, 100, 280, 200));
        assert!(press_is_the_panels(panel, Point::new(410, 110)));
        assert!(press_is_the_panels(panel, Point::new(679, 299)));
        assert!(!press_is_the_panels(panel, Point::new(399, 110)));
        assert!(!press_is_the_panels(panel, Point::new(680, 110)));
        assert!(!press_is_the_panels(None, Point::new(410, 110)));
    }

    /// Both of the hook's z-order checks ask the panel first; the source is
    /// held because the hook runs on a real Windows hook no test can drive.
    #[test]
    fn the_placement_hook_asks_the_panel_before_the_z_order() {
        let hook = include_str!("placement.rs");
        assert_eq!(
            hook.matches("crate::panel::takes_press(point) || shadowed_by_another_window(point)")
                .count(),
            2
        );
    }

    /// The founder's rig, 2026-11-03 (`I-478`): the ×, `Esc` and a press
    /// elsewhere all left the panel open. The window is shown by a raw
    /// `ShowWindow`, which tao does not see, so tao's own hide did nothing.
    /// Every hide goes through `hide_window`, whose only tao hide is the
    /// fallback for a window with no handle; held by source because no test
    /// here can run a tao window.
    #[test]
    fn the_panel_is_hidden_the_way_it_was_shown() {
        let source = include_str!("panel.rs");
        let code = source.split("#[cfg(test)]").next().unwrap_or_default();
        assert_eq!(
            code.matches(".hide()").count(),
            1,
            "only hide_window's fallback"
        );
        let at = |needle: &str| code.find(needle).unwrap_or(usize::MAX);
        let (helper, raw, fallback) = (
            at("fn hide_window("),
            at("ShowWindow(hwnd.0, SW_HIDE)"),
            at("let _ = window.hide();"),
        );
        assert!(fallback < usize::MAX, "the fallback is there");
        assert!(helper < raw && raw < fallback, "the raw hide comes first");
        assert!(code.contains("hide_window(&window);"));
    }

    #[test]
    fn the_window_is_none_of_the_others() {
        assert_ne!(super::LABEL, crate::overlay::WINDOW_LABEL);
        assert_ne!(super::LABEL, crate::settings_window::WINDOW_LABEL);
        assert!(!super::LABEL.starts_with(crate::notice::LABEL_PREFIX));
    }
}
