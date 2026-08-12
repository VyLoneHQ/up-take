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
use uptake_core::area::{AfterCreate, AreaId, AreaStore, AreaType, Input, Layer};
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
    // The warm sessions hold a *copy* of the monitor list too, and until
    // 2026-07-30 nothing resynced it: `sync_warm_sessions` was called only from
    // `apply`, i.e. on a state transition, so a display moved during a Placement
    // visit left the sessions keyed to entry-time bounds while `freeze` asked
    // with fresh ones. `capture_monitor` matches by centre containment, so after
    // a monitor swap one display's pixels were served and reported as another's
    // — published as the frozen still and croppable to the clipboard. Found as
    // `Vuln 2` in PR #28's security review.
    //
    // **Gated on the cache having actually changed**, not on the event: this
    // function is re-entered by its own `apply_bounds` corrections (see the
    // window-event handler in `lib.rs`), so an unconditional call resynced twice
    // per real change and on every no-op pass besides. `start` would short-
    // circuit those, but a rebuild is not free — it blocks on each pump's
    // handshake — and this path runs on the event-loop thread.
    if refresh_monitor_cache(&window) {
        crate::freeze::sync_warm_sessions(
            matches!(
                *lock(&app.state::<Mutex<OverlayState>>()),
                OverlayState::Placement
            ),
            placement::real_cursor(app),
        );
    }
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

/// Permanently excludes the overlay from every capture API — its own,
/// screen-sharing, and every other process's
/// ([ADR-0019](../../../Projects/UP-TAKE/DECISIONS/ADR-0019-overlay-excluded-from-capture.md)).
///
/// Called once from `setup`, right after the window exists. **Never** toggled
/// around a capture — decision 1 is explicit that `uptake-capture` stays
/// ignorant of the overlay forever, which is what makes a self-containing live
/// mirror structurally impossible rather than merely defended against. Would
/// need re-applying only if the window were ever destroyed and recreated,
/// which nothing here does today (`hide` keeps it alive).
///
/// A failed call **degrades, it does not abort** (decision 4): below Windows
/// 10.0.19041 `SetWindowDisplayAffinity` fails, logged rather than treated as
/// a startup failure, and the overlay is then visible in captures like any
/// other window.
#[cfg(windows)]
pub fn exclude_from_capture(app: &AppHandle) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    let window = overlay_window(app)?;
    let hwnd = window
        .hwnd()
        .map_err(|e| format!("Could not get the overlay window handle: {e}"))?
        .0;
    // SAFETY: `hwnd` is a live top-level window handle owned by this process,
    // valid for the duration of this call.
    let ok = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) };
    if ok == 0 {
        return Err(
            "SetWindowDisplayAffinity failed (Windows build below 10.0.19041?) — the overlay \
             will be visible in screen captures."
                .to_string(),
        );
    }
    Ok(())
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
    /// The type armed for the next drag, or `null` for none (ADR-0018 §3).
    ///
    /// **Absence means `Default`** — the indicator shows no type cue when
    /// nothing is armed, rather than showing "Default" as if it were a
    /// selection. That is the ADR's wording and it matters: a permanent cue
    /// naming the resting state is the "which mode am I in?" noise the design
    /// avoids.
    armed: Option<&'static str>,
    /// Whether the screen is frozen (task 1.9d). Only ever true in Placement.
    ///
    /// Sent on every state so the frontend has one source for it rather than
    /// inferring it from the last toggle it happened to see — an inferred copy
    /// of a fact Rust already owns is how the stale-menu and replayed-flash
    /// defects of 1.9b happened.
    frozen: bool,
    /// Each frozen still as `(x, y, width, height, url)` — physical
    /// virtual-desktop px plus the URL the WebView fetches the image from. The
    /// URL ends `.png` whatever the display format is — it is an opaque
    /// versioned identifier and never a filename, and the served `Content-Type`
    /// is what decides how the bytes are read.
    ///
    /// Empty whenever [`Self::frozen`] is false, and the two are derived from
    /// the same call rather than assembled separately: a payload claiming
    /// frozen with no stills would render as a live screen the app believes is
    /// frozen.
    stills: Vec<(i32, i32, u32, u32, String)>,
    /// The in-flight `Ctrl+Space` → painted probe, or `null`.
    ///
    /// Present on exactly one payload per freeze — the one carrying the new
    /// stills — because [`crate::freeze::take_paint_probe`] clears as it reads.
    /// Always `null` in a release build, where nothing stamps one.
    ///
    /// The frontend echoes it back through `overlay_report_freeze_latency`
    /// **after every still has decoded**, which is the half of
    /// `quality-bars.md` §1's row that 1.9f's `72–78 ms` never covered.
    freeze_probe: Option<u64>,
}

const fn state_name(state: OverlayState) -> &'static str {
    match state {
        OverlayState::Hidden => "hidden",
        OverlayState::Placement => "placement",
        OverlayState::Living => "living",
    }
}

