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
use uptake_core::area::{AreaId, AreaStore, AreaType, Input, Layer};
use uptake_core::geometry::{Monitor, Point, Rect, Size, virtual_desktop_bounds};
use uptake_core::interaction;

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
/// [`sync_bounds`]'s job, driven by `overlay_wndproc` and the window-event hook
/// in `lib.rs`.
pub fn show(app: &AppHandle) -> Result<(), String> {
    let window = overlay_window(app)?;
    apply_bounds(&window, desired_bounds(&window)?)?;
    // The same enumeration `desired_bounds` just did, kept for the placement
    // poll — see `MONITOR_CACHE`. Refreshed here because `show` is the path a
    // display change taken while the overlay was hidden arrives by.
    refresh_monitor_cache(&window);
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
    click_through::activate(app);
    Ok(())
}

/// Hides the overlay. The window stays alive so the next `show` is instant.
pub fn hide(app: &AppHandle) -> Result<(), String> {
    // Stop the poll first: quality-bars.md §1 requires zero poll activity
    // while the overlay is hidden. The poll thread re-asserts click-through —
    // the window's only state (ADR-0016) — as it parks.
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
/// monitor cache. This is M-6 while the overlay is up: a monitor hot-plugged,
/// unplugged, rearranged, or changing resolution or DPI.
///
/// Idempotent and self-converging: bounds are only written when they differ
/// from what the window already has, so the `Moved`/`Resized` events its own
/// writes raise come back here, find nothing left to fix, and stop. That
/// convergence is also what heals tao's `WM_DPICHANGED` handling — tao
/// rescales the window's physical size to preserve its *logical* size, which
/// is right for a normal window and wrong for one that must cover the virtual
/// desktop physically.
///
/// The re-fit is also what keeps the frontend's own conversions honest: a
/// scale change ends in the overlay being written back to a display-derived
/// rect, whose `resize` reaches the WebView and re-renders everything at the
/// fresh `devicePixelRatio` (ADR-0011 — the physical rects Rust emits are
/// converted frontend-side).
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
    // A display change is exactly when the cached monitor list goes stale, and
    // this is the function every display change routes through while visible.
    // An area snapped to a monitor that no longer exists would be contained
    // against a rectangle that is no longer there.
    refresh_monitor_cache(&window);
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
/// `Esc` backs out of exactly one thing, innermost first: an open area menu, then
/// a drag in progress (ADR-0012: mid-drag `Esc` = cancel, state unchanged), then
/// Placement itself. Anything else would make `Esc` skip past a transient thing
/// the user can see on screen — dismissing the menu *and* leaving Placement on
/// one keypress is the shape users read as "it did too much".
///
/// Both inner cases are read from the placement module rather than tracked here,
/// because the hook is the only thing that knows a gesture is live.
pub fn escape(app: &AppHandle) {
    if placement::close_menu(app) {
        return;
    }
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
/// the poll), set the placement layer's mode, and emit the new state and area
/// set to the frontend.
///
/// The `placement` module owns the mouse hook, which is installed for **every
/// visible state** (ADR-0016): in Placement it owns whole gestures, in Living
/// it routes per-area input. The crosshair cursor belongs to Placement alone —
/// `enter_living` drops it. `exit` (→ Hidden) tears the hook down as soon as
/// the transition allows — the one exception being a button still physically
/// held from an abandoned drag, which the hook briefly outlives on purpose
/// rather than leak that button's eventual release to the app underneath (see
/// the `placement` module docs).
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
            placement::enter_living(app);
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

/// One area as the frontend draws it.
#[derive(Serialize, Clone)]
struct AreaPayload {
    /// The store's id, so the frontend keys on identity rather than on
    /// geometry — two areas may legitimately share bounds.
    id: u64,
    /// Bounds in physical virtual-desktop px.
    rect: (i32, i32, u32, u32),
    /// The close control's rectangle, physical px. Computed here rather than in
    /// the frontend because the **hook hit-tests this exact rectangle**
    /// (`uptake_core::interaction`); a control drawn from a second, independent
    /// layout calculation would eventually be drawn somewhere it cannot be
    /// clicked, which is the F-13 failure in miniature.
    close: (i32, i32, u32, u32),
    /// `"front"`, `"auto"` or `"back"` — the area's stacking tier (ADR-0013),
    /// so a pinned area can be marked as such on screen.
    layer: &'static str,
}

/// The area set sent to the frontend.
#[derive(Serialize, Clone)]
struct AreasPayload {
    /// Every area, bottom-first (paint order — later areas draw over earlier
    /// ones), in the tier-aware order the store already maintains.
    areas: Vec<AreaPayload>,
}

const fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Front => "front",
        Layer::Auto => "auto",
        Layer::Back => "back",
    }
}

