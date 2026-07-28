//! Mouse input for the overlay — placement gestures *and* Living-area routing
//! (roadmap tasks 1.6/1.6c,
//! [ADR-0014](../../../Projects/UP-TAKE/DECISIONS/ADR-0014-capture-and-render-over-live-content.md),
//! [ADR-0016](../../../Projects/UP-TAKE/DECISIONS/ADR-0016-living-input-via-the-global-hook.md)).
//!
//! The overlay window is **never interactive** (ADR-0016, hardening ADR-0014's
//! click-through-whenever-visible rule into an unconditional one): a transparent
//! window that ignores the cursor is the only overlay state that does not
//! degrade hardware-accelerated video underneath it. Every mouse event the
//! overlay acts on therefore arrives through a **global low-level mouse hook**
//! (`WH_MOUSE_LL`) instead, installed for as long as the overlay is visible.
//! What the hook does with a button press depends on the [`Mode`]:
//!
//! - **`Placement`** — the hook owns the whole gesture (button-down → move →
//!   button-up) and swallows both buttons unconditionally: drags create, move
//!   and resize areas, and a **global cursor override** ([`SetSystemCursor`])
//!   supplies the pointer shape, because a click-through window can set no
//!   cursor of its own (no `WM_SETCURSOR` ever reaches it).
//! - **`Living`** — the user's apps own the pointer, and the hook takes only
//!   what the area model assigns to areas: a press on the topmost *interactive*
//!   area (`AreaStore::hit_test` — pass-through areas are invisible to input,
//!   V-7) is swallowed and acted on (left raises the area per §3.2a recency,
//!   right opens its menu); every other press is passed through untouched. No
//!   cursor override — the pointer belongs to whatever is underneath.
//! - **`Hidden`** — the hook is torn down (subject to the pending-button
//!   deferral below) and everything here is inert.
//!
//! The rectangles are drawn by the WebView from coordinates this module
//! publishes. All the Win32 pieces were validated in isolation by the spikes
//! recorded in ADR-0014 before this was written.
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
//! runs none of our code — a limitation ADR-0014 accepts explicitly. The *next*
//! launch repairs it, though: [`clear_cursor_residue`] runs at startup, and
//! [`snapshot_cursor`] reloads the registry before capturing the set it restores
//! from. Without that second part the residue would be worse than cosmetic — a
//! process starting up under a leftover crosshair would take the crosshair for
//! the user's real cursor and could then never change shape again.
//!
//! # A low-level hook can be removed without being told
//!
//! Windows drops a `WH_MOUSE_LL` hook whose callback overruns
//! `LowLevelHooksTimeout`, and starves one in a medium-integrity process while a
//! higher-integrity window holds the foreground (UIPI — F-25). Neither is
//! reported: [`HOOK`] still holds a handle, so nothing here would notice, and
//! `Placement` would sit on screen with no working input for the rest of the
//! session. [`pump_hook_health`] watches for it — the cursor moving while the
//! hook counts no events — and reinstalls. That does not defeat UIPI and does
//! not try to; it restores the overlay once the elevated window is no longer in
//! front, instead of leaving "press the hotkey twice" as the only way back.
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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uptake_core::area::{AreaId, AreaType, Input, Layer};
use uptake_core::geometry::{Point, Rect};
use uptake_core::interaction::{self, Handle, Resize};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LBUTTON, VK_MENU, VK_RBUTTON,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CopyIcon, GA_ROOT, GW_HWNDPREV, GetAncestor, GetWindow, HCURSOR, HHOOK,
    IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE,
    IDC_SIZEWE, LoadCursorW, MSLLHOOKSTRUCT, OCR_APPSTARTING, OCR_CROSS, OCR_HAND, OCR_IBEAM,
    OCR_NO, OCR_NORMAL, OCR_SIZEALL, OCR_SIZENESW, OCR_SIZENS, OCR_SIZENWSE, OCR_SIZEWE, OCR_UP,
    OCR_WAIT, SPI_SETCURSORS, SetSystemCursor, SetWindowsHookExW, SystemParametersInfoW,
    UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WindowFromPoint,
};

use crate::overlay;
use crate::precapture;

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

/// Which overlay state the hook is serving (ADR-0016). Decides what a fresh
/// button event means: a Placement gesture, a Living routing decision, or
/// nothing at all.
///
/// Kept as its own tri-state rather than read from the overlay's state mutex
/// because the hook callback consults it on every button event and must not
/// take a lock to do so. Set only on the event-loop thread by the mode
/// transitions ([`enter`], [`enter_living`], [`exit`]), independent of whether
/// the hook itself is still installed (see [`WANT_TEARDOWN`] and the module
/// docs on abandoned gestures).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The overlay is hidden; the hook is torn down or about to be.
    Hidden = 0,
    /// Areas float, apps have input; the hook routes per-area (ADR-0016).
    Living = 1,
    /// UP-TAKE owns the pointer; the hook owns whole gestures.
    Placement = 2,
}

/// The current [`Mode`], as its discriminant.
static MODE: AtomicU8 = AtomicU8::new(Mode::Hidden as u8);

/// Reads the current [`Mode`].
fn mode() -> Mode {
    match MODE.load(Ordering::SeqCst) {
        2 => Mode::Placement,
        1 => Mode::Living,
        _ => Mode::Hidden,
    }
}

/// Sets the current [`Mode`]. Event-loop thread only.
fn set_mode(mode: Mode) {
    MODE.store(mode as u8, Ordering::SeqCst);
}

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

/// How many events the hook has processed. Only ever compared against its own
/// previous value — see [`pump_hook_health`], which uses it to notice that
/// Windows has silently removed the hook.
static HOOK_EVENTS: AtomicU64 = AtomicU64::new(0);

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

/// The [`AreaType`] armed for the **next** drag, or `None` meaning
/// [`AreaType::Default`] (ADR-0018 §1).
///
/// **Transience is the whole point.** ADR-0009 §3 deleted global mode state by
/// name, and this is mode state — bought back only because it cannot outlive one
/// drag, so the "which mode am I in?" problem has no room to occur. It is
/// cleared when an area is created, when a not-mid-drag `Esc` disarms it, and on
/// every path that leaves Placement ([`enter_living_on_main_thread`],
/// [`exit_on_main_thread`], [`teardown_now`]). It is never written to disk and
/// never restored.
///
/// A `Mutex<Option<_>>` rather than an atomic, matching [`GESTURE`]: both are
/// read from the hook callback, and the pair should look alike.
static ARMED: Mutex<Option<AreaType>> = Mutex::new(None);

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
    /// Set whether the area takes input or lets it fall through (V-7). The
    /// menu row is a toggle, so the action carries the value the row would
    /// switch *to*.
    SetInput(Input),
    /// Remove the area.
    Dismiss,
    /// Capture the area and publish it to the clipboard alone (task 1.9,
    /// `Default` areas only — a typed capture area is 1.9b's).
    Copy,
    /// Capture the area and write it to `Pictures\UP-TAKE\` (task 1.9, same
    /// scope as `Copy`). A separate, explicit action — does not also copy.
    SaveToFile,
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
    /// Over a close control, or over a menu row **in PLACEMENT**.
    ///
    /// The mode qualifier is real, not pedantry: `pump_hover`'s LIVING branch
    /// resolves nothing while a menu is open, so a menu row there keeps the
    /// ordinary arrow. That matches how native Windows menus behave, and the row
    /// still highlights, so it is a deliberate difference rather than a gap — but
    /// this doc used to claim otherwise for both modes.
    Hand,
    /// **The user's own arrow** — not a shape UP-TAKE ever wants to *show*, but
    /// the one it must be able to put back.
    ///
    /// [ADR-0025](../../../Projects/UP-TAKE/DECISIONS/ADR-0025-living-cursor-via-a-narrow-override.md)
    /// needs this. LIVING overrides `OCR_NORMAL` alone and undoes it by
    /// overriding again with the genuine arrow, because the alternative —
    /// `SPI_SETCURSORS` — measures 7.9 ms and broadcasts `WM_SETTINGCHANGE`
    /// desktop-wide, which is unaffordable on a per-hover path. Nothing else in
    /// this enum is a "restore" value; this one exists only to be restored to.
    Arrow,
}

impl CursorShape {
    /// This shape's slot in [`CURSOR_SNAPSHOT`]. Kept in step with
    /// [`ALL_SHAPES`], which the snapshot iterates in the same order.
    const fn index(self) -> usize {
        match self {
            Self::Cross => 0,
            Self::Move => 1,
            Self::SizeNS => 2,
            Self::SizeWE => 3,
            Self::SizeNWSE => 4,
            Self::SizeNESW => 5,
            Self::Hand => 6,
            Self::Arrow => 7,
        }
    }

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
            Self::Arrow => IDC_ARROW,
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
    /// The id of the area this gesture is moving or resizing, so the frontend
    /// can draw it as the *source* of the drag instead of as a second area
    /// sitting where the first one used to be. `None` while creating.
    source: Option<u64>,
    /// A latency probe: nanoseconds since [`EPOCH`], set on sampled frames only.
    /// The frontend echoes it back once the frame has painted, and
    /// [`record_latency`] closes the loop. `None` on unsampled frames.
    probe: Option<u64>,
}

