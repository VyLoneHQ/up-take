//! Live-placement input for the overlay (roadmap task 1.6,
//! [ADR-0014](../../../Projects/UP-TAKE/DECISIONS/ADR-0014-capture-and-render-over-live-content.md)).
//!
//! The overlay is **click-through whenever it is visible** (ADR-0014): a
//! transparent window that ignores the cursor is the only overlay state that
//! does not degrade hardware-accelerated video underneath it. That is right for
//! the resting `Living` state, but it takes the mouse away from the one moment
//! the overlay genuinely needs it — dragging out a new area in `Placement`.
//!
//! The way back is a **global low-level mouse hook** (`WH_MOUSE_LL`). It runs
//! while the overlay stays click-through, so the desktop keeps compositing live
//! content crisply; it *owns the gesture* (button-down → move → button-up) and
//! **swallows the button events** so the app underneath receives nothing. The
//! rectangles are drawn by the WebView from coordinates this module publishes;
//! a **global cursor override** ([`SetSystemCursor`]) supplies the pointer
//! shape, because a click-through window can set no cursor of its own (no
//! `WM_SETCURSOR` ever reaches it). All three pieces were validated in isolation
//! by the spikes recorded in ADR-0014 before this was written.
//!
//! # Everything an area appears to have is a rectangle this module hit-tests
//!
//! Because no mouse event reaches the WebView, **nothing rendered in the overlay
//! can be clicked as a DOM element** — not the close control, not a menu row.
//! The area's whole lifecycle therefore runs through this hook: a press is
//! classified against the area under the cursor ([`classify_press`]), and what
//! it grabbed decides what the drag does — create, move, resize, dismiss, or
//! pick a menu row. The geometry of that classification is pure and lives in
//! `uptake_core::interaction`; this module supplies only the Win32 half. The
//! frontend receives the same rectangles and draws them, so the thing on screen
//! and the thing that responds are one rectangle rather than two that agree by
//! coincidence.
//!
//! # The hook writes atomics; the poll does the work
//!
//! A `WH_MOUSE_LL` callback that takes too long is *silently removed* by Windows
//! (`LowLevelHooksTimeout`), so anything that is not strictly per-event runs in
//! [`pump`], driven by the click-through poll at ~60 Hz: publishing the live
//! rectangle, tracking the cursor shape, and the hover highlights. The hook
//! takes a lock only on a button press, which happens once per gesture rather
//! than at the mouse's report rate.
//!
//! # Thread affinity — the one rule that makes or breaks the hook
//!
//! A `WH_MOUSE_LL` hook is serviced **only while the thread that installed it
//! pumps messages**, and its callback runs **on that same thread**. tao's event
//! loop pumps messages on the main thread, so [`enter`] and [`exit`] marshal the
//! install/uninstall onto it with `run_on_main_thread` rather than trusting
//! whatever thread a state transition happened to arrive on (an `Esc` IPC
//! command, for instance, runs on a Tauri worker thread). Installed anywhere
//! else, the hook would simply never fire.
//!
//! # The system cursor is global state that outlives a crash
//!
//! [`SetSystemCursor`] replaces the shared system cursors for **every process**,
//! and the system *destroys* the handle it is given — so each override is a
//! fresh [`CopyIcon`] of the crosshair, and the restore
//! ([`SystemParametersInfoW`] with `SPI_SETCURSORS`) reloads every cursor from
//! the registry. It is called on every exit path this process controls: leaving
//! `Placement` ([`exit`], subject to the deferral below), a graceful shutdown
//! ([`teardown`] from `RunEvent::Exit`), and a panic ([`install_panic_guard`]).
//! What it cannot cover is a **hard kill** (Task Manager) mid-placement, which
//! runs none of our code and leaves the crosshair set until the user's next
//! cursor-scheme reload — a limitation ADR-0014 accepts explicitly.
//!
//! # Abandoned gestures: a swallowed button-down obliges us to the button-up
//!
//! Two things can end `Placement` while a mouse button is still physically
//! held down: cancelling mid-drag (`Esc`, [`cancel_drag`]) and toggling away
//! (the hotkey) before releasing. In both cases the button's *down* was already
//! swallowed — nothing underneath ever saw it — so letting its eventual *up*
//! pass through would hand the app under the cursor at release time a lone
//! button-up with no matching down, which is exactly the leak this module
//! exists to prevent. [`LEFT_PENDING`]/[`RIGHT_PENDING`] track "a down was
//! swallowed and its up has not been seen yet" independently of [`DRAGGING`]
//! (the *visual* drag, which a cancel or a toggle-away clears immediately); the
//! hook keeps swallowing until the pending flag clears, regardless of whether
//! [`ACTIVE`] says placement itself is still current. [`exit`] defers the actual
//! hook uninstall and cursor restore ([`WANT_TEARDOWN`]) until that happens —
//! removing the hook early would take away the only thing left to catch the
//! outstanding release.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uptake_core::area::{AreaId, Layer};
use uptake_core::geometry::{Point, Rect};
use uptake_core::interaction::{self, Handle, Resize};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CopyIcon, HCURSOR, HHOOK, IDC_CROSS, IDC_HAND, IDC_SIZEALL, IDC_SIZENESW,
    IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, LoadCursorW, MSLLHOOKSTRUCT, OCR_APPSTARTING, OCR_CROSS,
    OCR_HAND, OCR_IBEAM, OCR_NO, OCR_NORMAL, OCR_SIZEALL, OCR_SIZENESW, OCR_SIZENS, OCR_SIZENWSE,
    OCR_SIZEWE, OCR_UP, OCR_WAIT, SPI_SETCURSORS, SetSystemCursor, SetWindowsHookExW,
    SystemParametersInfoW, UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use crate::overlay;