/// A rect as the `(x, y, width, height)` tuple the frontend receives.
pub(crate) const fn as_tuple(rect: Rect) -> (i32, i32, u32, u32) {
    (
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    )
}

/// Emits the current area set. Called on entering a visible state, on the
/// frontend's mount request, and by the placement hook after every change.
pub(crate) fn emit_areas(app: &AppHandle) -> Result<(), String> {
    // Fetched once, before the store lock: the close control's position depends
    // on the monitors, because on a small area it sits *outside* the area and
    // has to pick a corner that is actually on a screen.
    let monitors = monitor_rects();
    let store = app.state::<Mutex<AreaStore>>();
    let areas = lock(&store)
        .iter()
        .map(|area| AreaPayload {
            id: area.id.get(),
            rect: as_tuple(area.bounds),
            close: as_tuple(interaction::close_control(area.bounds, &monitors)),
            layer: layer_name(area.layer),
        })
        .collect();
    app.emit(AREAS_EVENT, AreasPayload { areas })
        .map_err(|e| format!("Could not emit overlay areas: {e}"))
}

/// What a hit-test resolves to: the identity and the two menu-relevant
/// properties of an area, detached from the store so no lock outlives the call.
#[derive(Clone, Copy)]
pub(crate) struct AreaSummary {
    pub id: AreaId,
    pub layer: Layer,
    pub input: Input,
}

impl AreaSummary {
    fn of(area: &uptake_core::area::Area) -> Self {
        Self {
            id: area.id,
            layer: area.layer,
            input: area.input,
        }
    }
}

/// The topmost area containing `point`, whatever its input mode — the area a
/// placement gesture or a Placement menu acts on. `None` when the point is
/// over empty overlay.
///
/// [`AreaStore::hit_test_any`], not `hit_test`: a pass-through area is invisible
/// to a click in `Living` and must still be grabbable while editing the layout,
/// or it can never be moved or removed.
pub(crate) fn area_at(app: &AppHandle, point: Point) -> Option<AreaSummary> {
    let store = app.state::<Mutex<AreaStore>>();
    let guard = lock(&store);
    guard.hit_test_any(point).map(AreaSummary::of)
}

/// The topmost **interactive** area containing `point` — the area that claims
/// a `Living` mouse event (ADR-0016, V-7). `None` means the event belongs to
/// the user's apps: the point is over empty overlay, or over areas that are
/// all pass-through.
///
/// [`AreaStore::hit_test`], not `hit_test_any` — the difference *is* the input
/// model: a pass-through area never takes a click in `Living`, however high it
/// is stacked, including a Filter pinned to `Front` (the property the store's
/// tests pin).
pub(crate) fn interactive_area_at(app: &AppHandle, point: Point) -> Option<AreaSummary> {
    let store = app.state::<Mutex<AreaStore>>();
    let guard = lock(&store);
    guard.hit_test(point).map(AreaSummary::of)
}

/// Raises an area to the top of its tier — §3.2a's "the area you last touched
/// is on top", applied to a `Living` click (ADR-0016). Returns whether the id
/// resolved.
pub(crate) fn raise_area(app: &AppHandle, id: AreaId) -> bool {
    let store = app.state::<Mutex<AreaStore>>();
    lock(&store).bring_to_front(id)
}