/// The process's monotonic zero, so a probe can round-trip through the WebView
/// and come back comparable.
///
/// **One clock, deliberately.** Rust's `Instant` and JS's `performance.now()`
/// have unrelated epochs, and reconciling them is its own source of error — so
/// the frontend never reads the probe's value, it only hands the same number
/// back. Everything is measured here.
static EPOCH: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// Sample one frame in this many while a gesture runs.
///
/// Every frame would add an IPC call per frame to the exact path being measured,
/// so the measurement would report its own weight. At ~220 Hz this still yields
/// roughly 27 samples a second.
const LATENCY_SAMPLE_EVERY: u64 = 8;

/// How many selection frames have been emitted, for the sampling stride.
static SELECTION_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Whether to stamp probes at all this run.
///
/// Split on `cfg` rather than tested with `cfg!`, because `dev_harness` does not
/// exist in a release build — a `cfg!` test compiles both arms and would fail to
/// resolve the path.
#[cfg(debug_assertions)]
fn probe_enabled() -> bool {
    crate::dev_harness::pacing_enabled()
}

/// Release builds never stamp a probe, so nothing echoes and
/// [`record_latency`] is never reached.
#[cfg(not(debug_assertions))]
fn probe_enabled() -> bool {
    false
}

/// What the round trip cost, accumulated across one gesture.
struct LatencySamples {
    count: u32,
    total_nanos: u128,
    max_nanos: u64,
}

static LATENCY: Mutex<LatencySamples> = Mutex::new(LatencySamples {
    count: 0,
    total_nanos: 0,
    max_nanos: 0,
});

/// Records a completed probe: emit → IPC → reactivity → layout → painted.
///
/// # What this measures, and what it does not
///
/// It covers the part of the pipeline **we control**. It excludes the
/// mouse-to-hook latency ahead of it (the hook only writes atomics, so that is
/// sub-millisecond) and DWM's final composite to the panel behind it. So it is a
/// **lower bound** on what the eye sees — the right tool for telling our own
/// costs apart, which is the open question, and not a claim about total
/// input-to-photon latency.
pub fn record_latency(probe_nanos: u64) {
    let now = u64::try_from(EPOCH.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let elapsed = now.saturating_sub(probe_nanos);
    let mut guard = lock(&LATENCY);
    guard.count = guard.count.saturating_add(1);
    guard.total_nanos = guard.total_nanos.saturating_add(u128::from(elapsed));
    guard.max_nanos = guard.max_nanos.max(elapsed);
}

/// Drains the accumulated samples as `(count, mean ms, worst ms)`.
///
/// Debug-only: its one caller is the poll's gesture report, which does not exist
/// in a release build. [`record_latency`] stays compiled in both, because the IPC
/// command that reaches it is registered unconditionally — in release nothing
/// stamps a probe, so nothing echoes and it is never called.
#[cfg(debug_assertions)]
pub fn take_latency_summary() -> Option<(u32, f64, f64)> {
    let mut guard = lock(&LATENCY);
    if guard.count == 0 {
        return None;
    }
    let count = guard.count;
    #[expect(
        clippy::cast_precision_loss,
        reason = "milliseconds for a log line; a nanosecond total cannot reach \
                  f64's exact-integer limit within a gesture"
    )]
    let mean = (guard.total_nanos as f64 / f64::from(count)) / 1_000_000.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "same: a single sample's nanoseconds are far below 2^53"
    )]
    let worst = guard.max_nanos as f64 / 1_000_000.0;
    *guard = LatencySamples {
        count: 0,
        total_nanos: 0,
        max_nanos: 0,
    };
    Some((count, mean, worst))
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
    if let Err(error) = app.run_on_main_thread(enter_placement_on_main_thread) {
        eprintln!("placement: could not schedule hook install on the main thread: {error}");
    }
}

/// Enters Living: the hook stays (or gets) installed for per-area routing
/// (ADR-0016), the cursor override is dropped — the apps own the pointer — and
/// any half-done placement gesture or open menu is cleared. Runs on the
/// event-loop thread. Idempotent.
pub fn enter_living(app: &AppHandle) {
    let _ = APP.set(app.clone());
    if let Err(error) = app.run_on_main_thread(enter_living_on_main_thread) {
        eprintln!("placement: could not schedule Living entry on the main thread: {error}");
    }
}

/// Leaves every visible state: marks the hook's mode `Hidden` and either
/// uninstalls the hook and restores the cursor immediately, or — if a button it
/// swallowed is still physically held — defers that until the pending release
/// is seen (see the module docs on abandoned gestures). Runs on the event-loop
/// thread. Idempotent.
pub fn exit(app: &AppHandle) {
    if let Err(error) = app.run_on_main_thread(exit_on_main_thread) {
        eprintln!("placement: could not schedule placement exit on the main thread: {error}");
    }
}

/// Clears any cursor override left installed by an earlier process, and is safe
/// when there is none — reloading the registry cursors over identical ones is a
/// no-op. Called once at startup; see the note on [`snapshot_cursor`] for why
/// this also protects the snapshot's correctness.
pub fn clear_cursor_residue() {
    restore_system_cursors();
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
    // The pre-capture this drag may have started is now waste — ADR-0022 calls
    // it "wasted but harmless", which is true of the *work* and not of the
    // memory: a held 4K frame is 33 MB, and leaving it would keep that resident
    // until the next drag happened to replace it.
    precapture::discard();
}

/// Arms `kind` for the next drag (ADR-0018 §1), replacing anything already
/// armed — pressing a second direct key changes your mind rather than erroring.
pub fn arm(kind: AreaType) {
    *lock(&ARMED) = Some(kind);
}

/// The type armed for the next drag, or `None` for [`AreaType::Default`].
///
/// Read by [`crate::overlay::escape`] for the ladder's middle rung, by the
/// release handler to decide what to create, and by the poll to tell the
/// indicator what the next drag will make.
#[must_use]
pub fn armed() -> Option<AreaType> {
    *lock(&ARMED)
}

/// Clears the arming, so the next drag makes a [`AreaType::Default`] area.
///
/// Called on the `Esc` ladder's middle rung and after a create. Idempotent —
/// disarming when nothing is armed is not an error, it is the common case.
pub fn disarm() {
    *lock(&ARMED) = None;
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
    /// The real cursor position the previous tick read, for the hook health
    /// check.
    last_cursor: Option<Point>,
    /// [`HOOK_EVENTS`] as of the previous tick.
    last_events: u64,
    /// Consecutive ticks in which the cursor moved but the hook saw nothing.
    silent_ticks: u32,
    /// Ticks left before the health check may act again, so a reinstall is not
    /// retried every frame while an elevated window holds the foreground.
    reinstall_cooldown: u32,
    /// The monitor the previous tick reported as holding the cursor.
    ///
    /// **Doubly optional on purpose.** The outer `None` means "nothing reported
    /// yet this Placement" and is cleared on every tick outside Placement, so
    /// re-entering always re-emits; the inner `None` is the real answer for a
    /// cursor in a dead zone between mismatched monitors. Collapsing the two
    /// would leave a re-entry silent whenever the cursor had not moved to a
    /// different monitor meanwhile — the common case, since the user usually
    /// re-summons where they left off.
    active_monitor: Option<Option<usize>>,
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
    pump_hook_health(app, state);
    pump_gesture(app, state);
    pump_precapture();
    pump_hover(app, state);
}

/// Keeps the held frame fresh while a capturing drag is being drawn (task 1.9c).
///
/// # Why the poll drives this and not the hook
///
/// The refresh has to happen *during* the drag, and the only thing that runs
/// during a drag is this poll — the `WH_MOUSE_LL` callback sees discrete events,
/// and the one it would key off (`WM_MOUSEMOVE`) stops firing the moment the
/// user holds the cursor still, which is exactly when a frame is quietly ageing
/// toward a fallback.
///
/// Cheap enough to sit on a 221 Hz path: two atomic loads and a lock in the
/// common case, and [`precapture::refresh`] itself does nothing until the frame
/// is [`REFRESH_AFTER`](precapture) old. Guarded on the gesture so a move, a
/// resize or a menu drag never starts a capture.
fn pump_precapture() {
    if !DRAGGING.load(Ordering::SeqCst) {
        return;
    }
    if !matches!(*lock(&GESTURE), Some(Gesture::Create)) {
        return;
    }
    if !armed().is_some_and(captures_on_create) {
        return;
    }
    precapture::refresh();
}

/// Ticks the health check may skip after a reinstall (~1 s at the poll's
/// cadence), so a foreground elevated window cannot make this spin.
const REINSTALL_COOLDOWN_TICKS: u32 = 60;

/// Consecutive silent ticks before the hook is presumed dead. Three is ~50 ms —
/// long enough that a single dropped frame is not a diagnosis.
const SILENT_TICKS_BEFORE_REINSTALL: u32 = 3;

