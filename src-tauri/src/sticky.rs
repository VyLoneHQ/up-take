//! Sticky areas: an area that follows a window (roadmap `1.47`, ADR-0049
//! decision 3).
//!
//! The founder's own idea: *"a sticky area stays relative to the "pinned"
//! window and stays useful exactly where it should be"*. Any area can be made
//! sticky from its menu. It is then anchored to the top-level window under it
//! and no longer to the screen. Where the area belongs on the window is
//! arithmetic and lives in [`uptake_core::sticky`]. This module is the Windows
//! half: which window, where it is now, and whether it is still there.
//!
//! # How a window is followed
//!
//! Measured on the founder's four monitors on 2026-10-09, with a probe that
//! watched one window both ways while he dragged it: Windows reports a new
//! position about every 4 ms during a drag, and the move event
//! (`EVENT_OBJECT_LOCATIONCHANGE`) arrived a median 0.56 ms after 1 ms polling
//! saw the same position. 1876 of 1887 positions came within 4 ms. The other
//! 11 were 21 to 35 ms late, and every one was the window crossing to a
//! monitor with another scale.
//!
//! So the design is the event, plus a poll once a frame while a drag is in
//! progress:
//!
//! - Outside a drag, [`on_event`] hears the move and the area is placed at
//!   once. Nothing runs while no window moves.
//! - Between `EVENT_SYSTEM_MOVESIZESTART` and `EVENT_SYSTEM_MOVESIZEEND`,
//!   [`follow_drag`] reads the window's rectangle once per composed frame
//!   (`DwmFlush`). That bounds the lag at a monitor crossing by one frame,
//!   and it also means 190 events a second become one update a frame.
//!
//! With Windows' *Show window contents while dragging* turned off the window
//! does not move until the drag ends, so the area stays where it is and jumps
//! at the drop. That is the founder's choice of 2026-10-10, and it is also
//! simply what the window does.
//!
//! # When the window cannot be followed
//!
//! A minimised, hidden or cloaked window pauses the area: it waits where it
//! was, shown as paused, and follows again when the window is back. A closed
//! window pauses it too, and once a second [`reattach`] looks for a window of
//! the same program with the same title and joins the area to that one (the
//! founder's decision of 2026-10-09).
//!
//! # What is never logged
//!
//! The window's title and its program's path are kept in memory to recognise
//! the window again. They say what the user has open, so they are never
//! written to a log.
//!
//! # Limits, stated
//!
//! - A program whose path cannot be read (one running as administrator while
//!   UP-TAKE is not) is followed, but is never re-attached after it closes,
//!   because "the same program" cannot be established.
//! - Events from such a program may not arrive at all. The once-a-second check
//!   still places the area, a second late at worst.
//! - An area moved by hand while its window is minimised or closed keeps its
//!   old anchor, and goes back to it when the window returns. There is no
//!   window rectangle to take a new anchor from.
//! - Content that scrolls inside the window is not followed. That is roadmap
//!   `1.43`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use uptake_core::area::{AreaId, AreaStore};
use uptake_core::geometry::{Point, Rect};
use uptake_core::sticky::{Anchor, Sticky};

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmFlush, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows_sys::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_PER_MONITOR_AWARE, GetAwarenessFromDpiAwarenessContext, GetDpiForMonitor,
    GetDpiForWindow, GetWindowDpiAwarenessContext, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, EnumWindows,
    GWL_EXSTYLE, GetClassNameW, GetMessageW, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, KillTimer, MSG, OBJID_WINDOW,
    PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW, SetTimer, WINEVENT_OUTOFCONTEXT,
    WM_APP, WM_TIMER, WM_USER, WS_EX_TRANSPARENT,
};

use crate::overlay;
use crate::placement;

/// The app handle, for the outlet's three functions, which are called from the
/// follower thread and are not handed one.
static APP: OnceLock<AppHandle> = OnceLock::new();

/// One decision of the follower: this area, in this state, and at this
/// rectangle if its window can be followed.
type Change = (AreaId, Sticky, Option<Rect>);

/// Where the follower's decisions go.
///
/// The follower thread knows windows and nothing about the app: it decides
/// where each area belongs and hands that here. [`init`] sets the outlet to
/// the three `app_*` functions, which write to the area store and tell the
/// page. Keeping the two apart is what lets a test follow a real window with
/// no app running, by setting an outlet of its own.
#[derive(Clone, Copy)]
struct Outlet {
    /// Which of these areas still exist and are still sticky.
    still_sticky: fn(&[AreaId]) -> HashSet<AreaId>,
    /// Takes the decisions and returns the areas that actually moved.
    place: fn(&[Change]) -> Vec<AreaId>,
    /// Called for each moved area once the windows have been still for
    /// [`SETTLE_MS`].
    settle: fn(AreaId),
}

static OUTLET: OnceLock<Outlet> = OnceLock::new();

/// Every sticky area's link to its window. Shared between the thread that
/// handles the menu and the follower thread, and never held across a call that
/// takes the area store's lock.
static LINKS: Mutex<Vec<Link>> = Mutex::new(Vec::new());

/// The follower thread, which does not exist until the first area is made
/// sticky. A user who never uses the feature never pays for it.
static FOLLOWER: Mutex<Follower> = Mutex::new(Follower::Idle);

