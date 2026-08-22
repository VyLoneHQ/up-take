//! Mouse input for the overlay: placement gestures *and* Living-area routing
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
//! - **`Placement`** gives the hook the whole gesture (button-down → move →
//!   button-up), and it swallows both buttons unconditionally: drags create, move
//!   and resize areas, and a **global cursor override** ([`SetSystemCursor`])
//!   supplies the pointer shape, because a click-through window can set no
//!   cursor of its own (no `WM_SETCURSOR` ever reaches it).
//! - **`Living`** leaves the pointer to the user's apps, and the hook takes only
//!   what the area model assigns to areas: a press on the topmost *interactive*
//!   area (`AreaStore::hit_test`; pass-through areas are invisible to input,
//!   V-7) is swallowed and acted on (left raises the area per §3.2a recency,
//!   right opens its menu); every other press is passed through untouched. No
//!   cursor override: the pointer belongs to whatever is underneath.
//! - **`Hidden`** tears the hook down (subject to the pending-button
//!   deferral below) and everything here is inert.
//!
//! The rectangles are drawn by the WebView from coordinates this module
//! publishes. All the Win32 pieces were validated in isolation by the spikes
//! recorded in ADR-0014 before this was written.
//!
//! # Everything an area appears to have is a rectangle this module hit-tests
//!
//! Because no mouse event reaches the WebView, **nothing rendered in the overlay
//! can be clicked as a DOM element**: not the close control, not a menu row.
//! The area's whole lifecycle therefore runs through this hook: a press is
//! classified against the area under the cursor ([`classify_press`]), and what
//! it grabbed decides what the drag does: create, move, resize, dismiss, or
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
//! # Thread affinity: the one rule that makes or breaks the hook
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
//! and the system *destroys* the handle it is given, so each override is a
//! fresh [`CopyIcon`] of the crosshair, and the restore
//! ([`SystemParametersInfoW`] with `SPI_SETCURSORS`) reloads every cursor from
//! the registry. It is called on every exit path this process controls: leaving
//! `Placement` ([`exit`], subject to the deferral below), a graceful shutdown
//! ([`teardown`] from `RunEvent::Exit`), and a panic ([`install_panic_guard`]).
//! What it cannot cover is a **hard kill** (Task Manager) mid-placement, which
//! runs none of our code, a limitation ADR-0014 accepts explicitly. The *next*
//! launch repairs it, though: [`clear_cursor_residue`] runs at startup, and
//! [`snapshot_cursor`] reloads the registry before capturing the set it restores
//! from. Without that second part the residue would be worse than cosmetic: a
//! process starting up under a leftover crosshair would take the crosshair for
//! the user's real cursor and could then never change shape again.
//!
//! # A low-level hook can be removed without being told
//!
//! Windows drops a `WH_MOUSE_LL` hook whose callback overruns
//! `LowLevelHooksTimeout`, and starves one in a medium-integrity process while a
//! higher-integrity window holds the foreground (UIPI, F-25). Neither is
//! reported: [`HOOK`] still holds a handle, so nothing here would notice, and
//! `Placement` would sit on screen with no working input for the rest of the
//! session. [`pump_hook_health`] watches for it (the cursor moving while the
//! hook counts no events) and reinstalls. That does not defeat UIPI and does
//! not try to; it restores the overlay once the elevated window is no longer in
//! front, instead of leaving "press the hotkey twice" as the only way back.
//!
//! # Abandoned gestures: a swallowed button-down obliges us to the button-up
//!
//! Two things can end `Placement` while a mouse button is still physically
//! held down: cancelling mid-drag (`Esc`, [`cancel_drag`]) and toggling away
//! (the hotkey) before releasing. In both cases the button's *down* was already
//! swallowed (nothing underneath ever saw it), so letting its eventual *up*
//! pass through would hand the app under the cursor at release time a lone
//! button-up with no matching down, which is exactly the leak this module
//! exists to prevent. [`LEFT_PENDING`]/[`RIGHT_PENDING`] track "a down was
//! swallowed and its up has not been seen yet" independently of [`DRAGGING`]
//! (the *visual* drag, which a cancel or a toggle-away clears immediately); the
//! hook keeps swallowing until the pending flag clears, regardless of whether
//! [`ACTIVE`] says placement itself is still current. [`exit`] defers the actual
//! hook uninstall and cursor restore ([`WANT_TEARDOWN`]) until that happens:
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
    UnhookWindowsHookEx, WH_MOUSE_LL, WHEEL_DELTA, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WindowFromPoint,
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

/// Whether a placement drag is visually in progress. Drives the on-screen
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

/// The drag's anchor and current corner, in physical virtual-desktop pixels,
/// the same space [`crate::overlay`] and `uptake_core` use. `MSLLHOOKSTRUCT.pt`
/// is already in that space for a per-monitor-DPI-aware process, so no
/// conversion happens here.
static START_X: AtomicI32 = AtomicI32::new(0);
static START_Y: AtomicI32 = AtomicI32::new(0);
static CUR_X: AtomicI32 = AtomicI32::new(0);
static CUR_Y: AtomicI32 = AtomicI32::new(0);

/// How many events the hook has processed. Only ever compared against its own
/// previous value. See [`pump_hook_health`], which uses it to notice that
/// Windows has silently removed the hook.
static HOOK_EVENTS: AtomicU64 = AtomicU64::new(0);

/// The app handle the hook callback needs to reach the `AreaStore` and emit.
/// Set on the first [`enter`]; a static because the `extern "system"` callback
/// captures nothing.
static APP: OnceLock<AppHandle> = OnceLock::new();

/// What the current left-button drag *means*, decided once, at button-down,
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
/// name, and this is mode state, bought back only because it cannot outlive one
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
    /// the cursor is still on the control**: the press-and-release-on-target
    /// contract every button on every platform honours, and the only way to
    /// change your mind about a gesture with no undo.
    Close { id: AreaId, control: Rect },
    /// A press on a row of the open area menu, resolved the same way. The hit
    /// names which of the two lists the row is in (roadmap 1.28).
    MenuItem { hit: MenuHit },
    /// A press that has already done its job and must do nothing more on
    /// release: closing an open menu by clicking away from it, or landing on
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
    /// The open child list, if a parent row has earned one (roadmap 1.28).
    open: Option<Submenu>,
    /// What the pointer currently argues the open state should be, and the
    /// millisecond since [`EPOCH`] at which it started arguing it.
    ///
    /// **The timestamp is what buys the diagonal travel**, which is one of the
    /// three costs roadmap 1.27 named and 1.28 pays. A pointer moving from the
    /// parent row to a child row crosses the rows between them, and each of
    /// those ticks argues *close*. Restarting the clock on every change rather
    /// than acting on it means those ticks cost time and nothing else: arrive
    /// inside the child list within [`SUBMENU_CLOSE_MS`] and the argument flips
    /// back before it was ever acted on. See [`submenu_step`].
    argument: (Option<usize>, u64),
}

/// The open child list of one parent row.
struct Submenu {
    /// The index in [`AreaMenu::items`] of the row this list belongs to.
    parent: usize,
    /// The list's outer rectangle, physical px.
    bounds: Rect,
    /// One entry per child row, in draw order.
    items: Vec<MenuEntry>,
    /// The child row under the cursor.
    hovered: Option<usize>,
}

/// One row of the area menu.
///
/// `Clone` rather than `Copy` since 1.28: a parent row carries the rows it
/// opens, so that the child list is laid out from the same spec the top level
/// was built from rather than from a second call to [`menu_rows`] that could
/// answer differently.
#[derive(Clone)]
struct MenuEntry {
    rect: Rect,
    action: MenuAction,
    label: &'static str,
    /// Whether this row shows a tick: the area's current tier.
    checked: bool,
    /// The rows this row opens as a child list. Empty on a leaf row, and empty
    /// on every row of a child list: menus here are two deep.
    children: Vec<MenuRow>,
}

/// Where a point falls inside an open menu.
///
/// The two lists are separate hit targets, which is the first of the three costs
/// roadmap 1.27 priced. One `usize` cannot carry both, and the failure of
/// pretending otherwise is silent: an index into the wrong list still resolves
/// to a row and still performs its action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuHit {
    /// A row of the top-level list.
    Row(usize),
    /// A row of the open child list.
    Child(usize),
}

/// What a menu row does when activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    /// Pin the area to a stacking tier (ADR-0013).
    SetLayer(Layer),
    /// Set whether the area takes input or lets it fall through (V-7). The
    /// menu row is a toggle, so the action carries the value the row would
    /// switch *to*.
    SetInput(Input),
    /// Open this row's child list (roadmap 1.28). The only action that leaves
    /// the menu up: it is a way *into* the menu rather than something done to
    /// the area. Which list to open is the row's own position, so the action
    /// carries nothing.
    OpenSubmenu,
    /// Convert the area to another type (roadmap 1.27). Unlike
    /// [`MenuAction::SetInput`] these are radio rows rather than a toggle, so
    /// the action carries the type the row *names*, and the row for the area's
    /// current type is drawn ticked. Clicking that one is harmless: the store
    /// reports nothing changed and no capture is discarded.
    SetType(AreaType),
    /// Remove the area.
    Dismiss,
    /// Capture the area and publish it to the clipboard alone (task 1.9,
    /// `Default` areas only: a typed capture area is 1.9b's).
    Copy,
    /// Capture the area and write it to `Pictures\UP-TAKE\` (task 1.9, same
    /// scope as `Copy`). A separate, explicit action. Does not also copy.
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
    /// still highlights, so it is a deliberate difference rather than a gap, but
    /// this doc used to claim otherwise for both modes.
    Hand,
    /// **The user's own arrow**, not a shape UP-TAKE ever wants to *show*, but
    /// the one it must be able to put back.
    ///
    /// [ADR-0025](../../../Projects/UP-TAKE/DECISIONS/ADR-0025-living-cursor-via-a-narrow-override.md)
    /// needs this. LIVING overrides `OCR_NORMAL` alone and undoes it by
    /// overriding again with the genuine arrow, because the alternative
    /// (`SPI_SETCURSORS`) measures 7.9 ms and broadcasts `WM_SETTINGCHANGE`
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
/// have unrelated epochs, and reconciling them is its own source of error, so
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
/// exist in a release build: a `cfg!` test compiles both arms and would fail to
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
/// **lower bound** on what the eye sees, the right tool for telling our own
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
/// command that reaches it is registered unconditionally: in release nothing
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

/// The menu's geometry in physical px: every rectangle is already laid out
/// here, so the frontend positions rows rather than computing them.
///
/// # Every field name here is one word, and that is a decision
///
/// `UT-F-72`: `HoverPayload` needs `#[serde(rename_all = "camelCase")]` to reach
/// the frontend at all, and that attribute is the only one in `src-tauri`. When
/// `UT-F-72` was found, removing it left **every gate in this repository green**
/// while the frontend silently read `undefined`. ⚠️ **That is no longer true for
/// this struct**: `#56` merged a covering test and `cargo test` goes red on it
/// now (measured 2026-08-22). It remains true for the other eleven payload
/// types, which have no such test, and the guard written for twice-written wire
/// names still says in its own doc that it cannot see a payload key. Roadmap 1.28
/// adds three fields
/// to this payload, and a `has_children` among them would have joined that
/// class. A single lowercase word serializes identically under both conventions,
/// so `child` and `parent` need no attribute and cannot be broken by deleting
/// one. It is a narrower fix than `I-67`'s and does not replace it: the class is
/// still open for every payload that *does* need a rename.
#[derive(Serialize, Clone)]
struct MenuView {
    rect: (i32, i32, u32, u32),
    items: Vec<MenuItemView>,
    /// The row under the cursor, for the highlight.
    hovered: Option<usize>,
    /// The open child list, drawn on top of the rows beneath it (roadmap 1.28).
    child: Option<ChildMenuView>,
}

/// The open child list of one parent row.
///
/// Not a `MenuView` holding another `MenuView`: menus here are exactly two deep
/// (`menu_rows` builds one level of children and nothing builds a third), and a
/// recursive payload would advertise a nesting the hit-testing does not
/// implement. A reader who sees `Option<Box<MenuView>>` reasonably expects
/// arbitrary depth to work.
#[derive(Serialize, Clone)]
struct ChildMenuView {
    rect: (i32, i32, u32, u32),
    items: Vec<MenuItemView>,
    /// The child row under the cursor.
    hovered: Option<usize>,
    /// The index in [`MenuView::items`] of the row this list belongs to, so the
    /// frontend can draw that row as **open** independently of what is hovered.
    ///
    /// Separate from `hovered` because both are true at once and neither is the
    /// other: while the pointer is inside this list, no top-level row is
    /// hovered, and the row that opened it must still read as the one it came
    /// from. An earlier build spent `hovered` on both and the parent went dark
    /// whenever the pointer crossed any other top-level row, leaving the list
    /// beside it with nothing pointing at it.
    owner: usize,
}

/// One drawn menu row.
#[derive(Serialize, Clone)]
struct MenuItemView {
    rect: (i32, i32, u32, u32),
    label: &'static str,
    /// Whether to show a tick: this is the area's current tier.
    checked: bool,
    /// Whether this row opens a child list, so the frontend draws the marker
    /// that says so. Never true of a row in a child list.
    parent: bool,
}

/// Which area the cursor is over, so its chrome can be revealed on hover.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct HoverPayload {
    id: Option<u64>,
    /// Reveal the close control, but do **not** light the area up.
    ///
    /// Set when the cursor is merely inside a pass-through area rather than on
    /// something it would grab. The highlight means *this is what a press will
    /// take*, which that body will not honour, so the two facts cannot share one
    /// field. See `overlay::LivingPointer::chrome_only`.
    chrome_only: bool,
}

/// Enters placement: install the mouse hook and override the cursor, on the
/// event-loop thread. Idempotent: summoning an already-placing overlay is a
/// no-op for the hook and simply re-asserts the cursor.
pub fn enter(app: &AppHandle) {
    // First entry wins; later ones are the same handle, so ignore the result.
    let _ = APP.set(app.clone());
    if let Err(error) = app.run_on_main_thread(enter_placement_on_main_thread) {
        eprintln!("placement: could not schedule hook install on the main thread: {error}");
    }
}

/// Enters Living: the hook stays (or gets) installed for per-area routing
/// (ADR-0016), the cursor override is dropped (the apps own the pointer) and
/// any half-done placement gesture or open menu is cleared. Runs on the
/// event-loop thread. Idempotent.
pub fn enter_living(app: &AppHandle) {
    let _ = APP.set(app.clone());
    if let Err(error) = app.run_on_main_thread(enter_living_on_main_thread) {
        eprintln!("placement: could not schedule Living entry on the main thread: {error}");
    }
}

/// Leaves every visible state: marks the hook's mode `Hidden` and either
/// uninstalls the hook and restores the cursor immediately, or (if a button it
/// swallowed is still physically held) defers that until the pending release
/// is seen (see the module docs on abandoned gestures). Runs on the event-loop
/// thread. Idempotent.
pub fn exit(app: &AppHandle) {
    if let Err(error) = app.run_on_main_thread(exit_on_main_thread) {
        eprintln!("placement: could not schedule placement exit on the main thread: {error}");
    }
}

/// Clears any cursor override left installed by an earlier process, and is safe
/// when there is none: reloading the registry cursors over identical ones is a
/// no-op. Called once at startup; see the note on [`snapshot_cursor`] for why
/// this also protects the snapshot's correctness.
pub fn clear_cursor_residue() {
    restore_system_cursors();
}

/// Restores the system cursors and removes the hook unconditionally (the
/// graceful-shutdown path, called from `RunEvent::Exit`). The process is
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

/// Whether a placement drag is currently in progress, read by
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
    // The pre-capture this drag may have started is now waste: ADR-0022 calls
    // it "wasted but harmless", which is true of the *work* and not of the
    // memory: a held 4K frame is 33 MB, and leaving it would keep that resident
    // until the next drag happened to replace it.
    precapture::discard();
}

/// Arms `kind` for the next drag (ADR-0018 §1), replacing anything already
/// armed. Pressing a second direct key changes your mind rather than erroring.
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
/// Called on the `Esc` ladder's middle rung and after a create. Idempotent:
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
    /// Whether the last reported hover was chrome-only. Part of the compared
    /// state, not a derived extra: the cursor can move from an area's chrome to
    /// its body without changing the id, and that transition has to reach the
    /// frontend or the highlight sticks.
    hovered_chrome_only: bool,
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
    /// different monitor meanwhile, the common case, since the user usually
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
/// and keeps the store lock off the mouse's critical path: hover classification
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
/// during a drag is this poll: the `WH_MOUSE_LL` callback sees discrete events,
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

/// Consecutive silent ticks before the hook is presumed dead. Three is ~50 ms:
/// long enough that a single dropped frame is not a diagnosis.
const SILENT_TICKS_BEFORE_REINSTALL: u32 = 3;

/// Notices that the mouse hook has stopped delivering events and reinstalls it.
///
/// **Windows removes a low-level hook without telling anyone.** It does so when
/// a callback overruns `LowLevelHooksTimeout`, and a hook in a medium-integrity
/// process is starved while a higher-integrity window holds the foreground
/// (UIPI, F-25, and the reason interacting with Task Manager used to leave the
/// overlay inert until the user toggled Placement off and on again). In both
/// cases [`HOOK`] still holds a handle, so [`install_on_main_thread`] believes
/// there is nothing to do and the overlay stays in Placement with no working
/// input for the rest of the session.
///
/// The detection needs no timers: if the real cursor has moved since the last
/// tick and the hook has not counted a single event, the hook is not receiving
/// input. Comparing positions alone would not do: during fast motion the
/// hook's last reported point legitimately lags the polled one, which is why
/// this compares the *event counter* against its own previous value.
///
/// Reinstalling cannot defeat UIPI, and does not try to: while Task Manager is
/// focused, input still belongs to it. What it restores is the state
/// *afterwards*, so the overlay works again the moment the elevated window is
/// no longer in front, instead of needing the hotkey twice.
fn pump_hook_health(app: &AppHandle, state: &mut PumpState) {
    // Guarded by mode, not by "is the hook installed": the whole point is to
    // notice a hook that *should* be installed and silently is not. Living
    // needs this exactly as much as Placement: a dead hook there means every
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
            // The main thread is not servicing its queue, which is itself the
            // reason the hook died, if something has put it in a modal loop.
            // Nothing here can fix that from another thread: installing and
            // removing a low-level hook are both thread-affine to the event
            // loop.
            eprintln!("placement: could not schedule a hook reinstall: {error}");
        }
    }
}