/// Notices that the mouse hook has stopped delivering events and reinstalls it.
///
/// **Windows removes a low-level hook without telling anyone.** It does so when
/// a callback overruns `LowLevelHooksTimeout`, and a hook in a medium-integrity
/// process is starved while a higher-integrity window holds the foreground
/// (UIPI — F-25, and the reason interacting with Task Manager used to leave the
/// overlay inert until the user toggled Placement off and on again). In both
/// cases [`HOOK`] still holds a handle, so [`install_on_main_thread`] believes
/// there is nothing to do and the overlay stays in Placement with no working
/// input for the rest of the session.
///
/// The detection needs no timers: if the real cursor has moved since the last
/// tick and the hook has not counted a single event, the hook is not receiving
/// input. Comparing positions alone would not do — during fast motion the
/// hook's last reported point legitimately lags the polled one — which is why
/// this compares the *event counter* against its own previous value.
///
/// Reinstalling cannot defeat UIPI, and does not try to: while Task Manager is
/// focused, input still belongs to it. What it restores is the state
/// *afterwards*, so the overlay works again the moment the elevated window is
/// no longer in front, instead of needing the hotkey twice.
fn pump_hook_health(app: &AppHandle, state: &mut PumpState) {
    // Guarded by mode, not by "is the hook installed": the whole point is to
    // notice a hook that *should* be installed and silently is not. Living
    // needs this exactly as much as Placement — a dead hook there means every
    // interactive area silently stops taking input (ADR-0016).
    if mode() == Mode::Hidden {
        state.silent_ticks = 0;
        return;
    }
    let events = HOOK_EVENTS.load(Ordering::Relaxed);
    let cursor = real_cursor(app);
    if state.reinstall_cooldown > 0 {
        // Keep the baselines current while waiting. Skipping them would leave
        // the next comparison reading against values a whole second old, which
        // is the sort of stale-baseline bug that makes a health check quietly
        // stop detecting anything.
        state.reinstall_cooldown -= 1;
        state.last_cursor = cursor;
        state.last_events = events;
        state.silent_ticks = 0;
        return;
    }
    let moved = matches!((cursor, state.last_cursor), (Some(now), Some(before)) if now != before);
    state.last_cursor = cursor;

    if moved && events == state.last_events {
        state.silent_ticks += 1;
    } else {
        state.silent_ticks = 0;
    }
    state.last_events = events;

    if state.silent_ticks >= SILENT_TICKS_BEFORE_REINSTALL {
        state.silent_ticks = 0;
        state.reinstall_cooldown = REINSTALL_COOLDOWN_TICKS;
        eprintln!("placement: mouse hook stopped receiving input; reinstalling");
        if let Err(error) = app.run_on_main_thread(reinstall_on_main_thread) {
            // The main thread is not servicing its queue — which is itself the
            // reason the hook died, if something has put it in a modal loop.
            // Nothing here can fix that from another thread: installing and
            // removing a low-level hook are both thread-affine to the event
            // loop.
            eprintln!("placement: could not schedule a hook reinstall: {error}");
        }
    }
}

/// The cursor position as the OS reports it, independent of the hook.
fn real_cursor(app: &AppHandle) -> Option<Point> {
    let position = overlay::overlay_window(app).ok()?.cursor_position().ok()?;
    Point::from_physical_f64(position.x, position.y)
}

/// Replaces a hook that is no longer delivering events. Runs on the event-loop
/// thread, the only one that may install or remove one.
///
/// The live gesture itself ([`DRAGGING`]/[`GESTURE`]) is discarded rather than
/// carried over: a hook that missed events may have missed the cursor moving
/// too, so the in-progress rectangle can no longer be trusted. The abandoned
/// gesture is then treated the same way [`cancel_drag`] treats one — its
/// eventual release is still swallowed (see below), it just commits nothing.
///
/// [`LEFT_PENDING`]/[`RIGHT_PENDING`] are **not** blindly cleared, though:
/// neither "clear" nor "keep" is safe on its own here. Clearing would leak the
/// pending button's release the moment the button is still physically held —
/// the down was genuinely swallowed while the hook was alive, and nothing else
/// will ever swallow the matching up (the reinstall-time version of the leak
/// the module docs describe for cancel/toggle-away). Keeping it regardless
/// risks the opposite: swallowing an unrelated future release if the button
/// cycled up and back down again while the hook was dead. [`GetAsyncKeyState`]
/// resolves the ambiguity the same way [`snapping_suppressed`] reads `Alt` —
/// by trusting what a button is doing right now rather than assuming what it
/// was doing before the hook died.
fn reinstall_on_main_thread() {
    let hook = HOOK.swap(0, Ordering::SeqCst);
    if hook != 0 {
        unsafe {
            UnhookWindowsHookEx(hook as HHOOK);
        }
    }
    LEFT_PENDING.store(vk_is_down(i32::from(VK_LBUTTON)), Ordering::SeqCst);
    RIGHT_PENDING.store(vk_is_down(i32::from(VK_RBUTTON)), Ordering::SeqCst);
    WANT_TEARDOWN.store(false, Ordering::SeqCst);
    DRAGGING.store(false, Ordering::SeqCst);
    *lock(&GESTURE) = None;
    // Only the hook is re-created — the mode and the cursor override are
    // whatever they already were. Re-entering the full Placement path here
    // would stomp a hover-refined cursor shape back to the crosshair, and in
    // Living would wrongly assert one.
    if mode() != Mode::Hidden {
        ensure_hook();
        eprintln!(
            "placement: mouse hook reinstalled (installed: {})",
            HOOK.load(Ordering::SeqCst) != 0
        );
    }
}

/// Publishes the live gesture rectangle, and clears it once when the gesture
/// ends.
fn pump_gesture(app: &AppHandle, state: &mut PumpState) {
    if let Some(rect) = pending_rect() {
        let frame = SELECTION_FRAMES.fetch_add(1, Ordering::Relaxed);
        // Debug builds only, and within those only when `UPTAKE_DEV_PACING` asks
        // for it. The probe costs an extra IPC round trip per sampled frame —
        // ~27 a second at the measured 221 Hz — which is load added to the exact
        // path it measures, so it is not something to leave running in every dev
        // build either, let alone ship to users who will never read the number.
        let probe = probe_enabled()
            .then(|| {
                frame
                    .is_multiple_of(LATENCY_SAMPLE_EVERY)
                    .then(|| u64::try_from(EPOCH.elapsed().as_nanos()).unwrap_or(u64::MAX))
            })
            .flatten();
        let _ = app.emit(
            SELECTION_EVENT,
            SelectionPayload {
                rect: Some(rect),
                source: dragged_area(),
                probe,
            },
        );
        state.was_dragging = true;
    } else if state.was_dragging {
        // Clearing both together is what restores the source area to its normal
        // appearance, so a cancelled or interrupted drag needs no separate undo
        // path — the styling was never stored, only derived from the live
        // gesture.
        let _ = app.emit(
            SELECTION_EVENT,
            SelectionPayload {
                rect: None,
                source: None,
                probe: None,
            },
        );
        state.was_dragging = false;
    }
}