/// The Tauri event the frontend listens on for the live selection rectangle.
const SELECTION_EVENT: &str = "placement://selection";

/// The Tauri event carrying the open area menu, or `null` when none is open.
const MENU_EVENT: &str = "overlay://menu";

/// The Tauri event carrying which area the cursor is over, or `null`.
const HOVER_EVENT: &str = "overlay://hover";

/// The installed hook, as an `HHOOK` cast to `isize`; `0` means "no hook". Only
/// [`install_on_main_thread`] / [`teardown_now`] touch it, and both run on the
/// event-loop thread, but it is atomic so [`is_dragging`] and friends can read
/// process-wide state without a lock.
static HOOK: AtomicIsize = AtomicIsize::new(0);

/// Whether placement is the current overlay state. Gates whether a fresh
/// `WM_LBUTTONDOWN`/`WM_RBUTTONDOWN` starts a new swallowed gesture — set by
/// [`enter`], cleared by [`exit`] the instant the state machine leaves
/// `Placement`, independent of whether the hook itself is still installed
/// (see [`WANT_TEARDOWN`] and the module docs on abandoned gestures).
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether a placement drag is visually in progress — drives the on-screen
/// selection box and [`is_dragging`]. **Not** the same thing as "a button is
/// down we still owe an up for" ([`LEFT_PENDING`]): the two diverge exactly
/// when a drag is cancelled ([`cancel_drag`]) or abandoned (toggled away)
/// while the button is still physically held, which is the case the module
/// docs on abandoned gestures exist to cover.
static DRAGGING: AtomicBool = AtomicBool::new(false);

/// Whether the hook has swallowed a `WM_LBUTTONDOWN` it has not yet seen the
/// balancing `WM_LBUTTONUP` for. Stays `true` across a cancelled or abandoned
/// drag so the eventual physical release is still swallowed rather than
/// leaking to whatever window is under the cursor when the button finally
/// comes up.
static LEFT_PENDING: AtomicBool = AtomicBool::new(false);

/// The same bookkeeping as [`LEFT_PENDING`], for the right button (swallowed
/// during placement so a stray right-click cannot pop a context menu
/// underneath or steal focus).
static RIGHT_PENDING: AtomicBool = AtomicBool::new(false);

/// Set by [`exit`] when it runs while a button is still pending: the hook and
/// cursor override are kept alive past the state transition until the pending
/// release is observed, at which point [`maybe_finish_teardown`] performs the
/// deferred uninstall. Tearing the hook down immediately instead would remove
/// the only thing left that could swallow the outstanding release.
static WANT_TEARDOWN: AtomicBool = AtomicBool::new(false);

/// The drag's anchor and current corner, in physical virtual-desktop pixels —
/// the same space [`crate::overlay`] and `uptake_core` use. `MSLLHOOKSTRUCT.pt`
/// is already in that space for a per-monitor-DPI-aware process, so no
/// conversion happens here.
static START_X: AtomicI32 = AtomicI32::new(0);
static START_Y: AtomicI32 = AtomicI32::new(0);
static CUR_X: AtomicI32 = AtomicI32::new(0);
static CUR_Y: AtomicI32 = AtomicI32::new(0);

/// The app handle the hook callback needs to reach the `AreaStore` and emit.
/// Set on the first [`enter`]; a static because the `extern "system"` callback
/// captures nothing.
static APP: OnceLock<AppHandle> = OnceLock::new();

/// What the current left-button drag *means* — decided once, at button-down,
/// from what was under the cursor.
///
/// Separate from [`DRAGGING`] rather than folded into it because the two answer
/// different questions and are cleared by different things: `DRAGGING` is "is a
/// drag visually in progress" (a cancel clears it immediately, from another
/// thread), while this is the payload that drag needs to commit. Both are
/// cleared together on every path that ends a gesture, and the release handler
/// reads the payload only when `DRAGGING` says the gesture is still live.
static GESTURE: Mutex<Option<Gesture>> = Mutex::new(None);

/// The open area menu (ADR-0013's per-area Layer control), or `None`.
///
/// The menu is **drawn by the WebView and hit-tested here**, from the same
/// rectangles: the overlay is click-through, so a DOM element could never
/// receive the click, and two independent layout calculations would eventually
/// disagree about where a row is. Rust computes each row's rectangle once,
/// sends it to be drawn, and tests clicks against that same value.
static MENU: Mutex<Option<AreaMenu>> = Mutex::new(None);

/// The cursor shape currently pushed to the OS, or `None` when the override is
/// not installed.
///
/// Process-wide rather than a field of [`PumpState`] on purpose. The poll's
/// per-show state is reset when the overlay is *shown*, but the cursor override
/// is installed and torn down on entering and leaving *Placement*, and those are
/// not the same moment: `Living → Placement` re-enters placement without
/// restarting the poll. With the cache on the poll, that transition would leave
/// the poll believing the OS still had the shape from before, and skip the write
/// that would have corrected it.
static APPLIED_CURSOR: Mutex<Option<CursorShape>> = Mutex::new(None);

