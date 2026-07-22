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
//! content crisply; it *owns the drag* (button-down → move → button-up) and
//! **swallows the button events** so the app underneath receives nothing. The
//! selection rectangle is drawn by the WebView from coordinates this module
//! publishes; a **global crosshair cursor** ([`SetSystemCursor`]) marks the
//! surface as draggable, because a click-through window can set no cursor of its
//! own (no `WM_SETCURSOR` ever reaches it). All three pieces were validated in
//! isolation by the spikes recorded in ADR-0014 before this was written.
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
//! `Placement` ([`exit`]), a graceful shutdown ([`teardown`] from
//! `RunEvent::Exit`), and a panic ([`install_panic_guard`]). What it cannot
//! cover is a **hard kill** (Task Manager) mid-placement, which runs none of our
//! code and leaves the crosshair set until the user's next cursor-scheme reload
//! — a limitation ADR-0014 accepts explicitly.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CopyIcon, HHOOK, IDC_CROSS, LoadCursorW, MSLLHOOKSTRUCT, OCR_APPSTARTING,
    OCR_CROSS, OCR_HAND, OCR_IBEAM, OCR_NO, OCR_NORMAL, OCR_SIZEALL, OCR_SIZENESW, OCR_SIZENS,
    OCR_SIZENWSE, OCR_SIZEWE, OCR_UP, OCR_WAIT, SPI_SETCURSORS, SetSystemCursor, SetWindowsHookExW,
    SystemParametersInfoW, UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use crate::overlay;

/// The Tauri event the frontend listens on for the live selection rectangle.
const SELECTION_EVENT: &str = "placement://selection";

/// The installed hook, as an `HHOOK` cast to `isize`; `0` means "no hook". Only
/// [`install_on_main_thread`] / [`remove_on_main_thread`] touch it, and both run
/// on the event-loop thread, but it is atomic so [`is_dragging`] and friends can
/// read process-wide state without a lock.
static HOOK: AtomicIsize = AtomicIsize::new(0);

/// Whether a placement drag is in progress. Written by the hook (button
/// down/up) and by [`cancel_drag`]; read by the poll and by [`is_dragging`].
static DRAGGING: AtomicBool = AtomicBool::new(false);

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

/// Leaves placement: uninstall the hook and restore the cursor, on the
/// event-loop thread. Idempotent.
pub fn exit(app: &AppHandle) {
    if let Err(error) = app.run_on_main_thread(remove_on_main_thread) {
        eprintln!("placement: could not schedule hook removal on the main thread: {error}");
    }
}

/// Restores the system cursors unconditionally — the graceful-shutdown path,
/// called from `RunEvent::Exit`. Also removes the hook if one is somehow still
/// installed. Safe to call when placement was never entered: reloading the
/// registry cursors over the identical ones is a no-op.
pub fn teardown() {
    remove_on_main_thread();
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
pub fn cancel_drag() {
    DRAGGING.store(false, Ordering::SeqCst);
}

/// Publishes the live selection rectangle to the frontend, and clears it once
/// when a drag ends. Called every poll tick (`click_through`), which paces the
/// emit to ~60 Hz regardless of the mouse's report rate — the hook itself only
/// writes atomics, so a 1000 Hz mouse cannot flood the IPC channel.
///
/// `was_dragging` is the caller's memory across ticks: it is what makes the
/// clearing emit fire exactly once on the drag→idle edge rather than every tick.
pub fn pump_selection(app: &AppHandle, was_dragging: &mut bool) {
    if is_dragging() {
        let _ = app.emit(
            SELECTION_EVENT,
            SelectionPayload {
                rect: Some(current_rect()),
            },
        );
        *was_dragging = true;
    } else if *was_dragging {
        let _ = app.emit(SELECTION_EVENT, SelectionPayload { rect: None });
        *was_dragging = false;
    }
}

/// The current drag rectangle from the anchor and moving corner, normalised so
/// a drag in any direction yields a positive-size rect (the geometry layer owns
/// that normalisation, and the empty-rect rejection at creation).
fn current_rect() -> (i32, i32, u32, u32) {
    use uptake_core::geometry::{Point, Rect};
    let rect = Rect::from_corner_points(
        Point::new(
            START_X.load(Ordering::SeqCst),
            START_Y.load(Ordering::SeqCst),
        ),
        Point::new(CUR_X.load(Ordering::SeqCst), CUR_Y.load(Ordering::SeqCst)),
    );
    (
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    )
}

/// Installs the low-level mouse hook (once) and asserts the crosshair. Runs on
/// the event-loop thread — see the module docs on why that is mandatory.
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
    override_system_cursors();
}