#[derive(Clone, Copy)]
enum Follower {
    Idle,
    /// Spawned, and not yet at the point where it can be posted to. It reads
    /// every link when it gets there, so nothing has to be posted.
    Starting,
    /// Running, with the thread id messages are posted to.
    Running(u32),
}

/// Posted to the follower thread when the links changed.
const WAKE: u32 = WM_APP + 0x47;

/// How often the follower checks every followed window when no event says to,
/// and looks for a window that closed. One second is what "re-attaches by
/// itself" costs a user in the worst case.
const TICK_MS: u32 = 1000;

/// How long the windows must be still before an area re-takes what it shows.
/// A snap or a maximise is several moves in a row, and each re-take is a
/// capture, so they are answered once.
const SETTLE_MS: u32 = 150;

/// How long a drag may show no movement before the per-frame poll gives up.
/// The move event still places the area after that. This exists so a program
/// that never reports the end of a drag cannot keep a thread waking every
/// frame for good.
const DRAG_IDLE: Duration = Duration::from_secs(5);

/// One sticky area's link to its window.
struct Link {
    area: AreaId,
    /// The window handle, as a number so the link can cross threads.
    window: isize,
    /// The process that owns the window. A handle whose owner changed is
    /// another window that was given the same number.
    pid: u32,
    /// The full path of the program, or `None` if Windows would not say.
    program: Option<String>,
    title: String,
    anchor: Anchor,
    /// The window's scale when the anchor was taken.
    scale: f64,
    /// What was last seen of the window and acted on. A poll that sees the
    /// same again does nothing at all.
    seen: Option<Seen>,
    /// The window closed. The link waits for [`reattach`].
    gone: bool,
}

/// What can be seen of a window right now.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Seen {
    /// It no longer exists.
    Gone,
    /// It exists and is not on screen: minimised, hidden or cloaked.
    Hidden,
    /// It is on screen here, drawn at this scale.
    At { rect: Rect, scale: f64 },
}

/// What an area should be, given what was seen of its window: its state, and
/// its rectangle if the window can be followed.
///
/// A window that cannot be followed moves nothing. The area waits where it
/// was, which is the founder's "waits, shown as paused".
fn plan(anchor: Anchor, scale: f64, seen: Seen) -> (Sticky, Option<Rect>) {
    match seen {
        Seen::At { rect, scale: now } => (Sticky::Following, Some(anchor.place(rect, now / scale))),
        Seen::Hidden | Seen::Gone => (Sticky::Paused, None),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere must not stop areas following their windows. Every
    // write under these locks leaves the data whole.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Remembers the app handle and points the follower's decisions at the app.
/// Called once at startup. It starts nothing.
pub(crate) fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let _ = OUTLET.set(Outlet {
        still_sticky: app_still_sticky,
        place: app_place,
        settle: app_settle,
    });
}

/// Makes an area sticky or frees it. Returns whether the area changed, so the
/// caller knows to send the page the new set.
pub(crate) fn set(app: &AppHandle, id: AreaId, sticky: bool) -> bool {
    if sticky {
        stick(app, id)
    } else {
        release(app, id)
    }
}

/// Anchors an area to the window under its centre. `false`, and nothing
/// changed, if there is no window there to follow.
fn stick(app: &AppHandle, id: AreaId) -> bool {
    let Some(bounds) = overlay::area_bounds(app, id) else {
        return false;
    };
    let Some(link) = link_under(id, bounds) else {
        crate::diagnostics::trouble_for_area("sticky: no window under the area to follow", id);
        return false;
    };
    {
        let mut links = lock(&LINKS);
        links.retain(|existing| existing.area != id);
        links.push(link);
    }
    let changed = {
        let store = app.state::<Mutex<AreaStore>>();
        lock(&store).set_sticky(id, Sticky::Following)
    };
    wake();
    changed
}

/// Frees an area: it is anchored to the screen again, where it is now.
fn release(app: &AppHandle, id: AreaId) -> bool {
    forget(id);
    let store = app.state::<Mutex<AreaStore>>();
    lock(&store).set_sticky(id, Sticky::Free)
}

/// Drops an area's link, because the area is gone or was freed.
pub(crate) fn forget(id: AreaId) {
    let dropped = {
        let mut links = lock(&LINKS);
        let before = links.len();
        links.retain(|link| link.area != id);
        links.len() != before
    };
    if dropped {
        wake();
    }
}

/// Takes a new anchor after the user moved or resized a sticky area by hand,
/// so the place they put it is the place it keeps.
///
/// Nothing happens if the area is not sticky, or if its window is not on
/// screen (see the module's limits).
pub(crate) fn reanchor(app: &AppHandle, id: AreaId) {
    let Some(bounds) = overlay::area_bounds(app, id) else {
        return;
    };
    let mut links = lock(&LINKS);
    let Some(link) = links.iter_mut().find(|link| link.area == id && !link.gone) else {
        return;
    };
    if let Seen::At { rect, scale } = see(link.window, link.pid) {
        link.anchor = Anchor::of(bounds, rect);
        link.scale = scale;
        link.seen = Some(Seen::At { rect, scale });
    }
}

/// Builds the link for an area that is about to become sticky.
fn link_under(id: AreaId, bounds: Rect) -> Option<Link> {
    let centre = centre_of(bounds);
    let own = std::process::id();
    let window = top_level_windows()
        .into_iter()
        .find(|&window| describe(window).is_under(centre, own))?;
    let pid = owner_of(window);
    let Seen::At { rect, scale } = see(window, pid) else {
        return None;
    };
    Some(Link {
        area: id,
        window,
        pid,
        program: program_of(pid),
        title: title_of(window),
        anchor: Anchor::of(bounds, rect),
        scale,
        seen: Some(Seen::At { rect, scale }),
        gone: false,
    })
}

fn centre_of(bounds: Rect) -> Point {
    let x = i64::from(bounds.origin.x) + i64::from(bounds.size.width / 2);
    let y = i64::from(bounds.origin.y) + i64::from(bounds.size.height / 2);
    Point::new(
        i32::try_from(x).unwrap_or(bounds.origin.x),
        i32::try_from(y).unwrap_or(bounds.origin.y),
    )
}

/// What decides whether a top-level window can be the one an area sticks to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    pid: u32,
    rect: Rect,
    /// Visible, not minimised and not cloaked.
    on_screen: bool,
    /// `WS_EX_TRANSPARENT`: the window lets the mouse through. Other
    /// programs' overlays are like this, cover whole monitors, and are never
    /// what the user means by "the window under the area".
    click_through: bool,
    class: String,
}