/// What a left-button drag is doing. Decided at button-down and fixed for the
/// gesture: re-classifying mid-drag would let a move turn into a resize because
/// the cursor happened to cross an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gesture {
    /// Rubber-band a new area out of empty space.
    Create,
    /// Move an existing area, from the bounds it had at button-down.
    Move { id: AreaId, start: Rect },
    /// Resize an existing area from one edge or corner.
    Resize {
        id: AreaId,
        resize: Resize,
        start: Rect,
    },
    /// A press on an area's close control. Dismisses **on release, and only if
    /// the cursor is still on the control** — the press-and-release-on-target
    /// contract every button on every platform honours, and the only way to
    /// change your mind about a gesture with no undo.
    Close { id: AreaId, control: Rect },
    /// A press on a row of the open area menu, resolved the same way.
    MenuItem { index: usize },
    /// A press that has already done its job and must do nothing more on
    /// release — closing an open menu by clicking away from it, or landing on
    /// menu padding between rows. It still exists as a gesture so the release is
    /// swallowed and cannot fall through to whatever is underneath.
    Inert,
}

/// The open per-area menu.
struct AreaMenu {
    /// The area whose menu this is.
    area: AreaId,
    /// The menu's outer rectangle, physical px.
    bounds: Rect,
    /// One entry per row, in draw order.
    items: Vec<MenuEntry>,
    /// The row under the cursor, for the hover highlight.
    hovered: Option<usize>,
}

/// One row of the area menu.
#[derive(Clone, Copy)]
struct MenuEntry {
    rect: Rect,
    action: MenuAction,
    label: &'static str,
    /// Whether this row shows a tick — the area's current tier.
    checked: bool,
}

/// What a menu row does when activated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    /// Pin the area to a stacking tier (ADR-0013).
    SetLayer(Layer),
    /// Remove the area.
    Dismiss,
}

/// The pointer shape placement wants for what is under the cursor.
///
/// A click-through window receives no `WM_SETCURSOR`, so this is not a CSS
/// cursor but a process-wide [`SetSystemCursor`] override, the same mechanism as
/// the crosshair. It is the only affordance an area's handles have: nothing
/// hovers, nothing highlights on the OS side, so the cursor *is* the signal that
/// an edge will resize rather than move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorShape {
    /// Over empty overlay: a drag here creates an area.
    Cross,
    /// Over an area's body: a drag moves it.
    Move,
    /// Over a north or south edge.
    SizeNS,
    /// Over an east or west edge.
    SizeWE,
    /// Over a north-west or south-east corner.
    SizeNWSE,
    /// Over a north-east or south-west corner.
    SizeNESW,
    /// Over a close control or a menu row.
    Hand,
}

impl CursorShape {
    /// The `IDC_*` cursor this shape maps to.
    const fn idc(self) -> *const u16 {
        match self {
            Self::Cross => IDC_CROSS,
            Self::Move => IDC_SIZEALL,
            Self::SizeNS => IDC_SIZENS,
            Self::SizeWE => IDC_SIZEWE,
            Self::SizeNWSE => IDC_SIZENWSE,
            Self::SizeNESW => IDC_SIZENESW,
            Self::Hand => IDC_HAND,
        }
    }

    /// The shape a given grab calls for.
    const fn for_handle(handle: Handle) -> Self {
        match handle {
            Handle::Close => Self::Hand,
            Handle::Body => Self::Move,
            Handle::Resize(resize) => match resize {
                Resize::North | Resize::South => Self::SizeNS,
                Resize::East | Resize::West => Self::SizeWE,
                Resize::NorthWest | Resize::SouthEast => Self::SizeNWSE,
                Resize::NorthEast | Resize::SouthWest => Self::SizeNESW,
            },
        }
    }
}

/// The system cursors overridden during placement. Overriding only `OCR_NORMAL`
/// would leave a text caret or a hand showing whenever the drag crossed a field
/// or a link underneath, so the whole common set is pinned to the crosshair and
/// restored together.
const OVERRIDDEN_CURSORS: [u32; 13] = [
    OCR_NORMAL,
    OCR_IBEAM,
    OCR_WAIT,
    OCR_CROSS,
    OCR_UP,
    OCR_SIZENWSE,
    OCR_SIZENESW,
    OCR_SIZEWE,
    OCR_SIZENS,
    OCR_SIZEALL,
    OCR_NO,
    OCR_HAND,
    OCR_APPSTARTING,
];

/// The live selection rectangle, physical virtual-desktop pixels, or `null`
/// while nothing is being dragged. The frontend converts it to CSS with its own
/// origin and `devicePixelRatio` (ADR-0011), exactly as it does the monitor
/// frames.
#[derive(Serialize, Clone)]
struct SelectionPayload {
    /// `(x, y, width, height)` or `None` to clear the box.
    rect: Option<(i32, i32, u32, u32)>,
}

/// The open area menu as the frontend draws it, or `None`.
#[derive(Serialize, Clone)]
struct MenuPayload {
    menu: Option<MenuView>,
}

/// The menu's geometry, physical px — every rectangle already laid out here, so
/// the frontend positions rows rather than computing them.
#[derive(Serialize, Clone)]
struct MenuView {
    rect: (i32, i32, u32, u32),
    items: Vec<MenuItemView>,
    /// The row under the cursor, for the highlight.
    hovered: Option<usize>,
}

/// One drawn menu row.
#[derive(Serialize, Clone)]
struct MenuItemView {
    rect: (i32, i32, u32, u32),
    label: &'static str,
    /// Whether to show a tick — this is the area's current tier.
    checked: bool,
}

/// Which area the cursor is over, so its chrome can be revealed on hover.
#[derive(Serialize, Clone)]
struct HoverPayload {
    id: Option<u64>,
}

/// Enters placement: install the mouse hook and override the cursor, on the
/// event-loop thread. Idempotent — summoning an already-placing overlay is a
/// no-op for the hook and simply re-asserts the cursor.
pub fn enter(app: &AppHandle) {
    // First entry wins; later ones are the same handle, so ignore the result.
    let _ = APP.set(app.clone());
    if let Err(error) = app.run_on_main_thread(install_on_main_thread) {
        eprintln!("placement: could not schedule hook install on the main thread: {error}");
    }
}