/// An [`AreaType`] as the frontend names it — the same lowercase wire
/// convention [`layer_name`] uses for [`Layer`].
const fn type_name(kind: AreaType) -> &'static str {
    match kind {
        AreaType::Default => "default",
        AreaType::Screenshot => "screenshot",
        AreaType::Record => "record",
        AreaType::Ocr => "ocr",
        AreaType::Upscale => "upscale",
        AreaType::Analysis => "analysis",
        AreaType::Filter => "filter",
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
/// the armed type (ADR-0018 §4), then Placement itself. Anything else would make
/// `Esc` skip past a transient thing the user can see on screen — dismissing the
/// menu *and* leaving Placement on one keypress is the shape users read as "it
/// did too much".
///
/// **A cancelled drag deliberately keeps its arming.** The user asked for a
/// Screenshot and mis-drew the rectangle; making them re-arm before retrying
/// would punish the correction. That is why the drag rung is strictly inside the
/// arming rung rather than clearing both.
///
/// Every inner case is read from the placement module rather than tracked here,
/// because the hook is the only thing that knows a gesture is live.
pub fn escape(app: &AppHandle) {
    if placement::close_menu(app) {
        return;
    }
    if placement::is_dragging() {
        placement::cancel_drag();
        drive(
            app,
            Event::Escape {
                mid_drag: true,
                armed: placement::armed().is_some(),
            },
        );
        return;
    }
    let armed = placement::armed().is_some();
    if armed {
        placement::disarm();
    }
    drive(
        app,
        Event::Escape {
            mid_drag: false,
            armed,
        },
    );
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
    // The state the machine settled on, logged where the effect happens rather
    // than where the decision is made — so the line reports what the overlay
    // actually became, not what `next` intended. Debug-only; task 1.15 owns
    // real logging. Added for task 1.9b's rig pass, where every other check
    // (did arming work, did the Screenshot auto-exit) is only readable if the
    // current state is an observation instead of an assumption.
    #[cfg(debug_assertions)]
    eprintln!("overlay: state -> {state:?}");
    // Every state transition returns the screen to live (ADR-0026 decision 4).
    // Placed here, at the one point every transition funnels through, rather
    // than at each entry: freeze exists only inside Placement, so "reset on
    // entry" and "never frozen outside Placement" are the same rule, and
    // writing it once means a state added later cannot forget it. A frozen
    // screen surviving into Living would be the worst version of this feature —
    // a still the user cannot dismiss and did not ask to keep.
    crate::freeze::thaw();
    // Warm capture sessions live and die with Placement, placed here beside the
    // thaw and for the same reason: one funnel, so a state added later cannot
    // forget to stop them. A no-op unless the setting is on (roadmap 1.9f).
    crate::freeze::sync_warm_sessions(
        matches!(state, OverlayState::Placement),
        placement::real_cursor(app),
    );
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
    // Read from `MONITOR_CACHE` rather than re-enumerating. `show` refreshes the
    // cache immediately before this runs, so it is current — and using the one
    // list means `overlay://active-monitor`'s index cannot address a different
    // array than the one sent here. A fresh enumeration would be a second
    // source of truth for the same fact, which is how the badge would end up
    // highlighting the wrong monitor.
    let monitors = if matches!(state, OverlayState::Placement) {
        monitor_rects()
            .iter()
            .map(|bounds| {
                (
                    bounds.origin.x,
                    bounds.origin.y,
                    bounds.size.width,
                    bounds.size.height,
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    // Arming is Placement-only state, and reading it in any other state would
    // report something the user cannot act on. `placement` clears it on exit
    // anyway, so this guard is the second lock on the same door.
    let armed = if matches!(state, OverlayState::Placement) {
        placement::armed().map(type_name)
    } else {
        None
    };
    // One read, two fields: `frozen` is "are there stills" by construction
    // rather than a flag that could disagree with the list beside it.
    let stills: Vec<(i32, i32, u32, u32, String)> = crate::freeze::stills_for_display()
        .into_iter()
        .map(|(bounds, url)| {
            (
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
                url,
            )
        })
        .collect();
    app.emit(
        STATE_EVENT,
        StatePayload {
            state: state_name(state),
            origin,
            monitors,
            armed,
            frozen: !stills.is_empty(),
            // Taken only when this payload actually carries stills. A thaw or an
            // unrelated re-emit must not consume a probe a freeze is still
            // waiting to attach to — and must not report one against a paint
            // that drew nothing.
            freeze_probe: if stills.is_empty() {
                None
            } else {
                crate::freeze::take_paint_probe()
            },
            stills,
        },
    )
    .map_err(|e| format!("Could not emit overlay state: {e}"))
}

/// Toggles the frozen view — `Ctrl+Space` in Placement (task 1.9d, ADR-0026).
///
/// # Placement-only, and it says so rather than going quiet
///
/// ADR-0026 decision 2 scopes freeze to Placement. Called from any other state
/// this logs and does nothing: the frontend does not send the key outside
/// Placement, so reaching here from Living means the two disagree about the
/// state, which is worth a line rather than a silent return.
///
/// # Why freezing spawns and thawing does not
///
/// A freeze is one capture per monitor — approaching a second on a four-monitor
/// desktop — and this runs on the event-loop thread. Blocking it would hang the
/// window it is about to redraw. Thawing only drops the stills, so it is
/// immediate and emits inline.
///
/// The state is emitted **after** the captures land, so the frontend never shows
/// a frozen indicator over live pixels. The visible cost is that a freeze takes
/// effect a beat after the key; the alternative is a lie on screen for the same
/// beat, and this project has already recorded what a green-looking wrong state
/// costs.
pub fn toggle_freeze(app: &AppHandle) {
    let state = *lock(&app.state::<Mutex<OverlayState>>());
    if !matches!(state, OverlayState::Placement) {
        eprintln!("overlay: freeze toggle ignored outside Placement (state {state:?})");
        return;
    }
    if crate::freeze::is_frozen() {
        crate::freeze::thaw();
        if let Err(error) = emit_state(app, state) {
            eprintln!("overlay: could not emit state after thawing: {error}");
        }
        return;
    }
    // Stamped here — on the key, on the calling thread, before the capture
    // thread is even spawned — because `quality-bars.md` §1's row measures what
    // the user waits for. Anything later would time a stage and call it the
    // promise.
    crate::freeze::stamp_paint_probe();
    // Read on this thread, before the spawn, for the same reason the probe is
    // stamped here: it is the cursor at the moment the user pressed the key, and
    // by the time the capture thread runs the pointer may have moved. The scope
    // must describe what the user was looking at when they asked.
    let cursor = placement::real_cursor(app);
    let app = app.clone();
    std::thread::spawn(move || {
        // Narrowed to the cursor's monitor unless the 1.14 setting widens it
        // (ADR-0026's third amendment, inverting ADR-0014 §4). `monitor_rects`
        // stays the full list because the log below reports the freeze as a
        // ratio of the desktop, and "1/1" would hide which of four screens is
        // frozen — the exact thing the ratio was added to show.
        let desktop = monitor_rects();
        let monitors = crate::freeze::monitors_in_scope(&desktop, crate::freeze::scope_for(cursor));
        // Timed because the wait between the key and the still appearing is the
        // whole felt cost of this feature and nothing else reported it. Logged
        // in the same shape `output.rs` uses for the export path, with the
        // stage split, because "slow" is not actionable and "the capture is
        // slow" is. The stage figures are per-monitor maxima and the total is
        // wall-clock, so they do not add up — see `FreezeReport`.
        let report = match crate::freeze::freeze(&monitors) {
            Ok(report) => report,
            // Nothing was published, so nothing is emitted: the state the
            // frontend already holds is the correct one in both cases. Logged
            // rather than silent — from the user's side the key did nothing, and
            // a ~420 ms window where that is the right behaviour is exactly what
            // a session reads a log to explain.
            Err(skipped) => {
                eprintln!("freeze: no stills published — {skipped}");
                return;
            }
        };
        // Reported as a ratio, not as a success: a freeze that captured three of
        // four monitors is a real state the user is about to select on, and
        // "frozen" alone would hide which screens are still live.
        //
        // **Two ratios since the narrowing, and collapsing them into one would
        // make the line lie.** Against the desktop it says how much of the screen
        // the user is now looking at frozen; against the scope it says whether
        // the freeze did what it was asked. A single `1/1` cannot distinguish a
        // narrowed freeze that worked from a whole-desktop freeze that lost three
        // monitors, and a single `1/4` cannot distinguish it from a whole-desktop
        // freeze that lost three either. `UT-F-46`'s rule is that a run reports
        // the condition it ran under, and the scope is that condition here.
        eprintln!(
            "freeze: froze {}/{} monitor(s) in scope, {} of {} on the desktop, in {} ms — \
             warm {}/{}, slowest monitor: capture {} ms, encode {} ms",
            report.count,
            monitors.len(),
            report.count,
            desktop.len(),
            report.elapsed_ms,
            report.warm_served,
            report.count,
            report.slowest_capture_ms,
            report.slowest_encode_ms
        );
        // Per-monitor, with the encoded size beside the timings, because
        // `quality-bars.md` §1's row is content-dependent and a maximum cannot
        // say what it was taken against. The byte length is the run describing
        // its own conditions mechanically — never the operator's word for what
        // was on screen, which is exactly what `UT-F-47` and `UT-F-46` cost.
        //
        // Unconditional rather than gated on a dev flag: `I-11` is what a switch
        // nobody can prove is on looks like, and a rig operator reading a freeze
        // line is the reader this exists for.
        for cost in &report.per_monitor {
            eprintln!(
                "freeze:   {}x{} at ({}, {}) — capture {} ms, encode {} ms, \
                 {} bytes, {}",
                cost.rect.size.width,
                cost.rect.size.height,
                cost.rect.origin.x,
                cost.rect.origin.y,
                cost.capture_ms,
                cost.encode_ms,
                cost.encoded_bytes,
                if cost.served_warm { "warm" } else { "cold" }
            );
        }
        if let Err(error) = emit_state(&app, OverlayState::Placement) {
            eprintln!("overlay: could not emit state after freezing: {error}");
        }
    });
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
const PIN_EVENT: &str = "overlay://pin";

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
    /// The area's type on the wire, following [`type_name`]'s convention, so a
    /// type that carries its own visual treatment can be styled without the
    /// frontend inferring what the area is.
    ///
    /// Added with the Filter type. Before it every area drew identically, so
    /// this field would have been inert: [`type_name`] existed only to name the
    /// *armed* type in the placement badge, never a placed one.
    kind: &'static str,
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
            kind: type_name(area.kind),
        })
        .collect();
    app.emit(AREAS_EVENT, AreasPayload { areas })
        .map_err(|e| format!("Could not emit overlay areas: {e}"))
}

/// What a hit-test resolves to: the identity and the menu-relevant properties
/// of an area, detached from the store so no lock outlives the call.
#[derive(Clone, Copy)]
pub(crate) struct AreaSummary {
    pub id: AreaId,
    pub layer: Layer,
    pub input: Input,
    /// What the area is — the area menu shows Copy/Save only for `Default`
    /// areas (task 1.9's scope; a typed capture area is 1.9b's).
    pub kind: AreaType,
}

impl AreaSummary {
    fn of(area: &uptake_core::area::Area) -> Self {
        Self {
            id: area.id,
            layer: area.layer,
            input: area.input,
            kind: area.kind,
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

/// The topmost area that claims a `Living` mouse event at `point` (ADR-0016,
/// V-7). `None` means the event belongs to the user's apps: the point is over
/// empty overlay, or over the *body* of a pass-through area.
///
/// [`AreaStore::hit_test`], not `hit_test_any` — the difference *is* the input
/// model. Since [ADR-0024](../../../Projects/UP-TAKE/DECISIONS/ADR-0024-direct-manipulation-in-living.md)
/// §2 that difference is narrower than it was: a pass-through area no longer
/// misses every click regardless of stacking, it misses clicks on its **body**
/// and takes them on its **chrome**. A Filter pinned to `Front` still never
/// steals a click from the app underneath it, which is the property that
/// motivated the split.
pub(crate) fn interactive_area_at(app: &AppHandle, point: Point) -> Option<AreaSummary> {
    let monitors = monitor_rects();
    let store = app.state::<Mutex<AreaStore>>();
    let guard = lock(&store);
    guard.hit_test(point, &monitors).map(AreaSummary::of)
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

/// The topmost **interactive** area whose interaction surface contains `point`.
///
/// The `Living` counterpart of [`area_handle_at`], and it draws the same
/// distinction [`AreaStore::hit_test`] and `hit_test_any` already do: in
/// `Placement` the user is editing the workspace, so every area is grabbable
/// whatever its [`Input`]; in `Living` a pass-through area's *body* belongs to
/// the app underneath.
///
/// # Task 1.17(b): chrome is grabbable, the body is not
///
/// This used to filter pass-through areas out entirely, which is what made them
/// **ungrabbable in `Living`** — a `Filter` or `Record` area (pass-through by
/// default) could only be touched by re-entering `Placement`, and flipping any
/// area to pass-through stranded it there. [ADR-0024](../../../Projects/UP-TAKE/DECISIONS/ADR-0024-direct-manipulation-in-living.md)
/// §2 redefines the property: the body passes clicks through, the chrome does
/// not.
///
/// The resolution deliberately mirrors [`AreaStore::hit_test`] rather than
/// re-deriving it — same rule, expressed once per side of the IPC boundary,
/// because two copies of "what takes input" drifting apart is how a click gets
/// swallowed by an area that would not have handled it.
///
/// **A pass-through area still cannot be *moved* in `Living`.** Its chrome is the
/// resize band and the close control; `Handle::Body` is the move grab, and that is
/// precisely what passes through. Moving arrives with 1.17(c)'s `Win+Shift` drag
/// or 1.17(b2)'s control bar, whichever lands first.
pub(crate) fn interactive_area_handle_at(
    app: &AppHandle,
    point: Point,
) -> Option<(AreaId, Rect, interaction::Handle)> {
    let monitors = monitor_rects();
    let store = app.state::<Mutex<AreaStore>>();
    let guard = lock(&store);
    guard.iter_top_down().find_map(|area| {
        let handle = interaction::handle_at(area.bounds, point, &monitors)?;
        // An interactive area answers for any handle; a pass-through one only for
        // chrome. `find_map` stops at the first area that answers, so a
        // pass-through area's body does not shadow an interactive area beneath it.
        if area.is_interactive() || !matches!(handle, interaction::Handle::Body) {
            Some((area.id, area.bounds, handle))
        } else {
            None
        }
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
        // The pinned capture goes with the area that displayed it — see
        // `captures`'s Lifetime note; a map that only grows is a leak the
        // 8-hour soak (M-20) would find and nothing else would.
        crate::captures::forget(app, id);
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

/// An area's current bounds, for the output pipeline (task 1.9) to capture.
///
/// Read fresh at the moment Copy/Save is activated rather than carried from
/// the menu's own opening: the menu can stay open across pump ticks, and a
/// capture should target where the area is *now*, not where it was when the
/// menu was drawn (it cannot move while a menu is open today, but this is the
/// same "read state at the point of action" discipline [`overlay_dismiss_focused`]
/// already follows).
pub(crate) fn area_bounds(app: &AppHandle, id: AreaId) -> Option<Rect> {
    let store = app.state::<Mutex<AreaStore>>();
    lock(&store).get(id).map(|area| area.bounds)
}

/// The monitor rectangles, cached.
///
/// Enumerating monitors is a Win32 round trip that allocates, and the placement
/// poll needs this list on **every tick** to snap and contain a dragged area —
/// 60 times a second, for a list that changes only when the user replugs a
/// display. So it is refreshed where the display configuration is already being
/// read ([`show`] and [`sync_bounds`], the two paths a display change reaches)
/// rather than polled.
/// **Holds whole [`Monitor`]s rather than bare rectangles**, so the change
/// detection below is as sharp as this source allows — see
/// [`refresh_monitor_cache`]. Bounds alone missed a scale-factor change at
/// identical bounds, which is a real reconfiguration.
static MONITOR_CACHE: Mutex<Vec<Monitor>> = Mutex::new(Vec::new());

/// Refreshes [`MONITOR_CACHE`] from the window's current monitor list, and
/// reports whether the list actually changed.
///
/// The return value exists for [`sync_bounds`]'s warm-session resync. Every
/// `apply_bounds` raises `Moved`/`Resized`, which route back here, so a real
/// display change produces a convergence pass behind it where nothing differs.
/// Acting on the change rather than on the event keeps the resync to one per
/// change (PR #28 review, finding B).
///
/// # What the gate saves, and what it cannot see — corrected 2026-07-31
///
/// **It saves an enumeration, not a rebuild.** The first version of this said a
/// rebuild "is not free — it blocks on each pump's handshake", which is true of
/// a rebuild and not of what this suppresses: on a no-op pass
/// [`warm::start`](uptake_capture::warm::start) *short-circuits* on its own
/// `covers` check and rebuilds nothing. What the gate removes from the
/// event-loop thread is one monitor enumeration and a `covers` walk per
/// convergence pass. Worth having on this thread; smaller than it read.
///
/// **And it is deliberately coarser than the thing it gates.** `covers` keys on
/// the Win32 **`HMONITOR` and bounds**; this keys on what Tauri reports, which
/// carries no handle. So a display **replaced at identical bounds and scale** —
/// a new `HMONITOR`, everything else equal — looks unchanged here and the
/// resync does not run. That is a gap this source cannot close, so it is stated
/// rather than papered over. It is bounded twice: `apply` calls
/// `sync_warm_sessions` unconditionally on the next state transition, and the
/// dead handle's pump exits and clears its retained frame, so the outcome is a
/// monitor stuck on the cold path until then — never one monitor's pixels
/// served as another's.
fn refresh_monitor_cache(window: &WebviewWindow) -> bool {
    let Ok(fresh) = monitors(window) else {
        // The list could not be read, so nothing is known to have changed and
        // the cache keeps what it had. Reporting `true` here would rebuild the
        // warm sessions on a failure to observe, which is the opposite of what
        // an unreadable list justifies.
        return false;
    };
    let mut cached = lock(&MONITOR_CACHE);
    if *cached == fresh {
        return false;
    }
    *cached = fresh;
    true
}

/// Perturbs one cached monitor's **scale factor, leaving its bounds alone**, so
/// the next [`refresh_monitor_cache`] sees a scale-only reconfiguration.
///
/// # Why this exists, and what it is honestly worth
///
/// `6e25555` widened [`MONITOR_CACHE`] from bare rectangles to whole
/// [`Monitor`]s precisely so a **scale change at identical bounds** would drive
/// the warm-session resync. Nothing verifies that. The owed rig check asked the
/// operator to change a monitor's DPI *while PLACEMENT is visible*, and
/// `UT-F-50` records that this **cannot be performed by any operator**:
/// entering PLACEMENT installs the global mouse hook and takes focus, so no
/// Windows display UI is reachable while the state under test is active. The
/// substitute an operator naturally reaches for, powering a monitor off, is not
/// a display-configuration change at all and returned a confident "everything
/// looked fine" on a check that never ran.
///
/// An unplug does not test it either: an unplug changes the bounds, so it would
/// have passed under the old bounds-keyed code and exercises nothing the
/// widening added.
///
/// **The fidelity, stated rather than left to be assumed.** The comparison
/// under test (`*cached == fresh`), the gate's return value, and the resync
/// behind it are all the real code on the real path. What is *not* exercised is
/// Windows raising the change and Tauri reporting a new scale factor — this
/// injects the difference at the cache instead. So a green here means "a
/// scale-only difference drives the resync", and it does **not** mean "a real
/// DPI change is observed". The second half has no route on this machine and is
/// recorded as still owed rather than quietly folded in.
///
/// Returns what it changed, for the caller to log, or `None` when the cache is
/// empty and there is nothing to perturb.
#[cfg(debug_assertions)]
pub(crate) fn dev_perturb_cached_scale() -> Option<(Rect, f64, f64)> {
    let mut cached = lock(&MONITOR_CACHE);
    let monitor = cached.first_mut()?;
    let was = monitor.scale_factor;
    // A value no real display reports, so a log line showing it cannot be
    // mistaken for a genuine reconfiguration by someone reading back later.
    let now = 3.5;
    monitor.scale_factor = now;
    Some((monitor.bounds, was, now))
}

/// The cached monitor rectangles, for snapping and containment.
///
/// Order is the cache's order, which is what makes an index from
/// [`monitor_at_point`] addressable against this list.
pub(crate) fn monitor_rects() -> Vec<Rect> {
    lock(&MONITOR_CACHE)
        .iter()
        .map(|monitor| monitor.bounds)
        .collect()
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

/// Creates an area of `kind` at the given physical bounds, returning its id and
/// stored rectangle, or `None` if nothing was created.
///
/// `kind` comes from the arming state (ADR-0018 §1) — `Default` when nothing is
/// armed. Task 1.6 shipped `Default` alone (R-17); 1.9b adds `Screenshot` as the
/// first type a gesture can actually select.
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
pub(crate) fn create_area(
    app: &AppHandle,
    kind: AreaType,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Option<(AreaId, Rect)> {
    let bounds = Rect {
        origin: Point::new(x, y),
        size: Size::new(width, height),
    };
    if !interaction::is_placeable(bounds) {
        return None;
    }
    let store = app.state::<Mutex<AreaStore>>();
    let id = lock(&store).create(kind, bounds)?;
    // The bounds travel back with the id because the capture that follows a
    // Screenshot create needs the *stored* rectangle, not the one the caller
    // asked for. They are equal today; returning the store's answer means a
    // future clamp or snap in `create` cannot silently desynchronise the pin
    // from the pixels.
    Some((id, bounds))
}

const ACTIVE_MONITOR_EVENT: &str = "overlay://active-monitor";

/// The payload of `overlay://active-monitor`: which monitor holds the cursor.
#[derive(Serialize, Clone)]
struct ActiveMonitorPayload {
    /// An index into the `monitors` array of the last `overlay://state` — both
    /// come from [`monitor_rects`], so they address the same list. `null` when
    /// the cursor is in a dead zone between mismatched monitors, where any
    /// answer would be a guess.
    index: Option<usize>,
}

/// Which monitor contains `point`, as an index into [`monitor_rects`].
///
/// The scan is [`uptake_core::geometry::index_at`]'s. It was one of five copies
/// of that rule until 2026-08-09, and one `I-30` does not name — see that
/// function's own table, which is the single place the count lives. Dead zones
/// stay `None` here, because the caller is the placement badge and a badge on a
/// guessed monitor is worse than no badge.
pub(crate) fn monitor_index_at(point: Point) -> Option<usize> {
    uptake_core::geometry::index_at(monitor_rects(), point)
}

/// Tells the frontend which monitor the per-monitor placement chrome belongs on.
///
/// Emitted on change from the placement poll rather than folded into
/// `overlay://state`: the answer changes as the cursor crosses a monitor edge,
/// which is not a state transition, and re-emitting the whole state payload for
/// it would recompute geometry for a one-integer change.
pub(crate) fn emit_active_monitor(app: &AppHandle, index: Option<usize>) {
    if let Err(error) = app.emit(ACTIVE_MONITOR_EVENT, ActiveMonitorPayload { index }) {
        eprintln!("overlay: could not emit the active monitor: {error}");
    }
}

const FLASH_EVENT: &str = "overlay://flash";

/// The payload of `overlay://flash`: an action on this area just succeeded.
#[derive(Serialize, Clone)]
struct FlashPayload {
    id: u64,
    /// Distinguishes one flash from the next so the frontend can restart the
    /// animation. Two Copies in a row are two events with identical `id`, which
    /// a reactive framework would otherwise coalesce into no visible change at
    /// all — the failure being *silence*, which is the very thing this fixes.
    nonce: u64,
}

/// Acknowledges a completed user-initiated action by flashing its area.
///
/// **The success half of F-35.** That row records the failure half — a failed
/// Copy or Save reaches nobody once the app is not run from a console — and
/// concludes the deciding axis is *"did a user ask for this?"*. By that test a
/// *successful* Copy needs an answer just as much: the user pressed a menu row
/// and, until now, absolutely nothing happened on screen.
///
/// This is the cheap half. The clickable "Image saved — open folder" toast is
/// task 1.15's, with the rest of F-35: the overlay is `WS_EX_TRANSPARENT`, so a
/// toast cannot be a clickable DOM element and has to be drawn by the WebView
/// and hit-tested in Rust the way the area menu already is. Not hard, but not a
/// two-line change either.
pub(crate) fn emit_flash(app: &AppHandle, id: AreaId) {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Err(error) = app.emit(
        FLASH_EVENT,
        FlashPayload {
            id: id.get(),
            nonce,
        },
    ) {
        eprintln!("overlay: could not emit the flash: {error}");
    }
}

/// The payload of `overlay://pin`: one area's capture is ready to render.
#[derive(Serialize, Clone)]
struct PinPayload {
    id: u64,
    /// The URL to load it from — a `uptake-area://` address, **not** the bytes.
    /// See `captures`'s module docs for why the pixels do not travel over this
    /// bridge.
    url: String,
}

/// Announces that `id`'s capture is pinned and available at its versioned URL.
///
/// Emitted rather than folded into `overlay://areas` because the two have
/// different timing: an area appears the instant the drag ends, and its capture
/// lands ~200 ms later. Making the area wait for its pixels would put a visible
/// hole where the user just dragged.
pub(crate) fn emit_pin(app: &AppHandle, id: AreaId, version: u64) -> Result<(), String> {
    app.emit(
        PIN_EVENT,
        PinPayload {
            id: id.get(),
            url: crate::captures::pin_url(id, version),
        },
    )
    .map_err(|e| format!("Could not emit the pin: {e}"))
}

/// Applies a type's ADR-0018 §6 after-create behaviour, on the event loop.
///
/// # `run_on_main_thread` does **not** defer when you are already on it
///
/// This function's first version called `app.run_on_main_thread` directly and
/// documented the hop as "queued rather than run inline, and that is the point".
/// **That is the opposite of what happens.** `tauri-runtime-wry`'s
/// `send_user_message` (2.11.4, `src/lib.rs:239`) begins:
///
/// ```text
/// if current_thread().id() == context.main_thread_id {
///   handle_user_message(...);   // executed inline, right here
/// } else {
///   context.proxy.send_event(message)   // actually queued
/// }
/// ```
///
/// The only caller is `placement::finish_gesture`, which runs inside the
/// `WH_MOUSE_LL` callback — and that callback runs on the thread that installed
/// the hook, which is the event-loop thread. So the identity test passes and the
/// closure ran **synchronously inside the mouse hook**, which is precisely what
/// the comment claimed it avoided.
///
/// What that actually cost, stated rather than implied: [`drive`] calls
/// [`apply`] unconditionally, even when `next` returns the state it was already
/// in — so every `Screenshot` create ran `apply(Placement)` in the hook, i.e.
/// `show` (which re-enumerates monitors into the cache), `placement::enter`
/// (inline for the same reason), a `window.inner_position()` Win32 call, and two
/// IPC emits. Nothing broke on the rig, and it stays far inside the 300 ms
/// `LowLevelHooksTimeout`. It still mattered twice over: that work lands inside
/// the mouse-up interval `quality-bars.md` §1 measures and task 1.9c has to
/// shrink, and the moment task 1.14 makes [`AfterCreate::ExitPlacement`]
/// reachable the inline path becomes `apply(Living)` →
/// `placement::enter_living` → `restore_system_cursors`, a **global**
/// `SPI_SETCURSORS` scheme reload that broadcasts `WM_SETTINGCHANGE`, executed
/// from within a low-level mouse hook. That is F-33's failure class exactly.
///
/// So the hop starts from a spawned thread, where the identity test fails and
/// the message is genuinely posted. One short-lived thread per area creation,
/// next to the capture thread `capture_on_create` already spawns.
pub(crate) fn area_created(app: &AppHandle, kind: AreaType) {
    let exits_placement = kind.after_create() == AfterCreate::ExitPlacement;
    let app = app.clone();
    std::thread::spawn(move || {
        let handle = app.clone();
        if let Err(error) = app.run_on_main_thread(move || {
            drive(&handle, Event::AreaCreated { exits_placement });
        }) {
            eprintln!("overlay: could not apply the after-create transition: {error}");
        }
    });
}

/// Locks a mutex, treating poisoning as recoverable — the state under it is a
/// plain enum, valid after any panic, and architecture §5 forbids `unwrap`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// IPC surface: the frontend returns a latency probe once its frame has painted.
///
/// Deliberately does no validation beyond existing: the value is one this
/// process minted moments earlier, and a nonsense one can only distort a debug
/// statistic. Rejecting it would be more code guarding less.
#[tauri::command]
pub fn overlay_report_latency(probe: u64) {
    placement::record_latency(probe);
}

/// IPC surface: the frontend returns the freeze probe once every still has
/// decoded **and** the following frame has painted.
///
/// Separate from [`overlay_report_latency`] rather than sharing its collector:
/// the poll probe accumulates ~1,200 samples across a drag and reports a mean,
/// while a freeze is one event whose single number is the answer. Pooling them
/// would average two different rows of `quality-bars.md` §1 into one figure
/// belonging to neither.
///
/// **Debug builds only.** Nothing stamps a probe in release, so the command
/// used to compile to an empty body and register anyway — an endpoint that
/// existed for no reason (PR #28 review, finding D). It is now absent from a
/// release build entirely, along with its registration in `lib.rs`; verified by
/// searching the release binary for this function's name.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn overlay_report_freeze_latency(probe: u64) {
    crate::freeze::record_paint_latency(probe);
}

/// IPC surface: `Esc` from the overlay emits this intent.
#[tauri::command]
pub fn overlay_escape(app: AppHandle) {
    escape(&app);
}

/// Parses a wire type name into an [`AreaType`], the inverse of
/// [`layer_name`]'s convention for [`Layer`].
///
/// Only the types a **direct key can arm** are accepted. The rest of
/// `AreaType` is modelled but has no gesture, and silently accepting a name
/// nothing can produce would turn a frontend typo into an area of a type the
/// app cannot render.
fn armable_type(name: &str) -> Option<AreaType> {
    match name {
        "screenshot" => Some(AreaType::Screenshot),
        // Filter is the second type to earn a gesture (PRODUCT-VISION §3.1,
        // key `F`). It is passive and pass-through by model default, so the
        // area draws a tint and the user keeps working underneath it.
        "filter" => Some(AreaType::Filter),
        _ => None,
    }
}

/// IPC surface: `Ctrl+Space` toggles the frozen view (task 1.9d, ADR-0026).
///
/// Returns `Ok` even when the toggle is ignored for being outside Placement.
/// **That is deliberate and is the opposite of `overlay_arm_type`'s choice
/// above**, because the two keys mean different things when they miss: arming
/// outside Placement is the frontend asking for something incoherent, while
/// `Ctrl+Space` outside Placement is a user pressing a key that simply does not
/// apply there. Surfacing the second as an IPC error would put a rejection in
/// the console every time someone taps it in Living, which is how a log stops
/// being read.
#[tauri::command]
pub fn overlay_toggle_freeze(app: AppHandle) {
    toggle_freeze(&app);
}

/// IPC surface: a direct key arms the type of the **next** drag (ADR-0018 §1).
///
/// Only meaningful in `Placement` — the state that has a next drag — and
/// rejected elsewhere rather than stored for later, because arming that
/// outlives the state it was set in is precisely the mode state ADR-0009 §3
/// deleted.
#[tauri::command]
pub fn overlay_arm_type(app: AppHandle, kind: String) -> Result<(), String> {
    let Some(kind) = armable_type(&kind) else {
        return Err(format!("{kind} is not an armable area type"));
    };
    let state = *lock(&app.state::<Mutex<OverlayState>>());
    if state != OverlayState::Placement {
        return Err("arming is only meaningful in placement".to_string());
    }
    placement::arm(kind);
    // Re-emit so the indicator picks up the new armed type; ADR-0018 §3 makes
    // the indicator the thing that buys down the cost of having mode state at
    // all, so arming without telling the frontend is the failure mode, not a
    // missing nicety.
    emit_state(&app, state)
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

    /// `type_name` and `armable_type` are hand-maintained inverses across a
    /// wire with no shared schema, and the frontend now styles areas on the
    /// result. A name that resolves to the wrong type would put one type's
    /// visual treatment on another, with nothing failing to say so.
    ///
    /// Both directions are pinned per pair rather than composed through an
    /// unwrap: the workspace denies `expect_used`, and a test module opting
    /// back out for one assertion widens the exemption further than the
    /// assertion is worth.
    #[test]
    fn every_armable_name_round_trips_through_type_name() {
        for (name, kind) in [
            ("screenshot", AreaType::Screenshot),
            ("filter", AreaType::Filter),
        ] {
            assert_eq!(armable_type(name), Some(kind), "{name} must arm");
            assert_eq!(type_name(kind), name, "{name} must round trip");
        }
    }

    /// The half that matters. `armable_type` is the gate that stops a frontend
    /// typo becoming an area of a type the app cannot draw, so what it
    /// *refuses* is the property, and a widening that quietly accepted the
    /// other five would be invisible from the accepting side alone.
    #[test]
    fn a_modelled_type_with_no_gesture_is_still_refused() {
        for name in ["default", "record", "ocr", "upscale", "analysis"] {
            assert_eq!(armable_type(name), None, "{name} has no gesture yet");
        }
        assert_eq!(armable_type("Filter"), None, "the wire name is lowercase");
        assert_eq!(armable_type(""), None);
    }

    /// All seven wire names, pinned. The frontend's `AreaKind` union is a
    /// hand-written mirror of this list and **nothing checks it from the other
    /// side**: an eighth `AreaType` forces an arm in `type_name` and leaves
    /// TypeScript silent, so the new name reaches the browser outside the union
    /// with no type error and the area draws as a default one (`I-55`).
    ///
    /// This test cannot close that gap. What it does is make a *rename* go red
    /// here, so the two lists cannot drift apart quietly in the cheaper of the
    /// two directions.
    #[test]
    fn every_area_type_has_its_wire_name_pinned() {
        for (kind, name) in [
            (AreaType::Default, "default"),
            (AreaType::Screenshot, "screenshot"),
            (AreaType::Record, "record"),
            (AreaType::Ocr, "ocr"),
            (AreaType::Upscale, "upscale"),
            (AreaType::Analysis, "analysis"),
            (AreaType::Filter, "filter"),
        ] {
            assert_eq!(type_name(kind), name);
        }
    }
}