/// The monitor the warm sessions should be following, as a packed `(x, y)`, or
/// `None` when no resync is wanted. Written by the poll, read by the worker.
static WANTED_WARM_POINT: Mutex<Option<Point>> = Mutex::new(None);

/// Whether a resync worker is currently running.
static RESYNC_RUNNING: AtomicBool = AtomicBool::new(false);

/// Moves a warm-session resync off the caller's thread, coalescing crossings.
///
/// # Why a worker and not a spawn per crossing
///
/// A rebuild blocks for as long as a pump takes to hand back its handshake, up
/// to a second in the pathological case, and the caller is the poll thread that
/// owns §1's 8 ms drag row. Spawning per crossing would fix the stall and
/// replace it with a worse problem: a pointer swept across four monitors would
/// have four threads calling `warm::start` concurrently, each tearing down what
/// the last built, against one `SESSIONS` lock.
///
/// So at most **one** worker runs, and a crossing that arrives while it is
/// working only overwrites the target. The worker re-reads the target after each
/// pass and runs again if it moved, so the last crossing always wins and the
/// intermediate ones are skipped rather than queued, which is the correct
/// reading of a sweep: the user is going somewhere, not visiting.
///
/// **What this deliberately does not do is make the target warm any sooner.** A
/// rebuilt session is not warm for ~330 ms (`warm`'s module docs), so a
/// `Ctrl+Space` pressed immediately after arriving on a monitor still takes the
/// cold path. That window is the honest cost of narrowing and it is recorded in
/// ADR-0026's third amendment rather than hidden here.
/// # It can outlive Placement, and that is handled where the rule lives
///
/// The worker is **detached** and a rebuild blocks for up to a second, so the
/// user can leave Placement while it is working, and this thread has no
/// `AppHandle` and cannot be cancelled. It therefore calls
/// [`crate::freeze::resync_warm_sessions`], which reads the overlay state itself
/// on both sides of the rebuild rather than taking anyone's word for it, and
/// stops what it built if Placement went in between. `I-29`.
///
/// **The cancellation deliberately does not live here.** Making this loop check
/// a flag would put "warm sessions are never held outside Placement" in a second
/// place, which is the two-implementations defect `freeze::Scope`'s own docs
/// record as `bug_003`'s shape. This thread decides *when* to rebuild; `freeze`
/// decides *whether* one is allowed.
///
/// **Both the flag and the target are written under `WANTED_WARM_POINT`'s
/// lock**, and that pairing is the whole of what makes a crossing undroppable.
/// Clearing the flag outside it leaves a window where the worker has seen no
/// target, has not yet cleared, and a crossing arrives: the setter would find
/// the flag still set, decline to spawn, and the crossing would be lost with no
/// worker left to notice. Same shape as `warm::stop`'s reason for holding its
/// lock across the `WM_QUIT` post.
fn resync_warm_off_thread(point: Point) {
    let mut wanted = lock(&WANTED_WARM_POINT);
    *wanted = Some(point);
    if RESYNC_RUNNING.swap(true, Ordering::SeqCst) {
        // A worker is already running and will pick up the point just written.
        return;
    }
    drop(wanted);
    std::thread::spawn(|| {
        loop {
            let next = {
                let mut wanted = lock(&WANTED_WARM_POINT);
                match wanted.take() {
                    Some(point) => point,
                    None => {
                        RESYNC_RUNNING.store(false, Ordering::SeqCst);
                        return;
                    }
                }
            };
            // `resync_warm_sessions` rather than `sync_warm_sessions(true, …)`:
            // this thread does not know that Placement is still up, it only
            // knows it was when the crossing was recorded. `I-29`.
            //
            // The outcome is discarded on purpose and the `let _` says so: this
            // worker has nothing to do differently for any of the three, and the
            // operator's signal is the line `resync_warm_sessions` prints. The
            // return value exists for the tests. `#[must_use]` on `Resync` is
            // what makes this a decision rather than an oversight.
            let _ = crate::freeze::resync_warm_sessions(Some(next));
        }
    });
}

/// The cursor position as the OS reports it, independent of the hook.
///
/// **The one cursor read in this codebase**, used by the gesture recovery below,
/// by the poll's seeding on entry, and by `overlay::toggle_freeze` to decide the
/// freeze scope. A second copy of this function was added to `overlay.rs` for
/// that last caller and deleted in the same review that found the seeding bug:
/// two readers of one fact is what puts them on different monitors.
pub(crate) fn real_cursor(app: &AppHandle) -> Option<Point> {
    let position = overlay::overlay_window(app).ok()?.cursor_position().ok()?;
    Point::from_physical_f64(position.x, position.y)
}

/// Replaces a hook that is no longer delivering events. Runs on the event-loop
/// thread, the only one that may install or remove one.
///
/// The live gesture itself ([`DRAGGING`]/[`GESTURE`]) is discarded rather than
/// carried over: a hook that missed events may have missed the cursor moving
/// too, so the in-progress rectangle can no longer be trusted. The abandoned
/// gesture is then treated the same way [`cancel_drag`] treats one. Its
/// eventual release is still swallowed (see below); it just commits nothing.
///
/// [`LEFT_PENDING`]/[`RIGHT_PENDING`] are **not** blindly cleared, though:
/// neither "clear" nor "keep" is safe on its own here. Clearing would leak the
/// pending button's release the moment the button is still physically held:
/// the down was genuinely swallowed while the hook was alive, and nothing else
/// will ever swallow the matching up (the reinstall-time version of the leak
/// the module docs describe for cancel/toggle-away). Keeping it regardless
/// risks the opposite: swallowing an unrelated future release if the button
/// cycled up and back down again while the hook was dead. [`GetAsyncKeyState`]
/// resolves the ambiguity the same way [`snapping_suppressed`] reads `Alt`:
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
    // Only the hook is re-created. The mode and the cursor override are
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
        // for it. The probe costs an extra IPC round trip per sampled frame,
        // ~27 a second at the measured 221 Hz, which is load added to the exact
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
        // path: the styling was never stored, only derived from the live
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
/// The menu hover highlight runs in every visible mode: the area menu is
/// reachable from `Living` too (ADR-0016), and a menu whose rows never light up
/// reads as a picture rather than a control.
///
/// # Both halves run in `Living` now, and this paragraph used to deny it
///
/// It read *"the cursor override and the area hover chrome remain
/// `Placement`-only ... hover chrome would advertise gestures (move, resize)
/// that only `Placement` offers"*. Task 1.17(a) gave `Living` its own move and
/// resize, which removed the premise, and ADR-0025 scoped the cursor override to
/// `OCR_NORMAL` alone so it no longer reaches inside the user's apps. The
/// sentence outlived both by three weeks and was found by the independent review
/// of `#56`, arguing against the code directly beneath it.
///
/// What survives from it is the *reason*, and it is now enforced rather than
/// promised: chrome must not advertise a gesture the state declines. That is why
/// a hover resolved by containment alone travels as `chrome_only` and gets the
/// close control without the highlight.
/// A hover reported as *nothing under the pointer*.
const NO_HOVER: (Option<u64>, bool) = (None, false);

/// What the `Living` hover should report this tick: the area id, and whether the
/// hover is chrome-only.
///
/// Extracted from [`pump_hover`] so it can be drilled. It is three decisions and
/// each one has been wrong at least once:
///
/// 1. **An open menu owns the pointer**, so nothing under it is hovered.
/// 2. **A live gesture FREEZES the hover rather than clearing it.** This returned
///    `NO_HOVER` for one commit, and the independent review of `#56` caught what
///    that does: pressing a close control set a `Gesture::Close`, the next poll
///    tick reported nothing hovered, and the control was **removed from under the
///    cursor while the button was held on it**. `Gesture::Close`'s own contract is
///    that "the release must land on the control it started on", so hiding the
///    control hid the target the gesture requires. Freezing also solves what the
///    clear was reaching for: the hover cannot wander onto a large Filter the
///    cursor happens to cross mid-drag, because it does not move at all.
/// 3. **Otherwise report what was resolved.**
const fn living_hover(
    menu_open: bool,
    gesture_live: bool,
    resolved: (Option<u64>, bool),
    previous: (Option<u64>, bool),
) -> (Option<u64>, bool) {
    if menu_open {
        return NO_HOVER;
    }
    if gesture_live {
        return previous;
    }
    resolved
}

/// How long the pointer must argue for a child list before it opens.
///
/// Roadmap 1.27 named the hazard at both ends: an instant open flickers as the
/// cursor crosses the parent row on its way somewhere else, and a slow one reads
/// as broken.
///
/// **These two numbers are the rig's to settle** -- they are a judgement about
/// feel, and nothing in a unit test can have an opinion about them. What the
/// tests below pin is that the delay is *obeyed*, never its value.
///
/// ⚠️ **This paragraph cited Windows' `MenuShowDelay` as "400 ms for both
/// edges" and both halves were unsound.** There is one such value under
/// `HKCU\Control Panel\Desktop` and it governs the *show* edge only, so there
/// was nothing for the close edge to be the default of. It was an unprobed claim
/// about a system this repository does not own, cited to justify a number it
/// cannot justify. Removed rather than corrected, because the rig is the
/// authority here and a borrowed default was never going to be.
///
/// ⚠️ **The correction that replaced it named a reading from one machine's
/// registry, and that was the same defect one layer down**: a claim about
/// foreign state, unprobed, un-registered, and true only on the day it was
/// typed. It is struck rather than restated. Whatever that key holds anywhere,
/// it is not evidence about these two constants, which is the point the
/// paragraph above already makes.
const SUBMENU_OPEN_MS: u64 = 220;

/// How long the pointer must argue against the open child list before it closes.
///
/// Longer than the open delay, and that asymmetry is the diagonal-travel
/// allowance rather than a hedge: a pointer moving from the parent row to a
/// child row spends these milliseconds over rows that argue *close*, and it must
/// be able to arrive before they are acted on. See [`AreaMenu::argument`].
const SUBMENU_CLOSE_MS: u64 = 400;