/// Leaves placement: marks it inactive and either uninstalls the hook and
/// restores the cursor immediately, or — if a button it swallowed is still
/// physically held — defers that until the pending release is seen (see the
/// module docs on abandoned gestures). Runs on the event-loop thread.
/// Idempotent.
pub fn exit(app: &AppHandle) {
    if let Err(error) = app.run_on_main_thread(leave_on_main_thread) {
        eprintln!("placement: could not schedule placement exit on the main thread: {error}");
    }
}

/// Restores the system cursors and removes the hook unconditionally — the
/// graceful-shutdown path, called from `RunEvent::Exit`. The process is
/// exiting either way, so an outstanding pending release no longer matters.
/// Safe to call when placement was never entered: reloading the registry
/// cursors over the identical ones is a no-op.
pub fn teardown() {
    teardown_now();
}

/// Chains a system-cursor restore onto the panic hook, so a panic while the
/// crosshair is set does not leave every app showing it. The no-unwrap rule
/// (architecture §5) makes panics rare, not impossible, and this is the one
/// piece of our state that a panic would leak process-wide.
pub fn install_panic_guard() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_system_cursors();
        previous(info);
    }));
}

/// Whether a placement drag is currently in progress — read by
/// [`crate::overlay::escape`] to tell a drag-cancel from backing out of the
/// state.
#[must_use]
pub fn is_dragging() -> bool {
    DRAGGING.load(Ordering::SeqCst)
}

/// Cancels an in-progress drag without creating an area (mid-drag `Esc`). The
/// poll clears the on-screen box on its next tick.
///
/// Deliberately does **not** touch [`LEFT_PENDING`]: the button that started
/// this drag is still physically down, and its eventual release must still be
/// swallowed rather than leaked to the app underneath (see the module docs on
/// abandoned gestures). Clearing only the visual [`DRAGGING`] flag is what
/// makes `WM_LBUTTONUP` discard the release instead of finishing it into an
/// area.
pub fn cancel_drag() {
    DRAGGING.store(false, Ordering::SeqCst);
    *lock(&GESTURE) = None;
}

/// What [`pump`] remembers between ticks, so each emit fires on a change rather
/// than every frame.
#[derive(Default)]
pub struct PumpState {
    /// Whether the previous tick saw a live gesture, so the clearing emit fires
    /// exactly once on the gesture→idle edge.
    was_dragging: bool,
    /// The area the previous tick reported as hovered.
    hovered_area: Option<u64>,
    /// The menu row the previous tick reported as hovered.
    hovered_item: Option<usize>,
}

/// The poll's placement work, run every tick (`click_through`, ~60 Hz).
///
/// **Everything expensive lives here rather than in the hook**, which is the
/// module's central performance rule and not a stylistic one: a `WH_MOUSE_LL`
/// callback that takes too long is silently *removed* by Windows
/// (`LowLevelHooksTimeout`), so the hook writes atomics and this reads them. It
/// also caps the IPC rate at the poll's cadence however fast the mouse reports,
/// and keeps the store lock off the mouse's critical path — hover classification
/// needs the area set, and a 1000 Hz mouse would take that lock 1000 times a
/// second for a result that can only be redrawn 60 times.
///
/// Three jobs: publish the live gesture rectangle, keep the cursor shape
/// matching what is under the pointer, and track the hover highlights.
pub fn pump(app: &AppHandle, state: &mut PumpState) {
    pump_gesture(app, state);
    pump_hover(app, state);
}

/// Publishes the live gesture rectangle, and clears it once when the gesture
/// ends.
fn pump_gesture(app: &AppHandle, state: &mut PumpState) {
    if let Some(rect) = pending_rect() {
        let _ = app.emit(SELECTION_EVENT, SelectionPayload { rect: Some(rect) });
        state.was_dragging = true;
    } else if state.was_dragging {
        let _ = app.emit(SELECTION_EVENT, SelectionPayload { rect: None });
        state.was_dragging = false;
    }
}

/// Classifies what is under the cursor and updates the cursor shape and the
/// hover highlights when they change.
///
/// Skipped entirely while placement is inactive: in `Living` the overlay does
/// not own the pointer, so overriding the system cursor there would change the
/// cursor inside the user's apps.
fn pump_hover(app: &AppHandle, state: &mut PumpState) {
    if !ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let point = Point::new(CUR_X.load(Ordering::SeqCst), CUR_Y.load(Ordering::SeqCst));

    // A menu, while open, owns the pointer above everything under it.
    let menu_item = menu_item_at(point);
    if let Some(menu_hover) = menu_hover_changed(menu_item) {
        state.hovered_item = menu_hover;
        emit_menu(app);
    }

    let menu_open = lock(&MENU).is_some();
    let (shape, hovered_area) = if menu_open {
        (
            if menu_item.is_some() {
                CursorShape::Hand
            } else {
                CursorShape::Cross
            },
            None,
        )
    } else {
        match overlay::area_at(app, point) {
            Some((id, bounds, _)) => (
                interaction::handle_at(bounds, point)
                    .map_or(CursorShape::Cross, CursorShape::for_handle),
                Some(id.get()),
            ),
            None => (CursorShape::Cross, None),
        }
    };

    // A live gesture keeps the shape it started with: the cursor must not flicker
    // between move and resize as the pointer crosses edges mid-drag.
    let shape = match *lock(&GESTURE) {
        Some(gesture) => gesture_cursor(gesture),
        None => shape,
    };
    set_cursor(shape);
    if state.hovered_area != hovered_area {
        state.hovered_area = hovered_area;
        let _ = app.emit(HOVER_EVENT, HoverPayload { id: hovered_area });
    }
}