/// The windows that are the desktop and the taskbar. An area over the bare
/// desktop has nothing to follow, and sticking it to the desktop would tick
/// the menu row and then never do anything.
const SHELL_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

impl Candidate {
    /// Whether this window is one an area could follow at all.
    fn can_be_followed(&self, own_pid: u32) -> bool {
        self.on_screen
            && !self.click_through
            && self.pid != own_pid
            && !SHELL_CLASSES.contains(&self.class.as_str())
    }

    /// Whether this is a followable window with `point` inside it.
    fn is_under(&self, point: Point, own_pid: u32) -> bool {
        self.can_be_followed(own_pid) && self.rect.contains(point)
    }
}

fn describe(window: isize) -> Candidate {
    let handle = window as HWND;
    // SAFETY: `GetWindowLongW` takes a handle and an index and returns a value.
    // A handle that has gone stale makes it return 0, which reads as "not
    // click-through", and `see` below then reports the window as gone.
    let style = unsafe { GetWindowLongW(handle, GWL_EXSTYLE) };
    let pid = owner_of(window);
    let seen = see(window, pid);
    Candidate {
        pid,
        rect: match seen {
            Seen::At { rect, .. } => rect,
            Seen::Hidden | Seen::Gone => Rect::new(0, 0, 0, 0),
        },
        on_screen: matches!(seen, Seen::At { .. }),
        click_through: style.cast_unsigned() & WS_EX_TRANSPARENT != 0,
        class: class_of(window),
    }
}

/// Every top-level window, topmost first. `EnumWindows` walks them in that
/// order, which is what makes "the window under the area" the first match.
fn top_level_windows() -> Vec<isize> {
    unsafe extern "system" fn collect(window: HWND, list: LPARAM) -> i32 {
        // SAFETY: `list` is the `&mut Vec<isize>` that `top_level_windows`
        // passes to `EnumWindows`, which calls back before it returns, so the
        // vector is alive and nothing else is using it.
        let list = unsafe { &mut *(list as *mut Vec<isize>) };
        list.push(window as isize);
        1
    }
    let mut windows: Vec<isize> = Vec::with_capacity(256);
    // SAFETY: the callback only pushes to the vector whose address is passed,
    // and `EnumWindows` does not keep the pointer after it returns.
    unsafe {
        EnumWindows(Some(collect), (&raw mut windows) as LPARAM);
    }
    windows
}

fn owner_of(window: isize) -> u32 {
    let mut pid = 0u32;
    // SAFETY: the call writes one `u32` through the pointer and tolerates a
    // stale handle, leaving `pid` at 0.
    unsafe {
        GetWindowThreadProcessId(window as HWND, &raw mut pid);
    }
    pid
}

/// The window's title, as it is now. Empty if it has none.
fn title_of(window: isize) -> String {
    let mut buffer = [0u16; 512];
    // SAFETY: the buffer is 512 units long and the call is told so. For a
    // window of another process this reads the title Windows holds and does
    // not send that program a message, so a hung program cannot hang us.
    let length = unsafe { GetWindowTextW(window as HWND, buffer.as_mut_ptr(), 512) };
    text_of(&buffer, length)
}

fn class_of(window: isize) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: the buffer is 256 units long and the call is told so.
    let length = unsafe { GetClassNameW(window as HWND, buffer.as_mut_ptr(), 256) };
    text_of(&buffer, length)
}

