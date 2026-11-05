//! The area settings panel (roadmap `1.60`, the founder's design of
//! 2026-11-03).
//!
//! Each area's own settings, in a small panel beside it: its name, its type,
//! the type's own settings (what a watch area tells the user about, how an
//! OCR area shows what it read), Pause, its depth, sticking to a window and
//! what it does on a resize, click-through, and Copy and Save image for a
//! screenshot: every row of the area menu but *Dismiss* (the founder's
//! approval of 2026-11-05). Later what a watch area looks for, its tolerance
//! and its reactions (`1.61` to `1.63`).
//!
//! # How it opens and closes
//!
//! - **Right-click on an area, in either mode,** or the menu's *Settings…*.
//!   **Ctrl+right-click** opens the area menu of before instead
//!   (`placement::RightClick`, his choice of 2026-11-05), which keeps
//!   *Dismiss* and *Settings…*.
//! - **Not on hover** (his rig, 2026-11-05: *"hovering over an area
//!   shouldn't open the settings menu anymore, only a right-click should"*;
//!   until then resting on an area while placing opened it).
//! - **It closes** on `Esc`, its ×, a press anywhere outside it in either
//!   mode, a right-click on no area, or its area going (his rig, 2026-11-05:
//!   *"clicking outside the settings menu should close it"*; until then only
//!   a press on the overlay while placing did).
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

use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use uptake_core::area::{AreaId, AreaStore, AreaType, Input, Layer, OcrBehaviour};
use uptake_core::geometry::{Monitor, Point, Rect};
use uptake_core::sticky::OnResize;
use uptake_core::watch::Watch;

/// The panel window's label. `capabilities/panel.json` grants it.
pub const LABEL: &str = "area-panel";

/// The event that tells the page its area changed and to read it again.
const PANEL_EVENT: &str = "panel://changed";

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
    /// Where it was placed last, physical pixels.
    rect: Rect,
}

static OPEN: Mutex<Option<Open>> = Mutex::new(None);

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
    /// The types it can be turned into, by wire name, in the menu's order
    /// (`placement::conversion_label` decides which, as for the menu).
    pub kinds: Vec<&'static str>,
    /// How an OCR area shows what it read; `null` for another type.
    pub ocr: Option<OcrBehaviour>,
    /// Its depth tier: [`depth_name`].
    pub depth: &'static str,
    /// Whether it follows the window under it (roadmap `1.47`).
    pub sticky: bool,
    /// Whether there is a window under it to stick to; the switch is greyed
    /// out when there is not and the area is free, as the menu row is.
    pub can_stick: bool,
    /// What a sticky area does when its window is resized; `null` when free.
    pub on_resize: Option<OnResize>,
    /// Whether clicks pass through it to what is underneath.
    pub click_through: bool,
    /// Whether it holds a capture to copy or save (a screenshot area).
    pub capture: bool,
}

/// A depth tier's wire name, as the page names it.
#[must_use]
pub const fn depth_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Front => "front",
        Layer::Auto => "auto",
        Layer::Back => "back",
    }
}

/// The tier a wire name names, if any.
#[must_use]
pub fn depth_named(name: &str) -> Option<Layer> {
    [Layer::Front, Layer::Auto, Layer::Back]
        .into_iter()
        .find(|layer| depth_name(*layer) == name)
}

/// The type a wire name names, among those an area can be turned into.
#[must_use]
pub fn kind_named(name: &str) -> Option<AreaType> {
    convertible().find(|kind| crate::overlay::type_name(*kind) == name)
}

/// The types an area can be turned into, in the menu's order.
fn convertible() -> impl Iterator<Item = AreaType> {
    AreaType::ALL
        .into_iter()
        .filter(|kind| crate::placement::conversion_label(*kind).is_some())
}

/// What the page shows for `id`; `can_stick` and `on_resize` are asked of
/// the app by the caller, which holds no store lock while it does.
fn view(store: &AreaStore, id: AreaId, can_stick: bool, on_resize: OnResize) -> Option<PanelView> {
    let area = store.get(id)?;
    let sticky = area.sticky.is_sticky();
    Some(PanelView {
        id: id.get(),
        kind: crate::overlay::type_name(area.kind),
        name: store.name(id).map(str::to_string),
        watch: (area.kind == AreaType::Watch).then_some(area.watch),
        held: area.kind.keeps_working().then_some(area.held),
        kinds: convertible().map(crate::overlay::type_name).collect(),
        ocr: (area.kind == AreaType::Ocr).then_some(area.ocr_behaviour),
        depth: depth_name(area.layer),
        sticky,
        can_stick: sticky || can_stick,
        on_resize: sticky.then_some(on_resize),
        click_through: area.input == Input::PassThrough,
        capture: area.kind == AreaType::Screenshot,
    })
}