/// The cursor a gesture in progress holds for its duration.
const fn gesture_cursor(gesture: Gesture) -> CursorShape {
    match gesture {
        Gesture::Create => CursorShape::Cross,
        Gesture::Move { .. } => CursorShape::Move,
        Gesture::Resize { resize, .. } => CursorShape::for_handle(Handle::Resize(resize)),
        Gesture::Close { .. } | Gesture::MenuItem { .. } => CursorShape::Hand,
        Gesture::Inert => CursorShape::Cross,
    }
}

/// Updates the open menu's hovered row, returning the new value only when it
/// changed (so the caller emits once rather than every tick).
fn menu_hover_changed(item: Option<usize>) -> Option<Option<usize>> {
    let mut guard = lock(&MENU);
    let menu = guard.as_mut()?;
    if menu.hovered == item {
        return None;
    }
    menu.hovered = item;
    Some(item)
}

/// The rectangle the current gesture would commit, or `None` when no gesture is
/// live or the gesture draws no rectangle (a button press).
///
/// This is the single place a gesture's geometry is derived, so what the user
/// sees while dragging and what is committed on release cannot disagree.
fn pending_rect() -> Option<(i32, i32, u32, u32)> {
    if !is_dragging() {
        return None;
    }
    let gesture = (*lock(&GESTURE))?;
    let anchor = Point::new(
        START_X.load(Ordering::SeqCst),
        START_Y.load(Ordering::SeqCst),
    );
    let current = Point::new(CUR_X.load(Ordering::SeqCst), CUR_Y.load(Ordering::SeqCst));
    // Saturating: the operands are screen coordinates, so a difference cannot
    // realistically overflow, but a wrapped delta would teleport an area.
    let dx = current.x.saturating_sub(anchor.x);
    let dy = current.y.saturating_sub(anchor.y);
    let rect = match gesture {
        Gesture::Create => Rect::from_corner_points(anchor, current),
        Gesture::Move { start, .. } => interaction::move_by(start, dx, dy),
        Gesture::Resize { start, resize, .. } => interaction::resize_by(start, resize, dx, dy),
        Gesture::Close { .. } | Gesture::MenuItem { .. } | Gesture::Inert => return None,
    };
    Some(overlay::as_tuple(rect))
}

/// Installs the low-level mouse hook (once), marks placement active, and
/// asserts the crosshair. Runs on the event-loop thread — see the module docs
/// on why that is mandatory.
///
/// Clearing [`WANT_TEARDOWN`] here matters for the case where placement is
/// re-entered before a previously deferred teardown fired (see [`exit`] and
/// [`maybe_finish_teardown`]): re-entering cancels the pending uninstall rather
/// than racing it.
fn install_on_main_thread() {
    if HOOK.load(Ordering::SeqCst) == 0 {
        // The current module handle, as the spike used. `dwThreadId = 0` makes
        // the hook global; the callback still runs in-process, on this thread.
        let hmod = unsafe { GetModuleHandleW(ptr::null()) };
        let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0) };
        if hook.is_null() {
            // Not fatal and not silently swallowed either: without the hook,
            // placement cannot capture a drag, but the global hotkey still
            // toggles state (F-13's guaranteed escape), so the user is not
            // stranded. Logged rather than shown, because this failure path is
            // essentially unreachable (SetWindowsHookExW fails only on resource
            // exhaustion or a locked desktop).
            eprintln!("placement: SetWindowsHookExW failed; drag-to-create is unavailable");
        } else {
            HOOK.store(hook as isize, Ordering::SeqCst);
        }
    }
    ACTIVE.store(true, Ordering::SeqCst);
    WANT_TEARDOWN.store(false, Ordering::SeqCst);
    // The resting shape; the poll refines it to a move or resize cursor as soon
    // as the pointer is over an area.
    set_cursor(CursorShape::Cross);
}

/// Marks placement inactive and clears the visual drag, then either tears the
/// hook down immediately or defers it. Runs on the event-loop thread.
///
/// The defer condition is exactly "a button we swallowed the down of has not
/// yet come back up": tearing the hook down anyway would remove the only thing
/// that can still catch that release, turning an abandoned gesture into
/// exactly the leak this module exists to prevent (see the module docs).
fn leave_on_main_thread() {
    ACTIVE.store(false, Ordering::SeqCst);
    DRAGGING.store(false, Ordering::SeqCst);
    *lock(&GESTURE) = None;
    // The menu belongs to Placement: leaving with it still on screen would draw
    // a control over a click-through overlay that nothing could ever click.
    if let Some(app) = APP.get() {
        close_menu(app);
    }
    if LEFT_PENDING.load(Ordering::SeqCst) || RIGHT_PENDING.load(Ordering::SeqCst) {
        WANT_TEARDOWN.store(true, Ordering::SeqCst);
    } else {
        teardown_now();
    }
}