fn text_of(buffer: &[u16], length: i32) -> String {
    let length = usize::try_from(length).unwrap_or(0).min(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

/// The full path of a process's program, or `None` if Windows will not open
/// the process for even this much.
fn program_of(pid: u32) -> Option<String> {
    // SAFETY: `OpenProcess` returns a handle or null. The handle is closed
    // below on every path that got one.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buffer = [0u16; 1024];
    let mut length = 1024u32;
    // SAFETY: the buffer is 1024 units long and `length` says so going in. The
    // call writes at most that many and sets `length` to what it wrote.
    let read =
        unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &raw mut length) };
    // SAFETY: `process` is the handle opened above, closed exactly once.
    unsafe {
        CloseHandle(process);
    }
    if read == 0 {
        return None;
    }
    let length = usize::try_from(length).unwrap_or(0).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// Looks at a window: gone, hidden, or on screen at a rectangle and a scale.
fn see(window: isize, pid: u32) -> Seen {
    let handle = window as HWND;
    // SAFETY: each call takes a handle and reads. All of them tolerate a
    // handle that has gone stale, which is the case `Seen::Gone` exists for.
    // `GetWindowRect` writes one `RECT` through a pointer to a local.
    unsafe {
        if IsWindow(handle) == 0 || owner_of(window) != pid {
            return Seen::Gone;
        }
        if IsWindowVisible(handle) == 0 || IsIconic(handle) != 0 || is_cloaked(handle) {
            return Seen::Hidden;
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(handle, &raw mut rect) == 0 {
            return Seen::Hidden;
        }
        Seen::At {
            rect: Rect::new(
                rect.left,
                rect.top,
                u32::try_from(rect.right.saturating_sub(rect.left)).unwrap_or(0),
                u32::try_from(rect.bottom.saturating_sub(rect.top)).unwrap_or(0),
            ),
            scale: scale_of(handle),
        }
    }
}

/// Whether the window is cloaked: it exists and is "visible", and Windows is
/// not drawing it. A window on another virtual desktop is the usual case.
fn is_cloaked(handle: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: the call writes one `u32`, and is told the size of one.
    let result = unsafe {
        DwmGetWindowAttribute(
            handle,
            DWMWA_CLOAKED.cast_unsigned(),
            (&raw mut cloaked).cast(),
            4,
        )
    };
    result == 0 && cloaked != 0
}

/// The scale the window's contents are drawn at, where 1.0 is 96 DPI.
///
/// A program that handles each monitor's scale itself says what it draws at.
/// One that does not is stretched by Windows to the monitor it is on, so the
/// monitor's scale is the answer for it.
fn scale_of(handle: HWND) -> f64 {
    // SAFETY: every call here takes a handle or a value and returns a value.
    // `GetDpiForMonitor` writes two `u32`s through pointers to locals.
    let dpi = unsafe {
        let awareness = GetAwarenessFromDpiAwarenessContext(GetWindowDpiAwarenessContext(handle));
        if awareness == DPI_AWARENESS_PER_MONITOR_AWARE {
            GetDpiForWindow(handle)
        } else {
            let monitor = MonitorFromWindow(handle, MONITOR_DEFAULTTONEAREST);
            let (mut across, mut down) = (0u32, 0u32);
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut across, &raw mut down) == 0 {
                across
            } else {
                0
            }
        }
    };
    if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 }
}

/// Starts the follower thread, or tells the running one the links changed.
fn wake() {
    let mut follower = lock(&FOLLOWER);
    match *follower {
        Follower::Running(thread) => {
            // SAFETY: posts a message with no payload to a thread id. If the
            // thread is gone the call fails and nothing else happens.
            unsafe {
                PostThreadMessageW(thread, WAKE, 0, 0);
            }
        }
        Follower::Starting => {}
        Follower::Idle => {
            *follower = Follower::Starting;
            let spawned = std::thread::Builder::new()
                .name("uptake-sticky".to_owned())
                .spawn(run);
            if let Err(error) = spawned {
                *follower = Follower::Idle;
                crate::diagnostics::trouble(
                    "sticky: could not start the thread that follows windows",
                    &error.to_string(),
                );
            }
        }
    }
}

thread_local! {
    /// The event hooks this thread holds, by the process they listen to.
    static HOOKS: RefCell<HashMap<u32, Vec<isize>>> = RefCell::new(HashMap::new());
    /// The window being dragged or resized right now, if it is a followed one.
    static DRAG: Cell<Option<isize>> = const { Cell::new(None) };
    /// The areas that moved and have not yet re-taken what they show.
    static UNSETTLED: RefCell<HashSet<AreaId>> = RefCell::new(HashSet::new());
    /// The once-a-second timer, while any area is sticky. 0 is "not running".
    static TICK: Cell<usize> = const { Cell::new(0) };
    /// The one-shot timer that ends in [`settle`]. 0 is "not running".
    static SETTLE: Cell<usize> = const { Cell::new(0) };
}