/// Uninstalls the hook (if any) and restores the system cursors. Runs on the
/// event-loop thread; `UnhookWindowsHookEx` must be called from the thread that
/// installed the hook, which [`install_on_main_thread`] guarantees.
fn remove_on_main_thread() {
    let hook = HOOK.swap(0, Ordering::SeqCst);
    if hook != 0 {
        unsafe {
            UnhookWindowsHookEx(hook as HHOOK);
        }
    }
    DRAGGING.store(false, Ordering::SeqCst);
    restore_system_cursors();
}

/// Points every common system cursor at the crosshair. Each `SetSystemCursor`
/// consumes the handle it is given, so every id gets its own [`CopyIcon`] of the
/// shared `IDC_CROSS` — passing the shared handle would have the system destroy
/// a cursor it does not own.
fn override_system_cursors() {
    let cross = unsafe { LoadCursorW(ptr::null_mut(), IDC_CROSS) };
    if cross.is_null() {
        eprintln!(
            "placement: could not load the crosshair cursor; leaving the system cursor as-is"
        );
        return;
    }
    for id in OVERRIDDEN_CURSORS {
        let copy = unsafe { CopyIcon(cross) };
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
/// Left button down/up **and** right button down/up are swallowed: the left
/// pair is the drag itself; the right pair is swallowed so a stray right-click
/// during placement neither pops a context menu underneath nor steals focus
/// (which would take the `Esc` dismiss path with it). Moves are **not**
/// swallowed — blocking `WM_MOUSEMOVE` in a low-level hook does not stop the
/// cursor moving, and a passing hover under the crosshair is harmless.
fn handle_mouse(wparam: WPARAM, lparam: LPARAM) -> bool {
    // Safe: for a mouse hook Windows passes an `MSLLHOOKSTRUCT` here, valid for
    // the duration of the call.
    let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
    let (x, y) = (info.pt.x, info.pt.y);
    match wparam as u32 {
        WM_LBUTTONDOWN => {
            START_X.store(x, Ordering::SeqCst);
            START_Y.store(y, Ordering::SeqCst);
            CUR_X.store(x, Ordering::SeqCst);
            CUR_Y.store(y, Ordering::SeqCst);
            DRAGGING.store(true, Ordering::SeqCst);
            true
        }
        WM_MOUSEMOVE => {
            if is_dragging() {
                CUR_X.store(x, Ordering::SeqCst);
                CUR_Y.store(y, Ordering::SeqCst);
            }
            false
        }
        WM_LBUTTONUP => {
            if DRAGGING.swap(false, Ordering::SeqCst) {
                CUR_X.store(x, Ordering::SeqCst);
                CUR_Y.store(y, Ordering::SeqCst);
                finish_drag();
                true
            } else {
                false
            }
        }
        WM_RBUTTONDOWN | WM_RBUTTONUP => true,
        _ => false,
    }
}

/// Turns a completed drag into an area. A drag that never moved is a zero-size
/// rectangle, which `AreaStore::create` rejects — so an ordinary click in
/// placement creates nothing, by construction rather than by a special case.
fn finish_drag() {
    let Some(app) = APP.get() else {
        return;
    };
    let (x, y, width, height) = current_rect();
    // The area's physical bounds, logged so a placement problem is an
    // observation rather than a guess (the F-15 lesson). The coordinate space
    // itself is settled: hardware testing confirmed `MSLLHOOKSTRUCT.pt` matches
    // `cursor_position` — the space the store and click-through regions use —
    // across every monitor, the 125% primary included.
    #[cfg(debug_assertions)]
    eprintln!("placement: created area {width}x{height} at ({x}, {y})");
    if overlay::create_default_area(app, x, y, width, height)
        && let Err(error) = overlay::emit_areas(app)
    {
        eprintln!("placement: created an area but could not emit the new set: {error}");
    }
}