/// Uninstalls the hook (if any) and restores the system cursors. Runs on the
/// event-loop thread: either directly, from [`leave_on_main_thread`] /
/// [`teardown`] (both already marshalled there), or from within the hook
/// callback itself via [`maybe_finish_teardown`] — which already runs on the
/// event-loop thread, since that is a `WH_MOUSE_LL` callback's only thread.
/// `UnhookWindowsHookEx` requires the thread that installed the hook, which
/// all three callers satisfy.
fn teardown_now() {
    let hook = HOOK.swap(0, Ordering::SeqCst);
    if hook != 0 {
        unsafe {
            UnhookWindowsHookEx(hook as HHOOK);
        }
    }
    WANT_TEARDOWN.store(false, Ordering::SeqCst);
    LEFT_PENDING.store(false, Ordering::SeqCst);
    RIGHT_PENDING.store(false, Ordering::SeqCst);
    DRAGGING.store(false, Ordering::SeqCst);
    *lock(&GESTURE) = None;
    restore_system_cursors();
    // The override is gone, so the cache must forget what it believes the OS
    // has — otherwise the next entry into Placement would skip re-applying a
    // shape that is no longer set.
    *lock(&APPLIED_CURSOR) = None;
}

/// Performs the deferred uninstall from [`leave_on_main_thread`] once nothing
/// is pending any more. Called after the hook clears a pending button; a no-op
/// unless [`exit`] actually deferred ([`WANT_TEARDOWN`]) and every pending
/// button has now been released.
fn maybe_finish_teardown() {
    if WANT_TEARDOWN.load(Ordering::SeqCst)
        && !LEFT_PENDING.load(Ordering::SeqCst)
        && !RIGHT_PENDING.load(Ordering::SeqCst)
    {
        teardown_now();
    }
}

/// Sets the system cursor shape, skipping the work when it is already applied.
///
/// The guard matters: [`apply_cursor`] is 13 `CopyIcon` + `SetSystemCursor`
/// pairs, and the poll asks for a shape 60 times a second. Only a change costs
/// anything.
fn set_cursor(shape: CursorShape) {
    let mut applied = lock(&APPLIED_CURSOR);
    if *applied == Some(shape) {
        return;
    }
    apply_cursor(shape);
    *applied = Some(shape);
}

/// Points every common system cursor at `shape`. Each `SetSystemCursor`
/// consumes the handle it is given, so every id gets its own [`CopyIcon`] of the
/// shared cursor — passing the shared handle would have the system destroy a
/// cursor it does not own.
///
/// The whole set is overridden rather than just `OCR_NORMAL` because the pointer
/// travels over the user's apps during placement: leaving `OCR_IBEAM` alone
/// would show a text caret the moment the cursor crossed a text field
/// underneath, which reads as "the overlay lost the pointer".
///
/// Called only from the poll thread and from the two entry points that own the
/// override, so the shape cannot be written by two racers at once.
fn apply_cursor(shape: CursorShape) {
    let cursor: HCURSOR = unsafe { LoadCursorW(ptr::null_mut(), shape.idc()) };
    if cursor.is_null() {
        eprintln!(
            "placement: could not load the {shape:?} cursor; leaving the system cursor as-is"
        );
        return;
    }
    for id in OVERRIDDEN_CURSORS {
        let copy = unsafe { CopyIcon(cursor) };
        if !copy.is_null() {
            // Ignoring the BOOL: a failed override on one id leaves that cursor
            // at its default, which is a cosmetic imperfection during placement,
            // not a correctness problem.
            unsafe {
                SetSystemCursor(copy, id);
            }
        }
    }
}

/// Reloads every system cursor from the registry, undoing [`override_system_cursors`]
/// for all processes. Harmless if no override is active.
/// Deliberately takes no lock: this also runs from the panic hook, and a panic
/// raised while [`APPLIED_CURSOR`] happened to be held would deadlock a process
/// that is already failing. Forgetting the cached shape is [`teardown_now`]'s
/// job instead — on the panic path nothing will read it again anyway.
fn restore_system_cursors() {
    unsafe {
        SystemParametersInfoW(SPI_SETCURSORS, 0, ptr::null_mut(), 0);
    }
}