/// Classifies what is under the cursor and updates the cursor shape and the
/// hover highlights when they change.
///
/// The menu hover highlight runs in every visible mode — the area menu is
/// reachable from `Living` too (ADR-0016), and a menu whose rows never light up
/// reads as a picture rather than a control. The cursor override and the area
/// hover chrome remain `Placement`-only: in `Living` the overlay does not own
/// the pointer, so overriding the system cursor there would change the cursor
/// inside the user's apps, and hover chrome would advertise gestures (move,
/// resize) that only `Placement` offers.
fn pump_hover(app: &AppHandle, state: &mut PumpState) {
    if mode() == Mode::Hidden {
        return;
    }
    let point = Point::new(CUR_X.load(Ordering::SeqCst), CUR_Y.load(Ordering::SeqCst));

    // A menu, while open, owns the pointer above everything under it.
    let menu_item = menu_item_at(point);
    if let Some(menu_hover) = menu_hover_changed(menu_item) {
        state.hovered_item = menu_hover;
        emit_menu(app);
    }
    let placing = mode() == Mode::Placement;
    if !placing {
        // Forget the reported monitor so the next entry into Placement re-emits
        // it — see `PumpState::active_monitor`.
        state.active_monitor = None;
        // Living needs hover chrome (task 1.17(a)): areas are grabbable here now,
        // and a handle the user cannot see is a handle they will not reach for.
        // Resolved through the same call `living_lbutton_down` acts on, so chrome
        // never appears on an area the press would then ignore — which since
        // task 1.17(b) includes a pass-through area's chrome but not its body.
        let grabbed = lock(&MENU)
            .is_none()
            .then(|| overlay::interactive_area_handle_at(app, point))
            .flatten();
        let hovered_area = grabbed.map(|(id, _, _)| id.get());
        // The cursor *is* the affordance (ADR-0025). A live gesture holds its
        // shape for the duration, matching Placement: it must not flicker between
        // move and resize as the pointer crosses an edge mid-drag.
        //
        // `None` hands the user's own arrow back — this is an `OCR_NORMAL`-only
        // override, so nothing else in their cursor table is touched. See
        // `set_living_cursor` for why the restore is not `SPI_SETCURSORS`.
        set_living_cursor(match *lock(&GESTURE) {
            Some(gesture) => Some(gesture_cursor(gesture)),
            None => grabbed.map(|(_, _, handle)| CursorShape::for_handle(handle)),
        });
        if state.hovered_area != hovered_area {
            state.hovered_area = hovered_area;
            let _ = app.emit(HOVER_EVENT, HoverPayload { id: hovered_area });
        }
        return;
    }

    // Which monitor the per-monitor chrome belongs on (F-13). The armed-type
    // badge follows the cursor: showing it on every monitor at once — as the
    // first cut did — reads as "every screen is armed" and buries the one fact
    // the indicator exists to convey, which ADR-0018 §3 makes load-bearing.
    let active_monitor = overlay::monitor_index_at(point);
    if state.active_monitor != Some(active_monitor) {
        state.active_monitor = Some(active_monitor);
        overlay::emit_active_monitor(app, active_monitor);
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
        match overlay::area_handle_at(app, point) {
            Some((id, _, handle)) => (CursorShape::for_handle(handle), Some(id.get())),
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
    let current = Point::new(CUR_X.load(Ordering::SeqCst), CUR_Y.load(Ordering::SeqCst));
    gesture_rect(gesture, current)
}

/// The area a live gesture is moving or resizing, so the frontend can show it as
/// the *source* of the drag rather than as a second area sitting where the first
/// one used to be.
fn dragged_area() -> Option<u64> {
    if !is_dragging() {
        return None;
    }
    match (*lock(&GESTURE))? {
        Gesture::Move { id, .. } | Gesture::Resize { id, .. } => Some(id.get()),
        Gesture::Create | Gesture::Close { .. } | Gesture::MenuItem { .. } | Gesture::Inert => None,
    }
}

/// Installs the low-level mouse hook if it is not already installed. Runs on
/// the event-loop thread — see the module docs on why that is mandatory.
fn ensure_hook() {
    if HOOK.load(Ordering::SeqCst) == 0 {
        // The current module handle, as the spike used. `dwThreadId = 0` makes
        // the hook global; the callback still runs in-process, on this thread.
        let hmod = unsafe { GetModuleHandleW(ptr::null()) };
        let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0) };
        if hook.is_null() {
            // Not fatal and not silently swallowed either: without the hook,
            // no area can take input, but the global hotkey still toggles
            // state (F-13's guaranteed escape), so the user is not stranded.
            // Logged rather than shown, because this failure path is
            // essentially unreachable (SetWindowsHookExW fails only on resource
            // exhaustion or a locked desktop).
            eprintln!("placement: SetWindowsHookExW failed; area input is unavailable");
        } else {
            HOOK.store(hook as isize, Ordering::SeqCst);
        }
    }
}

/// Enters `Placement` mode: hook installed, crosshair asserted. Runs on the
/// event-loop thread.
///
/// Clearing [`WANT_TEARDOWN`] here matters for the case where a visible state
/// is re-entered before a previously deferred teardown fired (see [`exit`] and
/// [`maybe_finish_teardown`]): re-entering cancels the pending uninstall rather
/// than racing it.
///
/// Closes a menu left open by a **different** mode — the same reasoning
/// [`enter_living_on_main_thread`] applies to a menu opened in Placement,
/// applied symmetrically: `Living`'s menu is resolved against `hit_test`
/// (interactive areas only) and anchored to wherever it was right-clicked, so
/// carrying it into `Placement` would leave a stale control on screen that
/// swallows the next click (`classify_press`'s menu-first precedence) instead
/// of starting the gesture the user actually made. Gated on the *previous*
/// mode, not unconditional, because [`enter`] is also reached by a `Summon`
/// while already in `Placement` (`overlay_state::next` sends `Placement` to
/// `Placement` on that event) — documented as idempotent, so a menu the user
/// is legitimately interacting with there must not be closed out from under
/// them.
fn enter_placement_on_main_thread() {
    let previous = mode();
    ensure_hook();
    set_mode(Mode::Placement);
    WANT_TEARDOWN.store(false, Ordering::SeqCst);
    // The resting shape; the poll refines it to a move or resize cursor as soon
    // as the pointer is over an area.
    set_cursor(CursorShape::Cross);
    if previous != Mode::Placement
        && let Some(app) = APP.get()
    {
        close_menu(app);
    }
    // Placement's own override covers `OCR_NORMAL` too, so anything Living left
    // there is superseded rather than leaked — but the cache has to agree, or the
    // next return to Living would skip the write that corrects it.
    *lock(&LIVING_CURSOR) = None;
}

/// Enters `Living` mode: hook kept for per-area routing (ADR-0016), cursor
/// override dropped, gesture state and menu cleared. Runs on the event-loop
/// thread.
///
/// A live gesture does not survive the transition — the hotkey was pressed
/// mid-drag, and the drag's meaning was a Placement meaning — but a *pending
/// button* does: its down was swallowed, so its eventual up must still be (the
/// abandoned-gesture contract in the module docs), which the mode change does
/// not disturb because [`LEFT_PENDING`]/[`RIGHT_PENDING`] are tracked
/// independently of mode.
fn enter_living_on_main_thread() {
    ensure_hook();
    set_mode(Mode::Living);
    WANT_TEARDOWN.store(false, Ordering::SeqCst);
    DRAGGING.store(false, Ordering::SeqCst);
    *lock(&GESTURE) = None;
    // Arming is Placement-only state and must not outlive it (ADR-0018 §2) —
    // this is one of the three exits that guarantee "there is no mode to still
    // be in later".
    *lock(&ARMED) = None;
    // The menu is re-resolved per mode (its target resolution differs — see
    // `open_menu`), so a menu opened in Placement does not carry over.
    if let Some(app) = APP.get() {
        close_menu(app);
    }
    // Drop Placement's all-slots override: Living does not own the pointer, so
    // pinning `OCR_IBEAM` and friends would change the cursor inside the user's
    // apps. Restore only if one is actually applied — the registry reload is
    // global state other apps see, not a free no-op.
    //
    // Living then takes its *own*, far narrower override on hover: `OCR_NORMAL`
    // alone, via `set_living_cursor` (ADR-0025). The two are deliberately not the
    // same mechanism, and this is the boundary between them — the wide one has to
    // be gone before the narrow one starts, or the narrow one's cache would
    // describe a slot the wide one is still holding.
    if lock(&APPLIED_CURSOR).take().is_some() {
        restore_system_cursors();
    }
    *lock(&LIVING_CURSOR) = None;
}