/// Milliseconds since [`EPOCH`], the process's one monotonic clock.
fn elapsed_ms() -> u64 {
    u64::try_from(EPOCH.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// What the child list should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmenuStep {
    /// Open the child list of this top-level row, replacing any other.
    Open(usize),
    /// Close the open child list.
    Close,
    /// Leave it as it is.
    Hold,
}

/// Advances the child list's clock by one tick: what to do now, and the
/// argument state to carry into the next tick.
///
/// Extracted so it can be drilled, the same reason [`living_hover`] is, and
/// **the restart is inside it rather than at the call site on purpose**. The
/// diagonal-travel allowance is not the two constants; it is the interaction
/// between them and the restart, so a version of this function that took an
/// already-computed `elapsed` would leave the only interesting half in
/// [`pump_menu`], where no test can step a clock. It was written that way first.
///
/// The `argument == open` arm is not an optimisation. Without it a pointer
/// resting inside an already-open child list would re-`Open` it every tick past
/// the delay, rebuilding its rectangles and dropping its hovered row each time.
fn submenu_step(
    now_ms: u64,
    argument: Option<usize>,
    open: Option<usize>,
    since: (Option<usize>, u64),
) -> (SubmenuStep, (Option<usize>, u64)) {
    // A changed argument starts its own clock. Every tick the pointer spends
    // over the rows between a parent row and its list argues *close* and costs
    // time and nothing else, which is what lets it arrive.
    let since = if since.0 == argument {
        since
    } else {
        (argument, now_ms)
    };
    if argument == open {
        return (SubmenuStep::Hold, since);
    }
    let elapsed = now_ms.saturating_sub(since.1);
    let step = match argument {
        Some(parent) if elapsed >= SUBMENU_OPEN_MS => SubmenuStep::Open(parent),
        None if elapsed >= SUBMENU_CLOSE_MS => SubmenuStep::Close,
        _ => SubmenuStep::Hold,
    };
    (step, since)
}

/// Which parent row the pointer at `point` argues should have its list open, or
/// `None` for none of them.
///
/// **Being anywhere inside the open child list argues for that list**, including
/// its padding, and that is what makes it dismissable without being fragile: a
/// pointer that has arrived is not asked to stay on a row.
fn submenu_argument(menu: &AreaMenu, point: Point) -> Option<usize> {
    if let Some(open) = menu.open.as_ref()
        && open.bounds.contains(point)
    {
        return Some(open.parent);
    }
    menu.items
        .iter()
        .position(|item| item.rect.contains(point) && !item.children.is_empty())
}

/// Where `point` falls in an open menu.
///
/// **The child list is tested first, because it draws on top.**
///
/// ⚠️ **This said the two lists "do not overlap today, so the order is
/// currently unobservable", and that is false.** Measured by the independent
/// review of `1.28` over the real [`interaction::menu_bounds`] and
/// [`interaction::submenu_bounds`]: below roughly `2 x MENU_WIDTH` of monitor
/// width the left-flip clamps the child list into the parent's rectangle, and
/// they overlap.
///
/// ```text
/// monitor width   menu.x   child.x   child.right   overlap
///           200       23         0           176   yes
///           300      123         0           176   yes
///           400      223        47           223   no
/// ```
///
/// No real monitor is that narrow, so nothing is broken. But the ordering is
/// **already load-bearing at the margin** rather than merely prudent, which is a
/// better argument for it than the one this comment used to make -- and a reader
/// who checks the old premise finds it false and concludes the constraint has
/// lapsed. Same failure as the `monitor_bounds_at` note in [`pump_menu`], found
/// in the same review.
fn menu_hit(menu: &AreaMenu, point: Point) -> Option<MenuHit> {
    if let Some(open) = menu.open.as_ref() {
        if let Some(index) = open.items.iter().position(|item| item.rect.contains(point)) {
            return Some(MenuHit::Child(index));
        }
        // Inside the child list but on its padding: it belongs to the child
        // list, and must not resolve to whatever is drawn beneath it.
        if open.bounds.contains(point) {
            return None;
        }
    }
    menu.items
        .iter()
        .position(|item| item.rect.contains(point))
        .map(MenuHit::Row)
}

/// Points the hover highlights at `hit`, returning whether either moved.
///
/// **Hover is hover: exactly the row under the pointer, or none.** Which row
/// owns the open child list is a separate fact and travels separately, as
/// [`ChildMenuView::owner`], because the two are independently true and one
/// `Option<usize>` cannot carry both.
///
/// ⚠️ **This function used to answer `open_parent` on the `Child` and `None`
/// arms**, under a doc comment claiming a parent row "stays lit for as long as
/// its list is open, whatever the pointer is doing". Its own `Row` arm
/// falsified that in the ordinary case: hover any other top-level row while the
/// list is open and the parent went dark while its list stayed up for the whole
/// of [`SUBMENU_CLOSE_MS`], which is precisely the orphaned list the sentence
/// said must not happen, on precisely the path the diagonal-travel grace exists
/// to allow. Found by the independent review of `1.28`, which drilled the arm
/// the test did not cover. The invariant was worth keeping and the mechanism
/// was wrong: a highlight borrowed from another row cannot express it.
fn apply_menu_hover(menu: &mut AreaMenu, hit: Option<MenuHit>) -> bool {
    let (row, child) = match hit {
        Some(MenuHit::Child(index)) => (None, Some(index)),
        Some(MenuHit::Row(index)) => (Some(index), None),
        None => (None, None),
    };
    let mut changed = false;
    if menu.hovered != row {
        menu.hovered = row;
        changed = true;
    }
    if let Some(open) = menu.open.as_mut()
        && open.hovered != child
    {
        open.hovered = child;
        changed = true;
    }
    changed
}

/// Everything one pointer tick decides about the open menu.
///
/// Returned by [`menu_pump_step`] so that the decisions are separable from the
/// two things that need an [`AppHandle`] -- resolving the monitor and emitting.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a tick that is computed and dropped leaves the frontend drawing a stale menu"]
struct MenuPump {
    /// Whether anything the frontend draws has changed.
    changed: bool,
    /// Where the pointer landed, so the caller can pick a cursor shape.
    hit: Option<MenuHit>,
    /// The top-level row whose child list this tick has earned, if any.
    to_open: Option<usize>,
}

/// Advances the open menu by one pointer tick, and decides nothing else.
///
/// # Why this is a function
///
/// **Every unit this composes was drilled and this composition was not.** The
/// independent review of `1.28` applied seven mutations to `pump_menu`'s body
/// and every one of them left all 316 tests green, five of them removing the
/// feature the roadmap row exists to ship: swapping the two arguments to
/// [`submenu_step`], dropping the clock advance, replacing the `Open` arm with
/// `Hold`, and disabling the emit each mean the child list never opens, or
/// opens and is never drawn. Twenty-four unit-level mutations went red in the
/// same pass. The units were never the problem.
///
/// So the locked block became this, taking `now` rather than reading the clock,
/// and returning the open decision rather than acting on it. That is the same
/// refactor this change already performed twice on itself -- `menu_payload` out
/// of `emit_menu`, `menu_covers` out of `menu_contains` -- and for the same
/// reason both times.
///
/// **What is still not reachable from a test**, and is held by
/// `pump_menu_composes_the_step_it_is_given` instead: resolving the monitor and
/// calling [`emit_menu`], both of which need an `AppHandle`.
fn menu_pump_step(menu: &mut AreaMenu, point: Point, now: u64) -> MenuPump {
    let hit = menu_hit(menu, point);
    let mut changed = apply_menu_hover(menu, hit);
    let argument = submenu_argument(menu, point);
    let (step, since) = submenu_step(
        now,
        argument,
        menu.open.as_ref().map(|open| open.parent),
        menu.argument,
    );
    menu.argument = since;
    if step == SubmenuStep::Close {
        menu.open = None;
        changed = true;
    }
    MenuPump {
        changed,
        hit,
        to_open: match step {
            SubmenuStep::Open(parent) => Some(parent),
            SubmenuStep::Close | SubmenuStep::Hold => None,
        },
    }
}

/// Advances the open menu for a pointer at `point`: the hover highlight in
/// whichever list holds it, and the child list's own open and close timing.
/// Emits when anything the frontend draws has changed, and returns where the
/// pointer landed so the caller can pick a cursor shape without asking twice.
///
/// The deciding is [`menu_pump_step`]'s; this holds the lock around it and does
/// the two things that need an `AppHandle`.
fn pump_menu(app: &AppHandle, point: Point) -> Option<MenuHit> {
    let MenuPump {
        mut changed,
        hit,
        to_open,
    } = {
        let mut guard = lock(&MENU);
        let menu = guard.as_mut()?;
        menu_pump_step(menu, point, elapsed_ms())
    };
    if let Some(parent) = to_open {
        // The monitor is resolved with `MENU` unlocked, matching `open_menu`,
        // which also resolves it before locking `MENU`. One nesting order for
        // this pair is cheaper than reasoning about whether a second is safe.
        //
        // ⚠️ **The reason given here until 2026-08-17 was that
        // `monitor_bounds_at` "takes the area store's lock". It takes no lock at
        // all** -- it reaches the window and the monitor list, and the area
        // store is never touched on that path. The conclusion survives the
        // correction and the argument for it does not, which is worse than
        // having no argument: a reader who checks the premise finds it false and
        // concludes the constraint has lapsed. Found by the independent review
        // of `1.28`.
        let monitor = overlay::monitor_bounds_at(app, point);
        changed |= open_submenu(parent, monitor);
    }
    if changed {
        emit_menu(app);
    }
    hit
}

fn pump_hover(app: &AppHandle, state: &mut PumpState) {
    if mode() == Mode::Hidden {
        return;
    }
    let point = Point::new(CUR_X.load(Ordering::SeqCst), CUR_Y.load(Ordering::SeqCst));

    // A menu, while open, owns the pointer above everything under it. Since
    // roadmap 1.28 that includes running the child list's open and close clock,
    // which is why this is a call rather than two lines: the hover highlight and
    // the timing read the same hit and must not resolve it separately.
    let menu_item = pump_menu(app, point);
    let placing = mode() == Mode::Placement;
    if !placing {
        // Forget the reported monitor so the next entry into Placement re-emits
        // it. See `PumpState::active_monitor`.
        state.active_monitor = None;
        // Living needs hover chrome (task 1.17(a)): areas are grabbable here now,
        // and a handle the user cannot see is a handle they will not reach for.
        //
        // Two questions, and until 2026-08-14 one answer served both: which area
        // would take a press, and which area the user is looking at. They differ
        // for exactly one thing on screen, the close control of a pass-through
        // area, and that is the one the founder could not find on the rig.
        // `overlay::living_pointer_at` answers both from one store snapshot, and
        // `LivingPointer::chrome_only` is how the second answer says it is not
        // also the first.
        //
        // A live gesture FREEZES the hover; it does not clear it. See
        // `living_hover`, which is where that decision lives and is tested.
        //
        // Read as two statements rather than one chain so neither lock is held
        // while the other is taken.
        let menu_open = lock(&MENU).is_some();
        let gesture_live = lock(&GESTURE).is_some();
        let pointer = (!menu_open && !gesture_live).then(|| overlay::living_pointer_at(app, point));
        let grabbed = pointer.as_ref().and_then(|p| p.grabbed);
        // Whether the hover is chrome-only travels with it, because the frontend
        // spends one id on two meanings: draw the control, and light the area to
        // show what a press would grab. Only the first is true of a pass-through
        // body, and sending the id alone lit every large Filter area permanently.
        let resolved = pointer
            .as_ref()
            .map_or(NO_HOVER, |p| (p.hovered.map(AreaId::get), p.chrome_only));
        let hover = living_hover(
            menu_open,
            gesture_live,
            resolved,
            (state.hovered_area, state.hovered_chrome_only),
        );
        // The cursor *is* the affordance (ADR-0025). A live gesture holds its
        // shape for the duration, matching Placement: it must not flicker between
        // move and resize as the pointer crosses an edge mid-drag.
        //
        // `None` hands the user's own arrow back: this is an `OCR_NORMAL`-only
        // override, so nothing else in their cursor table is touched. See
        // `set_living_cursor` for why the restore is not `SPI_SETCURSORS`.
        set_living_cursor(match *lock(&GESTURE) {
            Some(gesture) => Some(gesture_cursor(gesture)),
            None => grabbed.map(|(_, _, handle)| CursorShape::for_handle(handle)),
        });
        // Compared as one tuple rather than field by field: a hover that changes
        // only in `chrome_only` is a real change (the cursor crossing from an
        // area's chrome to its body at an unchanged id), and a comparison written
        // out per field is one a later edit can drop half of silently.
        if (state.hovered_area, state.hovered_chrome_only) != hover {
            (state.hovered_area, state.hovered_chrome_only) = hover;
            let _ = app.emit(
                HOVER_EVENT,
                HoverPayload {
                    id: hover.0,
                    chrome_only: hover.1,
                },
            );
        }
        return;
    }

    // Which monitor the per-monitor chrome belongs on (F-13). The armed-type
    // badge follows the cursor: showing it on every monitor at once (as the
    // first cut did) reads as "every screen is armed" and buries the one fact
    // the indicator exists to convey, which ADR-0018 §3 makes load-bearing.
    let active_monitor = overlay::monitor_index_at(point);
    if state.active_monitor != Some(active_monitor) {
        state.active_monitor = Some(active_monitor);
        overlay::emit_active_monitor(app, active_monitor);
        // The warm sessions follow the cursor for the same reason the badge does
        // (ADR-0026's third amendment): the held set is the monitor the freeze
        // will capture, and the pointer crosses monitor edges without any state
        // transition to notice it. Without this the narrowing would warm
        // whichever monitor Placement happened to open on and leave the user's
        // actual target cold, a change that provably does nothing, which is the
        // failure the amendment names by that name.
        //
        // **Gated on the change AND moved off this thread**, and the second half
        // is not belt-and-braces. Gating on the change bounds how *often* the
        // rebuild runs; it does nothing about what one costs. A rebuild posts
        // `WM_QUIT` to each held pump, spawns a thread, and then **blocks on that
        // pump's handshake for up to a second** (`warm::spawn_session`). This is
        // the click-through poll thread, which also publishes the live selection
        // rectangle every tick against §1's 8 ms drag row, so a crossing
        // mid-drag would have stalled the rectangle the user is dragging. Found
        // in PR #42's independent review, before it ran on hardware.
        //
        // `point` rather than a fresh cursor read, so the sessions and the badge
        // cannot end up describing different monitors: this is the position the
        // line above resolved `active_monitor` from, and since entering
        // Placement now seeds it from the OS, it is a real position on the first
        // tick rather than (0, 0).
        //
        // ⛔ This comment used to end: *"`true` rather than a state read because
        // this branch is already inside the `placing` guard above, reached only
        // in Placement."* **True of this line and false of the work it starts.**
        // The guard holds where the call is made; the rebuild runs on a detached
        // worker and finishes later, possibly after the user has left. That is
        // `I-29`, found in the independent review of #44, and the worker now
        // reads the state instead of being told it. See
        // `freeze::resync_guarded`. Kept struck rather than deleted, because the
        // sentence is a worked example of the thing this project keeps getting
        // wrong: a fact about the caller offered as a fact about the callee.
        resync_warm_off_thread(point);
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
    // Always `false` here: in Placement every area is grabbable whatever its
    // Input (`hit_test_any`), so the highlight is never a promise this state
    // declines to keep. Compared as a tuple for the same reason as the Living
    // side: leaving Living with a chrome-only hover live must re-emit even when
    // the id has not moved, or the frontend keeps drawing the control alone.
    if (state.hovered_area, state.hovered_chrome_only) != (hovered_area, false) {
        (state.hovered_area, state.hovered_chrome_only) = (hovered_area, false);
        let _ = app.emit(
            HOVER_EVENT,
            HoverPayload {
                id: hovered_area,
                chrome_only: false,
            },
        );
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
/// the event-loop thread. See the module docs on why that is mandatory.
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
/// Closes a menu left open by a **different** mode. The same reasoning
/// [`enter_living_on_main_thread`] applies to a menu opened in Placement,
/// applied symmetrically: `Living`'s menu is resolved against `hit_test`
/// (interactive areas only) and anchored to wherever it was right-clicked, so
/// carrying it into `Placement` would leave a stale control on screen that
/// swallows the next click (`classify_press`'s menu-first precedence) instead
/// of starting the gesture the user actually made. Gated on the *previous*
/// mode, not unconditional, because [`enter`] is also reached by a `Summon`
/// while already in `Placement` (`overlay_state::next` sends `Placement` to
/// `Placement` on that event), documented as idempotent, so a menu the user
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
    // there is superseded rather than leaked, but the cache has to agree, or the
    // next return to Living would skip the write that corrects it.
    *lock(&LIVING_CURSOR) = None;
    // Seed the hook's last-known point from the OS, because until this line it
    // was `(0, 0)` until the user moved the mouse.
    //
    // **Nothing wrote CUR_X/CUR_Y except the hook**, which only fires on
    // movement, so the first poll tick after entering Placement acted on the
    // origin of the virtual desktop rather than on the cursor. Two consequences,
    // and the second is what a review of PR #42 caught: the armed-type badge
    // opened on whichever monitor contains (0, 0) rather than the cursor's,
    // which is F-13's rule failing for one tick; and once the warm sessions
    // began following the cursor, that same tick tore down the correctly-warmed
    // monitor and rebuilt for the wrong one, so `Ctrl+Space` pressed without
    // moving the mouse took the cold path with the warm path switched on.
    //
    // `real_cursor` is the same read `toggle_freeze` uses to pick the freeze
    // scope, which is the point: seeding from anywhere else would leave the two
    // able to disagree at exactly the moment they must not.
    if let Some(app) = APP.get()
        && let Some(point) = real_cursor(app)
    {
        CUR_X.store(point.x, Ordering::SeqCst);
        CUR_Y.store(point.y, Ordering::SeqCst);
    }
}

/// Enters `Living` mode: hook kept for per-area routing (ADR-0016), cursor
/// override dropped, gesture state and menu cleared. Runs on the event-loop
/// thread.
///
/// A live gesture does not survive the transition (the hotkey was pressed
/// mid-drag, and the drag's meaning was a Placement meaning), but a *pending
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
    // Arming is Placement-only state and must not outlive it (ADR-0018 §2):
    // this is one of the three exits that guarantee "there is no mode to still
    // be in later".
    *lock(&ARMED) = None;
    // The menu is re-resolved per mode (its target resolution differs: see
    // `open_menu`), so a menu opened in Placement does not carry over.
    if let Some(app) = APP.get() {
        close_menu(app);
    }
    // Drop Placement's all-slots override: Living does not own the pointer, so
    // pinning `OCR_IBEAM` and friends would change the cursor inside the user's
    // apps. Restore only if one is actually applied: the registry reload is
    // global state other apps see, not a free no-op.
    //
    // Living then takes its *own*, far narrower override on hover: `OCR_NORMAL`
    // alone, via `set_living_cursor` (ADR-0025). The two are deliberately not the
    // same mechanism, and this is the boundary between them: the wide one has to
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
/// callback itself via [`maybe_finish_teardown`], which already runs on the
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
    // `SPI_SETCURSORS` is the right call *here*: it runs once, on the way out,
    // and puts every slot back including the `OCR_NORMAL` the Living path may
    // have overridden (ADR-0025). Its 7.9 ms only disqualifies it from the
    // per-hover path, not from teardown.
    restore_system_cursors();
    // The override is gone, so both caches must forget what they believe the OS
    // has. Otherwise the next entry would skip re-applying a shape that is no
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
/// tracks PLACEMENT's all-slots override: the two modes own different amounts of
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
/// override. This runs from the poll thread on every hover transition, at 221 Hz
/// during a gesture, so the reload would spend half of every tick on a global
/// broadcast and make the whole desktop stutter. That is the cost the old
/// `css_cursor` doc comment said had to be measured before taking this route.
fn set_living_cursor(shape: Option<CursorShape>) {
    let mut applied = lock(&LIVING_CURSOR);
    if *applied == shape {
        return;
    }
    // `None` is not "install nothing". It is "install the genuine arrow", which
    // is the whole reason `CursorShape::Arrow` exists.
    apply_cursor_to(shape.unwrap_or(CursorShape::Arrow), OCR_NORMAL);
    *applied = shape;
}

/// Private copies of the real system cursors, taken **before** the first
/// override and reused for every shape after it.
///
/// This indirection is not decoration; without it the cursor can only ever be
/// set once. [`SetSystemCursor`] replaces a cursor *globally*, and
/// [`LoadCursorW`] reads that same global table, so once `OCR_SIZEALL` has been
/// pointed at the crosshair, `LoadCursorW(IDC_SIZEALL)` hands back **the
/// crosshair**, and every later shape resolves to whatever is already showing.
/// Loading from the live table is self-defeating in the worst way: every call
/// succeeds, nothing logs, and the pointer simply never changes.
///
/// Stored as `isize` because a raw pointer is not `Sync`; `0` means that shape
/// failed to load and leaves the cursor alone. These handles are only ever
/// `CopyIcon`d, never passed to `SetSystemCursor` directly: the system destroys
/// what it is given, and destroying the snapshot would leave nothing to copy.
static CURSOR_SNAPSHOT: OnceLock<[isize; CURSOR_SNAPSHOT_LEN]> = OnceLock::new();

/// How many cursors the snapshot holds, **derived** from [`ALL_SHAPES`], never
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
        // its override was active: the crosshair it left behind is still
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
/// snapshot: passing the snapshot itself would have the system destroy the one
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

/// Overrides a **single** cursor slot, the LIVING path's unit of work
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
    // chrome in LIVING (the app's *resting* state), so the same failure recurs
    // per crossing rather than per session.
    //
    // What is **not** established is who owns `copy` when the call fails.
    // `SetSystemCursor` is documented as destroying the handle it is given; the
    // documentation does not say whether it still does so on failure. Adding a
    // `DestroyCursor` here would leak nothing if it does not, and double-destroy
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
/// job instead: on the panic path nothing will read it again anyway.
fn restore_system_cursors() {
    unsafe {
        SystemParametersInfoW(SPI_SETCURSORS, 0, ptr::null_mut(), 0);
    }
}

/// The `WH_MOUSE_LL` callback. Runs on the event-loop thread. Returning
/// `LRESULT(1)` without chaining **swallows** the event, so no window (the app
/// under the cursor included) ever sees the click.
///
/// A panic must not cross this FFI boundary: since Rust 1.81 an unwind out of an
/// `extern "system"` fn aborts the process (architecture §5, a dead tray app is
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
/// with it). Moves are **not** swallowed: blocking `WM_MOUSEMOVE` in a
/// low-level hook does not stop the cursor moving, and a passing hover under
/// the crosshair is harmless.
///
/// A button-down is only ever swallowed while [`ACTIVE`]; its balancing
/// button-up is swallowed **regardless** of whether placement is still active
/// by then, as long as [`LEFT_PENDING`]/[`RIGHT_PENDING`] says that down was
/// ours. Otherwise a drag cancelled or abandoned mid-gesture would leak its
/// eventual release to whatever window ends up under the cursor (see the
/// module docs on abandoned gestures). A release completes into an area only
/// if [`DRAGGING`] is *also* still set: a cancelled or abandoned drag cleared
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
    // here, before either button's handling, so neither path can forget it, and
    // only on a **press**, never on a move, so the per-event cost stays off the
    // hot path. See `shadowed_by_another_window`.
    //
    // This applies in Placement too, not just Living. Placement means "UP-TAKE
    // owns the mouse", but that was always shorthand for "UP-TAKE is the topmost
    // thing". When it demonstrably is not, swallowing the click is the same
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
                // than after it (ADR-0022). Spawns, never captures here; this
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
                // instead: leaving it would let the next press inherit it.
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
            // press the area menu will act on, or any right press while a menu
            // is open (the release will act on the menu state: close it, or
            // replace it over another area). Everything else is the user's,
            // untouched.
            //
            // **`pointer_target`, not a second copy of its rule.** This press and
            // the `open_menu` on the matching release have to agree exactly.
            // Where they do not, the hook claims a click that `open_menu` then
            // declines to act on: no menu appears *and* the application
            // underneath never receives the right-click it gets today, which is
            // strictly worse than not claiming at all.
            Mode::Living => {
                let claimed = lock(&MENU).is_some()
                    || APP
                        .get()
                        .is_some_and(|app| pointer_target(app, point).is_some());
                if claimed {
                    RIGHT_PENDING.store(true, Ordering::SeqCst);
                }
                claimed
            }
        },
        // Zoom (§3.4). The target resolves exactly as the area menu's does, and
        // for the same reason: in Placement the user is editing the layout, so
        // a pass-through area is still theirs to scroll; in Living ADR-0016
        // decision 3 governs, and a scroll over a pass-through body belongs to
        // the application underneath.
        //
        // **`pointer_target`, because that sentence was true and open-coded.**
        // This arm carried its own copy of the rule while claiming to agree with
        // the menu's, which is the drift the function exists to prevent, one call
        // site over from the two it was extracted for. Found by the independent
        // review of `#55`.
        WM_MOUSEWHEEL => match mode() {
            Mode::Hidden => false,
            _ => {
                let Some(app) = APP.get() else { return false };
                let target = pointer_target(app, point);
                // **Claimed on the type, not on whether the zoom moved.** A
                // scroll held at the ceiling changes nothing and must still be
                // swallowed: passing it through would scroll the document under
                // a magnified area, which is the one thing the user cannot see
                // happening.
                let Some(area) = target.filter(|area| area.kind.supports_zoom()) else {
                    return false;
                };
                // **Checked here, after the area resolves, and NOT in the
                // press guard above.** `WM_MOUSEWHEEL` was added to that guard
                // at first, which an independent review caught: the z-order
                // walk is measured at up to 2.77 ms and the guard runs before
                // any handler, so a precision touchpad emitting ~100 events a
                // second would have paid it on every scroll anywhere on the
                // machine while the overlay was visible, including sub-notch
                // events and scrolls over no area at all. Here it runs only
                // when a zoomable area is actually under the cursor, which is
                // the only case where the answer changes anything, and the
                // guard's own comment stays true.
                if shadowed_by_another_window(point) {
                    return false;
                }
                let notches = wheel_notches(area.id.get(), info.mouseData);
                if notches != 0 {
                    overlay::zoom_area(app, area.id, notches);
                }
                true
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

/// One notch of a notched wheel, in the signed type the arithmetic needs.
///
/// `windows_sys` types `WHEEL_DELTA` as `u32`, and every quantity it is
/// compared against here is signed, so a cast would be needed at each use. The
/// assertion below is what keeps this a restatement rather than a second
/// source: it fails to compile if the two ever disagree.
const WHEEL_STEP: i32 = 120;
const _: () = assert!(WHEEL_STEP as u32 == WHEEL_DELTA);

/// Wheel movement that has not yet added up to a whole notch.
///
/// **A mouse wheel is not the only thing that sends `WM_MOUSEWHEEL`.** A
/// notched wheel sends exactly `WHEEL_DELTA` (120) per click, and dividing by
/// it is all that is needed. A precision touchpad and a free-spinning wheel
/// send *fractions* (8, 17, 40), and integer division alone would floor every
/// one of them to zero notches. The area still swallows the event, so without
/// this the product would eat every touchpad scroll over a Default area and
/// magnify nothing: a dead area rather than a visible bug.
static WHEEL_RESIDUE: AtomicI32 = AtomicI32::new(0);

/// Which area [`WHEEL_RESIDUE`] was accumulated over. `0` is no area, because ids are
/// issued from 1 (`AreaStore::new`), so the sentinel cannot collide with one.
///
/// Residue is per-area because it is a partial gesture: carrying half a notch
/// from one area into the next would make the second area jump on a scroll too
/// small to move the first.
static WHEEL_RESIDUE_AREA: AtomicU64 = AtomicU64::new(0);

/// Whole scroll notches, accumulating anything left over for the next event.
///
/// `mouse_data`'s high word is the signed wheel delta; the low word is
/// undefined for `WM_MOUSEWHEEL` and must be discarded rather than sign-
/// extended along with it.
fn wheel_notches(area: u64, mouse_data: u32) -> i32 {
    // The documented layout of `MSLLHOOKSTRUCT.mouseData`: the high word is a
    // signed multiple of `WHEEL_DELTA`. Narrowing to `i16` first is what makes
    // the sign right, and it cannot lose information: the shift has already
    // put the whole delta in the low 16 bits.
    let delta = i32::from((mouse_data >> 16) as i16);
    // A different area starts a fresh accumulation rather than inheriting the
    // last one's part-notch.
    let previous = WHEEL_RESIDUE_AREA.swap(area, Ordering::SeqCst);
    let carried = if previous == area {
        WHEEL_RESIDUE.load(Ordering::SeqCst)
    } else {
        0
    };
    let total = carried.saturating_add(delta);
    WHEEL_RESIDUE.store(total % WHEEL_STEP, Ordering::SeqCst);
    total / WHEEL_STEP
}

/// Starts the pre-capture, if this press begins a drag that will capture.
///
/// Three conditions, all of them necessary. The gesture must be
/// [`Gesture::Create`]. A move, a resize, a close or a menu press produces no
/// capture, and pre-capturing for one would spend a full-monitor capture on
/// every click in Placement. The armed type must be one that captures on
/// create, read through the same [`captures_on_create`] predicate the release
/// path uses. And the cursor must be on a monitor: in a dead zone between
/// mismatched monitors there is nothing to pre-capture, and picking a neighbour
/// would hold a frame that every crop then declines.
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
/// under the point claims the press and is raised (§3.2a, touching an area
/// puts it on top of its tier, applied on the press like every window
/// manager); otherwise the press belongs to the user's apps and passes
/// through untouched. Returns whether the event is swallowed.
///
/// Living never starts drags: moving and resizing are `Placement` gestures,
/// so [`DRAGGING`] is set only for the menu-row press, where the existing
/// release path ([`finish_gesture`] → [`activate_menu_item`]) implements the
/// press-and-release-on-target contract. A raised area's press needs nothing
/// on release beyond being swallowed, which [`LEFT_PENDING`] alone provides.
fn living_lbutton_down(point: Point) -> bool {
    if menu_contains(point) {
        let gesture = match menu_item_at(point) {
            Some(hit) => Gesture::MenuItem { hit },
            // Menu padding, in either list: a press that does nothing, rather
            // than one that falls through to whatever is underneath the menu.
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
        // on (the standard contract), so it is swallowed even when it sits
        // over the user's app.
        LEFT_PENDING.store(true, Ordering::SeqCst);
        return true;
    }
    // Task 1.17(a): a press on an interactive area begins a real gesture here,
    // not just a raise. The routing for this already existed: the hook runs in
    // every visible state (ADR-0016) and this function already resolved and
    // raised the area under the cursor, so the only thing missing was calling
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
    // against whatever anchor the *last Placement drag* had left behind, a
    // constant offset, which is why every area jumped the same way on the first
    // click instead of following the cursor.
    START_X.store(point.x, Ordering::SeqCst);
    START_Y.store(point.y, Ordering::SeqCst);
    CUR_X.store(point.x, Ordering::SeqCst);
    CUR_Y.store(point.y, Ordering::SeqCst);
    // Raise first, so the gesture acts on an area that is already topmost:
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
/// windows, not thousands"*. Counted on the dev rig: **384-418** windows in the
/// desktop chain, only ~30 of them visible: hidden top-level windows sit in the
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
/// that is free.** Timed on the rig, the worst case, a full walk from the
/// bottom-most window of a 418-long chain, is **417 steps in 2.77 ms**, i.e.
/// ~1 % of the default 300 ms `LowLevelHooksTimeout`, and it runs on presses
/// only, never on moves. 8192 leaves ~20× headroom over the observed chain while
/// still bounding a corrupted one at roughly 54 ms.
///
/// The lesson worth keeping: **a cheaper algorithm that has not been run is not
/// an optimisation, it is an untested rewrite**, and this one traded a verified
/// behaviour for a 2.8 ms saving nobody had asked for.
const Z_ORDER_WALK_LIMIT: usize = 8192;

/// Whether some other window sits **above** the overlay at `point`, and should
/// therefore receive this press instead of us.
///
/// # The bug this exists for
///
/// The hook claims input by **screen position**, and was written assuming our
/// areas are the topmost thing at their coordinates, true, because the overlay
/// is always-on-top. **Shell surfaces break that assumption**: the Start and
/// search popups sit above even topmost windows, so a click over an area that
/// happens to be behind Start was swallowed by us and never reached it. With an
/// area covering the screen, Start became entirely unusable. Found on the rig
/// 2026-07-26.
///
/// Deliberately general rather than a check for Start specifically: any window
/// that gets above us has the same claim, and a class-name test would fix one
/// instance of the bug and rot across Windows builds. Same family as F-25, a
/// hook claiming input it has no right to.
///
/// # Why the obvious test does not work
///
/// `WindowFromPoint` **skips `WS_EX_TRANSPARENT` windows**, and the overlay is
/// transparent in every visible state (ADR-0016). So it never returns our
/// window, and "is the window under the cursor ours?" can only ever answer no.
/// What it does return is the window that *would* receive the click, so the
/// real question is whether that window is above us or below us, which is a
/// z-order walk: step upward **from the hit window** and see whether we are
/// passed on the way.
///
/// # Walk from the hit window, not from ours
///
/// This looks like it should be reversible (the two windows are in one chain, so
/// either end could answer "which is higher?"), and starting from our own
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
/// before: degrading to the previous behaviour beats dropping the user's input
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
    // The walk ran long: treat it as inconclusive and keep the press, rather
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
            Some(hit) => Gesture::MenuItem { hit },
            // Inside the menu but on its padding, in either list: a press that
            // does nothing, rather than one that falls through to the area
            // underneath.
            None => Gesture::Inert,
        };
    }
    if let Some(app) = APP.get() {
        // A click anywhere outside an open menu dismisses it, and does not also
        // act on what it landed on (the standard contract), which is what makes
        // a mis-click cheap.
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
/// Called only when [`DRAGGING`] was still set: a cancelled or abandoned
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
            // make the *next* drag (possibly minutes later, possibly meant as
            // a plain Default area) silently produce a Screenshot, which is
            // exactly the "which mode am I in?" failure the ADR is avoiding.
            let kind = armed().unwrap_or(AreaType::Default);
            disarm();
            let created = overlay::create_area(app, kind, x, y, width, height);
            // Logged so a placement problem is an observation rather than a
            // guess (the F-15 lesson), and logged *after* the attempt, with its
            // outcome. Printing "created area" before the call claimed a
            // creation that had not happened yet and sometimes never did: an
            // empty drag produced `created area 0x0`, which is precisely the
            // sort of confidently wrong log line that sends a later debugging
            // session in the wrong direction.
            //
            // The coordinate space itself is settled: hardware testing confirmed
            // `MSLLHOOKSTRUCT.pt` matches `cursor_position`, the space the
            // store and click-through regions use, across every monitor, the
            // 125% primary included.
            #[cfg(debug_assertions)]
            if created.is_some() {
                eprintln!("placement: created {kind:?} area {width}x{height} at ({x}, {y})");
            } else {
                eprintln!("placement: drag at ({x}, {y}) was {width}x{height}, nothing created");
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
            let moved = overlay::move_area(app, id, Rect::new(x, y, width, height));
            if moved {
                // A magnified area holds a still of the screen *inside its own
                // bounds*, so moving or resizing it leaves it showing a picture
                // of where it used to be: sharp, plausible, and of the wrong
                // place. Re-taken here, at the release, rather than per
                // mouse-move: the drag itself is far inside `LowLevelHooksTimeout`
                // and the frames in between are not what the user is looking at.
                overlay::refresh_magnification(app, id);
            }
            moved
        }
        // A press-and-release contract: the release must land on the control it
        // started on. Sliding off cancels, which is how a user takes back a
        // dismissal they have already begun.
        Gesture::Close { id, control } => {
            control.contains(release) && overlay::dismiss_area(app, id)
        }
        Gesture::MenuItem { hit } => return activate_menu_item(app, hit, release),
        Gesture::Inert => return,
    };
    if changed && let Err(error) = overlay::emit_areas(app) {
        eprintln!("placement: applied a gesture but could not emit the new set: {error}");
    }
}

/// Dispatches the capture a freshly created area needs, if its type has one.
///
/// Only `Screenshot` captures on create: ADR-0018 settles that for the one type
/// it decided, and the rest have no gesture yet. Written as an explicit match
/// rather than as another `AreaType` method because "does creating this capture
/// pixels?" has exactly one answer today, and inventing a fourth per-type axis
/// on one data point is how the other three got harder to change.
fn capture_on_create(app: &AppHandle, kind: AreaType, id: AreaId, bounds: Rect) {
    if captures_on_create(kind) {
        crate::output::capture_into_area(app, id, bounds);
        // The spawned capture is what consumes the held frame, so the drag is
        // ended *without* clearing it. See [`precapture::end_drag`]. Retiring
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
/// hand-maintained pair that agree today and drift the moment a type is added,
/// and the drift is silent in the worse direction. A type added here but not
/// there merely loses the fast path; added *there* but not here, every drag of
/// every other type pays for a full-monitor capture nobody reads. The PR #24
/// review found the same shape as a three-way cursor mapping and the fix was the
/// same: derive it, do not restate it.
///
/// Exhaustive rather than `matches!` with a `_` arm, so adding an `AreaType`
/// fails to compile here instead of defaulting to "captures nothing".
///
/// # A third reader, and the name is now half right
///
/// `overlay::convert_area` asks this too (roadmap 1.27): converting an area
/// *into* a capturing type has to take the capture, or the menu row promises the
/// one thing the resulting area does not do. So "on create" reads as "when an
/// area comes to hold this type", and creation is only one of the two ways that
/// happens. Kept rather than renamed for the reason the paragraph above gives:
/// what matters is that there is one predicate, and every caller reaches it.
pub(crate) const fn captures_on_create(kind: AreaType) -> bool {
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

/// The menu label for converting an area *to* this type, or `None` when the
/// menu does not offer it (roadmap task 1.27).
///
/// # Why only three of the seven
///
/// A conversion has to leave the user with an area that does something.
/// `Default`, `Screenshot` and `Filter` have behaviour on the screen today;
/// `Record`, `Ocr`, `Upscale` and `Analysis` are modelled and have none, so
/// offering them would ship four rows that turn a working area into a rectangle
/// indistinguishable from a bug. Two of the four have a roadmap task that will
/// give them behaviour and their row arrives with it: 1.24 for `Upscale`, 1.26
/// for `Ocr`. **`Record` and `Analysis` have no roadmap row at all**, so they are
/// not merely unbuilt, they are unplanned. Said "each earns its row with the
/// roadmap task that gives it behaviour" until the independent review of `#55`
/// resolved the ids and found the quantifier true of half the set.
///
/// The row's own argument is what this defers to rather than contradicts. 1.27
/// exists so those types are reachable *without inventing four more gestures*,
/// and that stays true: this is one line on the day the behaviour lands.
///
/// # Why a label rather than a predicate
///
/// It was a `bool` first, beside a separate list of labels, which is the
/// hand-maintained pair [`captures_on_create`] above already documents itself as
/// avoiding. One exhaustive match answers both questions, so a type cannot be
/// offered without a name or named without being offered.
///
/// Exhaustive rather than a `_` arm, so an eighth `AreaType` fails to compile
/// here instead of defaulting to either answer.
const fn conversion_label(kind: AreaType) -> Option<&'static str> {
    match kind {
        AreaType::Default => Some("Type: Default"),
        AreaType::Screenshot => Some("Type: Screenshot"),
        AreaType::Filter => Some("Type: Filter"),
        AreaType::Record | AreaType::Ocr | AreaType::Upscale | AreaType::Analysis => None,
    }
}

/// The rectangle a gesture commits, computed against an explicit release point
/// rather than the polled cursor: the release coordinate is the authoritative
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
    // Holding Alt turns edge snapping off for the rest of the drag (the
    // standard escape hatch for placing something a few pixels off an edge that
    // the snap would otherwise swallow). It does **not** disable containment:
    // that is the guarantee an area can always be reached again, and a modifier
    // key is not a good reason to let one be lost.
    let free = snapping_suppressed();
    let rect = match gesture {
        // A create drag needs no containment: both of its corners are places
        // the cursor actually reached, so it is on screen by construction, but
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
/// area follows immediately, which is how the modifier behaves in every tool
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

/// The area a pointer event at `point` acts on, in whichever mode is current.
///
/// **The resolution is mode-dependent, and the difference is the V-7 input
/// model itself.** In `Placement` the menu opens for the topmost area of *any*
/// input mode (`hit_test_any`): a pass-through area must stay editable while
/// the layout is being edited, or it becomes permanent. In `Living` it resolves
/// through `hit_test`, which since ADR-0024 §2 means an interactive area's whole
/// body **or a pass-through area's chrome**: the body of a pass-through area
/// belongs to whatever app is underneath (ADR-0016 decision 3), and its border
/// and close control do not.
///
/// # One function, three callers, and that is the whole reason it exists
///
/// The `WM_RBUTTONDOWN` arm decides whether to *claim* the press; `open_menu`
/// decides what the release *acts on*; the `WM_MOUSEWHEEL` arm decides what a
/// scroll magnifies. Roadmap 1.27 recorded the hazard for the first two: change
/// one and not the other, and the hook swallows a click the menu then declines,
/// so no menu appears and the application underneath loses the right-click it
/// would have had. Two copies of one rule is how that happens, so there is one.
///
/// **It was called `menu_target` and reached two of the three.** The wheel arm
/// open-coded the identical `Placement`/`Living` match under a comment saying it
/// *"resolves exactly as the area menu's does"*, which is a statement that the
/// two must agree, sitting on the copy that had not been migrated. Found by the
/// independent review of `#55`, and the name widened with the fix: a function
/// three different events resolve through is not the menu's.
///
/// **What the row expected to have to build here was already built.** It read
/// `interactive_area_at` as *interactive areas only* and concluded both sites
/// needed widening to admit a pass-through area's chrome. They did not:
/// `AreaStore::hit_test` was redefined by ADR-0024 §2, shipped in task 1.17(b),
/// and has admitted chrome ever since. Nothing here changed the resolution. It
/// is named, so that the next change to it cannot reach only one caller.
fn pointer_target(app: &AppHandle, point: Point) -> Option<overlay::AreaSummary> {
    match mode() {
        Mode::Placement => overlay::area_at(app, point),
        Mode::Living => overlay::interactive_area_at(app, point),
        Mode::Hidden => None,
    }
}

/// Opens the area menu for whatever is under `point`, replacing any open menu.
/// Does nothing if the point resolves to no area: then a click has nothing to
/// act on, and any open menu simply closes.
///
/// The target comes from [`pointer_target`], which is shared with the press that
/// claims the click. Note the consequence for the input toggle: flipping an area
/// to pass-through from its own Living menu leaves only its chrome able to
/// re-open that menu. That is deliberate, and it is the reason the toggle sits
/// next to the Layer rows that share the same recovery path.
fn open_menu(app: &AppHandle, point: Point) {
    let Some(area) = pointer_target(app, point) else {
        close_menu(app);
        return;
    };
    // Anchored to the monitor under the cursor, never to the virtual desktop:
    // desktop-relative chrome can land in a dead zone no cursor can reach (F-13).
    let monitor = overlay::monitor_bounds_at(app, point);
    let spec = menu_rows(&area);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a menu this short cannot overflow u32"
    )]
    let bounds = interaction::menu_bounds(point, spec.len() as u32, monitor);
    let items = laid_out(spec, bounds);
    *lock(&MENU) = Some(AreaMenu {
        area: area.id,
        bounds,
        items,
        hovered: None,
        // A fresh menu has no child list open and nothing argued for yet. The
        // timestamp is `0` rather than `now` on purpose: the first tick after
        // this resolves the pointer's real argument and restarts the clock from
        // there, so a menu opened with the cursor already on the parent row does
        // not inherit an age it never earned.
        open: None,
        argument: (None, 0),
    });
    emit_menu(app);
}

/// Gives a row spec its rectangles, top to bottom inside `bounds`.
fn laid_out(spec: Vec<MenuRow>, bounds: Rect) -> Vec<MenuEntry> {
    spec.into_iter()
        .enumerate()
        .map(|(index, row)| MenuEntry {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a menu this short cannot overflow u32"
            )]
            rect: interaction::menu_item_bounds(bounds, index as u32),
            action: row.action,
            label: row.label,
            checked: row.checked,
            children: row.children,
        })
        .collect()
}

/// Opens the child list of the top-level row at `parent`, replacing any other.
/// Returns whether anything changed, so the caller emits once.
///
/// Does nothing when the row has no children, which is every row but one: the
/// hover machinery only ever nominates a parent row, and a press only reaches
/// here through [`MenuAction::OpenSubmenu`], so this is a guard against a future
/// caller rather than a live case.
fn open_submenu(parent: usize, monitor: Rect) -> bool {
    let mut guard = lock(&MENU);
    let Some(menu) = guard.as_mut() else {
        return false;
    };
    if menu.open.as_ref().is_some_and(|open| open.parent == parent) {
        return false;
    }
    let Some(row) = menu.items.get(parent) else {
        return false;
    };
    if row.children.is_empty() {
        return false;
    }
    // Anchored to the parent ROW rather than to the cursor, and clamped against
    // the monitor under it (F-13). See `interaction::submenu_bounds`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a menu this short cannot overflow u32"
    )]
    let bounds = interaction::submenu_bounds(row.rect, row.children.len() as u32, monitor);
    let items = laid_out(row.children.clone(), bounds);
    menu.open = Some(Submenu {
        parent,
        bounds,
        items,
        hovered: None,
    });
    true
}