/// Sets whether an area takes input or lets it fall through (V-7).
pub(crate) fn set_area_input(app: &AppHandle, id: AreaId, input: Input) -> bool {
    let store = app.state::<Mutex<AreaStore>>();
    lock(&store).set_input(id, input)
}

/// The topmost area whose *interaction surface* contains `point`, and which
/// part of it was grabbed.
///
/// Distinct from [`area_at`] because that surface is no longer the area's own
/// rectangle: a small area's close control sits outside its bounds, so a point
/// that grabs a control need not be a point inside anything. Asking
/// `interaction::handle_at` per area, top-down, is what keeps "what is drawn"
/// and "what responds" the same set of rectangles.
pub(crate) fn area_handle_at(
    app: &AppHandle,
    point: Point,
) -> Option<(AreaId, Rect, interaction::Handle)> {
    let monitors = monitor_rects();
    let store = app.state::<Mutex<AreaStore>>();
    let guard = lock(&store);
    guard.iter_top_down().find_map(|area| {
        interaction::handle_at(area.bounds, point, &monitors)
            .map(|handle| (area.id, area.bounds, handle))
    })
}

/// The close control's rectangle for an area, against the current monitors.
pub(crate) fn close_control_of(bounds: Rect) -> Rect {
    interaction::close_control(bounds, &monitor_rects())
}

/// Commits a move or resize: the new bounds, plus a raise — manipulating an area
/// is exactly the §3.2a interaction that puts it on top of its tier.
///
/// A rejected `set_bounds` (unknown id, or an empty rectangle) leaves the area
/// untouched and skips the raise; there is nothing to raise if the gesture did
/// not apply.
pub(crate) fn move_area(app: &AppHandle, id: AreaId, bounds: Rect) -> bool {
    let store = app.state::<Mutex<AreaStore>>();
    let mut guard = lock(&store);
    guard.set_bounds(id, bounds) && guard.bring_to_front(id)
}

/// Removes an area. Returns whether one was removed.
pub(crate) fn dismiss_area(app: &AppHandle, id: AreaId) -> bool {
    let removed = {
        let store = app.state::<Mutex<AreaStore>>();
        lock(&store).remove(id).is_some()
    };
    if removed {
        collapse_living_if_empty(app);
    }
    removed
}

/// Collapses `Living` to `Hidden` when the last area is dismissed there.
///
/// `overlay_state::next` collapses Living-without-areas on every *event*, but a
/// dismissal is not an event through the state machine — and the Living menu's
/// Dismiss row (ADR-0016) made "the last area disappears while Living" an
/// ordinary path rather than a keyboard corner case (`Delete` right after a
/// transition was the only way before). Without this, the overlay would sit in
/// a state the state machine says cannot exist: visible to the OS but showing
/// nothing, click-through everywhere, hook installed and poll running —
/// indistinguishable from hidden except in cost.
///
/// Lock order is state → store (via [`has_areas`]), the same order [`drive`]
/// uses; nothing takes them the other way around.
fn collapse_living_if_empty(app: &AppHandle) {
    let target = {
        let cell = app.state::<Mutex<OverlayState>>();
        let mut guard = lock(&cell);
        if *guard != OverlayState::Living || has_areas(app) {
            return;
        }
        *guard = OverlayState::Hidden;
        OverlayState::Hidden
    };
    if let Err(error) = apply(app, target) {
        eprintln!("overlay: could not apply state {target:?}: {error}");
    }
}

/// Pins an area to a stacking tier (ADR-0013).
pub(crate) fn set_area_layer(app: &AppHandle, id: AreaId, layer: Layer) -> bool {
    let store = app.state::<Mutex<AreaStore>>();
    lock(&store).set_layer(id, layer)
}