/// Marks the mode `Hidden` and clears the visual drag, then either tears the
/// hook down immediately or defers it. Runs on the event-loop thread.
///
/// The defer condition is exactly "a button we swallowed the down of has not
/// yet come back up": tearing the hook down anyway would remove the only thing
/// that can still catch that release, turning an abandoned gesture into
/// exactly the leak this module exists to prevent (see the module docs).
fn exit_on_main_thread() {
    set_mode(Mode::Hidden);
    DRAGGING.store(false, Ordering::SeqCst);
    *lock(&GESTURE) = None;
    // See `enter_living_on_main_thread`: arming never outlives Placement.
    *lock(&ARMED) = None;
    // The menu belongs to a visible overlay: leaving with it still on screen
    // would draw a control over a hidden window that nothing could ever click.
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
    // Belt and braces: `exit_on_main_thread` has usually cleared this already,
    // but `teardown_now` is also reached from the deferred path, and arming
    // surviving a teardown would be exactly the mode state ADR-0009 §3 deleted.
    *lock(&ARMED) = None;
    // `SPI_SETCURSORS` is the right call *here* — it runs once, on the way out,
    // and puts every slot back including the `OCR_NORMAL` the Living path may
    // have overridden (ADR-0025). Its 7.9 ms only disqualifies it from the
    // per-hover path, not from teardown.
    restore_system_cursors();
    // The override is gone, so both caches must forget what they believe the OS
    // has — otherwise the next entry would skip re-applying a shape that is no
    // longer set.
    *lock(&APPLIED_CURSOR) = None;
    *lock(&LIVING_CURSOR) = None;
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

/// What the LIVING path currently has installed in `OCR_NORMAL`, or `None` when
/// the user's own arrow is in place. Separate from [`APPLIED_CURSOR`], which
/// tracks PLACEMENT's all-slots override — the two modes own different amounts of
/// the cursor table and must not share a cache.
static LIVING_CURSOR: Mutex<Option<CursorShape>> = Mutex::new(None);

/// Shows `shape` while the pointer is over an area's chrome in LIVING, or returns
/// the user's arrow when `shape` is `None`
/// ([ADR-0025](../../../Projects/UP-TAKE/DECISIONS/ADR-0025-living-cursor-via-a-narrow-override.md)).
///
/// # Two ways this is narrower than [`set_cursor`], both deliberate
///
/// **Only `OCR_NORMAL`.** PLACEMENT pins all of [`OVERRIDDEN_CURSORS`] to one
/// shape because it owns the whole surface and a text caret appearing over a
/// field underneath would read as the overlay losing the pointer. In LIVING the
/// user's apps own the pointer: an I-beam over their text or a wait cursor in
/// another process is theirs, and ours to leave alone.
///
/// **Restored by another `SetSystemCursor`, never by `SPI_SETCURSORS`.** Measured
/// on the dev rig: the registry reload is **7.9 ms** and broadcasts
/// `WM_SETTINGCHANGE` desktop-wide, against **0.072 ms** for a single slot
/// override. This runs from the poll thread on every hover transition — at 221 Hz
/// during a gesture — so the reload would spend half of every tick on a global
/// broadcast and make the whole desktop stutter. That is the cost the old
/// `css_cursor` doc comment said had to be measured before taking this route.
fn set_living_cursor(shape: Option<CursorShape>) {
    let mut applied = lock(&LIVING_CURSOR);
    if *applied == shape {
        return;
    }
    // `None` is not "install nothing" — it is "install the genuine arrow", which
    // is the whole reason `CursorShape::Arrow` exists.
    apply_cursor_to(shape.unwrap_or(CursorShape::Arrow), OCR_NORMAL);
    *applied = shape;
}

/// Private copies of the real system cursors, taken **before** the first
/// override and reused for every shape after it.
///
/// This indirection is not decoration; without it the cursor can only ever be
/// set once. [`SetSystemCursor`] replaces a cursor *globally*, and
/// [`LoadCursorW`] reads that same global table — so once `OCR_SIZEALL` has been
/// pointed at the crosshair, `LoadCursorW(IDC_SIZEALL)` hands back **the
/// crosshair**, and every later shape resolves to whatever is already showing.
/// Loading from the live table is self-defeating in the worst way: every call
/// succeeds, nothing logs, and the pointer simply never changes.
///
/// Stored as `isize` because a raw pointer is not `Sync`; `0` means that shape
/// failed to load and leaves the cursor alone. These handles are only ever
/// `CopyIcon`d, never passed to `SetSystemCursor` directly — the system destroys
/// what it is given, and destroying the snapshot would leave nothing to copy.
static CURSOR_SNAPSHOT: OnceLock<[isize; CURSOR_SNAPSHOT_LEN]> = OnceLock::new();

/// How many cursors the snapshot holds — **derived** from [`ALL_SHAPES`], never
/// written out.
///
/// The two used to be independent literals (`[isize; 7]` beside
/// `[CursorShape; 7]`). That is a mapping the compiler does not check: adding a
/// shape to one and not the other is a runtime index panic, and reordering
/// either silently hands every shape the wrong cursor image. ADR-0025 widened
/// both by hand and got it right; this makes getting it wrong impossible rather
/// than merely unlikely.
const CURSOR_SNAPSHOT_LEN: usize = ALL_SHAPES.len();

/// Every shape, in [`CursorShape::index`] order.
const ALL_SHAPES: [CursorShape; 8] = [
    CursorShape::Cross,
    CursorShape::Move,
    CursorShape::SizeNS,
    CursorShape::SizeWE,
    CursorShape::SizeNWSE,
    CursorShape::SizeNESW,
    CursorShape::Hand,
    CursorShape::Arrow,
];

/// The real cursor for a shape, loading the whole set on first use.
///
/// The set is reloaded from the registry before it is read, so what is captured
/// is the user's genuine scheme no matter what this or any previous process left
/// installed.
fn snapshot_cursor(shape: CursorShape) -> HCURSOR {
    let snapshot = CURSOR_SNAPSHOT.get_or_init(|| {
        // Reload the user's real cursors from the registry *first*. Reading the
        // live table would be circular whenever a previous run was killed while
        // its override was active — the crosshair it left behind is still
        // installed, so every shape would be captured as a crosshair and the
        // pointer could never change again. That is not hypothetical: a hard
        // kill leaves exactly that state, and so does every hot restart under
        // `tauri dev`.
        restore_system_cursors();
        let mut handles = [0_isize; CURSOR_SNAPSHOT_LEN];
        for (slot, shape) in handles.iter_mut().zip(ALL_SHAPES) {
            let loaded = unsafe { LoadCursorW(ptr::null_mut(), shape.idc()) };
            // Our own copy: the shared handle belongs to the system, and this one
            // has to outlive every `SetSystemCursor` we hand a copy of.
            if loaded.is_null() {
                continue;
            }
            let copy = unsafe { CopyIcon(loaded) };
            *slot = copy as isize;
        }
        handles
    });
    snapshot[shape.index()] as HCURSOR
}

/// Points every common system cursor at `shape`. Each `SetSystemCursor`
/// consumes the handle it is given, so every id gets its own [`CopyIcon`] of the
/// snapshot — passing the snapshot itself would have the system destroy the one
/// copy that cannot be reloaded.
///
/// The whole set is overridden rather than just `OCR_NORMAL` because the pointer
/// travels over the user's apps during placement: leaving `OCR_IBEAM` alone
/// would show a text caret the moment the cursor crossed a text field
/// underneath, which reads as "the overlay lost the pointer".
///
/// Called only from the poll thread and from the two entry points that own the
/// override, so the shape cannot be written by two racers at once.
fn apply_cursor(shape: CursorShape) {
    let cursor: HCURSOR = snapshot_cursor(shape);
    if cursor.is_null() {
        eprintln!(
            "placement: could not load the {shape:?} cursor; leaving the system cursor as-is"
        );
        return;
    }
    for id in OVERRIDDEN_CURSORS {
        install_cursor(cursor, id);
    }
}

/// Overrides a **single** cursor slot — the LIVING path's unit of work
/// (ADR-0025), where PLACEMENT's [`apply_cursor`] overrides the whole set.
fn apply_cursor_to(shape: CursorShape, id: u32) {
    let cursor: HCURSOR = snapshot_cursor(shape);
    if cursor.is_null() {
        eprintln!(
            "placement: could not load the {shape:?} cursor; leaving the system cursor as-is"
        );
        return;
    }
    install_cursor(cursor, id);
}

/// Installs a **copy** of `cursor` into slot `id`.
///
/// Always a `CopyIcon`: `SetSystemCursor` destroys the handle it is given, and
/// these come from [`CURSOR_SNAPSHOT`], which must survive to be installed again.
/// Handing it the snapshot directly would work exactly once and then leave
/// nothing to restore to.
fn install_cursor(cursor: HCURSOR, id: u32) {
    let copy = unsafe { CopyIcon(cursor) };
    if copy.is_null() {
        return;
    }
    // Ignoring the BOOL, and the justification is **weaker here than where it was
    // written**. It was written for `apply_cursor`, which runs on a mode
    // transition: a failure there leaves one slot at its default for the duration
    // of a placement session, which is cosmetic. Since ADR-0025 this helper also
    // serves `set_living_cursor`, which fires on every hover-in and hover-out of
    // chrome in LIVING — the app's *resting* state — so the same failure recurs
    // per crossing rather than per session.
    //
    // What is **not** established is who owns `copy` when the call fails.
    // `SetSystemCursor` is documented as destroying the handle it is given; the
    // documentation does not say whether it still does so on failure. Adding a
    // `DestroyCursor` here would leak nothing if it does not — and double-destroy
    // a handle the system may already have freed and reused if it does. That is
    // exactly the sort of unrun equivalence argument that cost this project a
    // working feature on 2026-07-27, so it is recorded as `BACKLOG.md` I-6 to be
    // settled by experiment rather than guessed at in a review.
    unsafe {
        SetSystemCursor(copy, id);
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
    // Proof of life for the health check. Relaxed: nothing is ordered against
    // it, only its own change from one poll tick to the next is read.
    HOOK_EVENTS.fetch_add(1, Ordering::Relaxed);
    // Safe: for a mouse hook Windows passes an `MSLLHOOKSTRUCT` here, valid for
    // the duration of the call.
    let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
    let (x, y) = (info.pt.x, info.pt.y);
    let point = Point::new(x, y);
    // A press belongs to whatever is actually on top at that point. Checked once
    // here, before either button's handling, so neither path can forget it — and
    // only on a **press**, never on a move, so the per-event cost stays off the
    // hot path. See `shadowed_by_another_window`.
    //
    // This applies in Placement too, not just Living. Placement means "UP-TAKE
    // owns the mouse", but that was always shorthand for "UP-TAKE is the topmost
    // thing" — when it demonstrably is not, swallowing the click is the same
    // mistake in either mode.
    if matches!(wparam as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN) && shadowed_by_another_window(point)
    {
        return false;
    }
    match wparam as u32 {
        WM_LBUTTONDOWN => match mode() {
            // The hook's mode has already been left (most likely: the brief
            // deferred-teardown window from an earlier abandoned drag). A
            // fresh press here belongs to whatever the user is doing now, not
            // to a gesture we should start.
            Mode::Hidden => false,
            Mode::Placement => {
                START_X.store(x, Ordering::SeqCst);
                START_Y.store(y, Ordering::SeqCst);
                CUR_X.store(x, Ordering::SeqCst);
                CUR_Y.store(y, Ordering::SeqCst);
                // Classified before the lock is taken, not inside the
                // assignment: `classify_press` takes the menu and store locks
                // itself, and nesting those inside this one would be a lock
                // order to reason about rather than one that cannot exist.
                let gesture = classify_press(point);
                // Task 1.9c: a drag that will capture on release starts its
                // capture *now*, so the ~200 ms is spent during the drag rather
                // than after it (ADR-0022). Spawns — never captures here; this
                // is the `WH_MOUSE_LL` callback (F-33).
                start_precapture(gesture, point);
                *lock(&GESTURE) = Some(gesture);
                DRAGGING.store(true, Ordering::SeqCst);
                LEFT_PENDING.store(true, Ordering::SeqCst);
                true
            }
            Mode::Living => living_lbutton_down(point),
        },
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
        WM_RBUTTONDOWN => match mode() {
            Mode::Hidden => false,
            // Placement swallows every right press: a stray right-click must
            // neither pop a context menu underneath nor steal focus (which
            // would take the `Esc` dismiss path with it).
            Mode::Placement => {
                RIGHT_PENDING.store(true, Ordering::SeqCst);
                true
            }
            // Living takes only what belongs to an area (ADR-0016): a right
            // press over the topmost *interactive* area, or any right press
            // while a menu is open (the release will act on the menu state —
            // close it, or replace it over another area). Everything else is
            // the user's, untouched.
            Mode::Living => {
                let claimed = lock(&MENU).is_some()
                    || APP
                        .get()
                        .is_some_and(|app| overlay::interactive_area_at(app, point).is_some());
                if claimed {
                    RIGHT_PENDING.store(true, Ordering::SeqCst);
                }
                claimed
            }
        },
        WM_RBUTTONUP if RIGHT_PENDING.swap(false, Ordering::SeqCst) => {
            // Opened on *release*, not on press: a menu that appears under a
            // still-held button is one the same gesture can dismiss by accident.
            if mode() != Mode::Hidden
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

/// Starts the pre-capture, if this press begins a drag that will capture.
///
/// Three conditions, all of them necessary. The gesture must be [`Gesture::Create`]
/// — a move, a resize, a close or a menu press produces no capture, and
/// pre-capturing for one would spend a full-monitor capture on every click in
/// Placement. The armed type must be one that captures on create, read through
/// the same [`captures_on_create`] predicate the release path uses. And the
/// cursor must be on a monitor: in a dead zone between mismatched monitors there
/// is nothing to pre-capture, and picking a neighbour would hold a frame that
/// every crop then declines.
///
/// Reads the overlay's cached monitor list rather than enumerating: this runs in
/// the hook callback, where a `EnumDisplayMonitors` round trip on every press is
/// exactly the cost that path exists to avoid. The cache is refreshed on display
/// changes; a stale entry costs a declined crop and a normal capture, not a
/// wrong image, because [`precapture::take`] validates containment against the
/// rectangle the capture crate **reports** rather than the one requested.
fn start_precapture(gesture: Gesture, point: Point) {
    if !matches!(gesture, Gesture::Create) {
        return;
    }
    if !armed().is_some_and(captures_on_create) {
        return;
    }
    if let Some(monitor) = precapture::monitor_holding(&overlay::monitor_rects(), point) {
        precapture::begin(monitor);
    }
}

/// What a left press means in `Living` (ADR-0016): an open menu owns every
/// click exactly as in Placement; otherwise the topmost *interactive* area
/// under the point claims the press and is raised (§3.2a — touching an area
/// puts it on top of its tier, applied on the press like every window
/// manager); otherwise the press belongs to the user's apps and passes
/// through untouched. Returns whether the event is swallowed.
///
/// Living never starts drags — moving and resizing are `Placement` gestures —
/// so [`DRAGGING`] is set only for the menu-row press, where the existing
/// release path ([`finish_gesture`] → [`activate_menu_item`]) implements the
/// press-and-release-on-target contract. A raised area's press needs nothing
/// on release beyond being swallowed, which [`LEFT_PENDING`] alone provides.
fn living_lbutton_down(point: Point) -> bool {
    if menu_contains(point) {
        let gesture = match menu_item_at(point) {
            Some(index) => Gesture::MenuItem { index },
            // Menu padding: a press that does nothing, rather than one that
            // falls through to whatever is underneath the menu.
            None => Gesture::Inert,
        };
        *lock(&GESTURE) = Some(gesture);
        DRAGGING.store(true, Ordering::SeqCst);
        LEFT_PENDING.store(true, Ordering::SeqCst);
        return true;
    }
    let Some(app) = APP.get() else {
        return false;
    };
    if close_menu(app) {
        // The click that dismisses a menu does not also act on what it landed
        // on — the standard contract — so it is swallowed even when it sits
        // over the user's app.
        LEFT_PENDING.store(true, Ordering::SeqCst);
        return true;
    }
    // Task 1.17(a): a press on an interactive area begins a real gesture here,
    // not just a raise. The routing for this already existed — the hook runs in
    // every visible state (ADR-0016) and this function already resolved and
    // raised the area under the cursor — so the only thing missing was calling
    // the classifier. That is why needing `Placement` to nudge an area felt
    // arbitrary: it was an unfinished edge, not a design position.
    //
    // **Two deliberate differences from `classify_press`.** It resolves against
    // *interactive* areas only, because a pass-through area's pixels belong to
    // the app underneath in `Living`; and a press that lands on no area returns
    // `false` rather than falling through to `Gesture::Create`, because empty
    // overlay in `Living` is the user's desktop and creating there would steal
    // a click from it. Creating areas stays a `Placement` gesture.
    let Some((id, bounds, handle)) = overlay::interactive_area_handle_at(app, point) else {
        return false;
    };
    // **The drag anchor, and it is not optional.** `gesture_rect` computes every
    // move and resize as `pointer - anchor`, reading it from these statics; the
    // `Placement` arm of the hook stores them before classifying. The first cut
    // of this function did not, so a Living drag differenced the live pointer
    // against whatever anchor the *last Placement drag* had left behind — a
    // constant offset, which is why every area jumped the same way on the first
    // click instead of following the cursor.
    START_X.store(point.x, Ordering::SeqCst);
    START_Y.store(point.y, Ordering::SeqCst);
    CUR_X.store(point.x, Ordering::SeqCst);
    CUR_Y.store(point.y, Ordering::SeqCst);
    // Raise first, so the gesture acts on an area that is already topmost —
    // §3.2a's "the area you last touched is on top" (ADR-0016), unchanged.
    if overlay::raise_area(app, id)
        && let Err(error) = overlay::emit_areas(app)
    {
        eprintln!("placement: raised an area but could not emit the new set: {error}");
    }
    *lock(&GESTURE) = Some(match handle {
        Handle::Close => Gesture::Close {
            id,
            control: overlay::close_control_of(bounds),
        },
        Handle::Resize(resize) => Gesture::Resize {
            id,
            resize,
            start: bounds,
        },
        Handle::Body => Gesture::Move { id, start: bounds },
    });
    DRAGGING.store(true, Ordering::SeqCst);
    LEFT_PENDING.store(true, Ordering::SeqCst);
    true
}

/// How far up the z-order [`shadowed_by_another_window`] will walk before giving
/// up. The bound exists **only** so a corrupted z-order chain cannot spin inside
/// a hook callback, where time spent is time Windows counts against
/// `LowLevelHooksTimeout`. It is not a performance knob.
///
/// # ⚠️ Both the original estimate and the "optimisation" that replaced it were wrong
///
/// The first value was 512, justified as *"a desktop has tens of top-level
/// windows, not thousands"*. Counted on the dev rig: **384–418** windows in the
/// desktop chain, only ~30 of them visible — hidden top-level windows sit in the
/// same chain and `GetWindow` returns them like any other. So "tens" was off by
/// more than 10×, and the margin against 512 was ~1.2×. That part was a real
/// problem: exceeding the limit makes [`shadowed_by_another_window`] answer
/// `false`, silently restoring the Start-menu-swallowing bug it exists to
/// prevent.
///
/// The fix attempted first was to walk from *our* window instead of from the hit
/// target, on the argument that both are in one chain so either end answers the
/// same question at very different cost. **It does not work, and the rig said so
/// within minutes: clicks inside the Start menu were swallowed again.** The
/// equivalence argument was made from `GetWindow`'s documentation and never
/// tested; whatever the mechanism (`GW_HWNDPREV` from a topmost window is
/// documented to identify *a topmost window*, and the shell's Start surface does
/// not reliably appear in the upward chain from ours), the two directions are not
/// interchangeable in practice. Reverted to the direction that was verified
/// working on hardware.
///
/// **What was actually needed was just a bigger number, and the cost data says
/// that is free.** Timed on the rig, the worst case — a full walk from the
/// bottom-most window of a 418-long chain — is **417 steps in 2.77 ms**, i.e.
/// ~1 % of the default 300 ms `LowLevelHooksTimeout`, and it runs on presses
/// only, never on moves. 8192 leaves ~20× headroom over the observed chain while
/// still bounding a corrupted one at roughly 54 ms.
///
/// The lesson worth keeping: **a cheaper algorithm that has not been run is not
/// an optimisation, it is an untested rewrite** — and this one traded a verified
/// behaviour for a 2.8 ms saving nobody had asked for.
const Z_ORDER_WALK_LIMIT: usize = 8192;

/// Whether some other window sits **above** the overlay at `point`, and should
/// therefore receive this press instead of us.
///
/// # The bug this exists for
///
/// The hook claims input by **screen position**, and was written assuming our
/// areas are the topmost thing at their coordinates — true, because the overlay
/// is always-on-top. **Shell surfaces break that assumption**: the Start and
/// search popups sit above even topmost windows, so a click over an area that
/// happens to be behind Start was swallowed by us and never reached it. With an
/// area covering the screen, Start became entirely unusable. Found on the rig
/// 2026-07-26.
///
/// Deliberately general rather than a check for Start specifically: any window
/// that gets above us has the same claim, and a class-name test would fix one
/// instance of the bug and rot across Windows builds. Same family as F-25 — a
/// hook claiming input it has no right to.
///
/// # Why the obvious test does not work
///
/// `WindowFromPoint` **skips `WS_EX_TRANSPARENT` windows**, and the overlay is
/// transparent in every visible state (ADR-0016). So it never returns our
/// window, and "is the window under the cursor ours?" can only ever answer no.
/// What it does return is the window that *would* receive the click — so the
/// real question is whether that window is above us or below us, which is a
/// z-order walk: step upward **from the hit window** and see whether we are
/// passed on the way.
///
/// # Walk from the hit window, not from ours
///
/// This looks like it should be reversible — the two windows are in one chain, so
/// either end could answer "which is higher?" — and starting from our own
/// (topmost, so near the top) window looks much cheaper. **It was tried during
/// the task 1.9b review and it broke the fix**: clicks inside the Start menu were
/// swallowed again within minutes of a rig pass. `GW_HWNDPREV` from a topmost
/// window is documented to identify *a topmost window*, and the shell's Start
/// surface does not reliably show up in the upward chain from ours, so the
/// symmetry is only apparent. See [`Z_ORDER_WALK_LIMIT`] for the full account and
/// for why the cost this direction pays is affordable (worst case measured at
/// 2.77 ms, on presses only).
///
/// **Do not "optimise" this direction again without a rig pass on the Start
/// menu.** Nothing in a unit test can see it.
///
/// Returns `false` when anything cannot be resolved, and `false` if the walk
/// runs past its bound. A press that fails this check is a press we handle as
/// before — degrading to the previous behaviour beats dropping the user's input
/// on the floor.
fn shadowed_by_another_window(point: Point) -> bool {
    let Some(app) = APP.get() else {
        return false;
    };
    let Ok(window) = overlay::overlay_window(app) else {
        return false;
    };
    let Ok(ours) = window.hwnd() else {
        return false;
    };
    let ours: HWND = ours.0;
    // SAFETY: `WindowFromPoint` takes a POINT by value and returns a handle or
    // null; `GetAncestor`/`GetWindow` take a handle and are null-tolerant. None
    // of them dereference anything we own.
    unsafe {
        let hit = WindowFromPoint(POINT {
            x: point.x,
            y: point.y,
        });
        if hit.is_null() {
            return false;
        }
        // Compare top-level windows: `WindowFromPoint` can land on a child
        // control, which has no position in the top-level z-order at all.
        let mut current = GetAncestor(hit, GA_ROOT);
        if current.is_null() || std::ptr::eq(current, ours) {
            return false;
        }
        // Walk upward. Reaching our overlay means the hit window is below it and
        // the press is ours; running off the top means it is above us.
        for _ in 0..Z_ORDER_WALK_LIMIT {
            let above = GetWindow(current, GW_HWNDPREV);
            if above.is_null() {
                return true;
            }
            if std::ptr::eq(above, ours) {
                return false;
            }
            current = above;
        }
    }
    // The walk ran long — treat it as inconclusive and keep the press, rather
    // than silently handing input away because a z-order chain was odd.
    false
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
        if let Some((id, bounds, handle)) = overlay::area_handle_at(app, point) {
            return match handle {
                Handle::Close => Gesture::Close {
                    id,
                    control: overlay::close_control_of(bounds),
                },
                Handle::Resize(resize) => Gesture::Resize {
                    id,
                    resize,
                    start: bounds,
                },
                Handle::Body => Gesture::Move { id, start: bounds },
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
    let pending = gesture_rect(gesture, release);
    let changed = match gesture {
        Gesture::Create => {
            let Some((x, y, width, height)) = pending else {
                return;
            };
            // The armed type is consumed here, and `disarm` runs on **every**
            // outcome including a rejected drag: ADR-0018 §2 clears arming "when
            // an area is created", and a sliver drag that creates nothing is
            // still the user having taken their shot. Leaving it armed would
            // make the *next* drag — possibly minutes later, possibly meant as
            // a plain Default area — silently produce a Screenshot, which is
            // exactly the "which mode am I in?" failure the ADR is avoiding.
            let kind = armed().unwrap_or(AreaType::Default);
            disarm();
            let created = overlay::create_area(app, kind, x, y, width, height);
            // Logged so a placement problem is an observation rather than a
            // guess (the F-15 lesson) — and logged *after* the attempt, with its
            // outcome. Printing "created area" before the call claimed a
            // creation that had not happened yet and sometimes never did: an
            // empty drag produced `created area 0x0`, which is precisely the
            // sort of confidently wrong log line that sends a later debugging
            // session in the wrong direction.
            //
            // The coordinate space itself is settled: hardware testing confirmed
            // `MSLLHOOKSTRUCT.pt` matches `cursor_position` — the space the
            // store and click-through regions use — across every monitor, the
            // 125% primary included.
            #[cfg(debug_assertions)]
            if created.is_some() {
                eprintln!("placement: created {kind:?} area {width}x{height} at ({x}, {y})");
            } else {
                eprintln!("placement: drag at ({x}, {y}) was {width}x{height} — nothing created");
            }
            if let Some((id, bounds)) = created {
                // Order matters. The capture is dispatched first so its ~200 ms
                // runs while the mode transition happens rather than after it,
                // and neither blocks this callback: `capture_on_create` spawns
                // and `area_created` posts to the event loop.
                capture_on_create(app, kind, id, bounds);
                overlay::area_created(app, kind);
            } else {
                // An empty or rejected drag creates nothing, so nothing will
                // consume the frame it may have pre-captured. Dropped here
                // rather than left for the next gesture to clear, which would
                // hold a full-monitor bitmap for as long as the user hesitates.
                precapture::discard();
            }
            created.is_some()
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

/// Dispatches the capture a freshly created area needs, if its type has one.
///
/// Only `Screenshot` captures on create — ADR-0018 settles that for the one type
/// it decided, and the rest have no gesture yet. Written as an explicit match
/// rather than as another `AreaType` method because "does creating this capture
/// pixels?" has exactly one answer today, and inventing a fourth per-type axis
/// on one data point is how the other three got harder to change.
fn capture_on_create(app: &AppHandle, kind: AreaType, id: AreaId, bounds: Rect) {
    if captures_on_create(kind) {
        crate::output::capture_into_area(app, id, bounds);
        // The spawned capture is what consumes the held frame, so the drag is
        // ended *without* clearing it — see [`precapture::end_drag`]. Retiring
        // the generation here is what stops a refresh capture still in flight
        // from landing after the gesture is over and sitting there unread.
        precapture::end_drag();
    } else {
        // Nothing will consume a frame for an area of this type. A leftover from
        // an earlier gesture would otherwise stay resident until the next
        // capturing drag happened to replace it.
        precapture::discard();
    }
}

/// Whether creating an area of `kind` captures pixels.
///
/// # Why this is one predicate and not two matches
///
/// Task 1.9c added a **second** reader of this fact: the pre-capture fires on
/// mouse-down only for a drag that is going to capture on release. Written as a
/// second `match` beside [`capture_on_create`]'s, the two would be a
/// hand-maintained pair that agree today and drift the moment a type is added —
/// and the drift is silent in the worse direction. A type added here but not
/// there merely loses the fast path; added *there* but not here, every drag of
/// every other type pays for a full-monitor capture nobody reads. The PR #24
/// review found the same shape as a three-way cursor mapping and the fix was the
/// same: derive it, do not restate it.
///
/// Exhaustive rather than `matches!` with a `_` arm, so adding an `AreaType`
/// fails to compile here instead of defaulting to "captures nothing".
const fn captures_on_create(kind: AreaType) -> bool {
    match kind {
        AreaType::Screenshot => true,
        AreaType::Default
        | AreaType::Record
        | AreaType::Ocr
        | AreaType::Upscale
        | AreaType::Analysis
        | AreaType::Filter => false,
    }
}

/// The rectangle a gesture commits, computed against an explicit release point
/// rather than the polled cursor — the release coordinate is the authoritative
/// one, and the poll may not have ticked since the last mouse move.
fn gesture_rect(gesture: Gesture, pointer: Point) -> Option<(i32, i32, u32, u32)> {
    let anchor = Point::new(
        START_X.load(Ordering::SeqCst),
        START_Y.load(Ordering::SeqCst),
    );
    // Saturating: the operands are screen coordinates, so a difference cannot
    // realistically overflow, but a wrapped delta would teleport an area.
    let dx = pointer.x.saturating_sub(anchor.x);
    let dy = pointer.y.saturating_sub(anchor.y);
    let monitors = overlay::monitor_rects();
    // Holding Alt turns edge snapping off for the rest of the drag — the
    // standard escape hatch for placing something a few pixels off an edge that
    // the snap would otherwise swallow. It does **not** disable containment:
    // that is the guarantee an area can always be reached again, and a modifier
    // key is not a good reason to let one be lost.
    let free = snapping_suppressed();
    let rect = match gesture {
        // A create drag needs no containment — both of its corners are places
        // the cursor actually reached, so it is on screen by construction — but
        // it snaps like everything else.
        Gesture::Create => {
            let drawn = Rect::from_corner_points(anchor, pointer);
            if free {
                drawn
            } else {
                interaction::snap_move(drawn, &monitors)
            }
        }
        Gesture::Move { start, .. } => {
            let moved = interaction::move_by(start, dx, dy);
            if free {
                interaction::contain(moved, &monitors)
            } else {
                interaction::settle_move(moved, &monitors)
            }
        }
        Gesture::Resize { start, resize, .. } => {
            let resized = interaction::resize_by(start, resize, dx, dy);
            if free {
                interaction::contain(resized, &monitors)
            } else {
                interaction::settle_resize(resized, resize, &monitors)
            }
        }
        Gesture::Close { .. } | Gesture::MenuItem { .. } | Gesture::Inert => return None,
    };
    Some(overlay::as_tuple(rect))
}

/// Whether the user is holding `Alt`, which suppresses edge snapping.
///
/// Read at the moment the rectangle is computed rather than latched at
/// button-down, so the key can be pressed or released *during* a drag and the
/// area follows immediately — which is how the modifier behaves in every tool
/// that has one, and it means a user who forgot to hold it need not restart the
/// gesture.
fn snapping_suppressed() -> bool {
    vk_is_down(i32::from(VK_MENU))
}

/// Whether a virtual key is physically down right now, independent of the
/// hook. The high bit of [`GetAsyncKeyState`]'s result is "currently down";
/// the low bit is "pressed since the last call", which would make this true
/// long after the key came back up.
fn vk_is_down(vk: i32) -> bool {
    let state = unsafe { GetAsyncKeyState(vk) };
    (state as u16 & 0x8000) != 0
}

// ---------------------------------------------------------------------------
// The per-area menu (ADR-0013): the control that sets an area's Layer tier.
// ---------------------------------------------------------------------------

/// Opens the area menu for whatever is under `point`, replacing any open menu.
/// Does nothing if the point resolves to no area — then a click has nothing to
/// act on, and any open menu simply closes.
///
/// **The target resolution is mode-dependent, and the difference is the V-7
/// input model itself.** In `Placement` the menu opens for the topmost area of
/// *any* input mode (`hit_test_any`) — a pass-through area must stay editable
/// while the layout is being edited, or it becomes permanent. In `Living` it
/// opens only for the topmost *interactive* area (`hit_test`): a pass-through
/// area is invisible to the cursor there by definition, and its pixels belong
/// to whatever app is underneath. That also means flipping an area to
/// pass-through from its own Living menu makes the menu unreachable until
/// Placement — deliberate, and the reason the toggle sits next to the Layer
/// rows that share the same recovery path.
fn open_menu(app: &AppHandle, point: Point) {
    let target = match mode() {
        Mode::Placement => overlay::area_at(app, point),
        Mode::Living => overlay::interactive_area_at(app, point),
        Mode::Hidden => None,
    };
    let Some(area) = target else {
        close_menu(app);
        return;
    };
    // Anchored to the monitor under the cursor, never to the virtual desktop:
    // desktop-relative chrome can land in a dead zone no cursor can reach (F-13).
    let monitor = overlay::monitor_bounds_at(app, point);
    // The toggle row switches to the opposite of the area's current input mode;
    // its tick shows the current state (ticked = pass-through).
    let toggled_input = match area.input {
        Input::Interactive => Input::PassThrough,
        Input::PassThrough => Input::Interactive,
    };
    let mut spec: Vec<(MenuAction, &'static str)> = Vec::with_capacity(7);
    // Copy/Save lead the menu — the primary actions, ahead of the layout
    // settings below them — and are scoped to **`Screenshot` areas only**.
    //
    // Task 1.9 scoped them to `Default` instead, as a placeholder: `Screenshot`
    // did not exist yet and its own menu was named as 1.9b's job. That got
    // inverted rather than extended, on the rig, 2026-07-26 — the rows belong to
    // the type that *has* a capture, not to the primitive that does not. A
    // `Default` area is a plain claimed rectangle; offering "Save image" on one
    // implies it holds an image it does not have.
    //
    // These actions export **the area's pinned capture**, not a fresh grab of
    // whatever is under it — see `captures::pinned_capture`. They used to capture
    // live, and this comment used to predict the consequence and defer it:
    // "wrong the moment an area is moved after capture". Task 1.17(a) made areas
    // movable in the same PR, so the moment arrived immediately, and the rig
    // found it on 2026-07-27. A predicted defect left in place is still a defect.
    if area.kind == AreaType::Screenshot {
        spec.push((MenuAction::Copy, "Copy"));
        spec.push((MenuAction::SaveToFile, "Save image"));
    }
    spec.push((MenuAction::SetLayer(Layer::Front), "Always on top"));
    spec.push((MenuAction::SetLayer(Layer::Auto), "Auto"));
    spec.push((MenuAction::SetLayer(Layer::Back), "Always behind"));
    spec.push((MenuAction::SetInput(toggled_input), "Click-through"));
    spec.push((MenuAction::Dismiss, "Dismiss"));
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a menu this short cannot overflow u32"
    )]
    let bounds = interaction::menu_bounds(point, spec.len() as u32, monitor);
    let items = spec
        .iter()
        .enumerate()
        .map(|(index, (action, label))| MenuEntry {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a menu this short cannot overflow u32"
            )]
            rect: interaction::menu_item_bounds(bounds, index as u32),
            action: *action,
            label,
            checked: match action {
                MenuAction::SetLayer(layer) => *layer == area.layer,
                MenuAction::SetInput(_) => area.input == Input::PassThrough,
                MenuAction::Dismiss | MenuAction::Copy | MenuAction::SaveToFile => false,
            },
        })
        .collect();
    *lock(&MENU) = Some(AreaMenu {
        area: area.id,
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
        MenuAction::SetInput(input) => overlay::set_area_input(app, area, input),
        MenuAction::Dismiss => overlay::dismiss_area(app, area),
        // Neither touches the area store — nothing to re-emit — and both are
        // spawned onto their own thread rather than run here: a capture is
        // ~100-300 ms even warm (`uptake_capture` crate docs, F-29), and this
        // function runs on the event-loop thread, inside the `WH_MOUSE_LL`
        // callback's call stack. A hook callback that blocks that long risks
        // Windows silently removing the hook (`LowLevelHooksTimeout`, F-33's
        // failure class) — see the `output` module docs.
        MenuAction::Copy => {
            if let Some(bounds) = overlay::area_bounds(app, area) {
                let app = app.clone();
                std::thread::spawn(move || crate::output::copy_to_clipboard(&app, area, bounds));
            }
            false
        }
        MenuAction::SaveToFile => {
            if let Some(bounds) = overlay::area_bounds(app, area) {
                let app = app.clone();
                std::thread::spawn(move || crate::output::save_to_file(&app, area, bounds));
            }
            false
        }
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

#[cfg(test)]
mod tests {
    use super::{ALL_SHAPES, CURSOR_SNAPSHOT_LEN};

    /// [`super::CursorShape::index`] and [`ALL_SHAPES`] are two hand-maintained
    /// halves of one mapping: [`super::snapshot_cursor`] fills the array by
    /// zipping `ALL_SHAPES` and reads it back by `index()`. Nothing in the type
    /// system ties them together, so a shape added to one and not the other — or
    /// added to both in a different order — silently hands **every** shape the
    /// wrong cursor image. No compile error, no panic, no failing test: just a
    /// pointer that shows the hand where it should show a resize.
    ///
    /// Written when ADR-0025 widened the pair from seven entries to eight. The
    /// widening was correct; the point is that nothing would have said so.
    /// Confirmed to fail on a deliberate swap before being kept.
    #[test]
    fn every_shape_sits_at_its_own_index() {
        for shape in ALL_SHAPES {
            assert_eq!(
                ALL_SHAPES[shape.index()],
                shape,
                "{shape:?} reads back as {:?} — `index()` and `ALL_SHAPES` disagree",
                ALL_SHAPES[shape.index()]
            );
        }
    }

    /// The snapshot array's length is derived from [`ALL_SHAPES`] rather than
    /// written out, so a ninth shape cannot leave it one slot short. This pins
    /// that it is still derived.
    #[test]
    fn the_snapshot_has_a_slot_for_every_shape() {
        assert_eq!(CURSOR_SNAPSHOT_LEN, ALL_SHAPES.len());
    }
}