/// One row of the area menu, before it has been given a rectangle.
///
/// [`open_menu`] needs an `AppHandle` to find the area and the monitor, which
/// puts the whole menu out of reach of a unit test. This is the half that has no
/// such dependency: which rows appear, in what order, with which ticks. Those
/// are the parts a change to the menu is most likely to get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuRow {
    action: MenuAction,
    label: &'static str,
    checked: bool,
    /// The rows this one opens as a child list (roadmap 1.28), empty on a leaf.
    /// Nothing builds a third level and [`MenuHit`] could not address one.
    children: Vec<MenuRow>,
}

/// A row with no child list, which is every row but one.
fn leaf(action: MenuAction, label: &'static str, checked: bool) -> MenuRow {
    MenuRow {
        action,
        label,
        checked,
        children: Vec::new(),
    }
}

/// The rows an area's menu shows, top to bottom.
fn menu_rows(area: &overlay::AreaSummary) -> Vec<MenuRow> {
    // The toggle row switches to the opposite of the area's current input mode;
    // its tick shows the current state (ticked = pass-through).
    let toggled_input = match area.input {
        Input::Interactive => Input::PassThrough,
        Input::PassThrough => Input::Interactive,
    };
    let mut rows: Vec<MenuRow> = Vec::with_capacity(8);
    // Copy/Save lead the menu (the primary actions, ahead of the layout
    // settings below them) and are scoped to **`Screenshot` areas only**.
    //
    // Task 1.9 scoped them to `Default` instead, as a placeholder: `Screenshot`
    // did not exist yet and its own menu was named as 1.9b's job. That got
    // inverted rather than extended, on the rig, 2026-07-26: the rows belong to
    // the type that *has* a capture, not to the primitive that does not. A
    // `Default` area is a plain claimed rectangle; offering "Save image" on one
    // implies it holds an image it does not have.
    //
    // These actions export **the area's pinned capture**, not a fresh grab of
    // whatever is under it. See `captures::pinned_capture`. They used to capture
    // live, and this comment used to predict the consequence and defer it:
    // "wrong the moment an area is moved after capture". Task 1.17(a) made areas
    // movable in the same PR, so the moment arrived immediately, and the rig
    // found it on 2026-07-27. A predicted defect left in place is still a defect.
    if area.kind == AreaType::Screenshot {
        rows.push(leaf(MenuAction::Copy, "Copy", false));
        rows.push(leaf(MenuAction::SaveToFile, "Save image", false));
    }
    // Type conversion (roadmap 1.27). It sits above Layer because it says what
    // the area *is*, where everything below says how it is placed or how it
    // takes input: a Default area is a claimed rectangle whose purpose is not
    // yet decided, and this is where it gets decided.
    //
    // **A child list of its own, which is roadmap 1.28 and the founder's sketch
    // of 2026-08-12 restored.**
    //
    // ~~Flat radio rows rather than a `Type` submenu. The Layer tier directly
    // below is already three flat ticked rows, so this needs no new interaction
    // machinery at all: a submenu means nested bounds, hover-to-open timing and
    // its own dismissal, for three rows. Revisit when the offered set is long
    // enough that a flat list is the worse read.~~ **Struck rather than deleted,
    // because the argument was sound and lost to evidence rather than to a
    // better argument, and a reader who remembers it needs to see it go.**
    //
    // **The trigger was not list length, which is what that comment predicted
    // would reverse it.** The set never grew. Check 1 of the `1.27` rig pass,
    // 2026-08-14, found `Type: Default` and `Auto` ticked at the same time: both
    // ticks are correct, and a flat list holding two independent radio groups
    // still reads as one group with two selections, so the menu misreported its
    // own state to the only person who has ever used it. A divider would have
    // hinted at the grouping; a child list makes it visible.
    //
    // **All three costs it priced were real and are paid here rather than
    // avoided**: nested bounds are [`MenuHit`], hover-to-open timing is
    // [`submenu_step`], and dismissal is the same function's `Close` arm with
    // [`AreaMenu::argument`] buying the diagonal travel.
    //
    // ⚠️ **The parent row names no type and carries no tick, which is a choice
    // the rig can overturn in one line.** `conversion_label(area.kind)` is a
    // ready-made "Type: Screenshot" for it, so the current type could be read
    // without opening the list. It is not used because the roadmap row asked for
    // a parent row, a plain one is unambiguous, and putting a radio group's
    // value on a row that is not in that group is the ambiguity this change
    // exists to remove, one level up. Worth trying on hardware before deciding.
    //
    // ⚠️ **`Type: Filter` is a one-click route into a state that was the hardest
    // this product had, and the sharpest edge is off it.** Filter is pass-through by
    // model default, so on an area below `CHROME_INSIDE_SPAN` (50 px) the whole
    // input surface becomes the 18 px close control placed *outside* the corner,
    // which the frontend draws while the cursor is anywhere inside the area. It
    // is reachable, so the area is not stranded and every conversion is
    // undoable. ~~But the way back is an invisible target that appears once the
    // cursor is already on it.~~ **WITHDRAWN 2026-08-14** and struck rather than
    // deleted, because a reader who remembers the sentence needs to see it go.
    // It was left standing in the present tense two sentences above its own
    // correction, which the independent review of `#56` picked up.
    // Roadmap 1.27 named this "the case to design for first" and it shipped
    // undesigned. ✅ **Half-answered 2026-08-14**: the control is no longer
    // invisible, because hover is resolved by containment rather than by the
    // Living input rule, so crossing the area reveals it (`AreaStore::hover_test`).
    // **What is still open is the target itself**: 18 px, outside the corner, and
    // no move grab at all below `CHROME_INSIDE_SPAN`. That is 1.17(b2)'s outside
    // resize handles and control bar, which is blocked on ADR-0028. The `Click-through` row below carries the same one-way
    // character and says so in `open_menu`'s doc, with the mitigation that it
    // sits among the Layer rows that share its recovery path; these rows sit
    // above them and inherit no such neighbour. Raised by the independent review
    // of `#55`; a real mitigation is a design call, not a comment.
    let types: Vec<MenuRow> = AreaType::ALL
        .into_iter()
        .filter_map(|kind| {
            conversion_label(kind)
                .map(|label| leaf(MenuAction::SetType(kind), label, kind == area.kind))
        })
        .collect();
    rows.push(MenuRow {
        action: MenuAction::OpenSubmenu,
        label: "Area type",
        checked: false,
        children: types,
    });
    rows.push(leaf(
        MenuAction::SetLayer(Layer::Front),
        "Always on top",
        area.layer == Layer::Front,
    ));
    rows.push(leaf(
        MenuAction::SetLayer(Layer::Auto),
        "Auto",
        area.layer == Layer::Auto,
    ));
    rows.push(leaf(
        MenuAction::SetLayer(Layer::Back),
        "Always behind",
        area.layer == Layer::Back,
    ));
    rows.push(leaf(
        MenuAction::SetInput(toggled_input),
        "Click-through",
        area.input == Input::PassThrough,
    ));
    rows.push(leaf(MenuAction::Dismiss, "Dismiss", false));
    rows
}