/// Opens the panel for `area`, with the pointer at `pointer`. Replaces any
/// panel already open.
pub fn open(app: &AppHandle, area: AreaId, pointer: Point) {
    let height = open_now().map_or(FIRST_HEIGHT, |open| open.height);
    *OPEN.lock().unwrap_or_else(PoisonError::into_inner) = Some(Open {
        area,
        pointer,
        height,
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
            // Shown by a raw `ShowWindow`, so hidden by one (`I-478`).
            if let Err(error) = crate::notice::hide_shown(&window) {
                crate::diagnostics::trouble("panel: could not close", &error);
            }
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
    // Listed before it is drawn there, so a watch reading leaves it out of its
    // comparison (`I-480`); `notice::hide_shown` takes it off the list.
    crate::notice::own_window_at(LABEL, rect);
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

/// A press outside an open panel closes it, the way a press outside the menu
/// closes the menu. Returns whether it closed one; while placing, the press
/// then does nothing else (`placement::classify_press`), and elsewhere it
/// goes on to whatever it landed on (`placement`'s hook).
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
    // Asked before the store is locked: both take locks of their own.
    let can_stick =
        crate::overlay::area_bounds(&app, open.area).is_some_and(crate::sticky::can_stick);
    let on_resize = crate::sticky::on_resize_of(open.area);
    let store = app.state::<Mutex<AreaStore>>();
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    view(&store, open.area, can_stick, on_resize)
}

/// Applies `act` to the open area and, when it changed something, tells
/// every page the areas changed, as a menu row does.
fn set_open(app: &AppHandle, act: impl FnOnce(AreaId) -> bool) -> Result<(), String> {
    if with_open_area(act) == Some(true) {
        changed(app)?;
    }
    Ok(())
}

/// IPC surface: turns the open area into the type `kind` names (the menu's
/// *Area type* rows, roadmap `1.27`).
#[tauri::command]
pub fn panel_set_type(app: AppHandle, kind: String) -> Result<(), String> {
    let kind = kind_named(&kind).ok_or_else(|| format!("not a type an area becomes: {kind}"))?;
    set_open(&app, |id| crate::overlay::convert_area(&app, id, kind))
}

/// IPC surface: how the open OCR area shows what it read (`1.45`).
#[tauri::command]
pub fn panel_set_ocr(app: AppHandle, behaviour: OcrBehaviour) -> Result<(), String> {
    set_open(&app, |id| {
        crate::overlay::set_area_ocr_behaviour(&app, id, behaviour)
    })
}

/// IPC surface: the open area's depth tier (`ADR-0013`).
#[tauri::command]
pub fn panel_set_depth(app: AppHandle, depth: String) -> Result<(), String> {
    let layer = depth_named(&depth).ok_or_else(|| format!("not a depth: {depth}"))?;
    set_open(&app, |id| crate::overlay::set_area_layer(&app, id, layer))
}

/// IPC surface: makes the open area follow the window under it, or frees it
/// (`1.47`).
#[tauri::command]
pub fn panel_set_sticky(app: AppHandle, sticky: bool) -> Result<(), String> {
    set_open(&app, |id| crate::sticky::set(&app, id, sticky))
}

/// IPC surface: what the open sticky area does when its window is resized.
#[tauri::command]
pub fn panel_set_on_resize(app: AppHandle, on_resize: OnResize) -> Result<(), String> {
    set_open(&app, |id| crate::sticky::set_on_resize(id, on_resize))
}

/// IPC surface: whether clicks pass through the open area (V-7).
#[tauri::command]
pub fn panel_set_click_through(app: AppHandle, on: bool) -> Result<(), String> {
    let input = if on {
        Input::PassThrough
    } else {
        Input::Interactive
    };
    set_open(&app, |id| crate::overlay::set_area_input(&app, id, input))
}

/// IPC surface: copies the open screenshot area's capture, or saves it to
/// `Pictures\UP-TAKE\` (`save`), on a thread of its own as the menu does.
#[tauri::command]
pub fn panel_export(app: AppHandle, save: bool) {
    let Some(id) = with_open_area(|id| id) else {
        return;
    };
    let Some(bounds) = crate::overlay::area_bounds(&app, id) else {
        return;
    };
    std::thread::spawn(move || {
        if save {
            crate::output::save_to_file(&app, id, bounds);
        } else {
            crate::output::copy_to_clipboard(&app, id, bounds);
        }
    });
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
                kinds: vec!["default", "watch"],
                ocr: None,
                depth: "auto",
                sticky: true,
                can_stick: true,
                on_resize: Some(uptake_core::sticky::OnResize::Corner),
                click_through: false,
                capture: false,
            },
            &[
                "id",
                "kind",
                "name",
                "watch",
                "held",
                "kinds",
                "ocr",
                "depth",
                "sticky",
                "can_stick",
                "on_resize",
                "click_through",
                "capture",
            ],
        );
        assert_payload_coverage("panel.rs", include_str!("panel.rs"), &["PanelView"], &[]);
    }

    /// The page sends depths and types by wire name; each name the panel
    /// shows comes back to what it names, and nothing else is accepted.
    #[test]
    fn depths_and_types_cross_by_their_wire_names() {
        use super::{depth_name, depth_named, kind_named};
        use uptake_core::area::{AreaType, Layer};
        for layer in [Layer::Front, Layer::Auto, Layer::Back] {
            assert_eq!(depth_named(depth_name(layer)), Some(layer));
        }
        assert_eq!(depth_named("top"), None);
        for kind in AreaType::ALL {
            let offered = crate::placement::conversion_label(kind).is_some();
            assert_eq!(
                kind_named(crate::overlay::type_name(kind)) == Some(kind),
                offered,
                "{kind:?}: the panel offers what the menu offers"
            );
        }
        assert_eq!(kind_named("nonsense"), None);
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
            1,
            "the release's check"
        );
        // The press's check: the panel first, then the z-order.
        let at = |needle: &str| hook.find(needle).unwrap_or(usize::MAX);
        let panel = at("        if crate::panel::takes_press(point) {
            return false;");
        assert!(panel < usize::MAX, "the press asks the panel");
        assert!(panel < at("        let shadowed = shadowed_by_another_window(point);"));
    }

    /// The founder's rig, 2026-11-03 (`I-478`): the ×, `Esc` and a press
    /// elsewhere all left the panel open. The window is shown by a raw
    /// `ShowWindow`, which tao does not see, so tao's own hide did nothing.
    /// Every hide goes through `notice::hide_shown`, which hides with
    /// `ShowWindow` (its own test holds that); held by source because no test
    /// here can run a tao window.
    #[test]
    fn the_panel_is_hidden_the_way_it_was_shown() {
        let source = include_str!("panel.rs");
        let code = source.split("#[cfg(test)]").next().unwrap_or_default();
        assert_eq!(code.matches(".hide()").count(), 0, "no tao hide");
        assert_eq!(
            code.matches("crate::notice::hide_shown(&window)").count(),
            1
        );
    }

    /// `I-480`: with *Show UP-TAKE in screen recordings* on, the panel over a
    /// watch area is in its reading; it is listed where it is shown, before
    /// it is drawn there, so the reading leaves it out. Held by source, as no
    /// test here can run its window.
    #[test]
    fn the_panel_is_listed_where_it_is_shown() {
        let source = include_str!("panel.rs");
        let code = source.split("#[cfg(test)]").next().unwrap_or_default();
        let at = |needle: &str| code.find(needle).unwrap_or(usize::MAX);
        let listed = at("crate::notice::own_window_at(LABEL, rect);");
        assert!(listed < usize::MAX, "listed");
        assert!(listed < at(".set_size(tauri::PhysicalSize::new(rect.size.width"));
    }

    #[test]
    fn the_window_is_none_of_the_others() {
        assert_ne!(super::LABEL, crate::overlay::WINDOW_LABEL);
        assert_ne!(super::LABEL, crate::settings_window::WINDOW_LABEL);
        assert!(!super::LABEL.starts_with(crate::notice::LABEL_PREFIX));
    }
}