/// The follower thread: a message loop that the event hooks call back into.
///
/// Out-of-context event hooks deliver on the thread that set them, and only
/// while that thread is waiting for messages. So the thread that sets the
/// hooks is this one, and it does nothing else.
fn run() {
    // SAFETY: `MSG` is plain integers and pointers, for which all zeroes is a
    // valid value. It is only ever written by the calls below.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: asking for a message creates this thread's queue, so a `WAKE`
    // posted from now on is kept. Nothing is removed and nothing is read.
    unsafe {
        PeekMessageW(
            &raw mut message,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        );
    }
    // SAFETY: takes nothing and returns this thread's id.
    *lock(&FOLLOWER) = Follower::Running(unsafe { GetCurrentThreadId() });
    refresh();
    loop {
        // SAFETY: writes one `MSG` through the pointer. Event callbacks run
        // inside this call, on this thread.
        let got = unsafe { GetMessageW(&raw mut message, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            break;
        }
        handle(&message);
        while let Some(window) = DRAG.get() {
            follow_drag(window);
        }
    }
    *lock(&FOLLOWER) = Follower::Idle;
}

fn handle(message: &MSG) {
    match message.message {
        WM_TIMER if message.wParam == TICK.get() => {
            refresh();
            reattach();
        }
        WM_TIMER if message.wParam == SETTLE.get() => settle(),
        WAKE => refresh(),
        _ => {}
    }
}

/// Brings everything in line with the links as they are now: drops the links
/// whose area is gone, listens to exactly the programs that are followed, runs
/// the once-a-second timer only while something is sticky, and places every
/// area by its window.
fn refresh() {
    prune();
    let (programs, windows, any) = {
        let links = lock(&LINKS);
        let live = || links.iter().filter(|link| !link.gone);
        (
            live().map(|link| link.pid).collect::<HashSet<u32>>(),
            live().map(|link| link.window).collect::<HashSet<isize>>(),
            !links.is_empty(),
        )
    };
    listen_to(&programs);
    keep_ticking(any);
    for window in windows {
        sync(window);
    }
}

/// Drops the links whose area no longer exists or is no longer sticky. The
/// store is the one answer to "is this area sticky", so a path that removes
/// an area without telling this module is still cleaned up within a second.
fn prune() {
    let Some(outlet) = OUTLET.get() else {
        return;
    };
    let linked: Vec<AreaId> = lock(&LINKS).iter().map(|link| link.area).collect();
    if linked.is_empty() {
        return;
    }
    let kept = (outlet.still_sticky)(&linked);
    lock(&LINKS).retain(|link| kept.contains(&link.area));
}

/// [`Outlet::still_sticky`] for the app: asks the area store.
fn app_still_sticky(linked: &[AreaId]) -> HashSet<AreaId> {
    let Some(app) = APP.get() else {
        return linked.iter().copied().collect();
    };
    let store = app.state::<Mutex<AreaStore>>();
    let store = lock(&store);
    linked
        .iter()
        .copied()
        .filter(|&id| store.get(id).is_some_and(|area| area.sticky.is_sticky()))
        .collect()
}

/// The events one followed program is listened to for, as inclusive ranges.
/// Five narrow ranges and not one wide one: everything between the first and
/// the last of these is every focus change and every redraw of every control
/// the program has.
const EVENT_RANGES: [(u32, u32); 5] = [
    (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
    (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
    (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
    (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE),
    (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
];

/// Sets hooks for the programs in `wanted` that have none, and removes the
/// hooks of programs no longer followed.
///
/// A hook is per program and not for the whole desktop. A desktop-wide hook on
/// the move event hears every movement of the mouse pointer, which would wake
/// this thread hundreds of times a second for as long as one area is sticky.
fn listen_to(wanted: &HashSet<u32>) {
    HOOKS.with_borrow_mut(|hooks| {
        hooks.retain(|pid, set| {
            if wanted.contains(pid) {
                return true;
            }
            for &hook in set.iter() {
                // SAFETY: `hook` came from `SetWinEventHook` on this thread
                // and is removed from the map here, so it is unhooked once.
                unsafe {
                    UnhookWinEvent(hook as HWINEVENTHOOK);
                }
            }
            false
        });
        for &pid in wanted {
            if hooks.contains_key(&pid) {
                continue;
            }
            let set = EVENT_RANGES
                .iter()
                .filter_map(|&(first, last)| {
                    // SAFETY: `on_event` matches the callback signature and
                    // touches only this module's own state. No module handle
                    // is needed for an out-of-context hook.
                    let hook = unsafe {
                        SetWinEventHook(
                            first,
                            last,
                            std::ptr::null_mut(),
                            Some(on_event),
                            pid,
                            0,
                            WINEVENT_OUTOFCONTEXT,
                        )
                    };
                    (!hook.is_null()).then_some(hook as isize)
                })
                .collect();
            hooks.insert(pid, set);
        }
    });
}

/// Runs the once-a-second timer while anything is sticky and stops it when
/// nothing is, so an idle UP-TAKE with no sticky area has a thread that never
/// wakes.
fn keep_ticking(wanted: bool) {
    let running = TICK.get();
    if wanted && running == 0 {
        // SAFETY: a timer with no window posts `WM_TIMER` to this thread's
        // queue. No callback is given.
        TICK.set(unsafe { SetTimer(std::ptr::null_mut(), 0, TICK_MS, None) });
    } else if !wanted && running != 0 {
        // SAFETY: `running` is the id `SetTimer` returned on this thread.
        unsafe {
            KillTimer(std::ptr::null_mut(), running);
        }
        TICK.set(0);
    }
}

/// The event callback. Runs on the follower thread, inside its wait for
/// messages.
unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only the window itself. The same events fire for its caret, its scroll
    // bars and every control inside it.
    if object != OBJID_WINDOW || child != 0 || window.is_null() {
        return;
    }
    let window = window as isize;
    if !lock(&LINKS)
        .iter()
        .any(|link| link.window == window && !link.gone)
    {
        return;
    }
    #[cfg(test)]
    tests::EVENTS_HEARD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match event {
        EVENT_SYSTEM_MOVESIZESTART => DRAG.set(Some(window)),
        EVENT_SYSTEM_MOVESIZEEND => {
            if DRAG.get() == Some(window) {
                DRAG.set(None);
            }
            sync(window);
        }
        // During a drag the per-frame poll places the area. Answering each of
        // the 190 events a second as well would only repeat it.
        EVENT_OBJECT_LOCATIONCHANGE if DRAG.get() == Some(window) => {}
        EVENT_OBJECT_NAMECHANGE => retitle(window),
        // A move outside a drag, and every way a window stops or starts being
        // on screen: minimised, restored, hidden, shown, cloaked, destroyed.
        // `sync` looks at the window itself, so they are all one case.
        _ => {
            sync(window);
        }
    }
}

/// Keeps the title a link remembers current, so that when the window closes
/// the title looked for is the one it closed with.
fn retitle(window: isize) {
    let title = title_of(window);
    for link in lock(&LINKS)
        .iter_mut()
        .filter(|link| link.window == window && !link.gone)
    {
        link.title.clone_from(&title);
    }
}

/// Follows one window through a drag or resize, once per composed frame.
///
/// Returns when the drag ended, the window went away, or nothing has moved for
/// [`DRAG_IDLE`]. The messages and event callbacks that arrive meanwhile are
/// handled between frames, which is how the end of the drag gets in.
fn follow_drag(window: isize) {
    let mut last_change = Instant::now();
    let mut last_seen = None;
    while DRAG.get() == Some(window) {
        // SAFETY: takes nothing. Blocks until the compositor has presented a
        // frame, which is the "once a frame" of the design.
        if unsafe { DwmFlush() } != 0 {
            // No compositor to wait on. Do not spin.
            std::thread::sleep(Duration::from_millis(4));
        }
        let seen = sync(window);
        if seen != last_seen {
            last_seen = seen;
            last_change = Instant::now();
        }
        let over = matches!(seen, None | Some(Seen::Gone)) || last_change.elapsed() > DRAG_IDLE;
        if over {
            DRAG.set(None);
        }
        // SAFETY: as in `run`. Each call writes one `MSG`.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: removes and returns one queued message, or returns 0. Event
        // callbacks run inside this call.
        while unsafe { PeekMessageW(&raw mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0
        {
            handle(&message);
        }
    }
}

/// Looks at one followed window and puts its areas where they belong.
///
/// Returns what was seen, or `None` if no live link follows this window. A
/// window that looks exactly as it did last time costs one look and no more.
fn sync(window: isize) -> Option<Seen> {
    let mut changes: Vec<Change> = Vec::new();
    let seen = {
        let mut links = lock(&LINKS);
        let pid = links
            .iter()
            .find(|link| link.window == window && !link.gone)?
            .pid;
        let seen = see(window, pid);
        for link in links
            .iter_mut()
            .filter(|link| link.window == window && !link.gone)
        {
            if link.seen == Some(seen) {
                continue;
            }
            link.seen = Some(seen);
            link.gone = seen == Seen::Gone;
            let (state, bounds) = plan(link.anchor, link.scale, seen);
            changes.push((link.area, state, bounds));
        }
        seen
    };
    if !changes.is_empty()
        && let Some(outlet) = OUTLET.get()
    {
        let moved = (outlet.place)(&changes);
        if !moved.is_empty() {
            UNSETTLED.with_borrow_mut(|unsettled| unsettled.extend(moved));
            // SAFETY: as in `keep_ticking`. Passing the id of the timer that
            // is already running restarts it, so the wait is counted from the
            // last move and not from the first.
            SETTLE.set(unsafe { SetTimer(std::ptr::null_mut(), SETTLE.get(), SETTLE_MS, None) });
        }
    }
    Some(seen)
}

/// [`Outlet::place`] for the app: writes the new states and rectangles to the
/// store and sends the page the new set, once, if anything changed. Returns
/// the areas that moved.
fn app_place(changes: &[Change]) -> Vec<AreaId> {
    let Some(app) = APP.get() else {
        return Vec::new();
    };
    let mut moved: Vec<AreaId> = Vec::new();
    let mut changed = false;
    {
        let store = app.state::<Mutex<AreaStore>>();
        let mut store = lock(&store);
        for &(id, state, bounds) in changes {
            changed |= store.set_sticky(id, state);
            let Some(bounds) = bounds else {
                continue;
            };
            // Following a window does not raise the area: the user did not
            // touch it, so the stack stays as they left it.
            if store.get(id).is_some_and(|area| area.bounds != bounds)
                && store.set_bounds(id, bounds)
            {
                moved.push(id);
            }
        }
    }
    if (changed || !moved.is_empty())
        && let Err(error) = overlay::emit_areas(app)
    {
        crate::diagnostics::trouble(
            "sticky: an area followed its window and the page was not told",
            &error,
        );
    }
    moved
}

/// [`Outlet::settle`] for the app: the area re-takes what it shows, exactly as
/// it does when the user lets go after moving it by hand.
fn app_settle(id: AreaId) {
    if let Some(app) = APP.get() {
        overlay::refresh_magnification(app, id);
        placement::reread_in_place_ocr(app, id);
    }
}

/// The windows have been still for [`SETTLE_MS`]: each area that moved
/// re-takes what it shows.
fn settle() {
    let timer = SETTLE.replace(0);
    if timer != 0 {
        // SAFETY: `timer` is the id `SetTimer` returned on this thread.
        unsafe {
            KillTimer(std::ptr::null_mut(), timer);
        }
    }
    let moved = UNSETTLED.take();
    if let Some(outlet) = OUTLET.get() {
        for id in moved {
            (outlet.settle)(id);
        }
    }
}

/// Looks for the windows of links whose window closed, and joins each area to
/// a window of the same program with the same title, if one is on screen.
fn reattach() {
    let waiting: Vec<(AreaId, String, String)> = lock(&LINKS)
        .iter()
        .filter(|link| link.gone)
        .filter_map(|link| Some((link.area, link.program.clone()?, link.title.clone())))
        .collect();
    if waiting.is_empty() {
        return;
    }
    let own = std::process::id();
    // The title is read first because it is cheap and almost never matches.
    // The program's path means opening the process, so it is read only for a
    // window whose title already fits.
    let titled: Vec<(isize, String)> = top_level_windows()
        .into_iter()
        .map(|window| (window, title_of(window)))
        .filter(|(_, title)| waiting.iter().any(|(_, _, wanted)| wanted == title))
        .collect();
    let mut joined = false;
    for (area, program, title) in waiting {
        let found = titled.iter().find_map(|(window, candidate)| {
            if *candidate != title || !describe(*window).can_be_followed(own) {
                return None;
            }
            let pid = owner_of(*window);
            (program_of(pid).as_deref() == Some(program.as_str())).then_some((*window, pid))
        });
        let Some((window, pid)) = found else {
            continue;
        };
        if let Some(link) = lock(&LINKS)
            .iter_mut()
            .find(|link| link.area == area && link.gone)
        {
            link.window = window;
            link.pid = pid;
            link.gone = false;
            link.seen = None;
            joined = true;
        }
    }
    if joined {
        refresh();
    }
}

#[cfg(test)]
mod tests {
    use uptake_core::geometry::{Point, Rect};
    use uptake_core::sticky::{Anchor, Sticky};

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use uptake_core::area::{AreaId, AreaStore, AreaType};

    use super::{
        Candidate, Change, EVENT_RANGES, FOLLOWER, Follower, LINKS, Link, OUTLET, Outlet, Seen,
        centre_of, forget, lock, plan, see, text_of, title_of, wake,
    };

    const WINDOW: Rect = Rect::new(100, 200, 900, 600);
    const OWN: u32 = 4242;

    /// Every placement `sync` decided, for the real-window test to read.
    static PLACED: Mutex<Vec<Change>> = Mutex::new(Vec::new());
    /// How many events about a followed window reached the callback.
    pub(super) static EVENTS_HEARD: AtomicUsize = AtomicUsize::new(0);

    fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    fn placed(id: AreaId, state: Sticky, bounds: Option<Rect>) -> bool {
        lock(&PLACED).contains(&(id, state, bounds))
    }

    /// The whole path with a real window: the thread starts, the hooks are
    /// set, Windows reports each move, the callback places the area, and a
    /// destroyed window pauses it.
    ///
    /// The window is created far off every monitor and never activated, so
    /// running this draws nothing on anyone's screen and takes no keystroke.
    /// What it cannot cover is a drag by hand (the per-frame poll) and the
    /// re-attach, which refuses windows of its own process. Those are rig
    /// steps.
    #[test]
    #[ignore = "needs a desktop session: it creates a real window, off every monitor, and listens to its events"]
    fn a_real_window_is_followed_by_its_events_and_paused_when_it_closes() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE,
            SWP_NOZORDER, SetWindowPos, ShowWindow, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
        };
        let class: Vec<u16> = "Static\0".encode_utf16().collect();
        let title: Vec<u16> = "uptake sticky test\0".encode_utf16().collect();
        // SAFETY: both strings are null-terminated and outlive the call. Every
        // handle argument may be null for a top-level window of a system class.
        let handle = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                30_000,
                30_000,
                400,
                300,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!handle.is_null(), "the test window was created");
        // SAFETY: `handle` is the window created above, on this thread.
        unsafe {
            ShowWindow(handle, SW_SHOWNOACTIVATE);
        }
        let window = handle as isize;
        let pid = std::process::id();
        let Seen::At { rect, scale } = see(window, pid) else {
            panic!("Windows counts the test window as on screen")
        };
        let area = Rect::new(rect.origin.x + 20, rect.origin.y + 30, 50, 40);
        let Some(id) = AreaStore::new().create(AreaType::Default, area) else {
            panic!("an area id")
        };
        lock(&LINKS).push(Link {
            area: id,
            window,
            pid,
            program: None,
            title: title_of(window),
            anchor: Anchor::of(area, rect),
            scale,
            seen: Some(Seen::At { rect, scale }),
            gone: false,
        });
        // An outlet of the test's own, in place of the app: every area is
        // still sticky, and each decision is written down to be read below.
        let _ = OUTLET.set(Outlet {
            still_sticky: |linked| linked.iter().copied().collect(),
            place: |changes| {
                lock(&PLACED).extend_from_slice(changes);
                Vec::new()
            },
            settle: |_| {},
        });
        wake();
        assert!(
            wait_for(Duration::from_secs(2), || matches!(
                *lock(&FOLLOWER),
                Follower::Running(_)
            )),
            "the follower thread started"
        );
        // The hooks are set in the thread's first pass, right after it reports
        // itself running.
        std::thread::sleep(Duration::from_millis(150));

        let heard_before = EVENTS_HEARD.load(Ordering::Relaxed);
        for step in 1..=5 {
            let (x, y) = (rect.origin.x + 10 * step, rect.origin.y + 7 * step);
            // SAFETY: moves the window created above. No size, order or
            // activation changes.
            unsafe {
                SetWindowPos(
                    handle,
                    std::ptr::null_mut(),
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            let expected = Some(Rect::new(x + 20, y + 30, 50, 40));
            assert!(
                wait_for(Duration::from_millis(250), || placed(
                    id,
                    Sticky::Following,
                    expected
                )),
                "move {step} was not followed within 250 ms"
            );
        }
        // The once-a-second check would also place the area, a second late.
        // Five moves followed inside 250 ms each cannot all be that check, and
        // this count says outright that the events arrived.
        let heard = EVENTS_HEARD.load(Ordering::Relaxed) - heard_before;
        assert!(heard >= 5, "{heard} events heard for 5 moves");

        // SAFETY: destroys the window created above, on the thread that made it.
        unsafe {
            DestroyWindow(handle);
        }
        assert!(
            wait_for(Duration::from_millis(2500), || placed(
                id,
                Sticky::Paused,
                None
            )),
            "a destroyed window pauses the area"
        );
        forget(id);
    }

    fn window(class: &str) -> Candidate {
        Candidate {
            pid: 7,
            rect: WINDOW,
            on_screen: true,
            click_through: false,
            class: class.to_owned(),
        }
    }

    #[test]
    fn a_window_on_screen_places_the_area_and_one_that_is_not_moves_nothing() {
        let area = Rect::new(140, 260, 200, 80);
        let anchor = Anchor::of(area, WINDOW);
        let moved = Rect::new(-1500, 40, 900, 600);
        assert_eq!(
            plan(
                anchor,
                1.0,
                Seen::At {
                    rect: moved,
                    scale: 1.0
                }
            ),
            (Sticky::Following, Some(Rect::new(-1460, 100, 200, 80)))
        );
        // Minimised, hidden or closed: the area waits where it was. A
        // minimised window reports a rectangle far off every screen, and an
        // area sent there would be lost.
        assert_eq!(plan(anchor, 1.0, Seen::Hidden), (Sticky::Paused, None));
        assert_eq!(plan(anchor, 1.0, Seen::Gone), (Sticky::Paused, None));
    }

    #[test]
    fn a_window_on_a_monitor_with_another_scale_scales_the_area_by_the_ratio() {
        // Anchored at 125 %, now on a 100 % monitor: everything is 0.8 times
        // as large, the 50 px distance and the 200 x 100 area alike.
        let area = Rect::new(150, 250, 200, 100);
        let anchor = Anchor::of(area, WINDOW);
        let smaller = Rect::new(3000, 0, 720, 480);
        assert_eq!(
            plan(
                anchor,
                1.25,
                Seen::At {
                    rect: smaller,
                    scale: 1.0
                }
            ),
            (Sticky::Following, Some(Rect::new(3040, 40, 160, 80)))
        );
    }

    #[test]
    fn an_ordinary_window_under_the_point_can_be_followed() {
        let inside = Point::new(500, 500);
        assert!(window("Notepad").is_under(inside, OWN));
        assert!(
            !window("Notepad").is_under(Point::new(50, 500), OWN),
            "outside it"
        );
    }

    #[test]
    fn the_desktop_the_taskbar_overlays_and_our_own_windows_are_never_followed() {
        let inside = Point::new(500, 500);
        for class in [
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
        ] {
            assert!(!window(class).is_under(inside, OWN), "{class}");
        }
        let overlay = Candidate {
            click_through: true,
            ..window("SomeOverlay")
        };
        assert!(!overlay.is_under(inside, OWN), "another program's overlay");
        let ours = Candidate {
            pid: OWN,
            ..window("UP-TAKE")
        };
        assert!(!ours.is_under(inside, OWN), "our own window");
        let minimised = Candidate {
            on_screen: false,
            ..window("Notepad")
        };
        assert!(!minimised.is_under(inside, OWN), "not on screen");
    }

    #[test]
    fn the_centre_of_an_area_is_inside_it_even_at_the_edge_of_the_range() {
        assert_eq!(centre_of(Rect::new(-100, 40, 60, 20)), Point::new(-70, 50));
        let far = Rect::new(i32::MAX - 10, 0, 4000, 10);
        assert_eq!(centre_of(far).x, i32::MAX - 10, "falls back to the corner");
    }

    #[test]
    fn text_is_cut_at_the_length_windows_reports_and_never_past_the_buffer() {
        let buffer: Vec<u16> = "Untitled - Notepad".encode_utf16().collect();
        assert_eq!(text_of(&buffer, 8), "Untitled");
        assert_eq!(text_of(&buffer, 0), "");
        assert_eq!(text_of(&buffer, -1), "", "a failed call");
        assert_eq!(text_of(&buffer, 9999), "Untitled - Notepad");
    }

    #[test]
    fn every_event_range_is_narrow_and_in_order() {
        // A range that is wide by accident listens to every event between its
        // ends, which is the cost the five separate ranges exist to avoid.
        for (first, last) in EVENT_RANGES {
            assert!(first <= last);
            assert!(last - first <= 2, "{first:#x} to {last:#x}");
        }
    }
}