/// Closes any open area menu. Returns whether one was open, which is what lets
/// `Esc` consume the menu instead of backing out of Placement.
pub fn close_menu(app: &AppHandle) -> bool {
    let was_open = lock(&MENU).take().is_some();
    if was_open {
        emit_menu(app);
    }
    was_open
}

/// The menu row containing `point`, if a menu is open at all.
fn menu_item_at(point: Point) -> Option<MenuHit> {
    let guard = lock(&MENU);
    menu_hit(guard.as_ref()?, point)
}

/// Whether `menu` covers `point` with **either list**.
///
/// The child list is not inside the parent's rectangle: it opens flush beside
/// it, so a press on a child row is outside `bounds` entirely. Testing only the
/// parent here would have made every press on a type row read as a press *away*
/// from the menu, which closes it and swallows the click, so the rows would have
/// been unclickable while looking perfectly normal.
///
/// Split from [`menu_contains`] so it can be drilled: that one reads the global
/// [`MENU`], and a test of it would either restate this predicate beside it and
/// pass whatever it said, or reach for the static and race every other test in
/// the binary that does.
///
/// ⚠️ **This said the global was one "no unit test can populate", and this
/// change's own `the_production_site_anchors_the_child_list_to_its_parent_row`
/// populates it.** Populating it is possible and is deliberately done exactly
/// once, because tests in one binary share the static; what the split buys is
/// that everything else can be asked the same question without contending for
/// it. Corrected after the round that falsified the sentence.
fn menu_covers(menu: &AreaMenu, point: Point) -> bool {
    menu.bounds.contains(point)
        || menu
            .open
            .as_ref()
            .is_some_and(|open| open.bounds.contains(point))
}

/// Whether `point` is inside the open menu, either list.
fn menu_contains(point: Point) -> bool {
    lock(&MENU)
        .as_ref()
        .is_some_and(|menu| menu_covers(menu, point))
}

/// The entry a hit names, in whichever of the two lists it belongs to.
///
/// Extracted from [`activate_menu_item`] so it can be drilled, which is the
/// finding that produced it: swapping the two arms left every test green.
/// [`MenuHit`]'s own doc says the type exists to stop an index resolving in the
/// wrong list, and until an independent review wrote the test, nothing checked
/// that it did. `entry.rect.contains(release)` downstream turns the swap into a
/// dead click rather than a wrong action, which is a mitigation and not a check.
fn entry_at(menu: &AreaMenu, hit: MenuHit) -> Option<&MenuEntry> {
    match hit {
        MenuHit::Row(index) => menu.items.get(index),
        MenuHit::Child(index) => menu.open.as_ref()?.items.get(index),
    }
}