/// The monitor rectangles, cached.
///
/// Enumerating monitors is a Win32 round trip that allocates, and the placement
/// poll needs this list on **every tick** to snap and contain a dragged area —
/// 60 times a second, for a list that changes only when the user replugs a
/// display. So it is refreshed where the display configuration is already being
/// read ([`show`] and [`sync_bounds`], the two paths a display change reaches)
/// rather than polled.
static MONITOR_CACHE: Mutex<Vec<Rect>> = Mutex::new(Vec::new());

/// Refreshes [`MONITOR_CACHE`] from the window's current monitor list.
fn refresh_monitor_cache(window: &WebviewWindow) {
    if let Ok(list) = monitors(window) {
        *lock(&MONITOR_CACHE) = list.iter().map(|monitor| monitor.bounds).collect();
    }
}

/// The cached monitor rectangles, for snapping and containment.
pub(crate) fn monitor_rects() -> Vec<Rect> {
    lock(&MONITOR_CACHE).clone()
}

/// The bounds of the monitor containing `point`, for positioning per-monitor
/// chrome. Falls back to the whole virtual desktop when the point is on no
/// monitor at all — which happens in the dead zones between mismatched monitors,
/// where any answer is a guess and the desktop is at least never `None`.
pub(crate) fn monitor_bounds_at(app: &AppHandle, point: Point) -> Rect {
    let fallback = Rect::new(point.x, point.y, 1, 1);
    let Ok(window) = overlay_window(app) else {
        return fallback;
    };
    let monitors = monitors(&window).unwrap_or_default();
    if let Some(monitor) = uptake_core::geometry::monitor_at(&monitors, point) {
        return monitor.bounds;
    }
    virtual_desktop_bounds(monitors.iter().map(|m| m.bounds)).unwrap_or(fallback)
}

/// Creates a `Default` area at the given physical bounds, returning whether one
/// was created. `Default` is the only type task 1.6 ships (R-17).
///
/// Two rejections, and they are different in kind. `AreaStore::create` refuses
/// an *empty* rectangle as a model invariant — a zero-pixel area could never be
/// drawn or dismissed. `interaction::is_placeable` refuses anything smaller than
/// `MIN_AREA_SPAN` as a *policy*: a click or a twitch of the hand should not
/// leave a sliver of an area behind, and a sliver has no room for the controls
/// that would remove it. The policy check runs first so the invariant stays the
/// last line of defence rather than the only one.
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
    if !interaction::is_placeable(bounds) {
        return false;
    }
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

/// IPC surface: `Delete` from the overlay dismisses the area under the cursor.
///
/// PRODUCT-VISION §4.3 asks for "`Delete` on the focused area". **Focused here
/// means the area under the cursor**, and that choice is deliberate rather than
/// a placeholder: it is the one definition where the user can see, before
/// pressing a key with no undo, exactly which area will go. A remembered
/// "last-touched" focus would let `Delete` remove something off-screen or on
/// another monitor. Keyboard-only focus that moves without a cursor is task
/// 1.16's (M-11); this is the mouse-adjacent half, and the close control is the
/// pure-pointer path.
///
/// With the cursor over empty overlay, `Delete` does nothing — deliberately not
/// "the topmost area", which would be a deletion the user never pointed at.
#[tauri::command]
pub fn overlay_dismiss_focused(app: AppHandle) -> Result<(), String> {
    // Read the cursor from the window rather than from the placement hook's last
    // reported position: the hook only reports while it is installed, so a
    // `Delete` pressed before the mouse has moved since entering Placement would
    // act on a stale point.
    let window = overlay_window(&app)?;
    let position = window
        .cursor_position()
        .map_err(|e| format!("Could not read the cursor position: {e}"))?;
    let Some(point) = Point::from_physical_f64(position.x, position.y) else {
        return Ok(());
    };
    let Some(area) = area_at(&app, point) else {
        return Ok(());
    };
    if dismiss_area(&app, area.id) {
        placement::close_menu(&app);
        emit_areas(&app)?;
    }
    Ok(())
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