/// The `WH_MOUSE_LL` callback. Runs on the event-loop thread. Returning
/// `LRESULT(1)` without chaining **swallows** the event, so no window — the app
/// under the cursor included — ever sees the click.
///
/// A panic must not cross this FFI boundary: since Rust 1.81 an unwind out of an
/// `extern "system"` fn aborts the process (architecture §5 — a dead tray app is
/// a lost session), so the work is wrapped in `catch_unwind`.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let swallow = catch_unwind(AssertUnwindSafe(|| handle_mouse(wparam, lparam)))
            .unwrap_or_else(|_| {
                eprintln!("placement: panic in the mouse hook");
                false
            });
        if swallow {
            return 1;
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// The hook's actual logic, split out so it can be `catch_unwind`-wrapped.
/// Returns whether to swallow the event.
///
/// Left button down/up **and** right button down/up are swallowed while
/// placement is [`ACTIVE`]: the left pair is the drag itself; the right pair is
/// swallowed so a stray right-click during placement neither pops a context
/// menu underneath nor steals focus (which would take the `Esc` dismiss path
/// with it). Moves are **not** swallowed — blocking `WM_MOUSEMOVE` in a
/// low-level hook does not stop the cursor moving, and a passing hover under
/// the crosshair is harmless.
///
/// A button-down is only ever swallowed while [`ACTIVE`]; its balancing
/// button-up is swallowed **regardless** of whether placement is still active
/// by then, as long as [`LEFT_PENDING`]/[`RIGHT_PENDING`] says that down was
/// ours — otherwise a drag cancelled or abandoned mid-gesture would leak its
/// eventual release to whatever window ends up under the cursor (see the
/// module docs on abandoned gestures). A release completes into an area only
/// if [`DRAGGING`] is *also* still set — a cancelled or abandoned drag cleared
/// it already, so that release is swallowed and discarded, not finished.
fn handle_mouse(wparam: WPARAM, lparam: LPARAM) -> bool {
    // Safe: for a mouse hook Windows passes an `MSLLHOOKSTRUCT` here, valid for
    // the duration of the call.
    let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
    let (x, y) = (info.pt.x, info.pt.y);
    let point = Point::new(x, y);
    match wparam as u32 {
        WM_LBUTTONDOWN => {
            if !ACTIVE.load(Ordering::SeqCst) {
                // Placement has already been left (most likely: the brief
                // deferred-teardown window from an earlier abandoned drag).
                // A fresh press here belongs to whatever the user is doing
                // now, not to a drag we should start.
                return false;
            }
            START_X.store(x, Ordering::SeqCst);
            START_Y.store(y, Ordering::SeqCst);
            CUR_X.store(x, Ordering::SeqCst);
            CUR_Y.store(y, Ordering::SeqCst);
            // Classified before the lock is taken, not inside the assignment:
            // `classify_press` takes the menu and store locks itself, and
            // nesting those inside this one would be a lock order to reason
            // about rather than one that cannot exist.
            let gesture = classify_press(point);
            *lock(&GESTURE) = Some(gesture);
            DRAGGING.store(true, Ordering::SeqCst);
            LEFT_PENDING.store(true, Ordering::SeqCst);
            true
        }
        WM_MOUSEMOVE => {
            // Recorded unconditionally, not only while dragging: the poll reads
            // this to decide the cursor shape and the hover highlight, both of
            // which exist precisely when no drag is in progress.
            CUR_X.store(x, Ordering::SeqCst);
            CUR_Y.store(y, Ordering::SeqCst);
            false
        }
        WM_LBUTTONUP => {
            if LEFT_PENDING.swap(false, Ordering::SeqCst) {
                if DRAGGING.swap(false, Ordering::SeqCst) {
                    CUR_X.store(x, Ordering::SeqCst);
                    CUR_Y.store(y, Ordering::SeqCst);
                    finish_gesture(point);
                }
                // A cancelled or abandoned gesture clears `DRAGGING` without
                // reaching `finish_gesture`, so the payload is dropped here
                // instead — leaving it would let the next press inherit it.
                *lock(&GESTURE) = None;
                maybe_finish_teardown();
                true
            } else {
                false
            }
        }
        WM_RBUTTONDOWN => {
            if !ACTIVE.load(Ordering::SeqCst) {
                return false;
            }
            RIGHT_PENDING.store(true, Ordering::SeqCst);
            true
        }
        WM_RBUTTONUP if RIGHT_PENDING.swap(false, Ordering::SeqCst) => {
            // Opened on *release*, not on press: a menu that appears under a
            // still-held button is one the same gesture can dismiss by accident.
            if ACTIVE.load(Ordering::SeqCst)
                && let Some(app) = APP.get()
            {
                open_menu(app, point);
            }
            maybe_finish_teardown();
            true
        }
        _ => false,
    }
}

/// Decides what a left-button press at `point` begins.
///
/// Precedence, outermost first: an open menu owns every click while it is up
/// (including one outside it, which closes it); then an area's own controls and
/// edges; then empty overlay, which rubber-bands a new area.
///
/// Takes the store lock, which is safe here and would not be on every mouse
/// *move*: a press happens once per gesture, so this runs at click rate rather
/// than at the mouse's report rate. See [`pump`] for the moves.
fn classify_press(point: Point) -> Gesture {
    if menu_contains(point) {
        return match menu_item_at(point) {
            Some(index) => Gesture::MenuItem { index },
            // Inside the menu but on its padding: a press that does nothing,
            // rather than one that falls through to the area underneath.
            None => Gesture::Inert,
        };
    }
    if let Some(app) = APP.get() {
        // A click anywhere outside an open menu dismisses it, and does not also
        // act on what it landed on — the standard contract, and the one that
        // makes a mis-click cheap.
        if close_menu(app) {
            return Gesture::Inert;
        }
        if let Some((id, bounds, _)) = overlay::area_at(app, point) {
            return match interaction::handle_at(bounds, point) {
                Some(Handle::Close) => Gesture::Close {
                    id,
                    control: interaction::close_control(bounds),
                },
                Some(Handle::Resize(resize)) => Gesture::Resize {
                    id,
                    resize,
                    start: bounds,
                },
                // `handle_at` returns `None` only for a point outside the area,
                // which `area_at` has already excluded — so this is the body.
                Some(Handle::Body) | None => Gesture::Move { id, start: bounds },
            };
        }
    }
    Gesture::Create
}

/// Commits whatever gesture just ended, at the release point.
///
/// Called only when [`DRAGGING`] was still set — a cancelled or abandoned
/// gesture never reaches here, so every path below is a deliberate completion.
fn finish_gesture(release: Point) {
    let Some(app) = APP.get() else {
        return;
    };
    let Some(gesture) = lock(&GESTURE).take() else {
        return;
    };
    let pending = pending_rect_for(gesture, release);
    let changed = match gesture {
        Gesture::Create => {
            let Some((x, y, width, height)) = pending else {
                return;
            };
            // Logged so a placement problem is an observation rather than a
            // guess (the F-15 lesson). The coordinate space itself is settled:
            // hardware testing confirmed `MSLLHOOKSTRUCT.pt` matches
            // `cursor_position` — the space the store and click-through regions
            // use — across every monitor, the 125% primary included.
            #[cfg(debug_assertions)]
            eprintln!("placement: created area {width}x{height} at ({x}, {y})");
            overlay::create_default_area(app, x, y, width, height)
        }
        Gesture::Move { id, .. } | Gesture::Resize { id, .. } => {
            let Some((x, y, width, height)) = pending else {
                return;
            };
            overlay::move_area(app, id, Rect::new(x, y, width, height))
        }
        // A press-and-release contract: the release must land on the control it
        // started on. Sliding off cancels, which is how a user takes back a
        // dismissal they have already begun.
        Gesture::Close { id, control } => {
            control.contains(release) && overlay::dismiss_area(app, id)
        }
        Gesture::MenuItem { index } => return activate_menu_item(app, index, release),
        Gesture::Inert => return,
    };
    if changed && let Err(error) = overlay::emit_areas(app) {
        eprintln!("placement: applied a gesture but could not emit the new set: {error}");
    }
}

/// The rectangle a gesture commits, computed against an explicit release point
/// rather than the polled cursor — the release coordinate is the authoritative
/// one, and the poll may not have ticked since the last mouse move.
fn pending_rect_for(gesture: Gesture, release: Point) -> Option<(i32, i32, u32, u32)> {
    let anchor = Point::new(
        START_X.load(Ordering::SeqCst),
        START_Y.load(Ordering::SeqCst),
    );
    let dx = release.x.saturating_sub(anchor.x);
    let dy = release.y.saturating_sub(anchor.y);
    let rect = match gesture {
        Gesture::Create => Rect::from_corner_points(anchor, release),
        Gesture::Move { start, .. } => interaction::move_by(start, dx, dy),
        Gesture::Resize { start, resize, .. } => interaction::resize_by(start, resize, dx, dy),
        Gesture::Close { .. } | Gesture::MenuItem { .. } | Gesture::Inert => return None,
    };
    Some(overlay::as_tuple(rect))
}

// ---------------------------------------------------------------------------
// The per-area menu (ADR-0013): the control that sets an area's Layer tier.
// ---------------------------------------------------------------------------

/// Opens the area menu for whatever is under `point`, replacing any open menu.
/// Does nothing if the point is over empty overlay — a menu with no area to act
/// on has nothing to offer.
fn open_menu(app: &AppHandle, point: Point) {
    let Some((area, _, layer)) = overlay::area_at(app, point) else {
        close_menu(app);
        return;
    };
    // Anchored to the monitor under the cursor, never to the virtual desktop:
    // desktop-relative chrome can land in a dead zone no cursor can reach (F-13).
    let monitor = overlay::monitor_bounds_at(app, point);
    let spec: [(MenuAction, &'static str); 4] = [
        (MenuAction::SetLayer(Layer::Front), "Always on top"),
        (MenuAction::SetLayer(Layer::Auto), "Auto"),
        (MenuAction::SetLayer(Layer::Back), "Always behind"),
        (MenuAction::Dismiss, "Dismiss"),
    ];
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a fixed four-item menu cannot overflow u32"
    )]
    let bounds = interaction::menu_bounds(point, spec.len() as u32, monitor);
    let items = spec
        .iter()
        .enumerate()
        .map(|(index, (action, label))| MenuEntry {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a fixed four-item menu cannot overflow u32"
            )]
            rect: interaction::menu_item_bounds(bounds, index as u32),
            action: *action,
            label,
            checked: *action == MenuAction::SetLayer(layer),
        })
        .collect();
    *lock(&MENU) = Some(AreaMenu {
        area,
        bounds,
        items,
        hovered: None,
    });
    emit_menu(app);
}