/// Performs the action of a menu row, if the release landed on the row the press
/// started on, the same press-and-release contract the close control uses.
fn activate_menu_item(app: &AppHandle, hit: MenuHit, release: Point) {
    let resolved = {
        let guard = lock(&MENU);
        let Some(menu) = guard.as_ref() else {
            return;
        };
        let Some(entry) = entry_at(menu, hit) else {
            return;
        };
        if !entry.rect.contains(release) {
            return;
        }
        (menu.area, entry.action)
    };
    let (area, action) = resolved;
    // A press on a parent row opens its list at once and leaves the menu up:
    // this is the only action that does not act on the area, so it is also the
    // only one that must not close the menu it is navigating. It bypasses
    // `SUBMENU_OPEN_MS` because a click is not an accidental hover; a user who
    // clicked has already decided.
    if action == MenuAction::OpenSubmenu {
        let MenuHit::Row(parent) = hit else {
            return;
        };
        let monitor = overlay::monitor_bounds_at(app, release);
        if open_submenu(parent, monitor) {
            emit_menu(app);
        }
        return;
    }
    close_menu(app);
    let changed = match action {
        MenuAction::SetLayer(layer) => overlay::set_area_layer(app, area, layer),
        MenuAction::SetInput(input) => overlay::set_area_input(app, area, input),
        MenuAction::SetType(kind) => overlay::convert_area(app, area, kind),
        MenuAction::Dismiss => overlay::dismiss_area(app, area),
        // Returned above, before the menu was closed.
        MenuAction::OpenSubmenu => false,
        // Neither touches the area store (nothing to re-emit), and both are
        // spawned onto their own thread rather than run here: a capture is
        // ~100-300 ms even warm (`uptake_capture` crate docs, F-29), and this
        // function runs on the event-loop thread, inside the `WH_MOUSE_LL`
        // callback's call stack. A hook callback that blocks that long risks
        // Windows silently removing the hook (`LowLevelHooksTimeout`, F-33's
        // failure class). See the `output` module docs.
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

/// One list's rows as the frontend draws them.
fn item_views(items: &[MenuEntry]) -> Vec<MenuItemView> {
    items
        .iter()
        .map(|item| MenuItemView {
            rect: overlay::as_tuple(item.rect),
            label: item.label,
            checked: item.checked,
            // Derived from the row's own children rather than from a flag set
            // beside them, so a row cannot advertise a list it does not have.
            parent: !item.children.is_empty(),
        })
        .collect()
}

/// The payload for `menu`, split out from [`emit_menu`] so that a test can
/// observe what actually goes on the wire.
///
/// ⚠️ **Separated because the mapping was unobserved, not for tidiness.** Round
/// 2 of the `1.28` review set `owner` here to `0` and to
/// `menu.hovered.unwrap_or(open.parent)` -- both of which reintroduce exactly
/// the defect [`ChildMenuView::owner`] exists to fix -- and all 314 tests stayed
/// green, because every test reached into [`AreaMenu`] and nothing built the
/// view. A field is only as pinned as the assignment that fills it.
fn menu_payload(menu: Option<&AreaMenu>) -> MenuPayload {
    MenuPayload {
        menu: menu.map(|menu| MenuView {
            rect: overlay::as_tuple(menu.bounds),
            hovered: menu.hovered,
            items: item_views(&menu.items),
            child: menu.open.as_ref().map(|open| ChildMenuView {
                rect: overlay::as_tuple(open.bounds),
                hovered: open.hovered,
                items: item_views(&open.items),
                owner: open.parent,
            }),
        }),
    }
}

/// Emits the open menu (or its absence) for the frontend to draw.
fn emit_menu(app: &AppHandle) {
    let payload = {
        let guard = lock(&MENU);
        menu_payload(guard.as_ref())
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
#[allow(
    clippy::unwrap_used,
    reason = "the workspace lint's documented test opt-out, Cargo.toml's lints note"
)]
mod tests {
    use std::sync::{Mutex, PoisonError};

    use uptake_core::area::{AreaType, Input, Layer};

    use uptake_core::geometry::{Point, Rect};
    use uptake_core::interaction;

    use crate::payload_keys::{assert_keys, assert_payload_coverage};

    use super::{
        ALL_SHAPES, CURSOR_SNAPSHOT_LEN, ChildMenuView, HoverPayload, MenuAction, MenuItemView,
        MenuPayload, MenuView, NO_HOVER, SUBMENU_CLOSE_MS, SUBMENU_OPEN_MS, SelectionPayload,
        SubmenuStep, WHEEL_STEP, conversion_label, living_hover, menu_rows, submenu_step,
        wheel_notches,
    };

    #[test]
    fn the_hover_payload_reaches_the_frontend_as_camel_case() {
        // The whole fix hangs on this one attribute and nothing else could see
        // it. `#[serde(rename_all = "camelCase")]` is the only reason the Rust
        // field `chrome_only` arrives as `chromeOnly`, which is the key
        // `+page.svelte` reads. Before this test existed, removing the attribute
        // left every gate in this repository green: the frontend then read
        // `undefined`, `areaFramesCss`'s default fired, and the highlight came
        // back on every hovered Filter area, which is the exact defect `#56`
        // removed.
        //
        // **This test is what changed that, so do not read the paragraph above
        // in the present tense.** It said "Remove the attribute and every gate
        // stays green" until 2026-08-22, written inside the test that makes the
        // sentence false. Drilled at that date: deleting the attribute fails
        // this test and `every_payload_this_module_emits_keeps_the_keys_the_
        // frontend_reads`, two red rather than none.
        //
        // `area-kinds.test.ts` is the guard for twice-written wire names and its
        // own doc says it "cannot see a name that reaches the frontend by any
        // route other than these four functions". A payload key is such a route,
        // so the guard is blind here by construction and this test is the cover.
        // Found by the independent review of `#56`.
        let Ok(json) = serde_json::to_string(&HoverPayload {
            id: Some(7),
            chrome_only: true,
        }) else {
            panic!("HoverPayload serializes")
        };

        assert!(
            json.contains("\"chromeOnly\""),
            "the frontend reads `chromeOnly`; got {json}"
        );
        assert!(
            !json.contains("chrome_only"),
            "the snake_case name must not reach the wire; got {json}"
        );
    }

    #[test]
    fn a_live_gesture_freezes_the_hover_rather_than_clearing_it() {
        // Pressing a close control starts a `Gesture::Close`, whose contract is
        // that the release lands on the control it started on. Clearing the hover
        // here removed that control from under the cursor while the button was
        // held on it. Found by the independent review of `#56`.
        let pressed = (Some(7), true);

        assert_eq!(
            living_hover(false, true, NO_HOVER, pressed),
            pressed,
            "a gesture holds whatever was hovered when it began"
        );
        // And the thing the clear was reaching for still holds: a hover frozen at
        // the dragged area cannot wander onto a Filter the cursor crosses.
        assert_eq!(
            living_hover(false, true, (Some(9), true), pressed),
            pressed,
            "a live gesture ignores what is under the pointer now"
        );
    }

    #[test]
    fn an_open_menu_owns_the_pointer_and_outranks_a_live_gesture() {
        // The menu is drawn over everything and takes the pointer while it is up,
        // so nothing beneath it is hovered. Asserted against a live gesture too,
        // because the two conditions are checked in order and swapping them would
        // otherwise be invisible.
        assert_eq!(
            living_hover(true, false, (Some(7), false), NO_HOVER),
            NO_HOVER
        );
        assert_eq!(
            living_hover(true, true, (Some(7), false), (Some(9), true)),
            NO_HOVER
        );
    }

    #[test]
    fn an_idle_pointer_reports_what_was_resolved() {
        // The ordinary case, pinned so a change to either guard above cannot
        // quietly swallow it.
        assert_eq!(
            living_hover(false, false, (Some(7), true), (Some(9), false)),
            (Some(7), true)
        );
        assert_eq!(
            living_hover(false, false, NO_HOVER, (Some(9), false)),
            NO_HOVER
        );
    }

    /// A summary standing in for an area the menu was opened over.
    ///
    /// `AreaSummary` is what `pointer_target` hands `open_menu`, and it carries no
    /// geometry, which is exactly why the row list is testable without a running
    /// app: the rows depend on the type, the tier and the input mode, and on
    /// nothing else.
    fn summary(kind: AreaType, layer: Layer, input: Input) -> crate::overlay::AreaSummary {
        crate::overlay::AreaSummary {
            id: uptake_core::area::AreaStore::new()
                .create(kind, uptake_core::geometry::Rect::new(0, 0, 100, 100))
                .unwrap(),
            layer,
            input,
            kind,
        }
    }

    fn labels(rows: &[super::MenuRow]) -> Vec<&'static str> {
        rows.iter().map(|row| row.label).collect()
    }

    /// An open menu over a Default area, laid out with the real geometry.
    ///
    /// The rectangles come from `menu_bounds`/`menu_item_bounds` rather than
    /// from made-up numbers, so a hit-testing test is asking the question the
    /// running program asks. `open_menu` itself needs an `AppHandle`; this is
    /// everything it does apart from finding the area and the monitor.
    fn open_menu_over_default() -> super::AreaMenu {
        open_menu_over(AreaType::Default)
    }

    /// The same fixture over an area of any type.
    ///
    /// The type decides the row order: `Copy` and `Save image` lead a
    /// `Screenshot` area's menu and are absent from every other, so the parent
    /// row that owns the type list sits at index 2 there and at index 0 over a
    /// `Default` area. A test that wants to tell a real index from a hard-coded
    /// zero has to ask for the former.
    fn open_menu_over(kind: AreaType) -> super::AreaMenu {
        let area = summary(kind, Layer::Auto, Input::Interactive);
        let rows = menu_rows(&area);
        let monitor = Rect::new(0, 0, 1920, 1080);
        let bounds = interaction::menu_bounds(Point::new(400, 300), rows.len() as u32, monitor);
        super::AreaMenu {
            area: area.id,
            bounds,
            items: super::laid_out(rows, bounds),
            hovered: None,
            open: None,
            argument: (None, 0),
        }
    }

    /// The index of the row that opens the type list.
    fn parent_index(menu: &super::AreaMenu) -> usize {
        let Some(index) = menu
            .items
            .iter()
            .position(|item| item.action == MenuAction::OpenSubmenu)
        else {
            panic!("the menu has a type parent row")
        };
        index
    }

    /// Opens the type list, as `open_submenu` does with a monitor in hand.
    fn with_type_list_open(menu: &mut super::AreaMenu) -> usize {
        let parent = parent_index(menu);
        let row = &menu.items[parent];
        let monitor = Rect::new(0, 0, 1920, 1080);
        let bounds = interaction::submenu_bounds(row.rect, row.children.len() as u32, monitor);
        menu.open = Some(super::Submenu {
            parent,
            bounds,
            items: super::laid_out(row.children.clone(), bounds),
            hovered: None,
        });
        parent
    }

    /// The centre of a rectangle, so a hit test is asked about a point that is
    /// unambiguously inside rather than on an edge.
    fn centre(rect: Rect) -> Point {
        Point::new(
            rect.origin.x + (rect.size.width / 2) as i32,
            rect.origin.y + (rect.size.height / 2) as i32,
        )
    }

    #[test]
    fn a_child_list_opens_only_once_the_hover_delay_has_passed() {
        // Roadmap 1.27 priced this and 1.28 pays it: an instant open flickers as
        // the cursor crosses the parent row on its way somewhere else. The value
        // is the rig's to settle; that the delay is obeyed at all is not.
        let start = (Some(2), 1_000);
        assert_eq!(
            submenu_step(1_000 + SUBMENU_OPEN_MS - 1, Some(2), None, start).0,
            SubmenuStep::Hold
        );
        assert_eq!(
            submenu_step(1_000 + SUBMENU_OPEN_MS, Some(2), None, start).0,
            SubmenuStep::Open(2)
        );
    }

    #[test]
    fn a_child_list_closes_only_once_the_longer_delay_has_passed() {
        let start = (None, 1_000);
        assert_eq!(
            submenu_step(1_000 + SUBMENU_CLOSE_MS - 1, None, Some(2), start).0,
            SubmenuStep::Hold
        );
        assert_eq!(
            submenu_step(1_000 + SUBMENU_CLOSE_MS, None, Some(2), start).0,
            SubmenuStep::Close
        );
    }

    #[test]
    fn the_close_delay_is_the_longer_of_the_two() {
        // Not a taste question. The close delay is what the pointer spends
        // travelling from the parent row to a child row, and the open delay is
        // how long a row it crosses on the way needs to steal the list. Invert
        // them and the diagonal-travel test below stops being satisfiable by any
        // implementation.
        const { assert!(SUBMENU_CLOSE_MS > SUBMENU_OPEN_MS) };
    }

    #[test]
    fn a_pointer_resting_in_an_open_list_never_re_opens_it() {
        // Re-opening rebuilds the child list's rectangles and drops its hovered
        // row, so a pointer sitting still would flicker at the poll's rate.
        assert_eq!(
            submenu_step(u64::MAX, Some(2), Some(2), (Some(2), 0)).0,
            SubmenuStep::Hold
        );
    }

    #[test]
    fn crossing_the_rows_between_a_parent_and_its_list_does_not_close_it() {
        // The diagonal travel roadmap 1.27 named as the third cost. The pointer
        // leaves the parent row, crosses two rows that each argue *close*, and
        // arrives inside the list. Every tick in between is a real tick of the
        // real function, so this is the property rather than a restatement of
        // the constants.
        let parent = 2;
        let mut since = (Some(parent), 0);
        let mut open = None;

        // Rest on the parent row until it opens.
        for now in [0, 100, SUBMENU_OPEN_MS] {
            let (step, next) = submenu_step(now, Some(parent), open, since);
            since = next;
            if let SubmenuStep::Open(index) = step {
                open = Some(index);
            }
        }
        assert_eq!(open, Some(parent), "the list never opened");

        // Cross the rows beneath it. Each argues `None`, and each is a fresh
        // argument only on the first tick, so the clock runs from there.
        let departure = SUBMENU_OPEN_MS;
        for elapsed in 1..SUBMENU_CLOSE_MS {
            let (step, next) = submenu_step(departure + elapsed, None, open, since);
            since = next;
            assert_eq!(
                step,
                SubmenuStep::Hold,
                "closed after {elapsed} ms of travel"
            );
        }

        // Arrive inside the list with the deadline not yet reached.
        let (step, _) = submenu_step(departure + SUBMENU_CLOSE_MS - 1, Some(parent), open, since);
        assert_eq!(step, SubmenuStep::Hold);
        assert_eq!(open, Some(parent), "the list did not survive the journey");
    }

    #[test]
    fn a_pointer_that_leaves_and_stays_away_does_close_the_list() {
        // The other half of the test above, and the reason it is a separate one:
        // a `Hold` for every input would satisfy that test on its own.
        let mut since = (Some(2), 0);
        let mut closed = false;
        // To `+ 1` because the first tick away is the one that restarts the
        // clock, so the deadline falls a tick after the bare constant. Worth
        // stating: the first draft stopped at the constant and went red.
        for elapsed in 1..=SUBMENU_CLOSE_MS + 1 {
            let (step, next) = submenu_step(elapsed, None, Some(2), since);
            since = next;
            closed |= step == SubmenuStep::Close;
        }
        assert!(closed, "the list stayed open forever");
    }

    #[test]
    fn a_point_on_a_child_row_resolves_to_that_row_and_not_the_one_beneath_it() {
        let mut menu = open_menu_over_default();
        let parent = with_type_list_open(&mut menu);
        let Some(list) = menu.open.as_ref() else {
            panic!("the list was just opened")
        };
        for (index, item) in list.items.iter().enumerate() {
            assert_eq!(
                super::menu_hit(&menu, centre(item.rect)),
                Some(super::MenuHit::Child(index))
            );
        }
        // The parent row still resolves to itself while its list is open.
        assert_eq!(
            super::menu_hit(&menu, centre(menu.items[parent].rect)),
            Some(super::MenuHit::Row(parent))
        );
    }

    #[test]
    fn a_press_on_a_child_row_is_inside_the_menu() {
        // `menu_contains` decides whether a press belongs to the menu at all. A
        // child list opens flush BESIDE the parent list, so every child row is
        // outside `bounds`: testing only that rectangle would make each type row
        // read as a press away from the menu, which closes it and swallows the
        // click. The rows would look right and do nothing.
        let mut menu = open_menu_over_default();
        with_type_list_open(&mut menu);
        let child_row = {
            let Some(list) = menu.open.as_ref() else {
                panic!("the list was just opened")
            };
            centre(list.items[0].rect)
        };
        // Without this the test would pass against a `menu_covers` that ignored
        // the child list entirely, because the point would be in both.
        assert!(
            !menu.bounds.contains(child_row),
            "the two lists overlap, so this test proves nothing"
        );
        assert!(super::menu_covers(&menu, child_row));
        // And a point in neither is still outside.
        assert!(!super::menu_covers(&menu, Point::new(10, 10)));
    }

    /// Drives the REAL `open_submenu`, through the real `MENU`, which is the
    /// only production site that positions a child list.
    ///
    /// **Every other test here builds its geometry with `with_type_list_open`,
    /// which re-implements that call**, so an independent review could swap
    /// `submenu_bounds` for `menu_bounds` at `open_submenu` and watch all 310
    /// tests stay green -- a guard drilled from a fixture that copies the
    /// production call rather than from the call itself. The wrong
    /// geometry there loses the top-alignment, the flush edge and the
    /// parent-row anchor at once, which is the whole of what makes the diagonal
    /// travel reachable.
    ///
    /// **This is the only test that touches the `MENU` static**, deliberately.
    /// Tests in one binary share it, so a second would race; the assertions
    /// that do not need the global stay on locally-built menus.
    #[test]
    fn the_production_site_anchors_the_child_list_to_its_parent_row() {
        let monitor = Rect::new(0, 0, 1920, 1080);
        let menu = open_menu_over_default();
        let parent = parent_index(&menu);
        let parent_rect = menu.items[parent].rect;
        let expected = interaction::submenu_bounds(
            parent_rect,
            menu.items[parent].children.len() as u32,
            monitor,
        );
        *super::lock(&super::MENU) = Some(menu);

        assert!(super::open_submenu(parent, monitor), "it opened");
        let opened = {
            let guard = super::lock(&super::MENU);
            let Some(menu) = guard.as_ref() else {
                panic!("the menu is open")
            };
            let Some(list) = menu.open.as_ref() else {
                panic!("the list is open")
            };
            (list.parent, list.bounds, list.items.len())
        };
        assert_eq!(opened.0, parent);
        assert_eq!(opened.1, expected, "the child list is not where it belongs");
        assert_eq!(opened.2, 3);
        // The three properties the geometry buys, asserted here rather than
        // trusted: flush against the parent row, top-aligned with it, and
        // anchored to that row rather than to the menu or the cursor.
        assert_eq!(i64::from(opened.1.origin.x), parent_rect.right());
        assert_eq!(
            interaction::menu_item_bounds(opened.1, 0).origin.y,
            parent_rect.origin.y
        );

        // Opening the same row again is a no-op rather than a rebuild, which is
        // what stops a resting pointer flickering at the poll's rate.
        assert!(!super::open_submenu(parent, monitor));
        *super::lock(&super::MENU) = None;
    }

    #[test]
    fn a_row_that_opens_a_list_is_marked_for_the_frontend_and_no_other_is() {
        // `item_views` derives `parent` from the row's own children. Nothing on
        // the Rust side could see that flag change until this test: the only
        // other coverage asserts against a hand-written payload rather than
        // against what `item_views` produces.
        let menu = open_menu_over_default();
        let views = super::item_views(&menu.items);
        let marked: Vec<usize> = views
            .iter()
            .enumerate()
            .filter(|(_, view)| view.parent)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(marked, vec![parent_index(&menu)]);
        // And a child list's own rows never claim to open anything.
        let mut menu = menu;
        with_type_list_open(&mut menu);
        let Some(list) = menu.open.as_ref() else {
            panic!("the list was just opened")
        };
        assert!(
            super::item_views(&list.items)
                .iter()
                .all(|view| !view.parent)
        );
    }

    #[test]
    fn a_hit_resolves_in_the_list_it_names_and_not_the_other_one() {
        // `MenuHit`'s doc says the type exists so that "an index into the wrong
        // list still resolves to a row and still performs its action" cannot
        // happen. Nothing checked that it did: swapping the two arms of
        // `entry_at` left every test green, because downstream
        // `entry.rect.contains(release)` turns the swap into a dead click,
        // which is a mitigation and not a check.
        let mut menu = open_menu_over_default();
        with_type_list_open(&mut menu);
        let Some(row) = super::entry_at(&menu, super::MenuHit::Row(0)) else {
            panic!("row 0 exists")
        };
        let Some(child) = super::entry_at(&menu, super::MenuHit::Child(0)) else {
            panic!("child 0 exists")
        };
        assert_eq!(row.label, "Area type");
        assert_eq!(child.label, "Type: Default");
        assert_ne!(row.rect, child.rect, "the two lists are separate targets");

        // A child index out of range resolves to nothing rather than to the
        // top-level row that happens to share the number.
        let beyond = menu.items.len() - 1;
        assert!(super::entry_at(&menu, super::MenuHit::Child(beyond)).is_none());

        // And with no list open, a child hit resolves to nothing at all.
        menu.open = None;
        assert!(super::entry_at(&menu, super::MenuHit::Child(0)).is_none());
    }

    #[test]
    fn hover_names_the_row_under_the_pointer_in_whichever_list_holds_it() {
        // All THREE arms, which is the finding that rewrote this test: the old
        // one drove `Child` and `None` and left `Row(other)` uncovered, so a
        // test named for an invariant passed while its own middle arm falsified
        // it. Found by the independent review of `1.28`.
        let mut menu = open_menu_over_default();
        let parent = with_type_list_open(&mut menu);
        let child_centre = {
            let Some(list) = menu.open.as_ref() else {
                panic!("the list was just opened")
            };
            centre(list.items[1].rect)
        };

        // In the child list: that child row is hovered, nothing at the top.
        let hit = super::menu_hit(&menu, child_centre);
        super::apply_menu_hover(&mut menu, hit);
        assert_eq!(menu.hovered, None);
        assert_eq!(menu.open.as_ref().and_then(|open| open.hovered), Some(1));

        // On another top-level row while the list is still open. This is the
        // ordinary path the diagonal-travel grace exists to allow, and it is
        // where the old invariant broke.
        let Some(other) = menu.items.iter().position(|item| item.children.is_empty()) else {
            panic!("the menu has a leaf row")
        };
        let hit = super::menu_hit(&menu, centre(menu.items[other].rect));
        super::apply_menu_hover(&mut menu, hit);
        assert_eq!(menu.hovered, Some(other));
        assert_ne!(menu.hovered, Some(parent));

        // Across the padding between the lists: nothing is hovered at all.
        super::apply_menu_hover(&mut menu, None);
        assert_eq!(menu.hovered, None);
        assert_eq!(menu.open.as_ref().and_then(|open| open.hovered), None);
    }

    #[test]
    fn the_open_list_names_its_owner_whatever_the_hover_is_doing() {
        // The invariant the old comment claimed and the old mechanism could not
        // express. `owner` is a fact about the list, not a borrowed highlight,
        // so it survives the pointer being anywhere at all.
        let mut menu = open_menu_over_default();
        let parent = with_type_list_open(&mut menu);
        let Some(other) = menu.items.iter().position(|item| item.children.is_empty()) else {
            panic!("the menu has a leaf row")
        };
        //
        // ⚠️ **The expected hover travels with each case on purpose.** This test
        // asserted `open.parent` alone until round 2 of the review, and
        // `apply_menu_hover` never writes that field -- so the test named for
        // this function's invariant passed with the function gutted to
        // `return false`. Owner surviving is only interesting while the hover is
        // actually moving underneath it.
        for (hit, row, child) in [
            (None, None, None),
            (Some(super::MenuHit::Row(other)), Some(other), None),
            (Some(super::MenuHit::Child(0)), None, Some(0)),
        ] {
            super::apply_menu_hover(&mut menu, hit);
            assert_eq!(
                menu.open.as_ref().map(|open| open.parent),
                Some(parent),
                "the list lost its owner with hit {hit:?}"
            );
            assert_eq!(menu.hovered, row, "the top-level hover with hit {hit:?}");
            assert_eq!(
                menu.open.as_ref().and_then(|open| open.hovered),
                child,
                "the child hover with hit {hit:?}"
            );
        }
    }

    #[test]
    fn one_whole_gesture_opens_holds_and_closes_the_child_list() {
        // `F1` of round 3, and the reason it is a SEQUENCE rather than five
        // assertions: every unit below was already drilled and every unit drill
        // went red, and then seven mutations to the site that COMPOSES them all
        // stayed green at 316/316. Five of those seven mean the list never opens
        // or never closes. A composition is its own unit.
        //
        // The gesture, as the pointer actually performs it: arrive on the parent
        // row, wait out the open delay, travel across a sibling row toward the
        // list, arrive inside it, then leave entirely.
        let mut menu = open_menu_over_default();
        let parent = parent_index(&menu);
        let on_parent = centre(menu.items[parent].rect);

        // Arrival argues for opening and nothing more: the delay is not served.
        let step = super::menu_pump_step(&mut menu, on_parent, 1_000);
        assert_eq!(step.to_open, None, "it opened before the delay was served");
        assert_eq!(step.hit, Some(super::MenuHit::Row(parent)));
        assert!(menu.open.is_none());

        // The delay passes with the pointer still there. THIS is the tick the
        // argument-swap and the dropped clock advance both kill.
        let step = super::menu_pump_step(&mut menu, on_parent, 1_000 + super::SUBMENU_OPEN_MS);
        assert_eq!(
            step.to_open,
            Some(parent),
            "the list never opened, which is the whole of roadmap 1.28"
        );
        // NOT `changed`, and that is the design rather than an oversight: the
        // hover has not moved, so this tick changes nothing by itself. The
        // opening is a DECISION here, and its frontend change is contributed
        // by `open_submenu`'s return value in `pump_menu` -- which is exactly
        // why dropping `to_open` on the floor is silent, and why `MenuPump`
        // is `#[must_use]`.
        assert!(
            !step.changed,
            "the step claimed a change of its own; the open is `pump_menu`'s to contribute"
        );

        // `to_open` is a decision, not an act -- `pump_menu` needs a monitor for
        // that -- so the fixture performs it, exactly as `open_submenu` would.
        let opened = with_type_list_open(&mut menu);
        assert_eq!(opened, parent);

        // Travel: the pointer crosses a sibling row on its way to the list. That
        // argues CLOSE on every tick, and the clock restart is what buys the
        // diagonal. If the list dies here the feature is unusable by pointer.
        let Some(other) = menu
            .items
            .iter()
            .enumerate()
            .find(|(index, item)| *index != parent && item.children.is_empty())
            .map(|(index, _)| index)
        else {
            panic!("the menu has a leaf row that is not the parent")
        };
        let travelling = centre(menu.items[other].rect);
        let mut now = 1_000 + super::SUBMENU_OPEN_MS;
        for tick in 1..super::SUBMENU_CLOSE_MS {
            now += 1;
            let step = super::menu_pump_step(&mut menu, travelling, now);
            assert!(
                menu.open.is_some(),
                "the list closed {tick} ms into the diagonal travel, before SUBMENU_CLOSE_MS"
            );
            assert_eq!(step.to_open, None, "it re-opened while travelling away");
        }

        // Arrival inside the list stops the argument and keeps it open.
        let Some(list) = menu.open.as_ref() else {
            panic!("the list is open")
        };
        let inside = centre(list.items[0].rect);
        now += 1;
        let step = super::menu_pump_step(&mut menu, inside, now);
        assert!(
            menu.open.is_some(),
            "the list closed after the pointer arrived in it"
        );
        assert_eq!(step.hit, Some(super::MenuHit::Child(0)));

        // Leaving entirely closes it, but not before the close delay. Driven
        // until it actually closes rather than for a count someone worked out:
        // the clock restarts on the first tick away, so an off-by-one in the
        // loop bound reads as "the list never closed" and hides which of the two
        // properties failed. This asserts both separately.
        let away = Point::new(10, 10);
        let departure = now;
        let mut closed_at = None;
        for _ in 0..=(super::SUBMENU_CLOSE_MS * 2) {
            now += 1;
            // Only the effect on `menu` matters here; `#[must_use]` is right
            // to ask, and this is the acknowledgement rather than a silencing.
            let _ = super::menu_pump_step(&mut menu, away, now);
            if menu.open.is_none() {
                closed_at = Some(now);
                break;
            }
        }
        let Some(closed_at) = closed_at else {
            panic!(
                "the list never closed, {}ms after the pointer left",
                now - departure
            )
        };
        assert!(
            closed_at - departure >= super::SUBMENU_CLOSE_MS,
            "the list closed {}ms after the pointer left, inside SUBMENU_CLOSE_MS ({}ms) --              the grace that buys the diagonal travel is gone",
            closed_at - departure,
            super::SUBMENU_CLOSE_MS
        );
    }

    #[test]
    fn a_tick_that_changes_nothing_says_so() {
        // The other side of `changed`, and the reason `pump_menu` may emit
        // conditionally at all: a pointer resting still must not re-emit the
        // menu on every tick of the pump.
        let mut menu = open_menu_over_default();
        let resting = centre(menu.items[parent_index(&menu)].rect);
        assert!(super::menu_pump_step(&mut menu, resting, 1_000).changed);
        for now in [1_001, 1_002, 1_003] {
            let step = super::menu_pump_step(&mut menu, resting, now);
            assert!(
                !step.changed,
                "a still pointer reported a change at {now}, so the menu re-emits every tick"
            );
        }
    }

    /// The body of one top-level `fn` in this file, or a panic naming it.
    ///
    /// Used by the composition controls below. It slices to the next top-level
    /// `fn`, which is enough because every function it is asked about is at
    /// module level.
    fn fn_body(source: &str, signature: &str) -> String {
        // Production code only. `include_str!` pulls in this test module too,
        // and every required token below appears in it as an assertion string,
        // so a reordering that put the tests first would let a control read its
        // own expectations back and pass on a gutted function.
        let production = production_of(source);
        // Two failure modes, two messages, and that is `R5-F5` rather than
        // pedantry: they used to share one, so a `#[should_panic]` for the
        // missing-signature case also passed when the fixture merely lost its
        // trailing `fn` -- and the fixture comment that says the terminator is
        // load-bearing was itself the only thing guarding it.
        let Some((_, after)) = production.split_once(signature) else {
            panic!(
                "`{signature}` not found in production code -- renamed? An unfound function \
                 must not read as a pass"
            )
        };
        let Some((body, _)) = after.split_once("\nfn ") else {
            panic!(
                "`{signature}` has no following top-level `fn` to slice to -- the fixture or \
                 the file lost its terminator, which is not the same as the signature being \
                 absent"
            )
        };
        let body = body.to_owned();
        strip_comments(&body)
    }

    /// `source` with `//` and `/* */` comments removed.
    ///
    /// **The whole of `R4-F1`.** Round 4's source controls searched the raw
    /// text, so `// emit_menu(app);` left the token sitting in a comment for
    /// `contains` to find: the menu never redrew, the child list opened
    /// invisibly, hover highlighting died across the whole menu, and every gate
    /// in the repository stayed green. Commenting a line out is the most
    /// ordinary way there is to disable it, and it was the one form the control
    /// could not see.
    ///
    /// `menu-styles.test.ts` strips CSS comments before matching, in this same
    /// change, for exactly this reason. A class fixed at one member is a class
    /// not fixed, and this file is where it was not fixed.
    ///
    /// Naive about string literals containing comment markers, which is
    /// acceptable because it is only ever asked about function bodies whose
    /// required tokens are calls.
    fn strip_comments(source: &str) -> String {
        strip_strings(&strip_comment_syntax(source))
    }

    /// `source` with double-quoted string literals emptied.
    ///
    /// **`R5-F2`, second half.** The exactly-once check on `emit_menu(app);`
    /// catches a string literal ADDED beside a live call and not one that
    /// REPLACES it: `if changed { let _s = "emit_menu(app);"; }` leaves the
    /// count at one, `#[cfg(` absent and `if changed {` present, and every gate
    /// green. Emptying literals makes the token disappear entirely, so the
    /// required-token check fails, which is the right answer for a call that no
    /// longer exists.
    ///
    /// Escapes are honoured so a literal ending in `" is not read as still open.
    fn strip_strings(source: &str) -> String {
        let mut out = String::with_capacity(source.len());
        let mut chars = source.chars();
        while let Some(c) = chars.next() {
            if c != '"' {
                out.push(c);
                continue;
            }
            out.push('"');
            let mut escaped = false;
            for inner in chars.by_ref() {
                if escaped {
                    escaped = false;
                } else if inner == '\\' {
                    escaped = true;
                } else if inner == '"' {
                    break;
                }
            }
            out.push('"');
        }
        out
    }

    fn strip_comment_syntax(source: &str) -> String {
        let mut out = String::with_capacity(source.len());
        let mut rest = source;
        loop {
            let line = rest.find("//");
            let block = rest.find("/*");
            match (line, block) {
                (None, None) => {
                    out.push_str(rest);
                    return out;
                }
                (Some(at), None) => {
                    out.push_str(&rest[..at]);
                    rest = rest[at..].find('\n').map_or("", |end| &rest[at + end..]);
                }
                (None, Some(at)) => {
                    out.push_str(&rest[..at]);
                    rest = rest[at..]
                        .find("*/")
                        .map_or("", |end| &rest[at + end + 2..]);
                }
                (Some(l), Some(b)) if l < b => {
                    out.push_str(&rest[..l]);
                    rest = rest[l..].find('\n').map_or("", |end| &rest[l + end..]);
                }
                (Some(_), Some(b)) => {
                    out.push_str(&rest[..b]);
                    rest = rest[b..].find("*/").map_or("", |end| &rest[b + end + 2..]);
                }
            }
        }
    }

    #[test]
    fn a_commented_out_call_is_not_a_call() {
        // The control on the control. `R4-F1` was found by an independent
        // review that commented out `emit_menu(app);` and watched every gate
        // stay green, so this pins the stripping itself rather than trusting
        // that the helper above does what its name says.
        // The blank line left behind is deliberate and harmless: only the
        // presence of tokens is ever asked about, never the layout.
        assert_eq!(strip_comments("a();\n// b();\nc();\n"), "a();\n\nc();\n");
        assert_eq!(strip_comments("a(); /* b(); */ c();"), "a();  c();");
        assert!(!strip_comments("    // emit_menu(app);\n").contains("emit_menu"));
        // And it must not eat live code that merely follows a comment.
        assert!(strip_comments("// note\nemit_menu(app);\n").contains("emit_menu(app);"));
    }

    #[test]
    fn clicking_a_parent_row_opens_its_list_and_leaves_the_menu_up() {
        // The seventh of round 3's `F1` mutations, and the one that is not on
        // the pump path at all: `if action == MenuAction::OpenSubmenu` -> `if
        // false && ...` killed click-to-open with 316 tests green.
        //
        // A press on a parent row is the only action that does not act on the
        // area, so it is the only one that must NOT close the menu it is
        // navigating -- and it bypasses `SUBMENU_OPEN_MS`, because a click is
        // not an accidental hover.
        //
        // Source control, with the same weakness as the one below and for the
        // same reason: the branch needs an `AppHandle` for both the monitor and
        // the emit, and no seam for one exists TODAY. ⚠️ One is possible: the
        // round-5 reviewer built `pump_menu_tail(changed, to_open, monitor_of,
        // open, emit)` -- five plain parameters, three of them closures,
        // touching neither `MENU` nor an `AppHandle` -- and measured the
        // `if changed` branch becoming observable. See `I-81`.
        let body = fn_body(
            include_str!("placement.rs"),
            "fn activate_menu_item(app: &AppHandle, hit: MenuHit, release: Point) {",
        );
        for required in [
            // The branch exists and is taken on the real action.
            "if action == MenuAction::OpenSubmenu {",
            // It opens at the monitor under the release point...
            "open_submenu(parent, monitor)",
            // ...draws the result...
            "emit_menu(app);",
            // ...and returns WITHOUT falling through to `close_menu`.
            "        return;",
        ] {
            assert!(
                body.contains(required),
                "`activate_menu_item` no longer contains `{required}` -- click-to-open is dead, \
                 or the navigating action now closes the menu it is navigating"
            );
        }
        // The ordering half: the early return must precede `close_menu`, or the
        // menu closes under the list it just opened.
        let Some(returns) = body.find("        return;") else {
            panic!("the early return is gone")
        };
        let Some(closes) = body.find("close_menu(app);") else {
            panic!("`activate_menu_item` no longer closes the menu for ordinary actions")
        };
        assert!(
            returns < closes,
            "the parent-row branch no longer returns before `close_menu`"
        );
    }

    /// A source whose only matching signature lives inside `#[cfg(test)]`.
    ///
    /// Shared by the two tests below so the fixture cannot drift between them.
    fn signature_only_in_the_test_module(signature: &str) -> String {
        // The trailing `fn after` is load-bearing. `fn_body` slices from the
        // signature to the NEXT top-level `fn`, so a fixture that ends without
        // one makes it panic for lack of a terminator rather than for lack of
        // the signature -- and the `#[should_panic]` below then passes whether
        // the truncation works or not. Caught by drilling the drill.
        format!(
            "fn other() {{}}\n#[cfg(test)]\nmod tests {{\n    {signature}\n        assert!(true);\n    }}\n}}\nfn after() {{}}\n"
        )
    }

    #[test]
    #[should_panic(expected = "not found in production code")]
    fn fn_body_refuses_to_read_the_test_module() {
        // `R5-F3`. `fn_body` truncates at `#[cfg(test)]` because `include_str!`
        // pulls that module in too, and every token the source controls require
        // appears there as an assertion string -- so a reordering would let a
        // control read its own expectations back and pass against a gutted
        // function.
        //
        // Asserted THROUGH `fn_body` rather than through the helper it calls,
        // because the first two attempts at this test checked the helper and
        // stayed green when `fn_body` stopped calling it. A defence is only
        // pinned at the point it is used.
        const SIGNATURE: &str = "fn target(app: &AppHandle) {";
        let _ = fn_body(&signature_only_in_the_test_module(SIGNATURE), SIGNATURE);
    }

    #[test]
    #[should_panic(expected = "no following top-level")]
    fn fn_body_says_so_when_it_has_nothing_to_slice_to() {
        // The other panic, and the reason it is a separate message. Both
        // failures used to say "not found", so the `#[should_panic]` above
        // passed when the fixture merely lost its trailing `fn` -- which is the
        // fourth way that test has been wrong, found by the confirmation pass
        // drilling my own fixture rather than the code.
        const SIGNATURE: &str = "fn target(app: &AppHandle) {";
        let _ = fn_body(&format!("{SIGNATURE}\n    let x = 1;\n}}\n"), SIGNATURE);
    }

    #[test]
    fn fn_body_reads_production_code_with_its_comments_removed() {
        // `R5-F3`, and the first version of this test did not have a falsifying
        // input either -- it asserted a property of THIS file, where the
        // production definition already precedes the test module, so both
        // defences could be deleted and it stayed green. The reviewer said as
        // much about the truncation; the same was true of the stripping, because
        // `a_commented_out_call_is_not_a_call` exercises `strip_comments`
        // directly and nothing observed `fn_body` still calling it.
        //
        // Synthetic sources fix that: each defence gets an input that fails
        // without it, independent of how this file happens to be ordered.
        const SIGNATURE: &str = "fn target(app: &AppHandle) {";

        // Truncation. The signature appears ONLY inside the test module, so
        // production code contains no such function and `fn_body` must say so.
        // Without the `#[cfg(test)]` cut it finds the test-module copy and
        // happily returns assertions as if they were production code.
        // Stripping, observed through `fn_body` rather than through
        // `strip_comments`. A body whose only mention of the call is commented
        // out must come back without it.
        let commented =
            format!("{SIGNATURE}\n    // emit_menu(app);\n    let x = 1;\n}}\nfn after() {{}}\n");
        let body = fn_body(&commented, SIGNATURE);
        assert!(
            !body.contains("emit_menu"),
            "`fn_body` returned a commented-out call as if it were live code: {body:?}"
        );
        // ...and a live call after a comment must survive, or the controls go
        // red on working code and get deleted for crying wolf.
        let live = format!("{SIGNATURE}\n    // note\n    emit_menu(app);\n}}\nfn after() {{}}\n");
        assert!(
            fn_body(&live, SIGNATURE).contains("emit_menu(app);"),
            "`fn_body` ate a live call that merely followed a comment"
        );
    }

    /// The production half of `source`, by the same rule [`fn_body`] uses.
    ///
    /// Split out so the truncation has an input that can falsify it without
    /// depending on the order this file happens to be written in.
    fn production_of(source: &str) -> &str {
        source
            .split_once("\n#[cfg(test)]")
            .map_or(source, |(head, _)| head)
    }

    #[test]
    fn pump_menu_composes_the_step_it_is_given() {
        // The residue of `F1` that no unit test can reach: resolving the monitor
        // and emitting both need an `AppHandle`, and no seam for one exists
        // today. One is possible; see `I-81` and the note in `pump_menu`'s
        // control above.
        //
        // ⚠️ **This is a SOURCE control and it is weaker than the test above.**
        // It pins the shape of three lines rather than their behaviour. It is
        // here because the alternative measured by the review is nothing at
        // all: disabling the emit left 316 tests green.
        //
        // **What it catches**: deletion, neutering (`if changed` -> `if false`),
        // and -- since `R4-F1` -- commenting the line out, which is the ordinary
        // way a line gets disabled and the way this control could not see.
        //
        // ⚠️ **What it does NOT catch, measured rather than guessed**: a
        // token-preserving move, such as the call being lifted into a closure
        // that is never invoked. Round 4's `S4` did exactly that and passed both
        // this control and `clippy -D warnings`. No source control can catch
        // that class by itself; a seam that lets a test observe the emit
        // shrinks it a long way, and the round-5 reviewer built and ran one.
        //
        // ⚠️ **This said "there is none because it needs an `AppHandle`", and
        // that was measured false.** The seam replaces the calls that need the
        // handle rather than reading through them, so it needs neither the
        // handle nor the `MENU` static. It does not close `S4` -- the closure
        // then sits in an argument list, still shape-pinned -- but it makes the
        // tail's branching genuinely observable. Recorded as UP-TAKE `I-81`,
        // whose remediation column carries the measurement.
        let body = fn_body(
            include_str!("placement.rs"),
            "fn pump_menu(app: &AppHandle, point: Point) -> Option<MenuHit> {",
        );
        for required in [
            // The decision comes from the extracted step, not from a second copy.
            "menu_pump_step(menu, point, elapsed_ms())",
            // An earned list is actually opened, at the monitor under the pointer.
            "if let Some(parent) = to_open {",
            "open_submenu(parent, monitor)",
            // ...and a change is actually drawn. `if false {` fails this.
            "if changed {",
            "emit_menu(app);",
        ] {
            assert!(
                body.contains(required),
                "`pump_menu` no longer contains `{required}` -- the composition the review \
                 drilled seven times is unguarded again"
            );
        }
        // Two further token-preserving ways to disable a call, both found by the
        // round-5 reviewer and both mechanically detectable, unlike `S4`:
        // `#[cfg(any())]` keeps the token and removes the call at compile time,
        // and a string literal parks the token where nothing runs it.
        //
        // ⚠️ **The string-literal half needed TWO fixes and the first was
        // reported as complete when it was not.** The exactly-once check below
        // caught a literal ADDED beside a live call and NOT one that REPLACES
        // the call, which leaves the count at exactly one -- the confirmation
        // pass re-ran its own mutation to show it. Literals are emptied in
        // `strip_comments` now, so a replaced call has no token at all and the
        // required-token loop above fails. Drilled.
        //
        // Which changes what the count below means, and it is worth saying
        // rather than leaving the reader to work out: it now counts REAL calls,
        // because a decoy in a literal is gone before it is asked. A decoy
        // sitting beside a working call therefore passes, and that is correct --
        // the call still runs, so there is nothing to report.
        assert!(
            !body.contains("#[cfg("),
            "a `#[cfg(...)]` inside `pump_menu` can remove a call while leaving its \
             text for this control to find"
        );
        assert_eq!(
            body.matches("emit_menu(app);").count(),
            1,
            "`emit_menu(app);` must appear in `pump_menu` exactly once, counting real calls \
             only -- string literals are emptied before this. Two means one of them is in a \
             branch that never runs, and the required-token loop above would find that one \
             and be satisfied."
        );
    }

    #[test]
    fn the_wire_names_the_row_that_owns_the_open_list() {
        // `owner` on the wire is the whole of the parent-stays-lit fix, and
        // nothing observed the assignment that fills it: every other test in
        // this file reaches into `AreaMenu`, so round 2 of the `1.28` review set
        // the field to `0` and to `menu.hovered.unwrap_or(open.parent)` -- both
        // of which restore the exact defect it exists to fix -- and all 314
        // tests stayed green. This one builds the payload.
        //
        // Over a **Screenshot** area deliberately: `Copy` and `Save image` lead
        // that menu, so the parent row is index 2, and a wire field hard-coded
        // to `0` fails here rather than passing by coincidence.
        let mut menu = open_menu_over(AreaType::Screenshot);
        let parent = with_type_list_open(&mut menu);
        assert_ne!(parent, 0, "the fixture puts the parent row off index zero");
        let Some(other) = menu.items.iter().position(|item| item.children.is_empty()) else {
            panic!("the menu has a leaf row")
        };
        assert_ne!(other, parent);
        // The pointer on some *other* top-level row is the case the fix is
        // about: the hover moves away and the list must still say where it came
        // from. This is also what separates `owner` from the hover: a wire field
        // borrowed from `hovered` reads `other` here and is wrong.
        super::apply_menu_hover(&mut menu, Some(super::MenuHit::Row(other)));

        let payload = super::menu_payload(Some(&menu));
        let Some(view) = payload.menu else {
            panic!("the payload carries the open menu")
        };
        let Some(child) = view.child else {
            panic!("the payload carries the open child list")
        };
        assert_eq!(
            child.owner, parent,
            "the wire lost the row that owns the list"
        );
        assert_eq!(view.hovered, Some(other), "the wire lost the hovered row");
    }

    #[test]
    fn an_absent_menu_reaches_the_wire_as_an_absent_menu() {
        // The other arm of the same mapping: dismissal is an emit of `None`, so
        // a `menu_payload` that always produced a view would leave a dead menu
        // drawn on screen.
        let payload = super::menu_payload(None);
        assert!(payload.menu.is_none());
    }

    #[test]
    fn being_anywhere_in_an_open_list_argues_for_keeping_it_open() {
        // Including its padding: a pointer that has arrived is not asked to stay
        // on a row, or the list would close whenever it passed between two.
        let mut menu = open_menu_over_default();
        let parent = with_type_list_open(&mut menu);
        let Some(list) = menu.open.as_ref() else {
            panic!("the list was just opened")
        };
        let list_bounds = list.bounds;
        let padding = Point::new(centre(list_bounds).x, list_bounds.origin.y);
        assert_eq!(super::submenu_argument(&menu, padding), Some(parent));
        assert_eq!(super::menu_hit(&menu, padding), None);
        // Well outside argues to close.
        assert_eq!(super::submenu_argument(&menu, Point::new(10, 10)), None);
    }

    /// Every payload this module emits, and the keys the frontend indexes it
    /// with (`I-67`).
    ///
    /// **Replaces the single-struct test `1.28` added**, which asserted with
    /// `json.contains("\"child\"")` over a nested payload. That shape is weaker
    /// than it looks: a key present at the wrong level satisfies it, and it
    /// cannot notice a key that should NOT be there. Each struct is asserted on
    /// its own, as an exact set.
    ///
    /// **`HoverPayload` is the only renamed type in `src-tauri`**, and the two
    /// conventions sitting side by side here is the fact worth pinning rather
    /// than the rename itself. `chromeOnly` is camelCase because of one
    /// attribute; everything else is snake_case verbatim. Delete the attribute
    /// and this goes red, which is what `UT-F-72` says nothing did.
    #[test]
    fn every_payload_this_module_emits_keeps_the_keys_the_frontend_reads() {
        assert_keys(
            "SelectionPayload",
            &SelectionPayload {
                rect: None,
                source: None,
                probe: None,
            },
            &["rect", "source", "probe"],
        );
        let item = MenuItemView {
            rect: (1, 2, 3, 4),
            label: "Area type",
            checked: false,
            parent: true,
        };
        assert_keys(
            "MenuItemView",
            &item,
            &["rect", "label", "checked", "parent"],
        );
        let child = ChildMenuView {
            rect: (5, 6, 7, 8),
            items: vec![item.clone()],
            hovered: None,
            owner: 0,
        };
        assert_keys(
            "ChildMenuView",
            &child,
            &["rect", "items", "hovered", "owner"],
        );
        let view = MenuView {
            rect: (1, 2, 3, 4),
            items: vec![item],
            hovered: Some(0),
            child: Some(child),
        };
        assert_keys("MenuView", &view, &["rect", "items", "hovered", "child"]);
        assert_keys("MenuPayload", &MenuPayload { menu: Some(view) }, &["menu"]);
        assert_keys(
            "HoverPayload",
            &HoverPayload {
                id: Some(7),
                chrome_only: true,
            },
            &["id", "chromeOnly"],
        );
    }

    #[test]
    fn no_payload_in_this_module_escapes_the_key_table() {
        // The completeness half. A hand-maintained table cannot tell you what it
        // is missing, and `A9`'s rule is that the completeness of an author's own
        // list is the one thing the author cannot check. This reads the file.
        assert_payload_coverage(
            "placement.rs",
            include_str!("placement.rs"),
            &[
                "SelectionPayload",
                "MenuPayload",
                "MenuView",
                "ChildMenuView",
                "MenuItemView",
                "HoverPayload",
            ],
            &[],
        );
    }

    /// The children of the one row that has any.
    fn type_rows(rows: &[super::MenuRow]) -> &[super::MenuRow] {
        let Some(parent) = rows
            .iter()
            .find(|row| row.action == MenuAction::OpenSubmenu)
        else {
            panic!("the menu has a type parent row")
        };
        &parent.children
    }

    #[test]
    fn the_top_level_menu_holds_one_radio_group_and_the_types_are_not_in_it() {
        // Roadmap 1.28, and this assertion IS the row's reason for existing. The
        // rig found `Type: Default` and `Auto` ticked at once on 2026-08-14:
        // both correct, and unreadable together, because a flat list holding two
        // radio groups reads as one group with two selections. Asserted as the
        // whole list rather than as "contains", so a row nobody decided on also
        // fails here.
        let rows = menu_rows(&summary(AreaType::Default, Layer::Auto, Input::Interactive));
        assert_eq!(
            labels(&rows),
            vec![
                "Area type",
                "Always on top",
                "Auto",
                "Always behind",
                "Click-through",
                "Dismiss",
            ]
        );
        // The Layer tier is the only radio group left at this level, so at most
        // one row of one can be ticked here. `Click-through` is a checkbox and
        // is allowed to be ticked beside it; that pairing is not what the rig
        // read as two selections, and it is not this row's to change.
        let radio_ticks = rows
            .iter()
            .filter(|row| matches!(row.action, MenuAction::SetLayer(_)) && row.checked)
            .count();
        assert_eq!(radio_ticks, 1);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.action, MenuAction::SetType(_))),
            "a type row escaped back into the top level"
        );
    }

    #[test]
    fn the_type_list_offers_the_three_types_that_do_something() {
        // The other four are modelled and have no behaviour, so a row for them
        // would convert a working area into one indistinguishable from a bug.
        let rows = menu_rows(&summary(AreaType::Default, Layer::Auto, Input::Interactive));
        assert_eq!(
            labels(type_rows(&rows)),
            vec!["Type: Default", "Type: Screenshot", "Type: Filter"]
        );
    }

    #[test]
    fn exactly_one_row_opens_a_list_and_no_child_opens_another() {
        // Two deep, and `MenuHit` can address exactly two. A third level would
        // be built here and be unreachable everywhere else, which is the shape
        // of a defect that draws correctly and cannot be clicked.
        for kind in AreaType::ALL {
            let rows = menu_rows(&summary(kind, Layer::Auto, Input::Interactive));
            let parents = rows.iter().filter(|row| !row.children.is_empty()).count();
            assert_eq!(parents, 1, "{kind:?}");
            for child in type_rows(&rows) {
                assert!(child.children.is_empty(), "{kind:?} {}", child.label);
            }
        }
    }

    #[test]
    fn the_type_rows_tick_the_type_the_area_already_is_and_no_other() {
        for kind in AreaType::ALL {
            let rows = menu_rows(&summary(kind, Layer::Auto, Input::Interactive));
            let ticked: Vec<&'static str> = type_rows(&rows)
                .iter()
                .filter(|row| matches!(row.action, MenuAction::SetType(_)) && row.checked)
                .map(|row| row.label)
                .collect();
            match conversion_label(kind) {
                Some(label) => assert_eq!(ticked, vec![label], "{kind:?}"),
                // A Record area is not offered as a target, so nothing is
                // ticked. That is honest: the menu is showing what it can
                // convert *to*, and none of those is what this area is.
                None => assert!(ticked.is_empty(), "{kind:?}"),
            }
        }
    }

    #[test]
    fn the_parent_row_itself_is_never_ticked() {
        // It is not a member of the group it opens. A tick on it would put a
        // second mark at the top level and re-create, one row up, exactly the
        // misreading roadmap 1.28 exists to remove.
        for kind in AreaType::ALL {
            let rows = menu_rows(&summary(kind, Layer::Auto, Input::Interactive));
            let Some(parent) = rows
                .iter()
                .find(|row| row.action == MenuAction::OpenSubmenu)
            else {
                panic!("the menu has a type parent row")
            };
            assert!(!parent.checked, "{kind:?}");
        }
    }

    #[test]
    fn a_screenshots_menu_still_leads_with_copy_and_save() {
        // Pinned because the export actions are the primary ones for the only
        // type that holds a capture, and a reordering that buries them is a
        // regression. The type parent row went in above Layer and below these
        // two, where the flat type rows used to sit.
        let rows = menu_rows(&summary(
            AreaType::Screenshot,
            Layer::Auto,
            Input::Interactive,
        ));
        assert_eq!(labels(&rows)[..3], ["Copy", "Save image", "Area type"]);
    }

    #[test]
    fn no_two_rows_of_one_list_share_a_label() {
        // The rows are identified to the user by their text alone, and the type
        // rows are the only group whose labels are generated rather than written
        // out one at a time.
        //
        // Per list rather than across both, and the distinction is real since
        // 1.28: two rows with one label are ambiguous only when they are on
        // screen together, and a parent row named after the type it opens would
        // be a deliberate repeat rather than a collision.
        let unique = |rows: &[super::MenuRow], what: &str, kind: AreaType| {
            let mut seen = labels(rows);
            let before = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), before, "{kind:?} {what}");
        };
        for kind in AreaType::ALL {
            let rows = menu_rows(&summary(kind, Layer::Front, Input::PassThrough));
            unique(&rows, "top level", kind);
            unique(type_rows(&rows), "type list", kind);
        }
    }

    #[test]
    fn converting_to_a_type_that_captures_is_exactly_screenshot() {
        // `overlay::convert_area` needs an `AppHandle`, so the branch that takes
        // a capture on conversion cannot be driven from here. This pins the
        // predicate that branch asks, which is the half that can go wrong
        // silently: `captures_on_create` answering false for `Screenshot` gives a
        // menu row promising a still it never takes, and answering true for any
        // other type spends a full capture on an area that will never show it.
        //
        // The conversion caller arrived in `#55` after the independent review
        // found the row shipping without one.
        for kind in AreaType::ALL {
            assert_eq!(
                super::captures_on_create(kind),
                kind == AreaType::Screenshot,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn each_type_row_is_labelled_with_its_own_type() {
        // The hazard is a copy-paste one: `Filter => Some("Type: Screenshot")`
        // compiles, reads fine, and converts an area to something other than the
        // row the user clicked. Checked against `overlay::type_name`, which is
        // the wire name for the same variant and is a second author.
        //
        // This does couple the label text to the wire name. That is a real
        // constraint on future labels rather than an accident: a row named
        // something other than its type needs this test edited, which is the
        // moment to ask whether the user can still tell what they picked.
        for kind in AreaType::ALL {
            if let Some(label) = conversion_label(kind) {
                assert_eq!(
                    label.to_ascii_lowercase(),
                    format!("type: {}", crate::overlay::type_name(kind)),
                    "{kind:?}"
                );
            }
        }
    }

    /// `mouseData` as Windows fills it for a wheel event: the delta in the high
    /// word, and a low word that is undefined and must be discarded rather than
    /// sign-extended along with it.
    ///
    /// The `0xFFFF` is what makes these tests worth having. Reading `mouseData`
    /// as a whole signed value instead of taking the high word gives an answer
    /// that is *almost* right, off by a fraction of a notch, so it survives a
    /// hand test on a notched wheel and only shows up as drift on a touchpad.
    /// Serialises the tests that drive [`wheel_notches`], which reads and
    /// writes two process-global atomics.
    ///
    /// `libtest` runs tests concurrently in one process, so without this the
    /// three below race on `WHEEL_RESIDUE` and each other's part-notches, and
    /// they race *intermittently*, which is worse than failing: the suite would
    /// go green on most runs and red on a machine with a different core count.
    /// Same reasoning, and the same shape, as `precapture::frame_store_guard`.
    fn wheel_guard() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: Mutex<()> = Mutex::new(());
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wheel(delta: i16) -> u32 {
        #[expect(
            clippy::cast_sign_loss,
            reason = "reconstructing the documented bit layout"
        )]
        let high = ((delta as u32) << 16) | 0xFFFF;
        high
    }

    #[test]
    fn a_notched_wheel_gives_one_notch_per_click() {
        let _serial = wheel_guard();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "WHEEL_STEP is 120, asserted equal to WHEEL_DELTA at compile time"
        )]
        let step = WHEEL_STEP as i16;
        assert_eq!(wheel_notches(1, wheel(step)), 1);
        assert_eq!(wheel_notches(1, wheel(-step)), -1);
        assert_eq!(wheel_notches(1, wheel(step * 3)), 3);
    }

    /// The defect this accumulator exists for. A precision touchpad sends
    /// fractions of a notch; integer division alone floors every one of them to
    /// zero, and because the area swallows the event either way the result is
    /// an area that eats scrolls and never magnifies.
    #[test]
    fn fractions_of_a_notch_accumulate_into_one() {
        let _serial = wheel_guard();
        let id = 2;
        assert_eq!(wheel_notches(id, wheel(40)), 0);
        assert_eq!(wheel_notches(id, wheel(40)), 0);
        assert_eq!(wheel_notches(id, wheel(40)), 1, "40 * 3 == WHEEL_DELTA");
        // And the residue is spent, not counted twice.
        assert_eq!(wheel_notches(id, wheel(40)), 0);
    }

    /// A part-notch belongs to the area it was scrolled over. Carrying it
    /// across would make a second area jump on a scroll too small to have moved
    /// the first.
    #[test]
    fn residue_does_not_cross_from_one_area_to_another() {
        let _serial = wheel_guard();
        assert_eq!(wheel_notches(3, wheel(80)), 0);
        assert_eq!(wheel_notches(4, wheel(80)), 0, "starts fresh");
        assert_eq!(wheel_notches(4, wheel(40)), 1);
    }

    /// [`super::CursorShape::index`] and [`ALL_SHAPES`] are two hand-maintained
    /// halves of one mapping: [`super::snapshot_cursor`] fills the array by
    /// zipping `ALL_SHAPES` and reads it back by `index()`. Nothing in the type
    /// system ties them together, so a shape added to one and not the other, or
    /// added to both in a different order, silently hands **every** shape the
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
                "{shape:?} reads back as {:?}, `index()` and `ALL_SHAPES` disagree",
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