/// Closes any open area menu. Returns whether one was open — which is what lets
/// `Esc` consume the menu instead of backing out of Placement.
pub fn close_menu(app: &AppHandle) -> bool {
    let was_open = lock(&MENU).take().is_some();
    if was_open {
        emit_menu(app);
    }
    was_open
}

/// The index of the menu row containing `point`, if a menu is open at all.
fn menu_item_at(point: Point) -> Option<usize> {
    let guard = lock(&MENU);
    let menu = guard.as_ref()?;
    menu.items.iter().position(|item| item.rect.contains(point))
}

/// Whether `point` is inside the open menu's outer rectangle.
fn menu_contains(point: Point) -> bool {
    lock(&MENU)
        .as_ref()
        .is_some_and(|menu| menu.bounds.contains(point))
}

/// Performs the action of a menu row, if the release landed on the row the press
/// started on — the same press-and-release contract the close control uses.
fn activate_menu_item(app: &AppHandle, index: usize, release: Point) {
    let action = {
        let guard = lock(&MENU);
        let Some(menu) = guard.as_ref() else {
            return;
        };
        let Some(entry) = menu.items.get(index) else {
            return;
        };
        if !entry.rect.contains(release) {
            return;
        }
        (menu.area, entry.action)
    };
    let (area, action) = action;
    close_menu(app);
    let changed = match action {
        MenuAction::SetLayer(layer) => overlay::set_area_layer(app, area, layer),
        MenuAction::Dismiss => overlay::dismiss_area(app, area),
    };
    if changed && let Err(error) = overlay::emit_areas(app) {
        eprintln!("placement: menu action applied but could not emit the new set: {error}");
    }
}

/// Emits the open menu (or its absence) for the frontend to draw.
fn emit_menu(app: &AppHandle) {
    let payload = {
        let guard = lock(&MENU);
        MenuPayload {
            menu: guard.as_ref().map(|menu| MenuView {
                rect: overlay::as_tuple(menu.bounds),
                hovered: menu.hovered,
                items: menu
                    .items
                    .iter()
                    .map(|item| MenuItemView {
                        rect: overlay::as_tuple(item.rect),
                        label: item.label,
                        checked: item.checked,
                    })
                    .collect(),
            }),
        }
    };
    let _ = app.emit(MENU_EVENT, payload);
}

/// Locks a mutex, treating poisoning as recoverable: everything under these
/// locks is plain data that stays valid after a panic, and architecture §5
/// forbids `unwrap`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
