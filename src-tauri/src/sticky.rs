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
//!   once. There is no per-frame poll outside a drag. There IS a slow one: for
//!   as long as any area is sticky, every followed window is looked at once a
//!   second (see below). The design measured on 2026-10-09 had no such check
//!   and "cost nothing while no window moves". This build costs one look per
//!   followed window per second, and that cost is not yet measured against the
//!   idle CPU bar.
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
//! founder's decision of 2026-10-09). His word was that such a window
//! "appears", so a window that was already open when the followed one closed
//! is not taken: two windows of one program can share a title, and the area
//! was on one of them, not on the other.
//!
//! The once-a-second check does three things while any area is sticky: it
//! drops the links of areas that no longer exist, it looks at every followed
//! window in case an event never came, and it runs [`reattach`]. With no
//! sticky area the timer is stopped and the thread never wakes.
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
//! - "The same program" is the same full path. A program that updates itself
//!   into a folder named for its version is another program by this test, and
//!   its area stays paused.
//! - "The window under the area" is the first one `EnumWindows` reports that
//!   contains the area's centre. Windows reports top-level windows topmost
//!   first in practice, and does not promise to.
//! - A link remembers which windows were open when its window closed, by
//!   handle. A new window that Windows gives one of those handle values is
//!   passed by, and the area would wait until the next one.
//! - Content that scrolls inside the window is not followed. That is roadmap
//!   `1.43`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use uptake_core::area::{AreaId, AreaStore};
use uptake_core::geometry::{Point, Rect};
use uptake_core::interaction;
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
/// handles the menu and the follower thread.
///
/// **The lock rule.** This lock may be held while the area store's lock is
/// taken. The area store's lock is never held while this one is taken. One
/// direction only, so the two cannot wait on each other.
///
/// Until the second review this lock was never held across a store call at
/// all, and every step that touched both did so in two halves. Each pair of
/// halves had a gap, and two reviews each found a different thread landing in
/// one: a new link dropped by the once-a-second check, an area left ticked
/// with nothing following it. The steps that touch both ([`stick_in`],
/// [`release_in`], [`prune_in`]) now hold this lock for the whole step, so
/// there is no gap to land in.
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

/// Posted by the follower thread to itself when a drag starts, so its wait for
/// a message ends and the per-frame poll begins. It carries nothing: the drag
/// is in [`DRAGGED`].
const DRAG_STARTED: u32 = WM_APP + 0x48;

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
    /// Every top-level window that was open when this one closed. None of
    /// them is the window "appearing" again, so [`reattach`] passes them by.
    /// Empty while the window is alive.
    open_at_close: HashSet<isize>,
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
    let store = app.state::<Mutex<AreaStore>>();
    let changed = stick_in(store.inner(), &LINKS, link);
    wake();
    changed
}

/// Records that an area follows a window, as one step: the links are held
/// while the store is told and the link is added, so the follower never sees
/// one without the other.
fn stick_in(store: &Mutex<AreaStore>, links: &Mutex<Vec<Link>>, link: Link) -> bool {
    let id = link.area;
    let mut links = lock(links);
    let changed = {
        let mut store = lock(store);
        if store.get(id).is_none() {
            return false;
        }
        store.set_sticky(id, Sticky::Following)
    };
    links.retain(|existing| existing.area != id);
    links.push(link);
    changed
}

/// Frees an area: it is anchored to the screen again, where it is now.
fn release(app: &AppHandle, id: AreaId) -> bool {
    let store = app.state::<Mutex<AreaStore>>();
    let changed = release_in(store.inner(), &LINKS, id);
    wake();
    changed
}

/// Drops the link and tells the store the area is free, as one step. A
/// decision the follower made a moment before cannot undo this: [`write`]
/// leaves an area alone once the store calls it free.
fn release_in(store: &Mutex<AreaStore>, links: &Mutex<Vec<Link>>, id: AreaId) -> bool {
    let mut links = lock(links);
    links.retain(|link| link.area != id);
    lock(store).set_sticky(id, Sticky::Free)
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
/// so the place they put it is the place it keeps. `placed` is where the hand
/// left it.
///
/// Nothing happens if the area is not sticky, or if its window is not on
/// screen (see the module's limits).
pub(crate) fn reanchor(app: &AppHandle, id: AreaId, placed: Rect) {
    let store = app.state::<Mutex<AreaStore>>();
    reanchor_in(store.inner(), &LINKS, id, placed, see);
}

/// [`reanchor`] on given state. The links are held throughout, as in every
/// step that touches both them and the store.
///
/// The area is put back at `placed` first. The follower may have written its
/// own placement between the hand's write and this call, from a decision it
/// made a moment earlier, and the hand is the later word.
fn reanchor_in(
    store: &Mutex<AreaStore>,
    links: &Mutex<Vec<Link>>,
    id: AreaId,
    placed: Rect,
    see: impl FnOnce(isize, u32) -> Seen,
) {
    let mut links = lock(links);
    let Some(link) = links.iter_mut().find(|link| link.area == id && !link.gone) else {
        return;
    };
    {
        let mut store = lock(store);
        if store.get(id).is_some_and(|area| area.bounds != placed) {
            store.set_bounds(id, placed);
        }
    }
    let seen = see(link.window, link.pid);
    retake(link, placed, seen);
}

/// Takes a link's anchor again from where the area and its window are now. A
/// window that is not on screen has no rectangle to measure from, so the old
/// anchor stays.
fn retake(link: &mut Link, bounds: Rect, seen: Seen) {
    if let Seen::At { rect, scale } = seen {
        link.anchor = Anchor::of(bounds, rect);
        link.scale = scale;
        link.seen = Some(seen);
    }
}

/// The first of `windows`, in the order given, that can be followed and has
/// `point` inside it. The order is topmost first, so this is the window the
/// user sees under the area.
fn first_under(
    windows: impl IntoIterator<Item = (isize, Candidate)>,
    point: Point,
    own_pid: u32,
) -> Option<isize> {
    windows
        .into_iter()
        .find(|(_, candidate)| candidate.is_under(point, own_pid))
        .map(|(window, _)| window)
}

/// Whether an area at `bounds` has a window under it to stick to. The menu
/// greys its row out when it has none, so the row never ticks nothing and
/// says nothing. It asks [`window_under`], the question [`stick`] asks, so
/// the two cannot disagree about the same screen.
pub(crate) fn can_stick(bounds: Rect) -> bool {
    window_under(bounds).is_some()
}

/// The window an area at `bounds` would stick to: the topmost one under its
/// centre that can be followed, with its owner, its rectangle and its scale.
fn window_under(bounds: Rect) -> Option<(isize, u32, Rect, f64)> {
    let window = first_under(
        top_level_windows()
            .into_iter()
            .map(|window| (window, describe(window))),
        centre_of(bounds),
        std::process::id(),
    )?;
    let pid = owner_of(window);
    let Seen::At { rect, scale } = see(window, pid) else {
        return None;
    };
    Some((window, pid, rect, scale))
}

/// Builds the link for an area that is about to become sticky.
fn link_under(id: AreaId, bounds: Rect) -> Option<Link> {
    let (window, pid, rect, scale) = window_under(bounds)?;
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
        open_at_close: HashSet::new(),
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

/// The window being dragged or resized right now, if it is a followed one. 0
/// is "none", which no window handle is. Written only by the follower thread.
/// It is a static and not a thread-local so a test can see a drag end.
static DRAGGED: AtomicIsize = AtomicIsize::new(0);

fn dragged() -> Option<isize> {
    match DRAGGED.load(Ordering::Relaxed) {
        0 => None,
        window => Some(window),
    }
}

fn set_dragged(window: Option<isize>) {
    DRAGGED.store(window.unwrap_or(0), Ordering::Relaxed);
}

thread_local! {
    /// The event hooks this thread holds, by the process they listen to.
    static HOOKS: RefCell<HashMap<u32, Vec<isize>>> = RefCell::new(HashMap::new());
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
        while let Some(window) = dragged() {
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
    if let Some(outlet) = OUTLET.get() {
        prune_in(&LINKS, outlet.still_sticky);
    }
}

/// [`prune`] on a given set of links. The links are held from the question to
/// the answer, so a link added meanwhile is either asked about or not there
/// yet. It cannot be dropped for never having been asked about.
fn prune_in(links: &Mutex<Vec<Link>>, still_sticky: impl FnOnce(&[AreaId]) -> HashSet<AreaId>) {
    let mut links = lock(links);
    if links.is_empty() {
        return;
    }
    let linked: Vec<AreaId> = links.iter().map(|link| link.area).collect();
    let kept = still_sticky(&linked);
    links.retain(|link| kept.contains(&link.area));
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
        EVENT_SYSTEM_MOVESIZESTART => {
            set_dragged(Some(window));
            // This callback runs inside the thread's wait for a message, and
            // the wait does not end because a callback ran. Without a message
            // of its own the per-frame poll would start at the next timer
            // tick, up to a second into the drag, with every move ignored
            // until then.
            // SAFETY: posts a message with no payload to this thread.
            unsafe {
                PostThreadMessageW(GetCurrentThreadId(), DRAG_STARTED, 0, 0);
            }
        }
        EVENT_SYSTEM_MOVESIZEEND => {
            if dragged() == Some(window) {
                set_dragged(None);
            }
            sync(window);
        }
        // During a drag the per-frame poll places the area. Answering each of
        // the 190 events a second as well would only repeat it.
        EVENT_OBJECT_LOCATIONCHANGE if dragged() == Some(window) => {}
        EVENT_OBJECT_NAMECHANGE => retitle(window),
        // Windows says the window is destroyed, so it is gone, whatever a look
        // at it says. Measured: when this event arrives the handle still
        // answers as a live, hidden window, and it went on doing so until the
        // once-a-second check, about 790 ms later.
        EVENT_OBJECT_DESTROY => {
            sync_as(window, Some(Seen::Gone));
        }
        // A move outside a drag, and every way a window stops or starts being
        // on screen: minimised, restored, hidden, shown, cloaked. `sync` looks
        // at the window itself, so they are all one case.
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
    while dragged() == Some(window) {
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
            set_dragged(None);
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
    sync_as(window, None)
}

/// [`sync`], with what is known of the window passed in when something
/// already knows it and a look would say otherwise. `None` looks.
fn sync_as(window: isize, known: Option<Seen>) -> Option<Seen> {
    let mut changes: Vec<Change> = Vec::new();
    let mut moved: Vec<AreaId> = Vec::new();
    let seen = {
        let mut links = lock(&LINKS);
        let pid = links
            .iter()
            .find(|link| link.window == window && !link.gone)?
            .pid;
        let seen = known.unwrap_or_else(|| see(window, pid));
        // Read once, and only when a window closed: what else is open now is
        // what must not be mistaken for it coming back.
        let open_now: HashSet<isize> = if seen == Seen::Gone {
            top_level_windows().into_iter().collect()
        } else {
            HashSet::new()
        };
        for link in links
            .iter_mut()
            .filter(|link| link.window == window && !link.gone)
        {
            if link.seen == Some(seen) {
                continue;
            }
            link.seen = Some(seen);
            link.gone = seen == Seen::Gone;
            if link.gone {
                link.open_at_close.clone_from(&open_now);
            }
            let (state, bounds) = plan(link.anchor, link.scale, seen);
            changes.push((link.area, state, bounds));
        }
        // Written before the links are let go. A hand move takes the links to
        // record its new anchor, so it comes wholly before this decision or
        // wholly after it. Let go first, an older decision could be written
        // over the place the user had just put the area.
        if !changes.is_empty()
            && let Some(outlet) = OUTLET.get()
        {
            moved = (outlet.place)(&changes);
        }
        seen
    };
    if !moved.is_empty() {
        UNSETTLED.with_borrow_mut(|unsettled| unsettled.extend(moved));
        // SAFETY: as in `keep_ticking`. Passing the id of the timer that is
        // already running restarts it, so the wait is counted from the last
        // move and not from the first.
        SETTLE.set(unsafe { SetTimer(std::ptr::null_mut(), SETTLE.get(), SETTLE_MS, None) });
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
    let (changed, moved) = {
        let store = app.state::<Mutex<AreaStore>>();
        write(&mut lock(&store), changes, &overlay::monitor_rects())
    };
    if changed || !moved.is_empty() {
        tell_the_page(app);
    }
    moved
}

/// Sends the page the area set, from the main thread.
///
/// The follower runs on its own thread, and where a set is sent from decides
/// when the page gets it. Sent from the main thread it is handed over at once.
/// Sent from any other thread it is queued behind the main thread's event
/// loop. So a set read here and queued could arrive AFTER a newer one that the
/// mouse hook, which runs on the main thread, read and sent a moment later,
/// and the page would stay on the older set. The second review found that
/// ordering, and that a lock around the send did not prevent it.
///
/// Running the send on the main thread puts it in the same line as every
/// other one. The set is read when the closure runs and not when it is
/// posted, so it is never older than a set already sent.
fn tell_the_page(app: &AppHandle) {
    let on_main = app.clone();
    let posted = app.run_on_main_thread(move || {
        if let Err(error) = overlay::emit_areas(&on_main) {
            crate::diagnostics::trouble(
                "sticky: an area followed its window and the page was not told",
                &error,
            );
        }
    });
    if let Err(error) = posted {
        crate::diagnostics::trouble(
            "sticky: an area followed its window and the main thread could not be reached",
            &error.to_string(),
        );
    }
}

/// Writes the follower's decisions to the store. Returns whether any state
/// changed, and the areas that moved.
///
/// An area the store calls free is left alone, state and rectangle both. The
/// follower decides on its own thread, so a decision can arrive just after the
/// user freed the area from its menu, and writing it would tick the menu row
/// again on an area nothing follows.
fn write(store: &mut AreaStore, changes: &[Change], monitors: &[Rect]) -> (bool, Vec<AreaId>) {
    let mut moved: Vec<AreaId> = Vec::new();
    let mut changed = false;
    for &(id, state, bounds) in changes {
        let Some(area) = store.get(id).copied() else {
            continue;
        };
        if !area.sticky.is_sticky() {
            continue;
        }
        changed |= store.set_sticky(id, state);
        // A followed area goes where its window is, off the desktop too: the
        // window brings it back. A paused area has no window to bring it
        // back, and an area off the desktop cannot be reached to move, free or
        // dismiss it (`interaction::contain` says why that is a correctness
        // rule). So an area that pauses while it hangs off the desktop is
        // pulled back onto it, and waits there.
        let bounds = bounds.unwrap_or_else(|| interaction::contain(area.bounds, monitors));
        // Following a window does not raise the area: the user did not touch
        // it, so the stack stays as they left it.
        if area.bounds != bounds && store.set_bounds(id, bounds) {
            moved.push(id);
        }
    }
    (changed, moved)
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

/// One open top-level window, as far as re-attaching cares.
struct Open {
    window: isize,
    pid: u32,
    title: String,
}

/// The window a waiting link should join, if one of `open` is its window come
/// back: the same title, not one of the windows that were already open when
/// it closed, and passing `same_program`.
///
/// `same_program` is asked last and only about windows that fit otherwise,
/// because answering it means opening the window's process.
fn window_again<'a>(
    title: &str,
    open_at_close: &HashSet<isize>,
    open: &'a [Open],
    mut same_program: impl FnMut(&Open) -> bool,
) -> Option<&'a Open> {
    open.iter().find(|candidate| {
        candidate.title == title
            && !open_at_close.contains(&candidate.window)
            && same_program(candidate)
    })
}

/// One link whose window closed, as far as re-attaching cares.
struct Waiting {
    area: AreaId,
    /// The window it was on, which no longer exists. Areas with the same one
    /// here were on the same window.
    was: isize,
    program: String,
    title: String,
    open_at_close: HashSet<isize>,
}

/// Decides which open window each waiting link joins, in the order given.
///
/// Areas that were on one window go to one window. Areas that were on two
/// different windows never share one: with two windows of one program closed
/// under one title, the first to reopen takes the areas of one of them, and
/// the areas of the other wait for the second. This function holds that
/// inside one check. [`join_in`] carries it to the checks that follow.
fn rejoin<'a>(
    waiting: &[Waiting],
    open: &'a [Open],
    mut same_program: impl FnMut(&Waiting, &Open) -> bool,
) -> Vec<Option<&'a Open>> {
    // Which reopened window each closed window's areas went to.
    let mut taken: HashMap<isize, isize> = HashMap::new();
    waiting
        .iter()
        .map(|link| {
            let found = match taken.get(&link.was) {
                Some(&window) => open.iter().find(|open| open.window == window),
                None => window_again(&link.title, &link.open_at_close, open, |candidate| {
                    !taken.values().any(|&window| window == candidate.window)
                        && same_program(link, candidate)
                }),
            };
            if let Some(found) = found {
                taken.insert(link.was, found.window);
            }
            found
        })
        .collect()
}

/// The links whose window closed, as [`rejoin`] wants them. A link whose
/// program could not be read is left out: it has nothing to be matched by.
fn waiting_of(links: &[Link]) -> Vec<Waiting> {
    links
        .iter()
        .filter(|link| link.gone)
        .filter_map(|link| {
            Some(Waiting {
                area: link.area,
                was: link.window,
                program: link.program.clone()?,
                title: link.title.clone(),
                open_at_close: link.open_at_close.clone(),
            })
        })
        .collect()
}

/// One area joining a reopened window: the area, the closed window it was on,
/// and the window and owner it joins.
struct Join {
    area: AreaId,
    was: isize,
    window: isize,
    pid: u32,
}

/// What [`rejoin`] decided, as the joins to make.
fn joins_of(
    waiting: &[Waiting],
    open: &[Open],
    same_program: impl FnMut(&Waiting, &Open) -> bool,
) -> Vec<Join> {
    waiting
        .iter()
        .zip(rejoin(waiting, open, same_program))
        .filter_map(|(link, found)| {
            found.map(|found| Join {
                area: link.area,
                was: link.was,
                window: found.window,
                pid: found.pid,
            })
        })
        .collect()
}

/// Applies one check's joins to the links. Returns whether any link joined.
///
/// A window that the areas of one closed window joined is taken. Every link
/// still waiting that was on another window writes it down beside the windows
/// that were open when its own closed, and passes it by from then on.
/// [`rejoin`] keeps two closed windows apart inside one check and knows
/// nothing of the check before it. Without this note the areas of the second
/// window joined the first one's window a second later (review round 4).
fn join_in(links: &mut [Link], joins: &[Join]) -> bool {
    let mut made: Vec<&Join> = Vec::new();
    for join in joins {
        if let Some(live) = links
            .iter_mut()
            .find(|live| live.area == join.area && live.gone)
        {
            live.window = join.window;
            live.pid = join.pid;
            live.gone = false;
            live.seen = None;
            live.open_at_close.clear();
            made.push(join);
        }
    }
    for join in &made {
        for other in links
            .iter_mut()
            .filter(|link| link.gone && link.window != join.was)
        {
            other.open_at_close.insert(join.window);
        }
    }
    !made.is_empty()
}

/// Looks for the windows of links whose window closed, and joins each area to
/// a window of the same program with the same title, if one has appeared.
fn reattach() {
    let waiting = waiting_of(&lock(&LINKS));
    if waiting.is_empty() {
        return;
    }
    let own = std::process::id();
    // UP-TAKE's own windows are passed by before their title is read. Reading
    // the title of a window in this process sends that window a message and
    // waits for its thread to answer, which this thread must never do.
    let open: Vec<Open> = top_level_windows()
        .into_iter()
        .filter_map(|window| {
            let pid = owner_of(window);
            (pid != own).then(|| Open {
                window,
                pid,
                title: title_of(window),
            })
        })
        .filter(|open| waiting.iter().any(|waiting| waiting.title == open.title))
        .collect();
    let joins = joins_of(&waiting, &open, |link, candidate| {
        describe(candidate.window).can_be_followed(own)
            && program_of(candidate.pid).as_deref() == Some(link.program.as_str())
    });
    let joined = join_in(&mut lock(&LINKS), &joins);
    if joined {
        refresh();
    }
}

#[cfg(test)]
mod tests {
    use uptake_core::geometry::{Point, Rect};
    use uptake_core::sticky::{Anchor, Sticky};

    use super::{Join, join_in, joins_of, reanchor_in, waiting_of};
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use uptake_core::area::{AreaId, AreaStore, AreaType};

    use super::{
        Candidate, Change, EVENT_RANGES, FOLLOWER, Follower, LINKS, Link, OUTLET, Open, Outlet,
        Seen, Waiting, centre_of, dragged, first_under, forget, lock, plan, prune_in, rejoin,
        release_in, retake, see, stick_in, text_of, title_of, wake, window_again, write,
    };

    const WINDOW: Rect = Rect::new(100, 200, 900, 600);
    const OWN: u32 = 4242;

    /// Every placement `sync` decided, for the real-window test to read.
    static PLACED: Mutex<Vec<Change>> = Mutex::new(Vec::new());
    /// Every area the follower asked to settle, for the same test.
    static SETTLED: Mutex<Vec<AreaId>> = Mutex::new(Vec::new());
    /// How many placements reached the outlet after the follower had let go
    /// of the links, for the same test. It must stay at zero.
    static PLACED_UNHELD: AtomicUsize = AtomicUsize::new(0);
    /// How many events about a followed window reached the callback.
    pub(super) static EVENTS_HEARD: AtomicUsize = AtomicUsize::new(0);

    /// Waits for `done`, answering this thread's own window messages while it
    /// waits. The real-window test owns its window, and reading the title of
    /// a window in the same process asks the window's thread, which is this
    /// one. Asleep and not answering, it would hold the follower thread for
    /// good. UP-TAKE never follows its own windows, so only the test needs
    /// this.
    fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
        };
        let start = Instant::now();
        loop {
            // SAFETY: `MSG` is plain integers and pointers, valid as zeroes,
            // and each call writes one through the pointer.
            unsafe {
                let mut message: MSG = std::mem::zeroed();
                while PeekMessageW(&raw mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&raw const message);
                }
            }
            if done() {
                return true;
            }
            if start.elapsed() >= limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
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
    /// It covers the per-frame poll by announcing a drag the way Windows does.
    /// What it cannot cover is the re-attach, which refuses windows of its own
    /// process, and a drag by a hand on a mouse. CI runs this test on the
    /// Windows runner, and `quality-bars.md` section 3 carries the two rig
    /// scenarios.
    #[test]
    #[ignore = "needs a desktop session: it creates a real window, off every monitor, and listens to its events"]
    fn a_real_window_is_followed_by_its_events_and_paused_when_it_closes() {
        use windows_sys::Win32::UI::Accessibility::NotifyWinEvent;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SW_SHOWMINNOACTIVE,
            SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
            SetWindowTextW, ShowWindow, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART, OBJID_WINDOW,
        };
        // A class of the test's own with Windows' default procedure: an
        // ordinary top-level window, as a followed program's would be. With
        // the system `Static` class, which is a control, the new title below
        // never reached the follower as a name change of the window. That was
        // observed in five runs of this test and not looked into further.
        let class: Vec<u16> = "UptakeStickyTest\0".encode_utf16().collect();
        let title: Vec<u16> = "uptake sticky test\0".encode_utf16().collect();
        // SAFETY: `WNDCLASSW` is plain integers and pointers, valid as zeroes.
        // The class name outlives the registration's use in this test, and a
        // second registration of the same name fails harmlessly.
        unsafe {
            let mut definition: WNDCLASSW = std::mem::zeroed();
            definition.lpfnWndProc = Some(DefWindowProcW);
            definition.lpszClassName = class.as_ptr();
            RegisterClassW(&raw const definition);
        }
        // SAFETY: both strings are null-terminated and outlive the call. Every
        // handle argument may be null for a top-level window.
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
            open_at_close: HashSet::new(),
        });
        // A handle whose owner is another process is another window that was
        // given the same number, and must never be followed as this one.
        assert_eq!(see(window, pid.wrapping_add(1)), Seen::Gone);
        // An outlet of the test's own, in place of the app: every area is
        // still sticky, and each decision is written down to be read below.
        let _ = OUTLET.set(Outlet {
            still_sticky: |linked| linked.iter().copied().collect(),
            // Every area given a rectangle counts as moved, as it would be
            // in the app, so the follower's settle timer is set and handled.
            place: |changes| {
                // The follower writes a decision before it lets go of the
                // links, so a hand move comes wholly before it or wholly
                // after. A lock that can be taken here was let go too early.
                if LINKS.try_lock().is_ok() {
                    PLACED_UNHELD.fetch_add(1, Ordering::SeqCst);
                }
                lock(&PLACED).extend_from_slice(changes);
                changes
                    .iter()
                    .filter(|(_, _, bounds)| bounds.is_some())
                    .map(|&(area, _, _)| area)
                    .collect()
            },
            settle: |area| lock(&SETTLED).push(area),
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

        // The moves have stopped, so the area re-takes what it shows. This
        // asks only that it happens: the follower's timer was set and was
        // handled. That the five moves settle once and not five times is not
        // asserted, because it turns on the moves landing inside one 150 ms
        // wait, which a slow CI runner does not promise.
        assert!(
            wait_for(Duration::from_millis(1000), || lock(&SETTLED).contains(&id)),
            "a moved area settles once its window is still"
        );

        // A drag. Windows announces one with `EVENT_SYSTEM_MOVESIZESTART`, and
        // a program may raise that event for its own window, which is how this
        // test starts one without a hand on the mouse. From here to the end
        // event the callback ignores every move, so an area that still follows
        // inside 250 ms was placed by the per-frame poll and by nothing else.
        // SAFETY: raises an event for the window created above.
        unsafe {
            NotifyWinEvent(EVENT_SYSTEM_MOVESIZESTART, handle, OBJID_WINDOW, 0);
        }
        for step in 6..=10 {
            let (x, y) = (rect.origin.x + 10 * step, rect.origin.y + 7 * step);
            // SAFETY: as above.
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
                "move {step}, during a drag, was not followed within 250 ms"
            );
        }
        // SAFETY: as above.
        unsafe {
            NotifyWinEvent(EVENT_SYSTEM_MOVESIZEEND, handle, OBJID_WINDOW, 0);
        }
        // The end event ends the drag. Left set, the per-frame poll would go
        // on waking this thread every frame until five idle seconds had passed.
        assert!(
            wait_for(Duration::from_millis(250), || dragged().is_none()),
            "the end event clears the drag"
        );
        // After the end event a plain move is followed by its event again.
        let (x, y) = (rect.origin.x + 200, rect.origin.y + 100);
        // SAFETY: as above.
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
        assert!(
            wait_for(Duration::from_millis(250), || placed(
                id,
                Sticky::Following,
                Some(Rect::new(x + 20, y + 30, 50, 40))
            )),
            "a move after the drag ended was not followed within 250 ms"
        );

        // Minimised: the area pauses and is NOT sent to where Windows parks a
        // minimised window, far off every screen. Restored: it follows again.
        // SAFETY: shows the window created above, minimised and then as it
        // was, without activating it either time.
        unsafe {
            ShowWindow(handle, SW_SHOWMINNOACTIVE);
        }
        assert!(
            wait_for(Duration::from_millis(500), || placed(
                id,
                Sticky::Paused,
                None
            )),
            "a minimised window pauses the area and moves nothing"
        );
        lock(&PLACED).clear();
        // SAFETY: as above.
        unsafe {
            ShowWindow(handle, SW_SHOWNOACTIVATE);
        }
        assert!(
            wait_for(Duration::from_millis(500), || {
                lock(&PLACED)
                    .iter()
                    .any(|&(area, state, _)| area == id && state == Sticky::Following)
            }),
            "a restored window is followed again"
        );

        // A new title is remembered, so that when the window closes the title
        // looked for is the one it closed with.
        let renamed: Vec<u16> = "uptake sticky test, renamed\0".encode_utf16().collect();
        // SAFETY: the string is null-terminated and outlives the call.
        unsafe {
            SetWindowTextW(handle, renamed.as_ptr());
        }
        assert!(
            wait_for(Duration::from_millis(250), || lock(&LINKS).iter().any(
                |link| link.area == id && link.title == "uptake sticky test, renamed"
            )),
            "a changed title is remembered"
        );

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
        // Paused because it is GONE, not merely hidden. Only a link marked
        // gone is looked for again, so a closed window read as hidden would
        // leave its area waiting for a handle that never comes back. Windows
        // reports the window hidden first and destroyed a moment later, and
        // 250 ms is well inside the once-a-second check: only the destroyed
        // event itself can mark the link this soon.
        assert!(
            wait_for(Duration::from_millis(250), || lock(&LINKS)
                .iter()
                .any(|link| link.area == id && link.gone)),
            "a destroyed window is marked gone, so it can be re-attached"
        );
        // And the link wrote down what was open at that moment. This desktop
        // has other windows, and none of them is this one coming back.
        assert!(
            lock(&LINKS)
                .iter()
                .any(|link| link.area == id && !link.open_at_close.is_empty()),
            "what was open when the window closed is remembered"
        );
        // Every decision above was written with the links still held.
        assert!(!lock(&PLACED).is_empty());
        assert_eq!(
            PLACED_UNHELD.load(Ordering::SeqCst),
            0,
            "the follower let go of the links before it wrote a decision"
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

    fn link_for(area: AreaId, bounds: Rect) -> Link {
        Link {
            area,
            window: 0x51,
            pid: 7,
            program: Some("C:\\Program Files\\Editor\\editor.exe".to_owned()),
            title: "notes.txt".to_owned(),
            anchor: Anchor::of(bounds, WINDOW),
            scale: 1.0,
            seen: None,
            gone: false,
            open_at_close: HashSet::new(),
        }
    }

    fn store_with_two() -> (AreaStore, AreaId, AreaId) {
        let mut store = AreaStore::new();
        let (Some(first), Some(second)) = (
            store.create(AreaType::Default, Rect::new(140, 260, 200, 80)),
            store.create(AreaType::Ocr, Rect::new(600, 500, 100, 50)),
        ) else {
            panic!("two areas")
        };
        (store, first, second)
    }

    #[test]
    fn sticking_tells_the_store_and_then_adds_the_link() {
        let (store, first, second) = store_with_two();
        let store = Mutex::new(store);
        let links: Mutex<Vec<Link>> = Mutex::new(Vec::new());
        assert!(stick_in(
            &store,
            &links,
            link_for(first, Rect::new(140, 260, 200, 80))
        ));
        let Some(area) = lock(&store).get(first).copied() else {
            panic!("the area is still there")
        };
        assert_eq!(area.sticky, Sticky::Following, "the store was told");
        assert_eq!(lock(&links).len(), 1, "the link was added");
        // Sticking the same area again replaces its link, never adds a second.
        assert!(!stick_in(
            &store,
            &links,
            link_for(first, Rect::new(140, 260, 200, 80))
        ));
        assert_eq!(lock(&links).len(), 1);
        // The other area is untouched.
        let Some(other) = lock(&store).get(second).copied() else {
            panic!("the other area")
        };
        assert_eq!(other.sticky, Sticky::Free);
    }

    #[test]
    fn an_area_that_is_gone_gets_no_link() {
        let (mut store, first, _) = store_with_two();
        assert!(store.remove(first).is_some());
        let store = Mutex::new(store);
        let links: Mutex<Vec<Link>> = Mutex::new(Vec::new());
        assert!(!stick_in(
            &store,
            &links,
            link_for(first, Rect::new(140, 260, 200, 80))
        ));
        assert!(
            lock(&links).is_empty(),
            "a link to no area would never be dropped by its area"
        );
    }

    #[test]
    fn releasing_drops_the_link_and_frees_the_area() {
        let (store, first, second) = store_with_two();
        let store = Mutex::new(store);
        let links: Mutex<Vec<Link>> = Mutex::new(Vec::new());
        stick_in(
            &store,
            &links,
            link_for(first, Rect::new(140, 260, 200, 80)),
        );
        stick_in(
            &store,
            &links,
            link_for(second, Rect::new(600, 500, 100, 50)),
        );
        assert!(release_in(&store, &links, first));
        let Some(area) = lock(&store).get(first).copied() else {
            panic!("the area is still there")
        };
        assert_eq!(area.sticky, Sticky::Free, "the store was told");
        let left: Vec<AreaId> = lock(&links).iter().map(|link| link.area).collect();
        assert_eq!(left, vec![second], "only the released area's link is gone");
        assert!(!release_in(&store, &links, first), "nothing left to change");
    }

    #[test]
    fn the_followers_decisions_reach_the_store() {
        let (mut store, first, second) = store_with_two();
        store.set_sticky(first, Sticky::Following);
        store.set_sticky(second, Sticky::Following);
        let moved_to = Rect::new(900, 40, 200, 80);
        let changes: Vec<Change> = vec![
            (first, Sticky::Following, Some(moved_to)),
            (second, Sticky::Paused, None),
        ];
        let (changed, moved) = write(&mut store, &changes, &[]);
        assert!(changed, "the second area's state changed");
        assert_eq!(moved, vec![first]);
        let (Some(a), Some(b)) = (store.get(first).copied(), store.get(second).copied()) else {
            panic!("both areas")
        };
        assert_eq!(a.bounds, moved_to);
        assert_eq!(
            b.sticky,
            Sticky::Paused,
            "paused is written, not only decided"
        );
        assert_eq!(
            b.bounds,
            Rect::new(600, 500, 100, 50),
            "a paused area waits where it was"
        );
        // The same decisions again change nothing and move nothing, so the
        // page is not sent a set that says what it already shows.
        assert_eq!(write(&mut store, &changes, &[]), (false, Vec::new()));
    }

    #[test]
    fn an_area_that_pauses_off_the_desktop_is_brought_back_onto_it() {
        // The third review's first finding. The window was dragged so that
        // the area hung off the right of the only monitor, and then closed.
        // Paused out there, nothing could reach the area: every way to move,
        // free or dismiss one needs the pointer on it.
        let monitor = [Rect::new(0, 0, 1920, 1080)];
        let (mut store, first, second) = store_with_two();
        store.set_sticky(first, Sticky::Following);
        store.set_sticky(second, Sticky::Following);
        let off = Rect::new(2450, 300, 200, 80);
        // Following: the area goes where its window is, off the desktop too.
        let (_, moved) = write(
            &mut store,
            &[(first, Sticky::Following, Some(off))],
            &monitor,
        );
        assert_eq!(moved, vec![first]);
        assert_eq!(store.get(first).map(|area| area.bounds), Some(off));
        // The window closes. The area pauses, and comes back onto the monitor
        // by the same rule a hand move obeys.
        let (changed, moved) = write(&mut store, &[(first, Sticky::Paused, None)], &monitor);
        assert!(changed);
        assert_eq!(moved, vec![first]);
        let held = uptake_core::interaction::contain(off, &monitor);
        assert_ne!(held, off);
        assert_eq!(store.get(first).map(|area| area.bounds), Some(held));
        // An area that pauses where it can be reached does not move at all.
        let was = store.get(second).map(|area| area.bounds);
        let (_, moved) = write(&mut store, &[(second, Sticky::Paused, None)], &monitor);
        assert!(moved.is_empty());
        assert_eq!(store.get(second).map(|area| area.bounds), was);
    }

    #[test]
    fn a_hand_move_is_the_later_word_over_a_decision_made_just_before_it() {
        // The third review's second finding. The follower decided where the
        // area goes, the user dropped the area somewhere else, and the older
        // decision was then written over the drop. The hand move now puts the
        // area back where the hand left it and takes its anchor from there.
        let (store, first, _) = store_with_two();
        let store = Mutex::new(store);
        let links: Mutex<Vec<Link>> = Mutex::new(Vec::new());
        let was = Rect::new(140, 260, 200, 80);
        stick_in(&store, &links, link_for(first, was));
        let dropped = Rect::new(600, 500, 200, 80);
        // The follower's write lands after the hand's own, with the old place.
        lock(&store).set_bounds(first, Rect::new(440, 360, 200, 80));
        reanchor_in(&store, &links, first, dropped, |_, _| Seen::At {
            rect: WINDOW,
            scale: 1.0,
        });
        assert_eq!(
            lock(&store).get(first).map(|area| area.bounds),
            Some(dropped)
        );
        let anchors: Vec<Anchor> = lock(&links).iter().map(|link| link.anchor).collect();
        assert_eq!(anchors, vec![Anchor::of(dropped, WINDOW)]);
        // And it holds the links while it writes the store, like every step
        // that touches both.
        let held = lock(&store);
        std::thread::scope(|scope| {
            let step = scope.spawn(|| {
                reanchor_in(&store, &links, first, was, |_, _| Seen::Hidden);
            });
            let blocked = wait_for(Duration::from_secs(2), || links.try_lock().is_err());
            drop(held);
            let _ = step.join();
            assert!(blocked, "the links are held across the store");
        });
    }

    #[test]
    fn following_a_window_does_not_raise_the_area() {
        let (mut store, first, second) = store_with_two();
        store.set_sticky(first, Sticky::Following);
        let order = |store: &AreaStore| store.iter().map(|area| area.id).collect::<Vec<_>>();
        let before = order(&store);
        assert_eq!(before, vec![first, second]);
        write(
            &mut store,
            &[(first, Sticky::Following, Some(Rect::new(0, 0, 200, 80)))],
            &[],
        );
        assert_eq!(order(&store), before, "the user did not touch it");
    }

    #[test]
    fn a_decision_that_arrives_after_the_area_was_freed_is_dropped() {
        // The second ordering the first review found: the follower decided,
        // the user freed the area from its menu, and the decision was then
        // written, ticking the row again on an area nothing follows.
        let (mut store, first, _) = store_with_two();
        let was = Rect::new(140, 260, 200, 80);
        let late: Vec<Change> = vec![(first, Sticky::Following, Some(Rect::new(900, 40, 200, 80)))];
        assert_eq!(write(&mut store, &late, &[]), (false, Vec::new()));
        let Some(area) = store.get(first).copied() else {
            panic!("the area")
        };
        assert_eq!(area.sticky, Sticky::Free);
        assert_eq!(area.bounds, was, "a free area is not moved either");
        // An id that no longer exists is passed by the same way.
        assert!(store.remove(first).is_some());
        assert_eq!(write(&mut store, &late, &[]), (false, Vec::new()));
    }

    #[test]
    fn a_hand_move_takes_a_new_anchor_only_from_a_window_on_screen() {
        let was = Rect::new(140, 260, 200, 80);
        let Some(id) = AreaStore::new().create(AreaType::Default, was) else {
            panic!("an area id")
        };
        let mut link = link_for(id, was);
        // Dragged by hand to the window's bottom right corner.
        let now = Rect::new(100 + 900 - 20 - 200, 200 + 600 - 30 - 80, 200, 80);
        retake(
            &mut link,
            now,
            Seen::At {
                rect: WINDOW,
                scale: 1.25,
            },
        );
        assert_eq!(
            link.anchor,
            Anchor::of(now, WINDOW),
            "the place they put it"
        );
        assert!((link.scale - 1.25).abs() < f64::EPSILON);
        assert_eq!(
            link.seen,
            Some(Seen::At {
                rect: WINDOW,
                scale: 1.25
            })
        );
        // Minimised or closed: there is no rectangle to measure from.
        let kept = link.anchor;
        retake(&mut link, was, Seen::Hidden);
        retake(&mut link, was, Seen::Gone);
        assert_eq!(link.anchor, kept);
    }

    fn open(window: isize, title: &str) -> Open {
        Open {
            window,
            pid: 7,
            title: title.to_owned(),
        }
    }

    #[test]
    fn a_closed_window_is_found_again_by_title_and_program() {
        let windows = [
            open(10, "other.txt"),
            open(11, "notes.txt"),
            open(12, "notes.txt"),
        ];
        let none = HashSet::new();
        let found = window_again("notes.txt", &none, &windows, |_| true);
        assert_eq!(found.map(|open| open.window), Some(11), "the topmost match");
        // Another program with the same title is not the window coming back.
        assert!(window_again("notes.txt", &none, &windows, |_| false).is_none());
        // The program is asked only about windows whose title already fits.
        let mut asked = Vec::new();
        window_again("notes.txt", &none, &windows, |open| {
            asked.push(open.window);
            false
        });
        assert_eq!(asked, vec![11, 12]);
        assert!(window_again("gone.txt", &none, &windows, |_| true).is_none());
    }

    #[test]
    fn a_window_that_was_already_open_is_not_the_one_coming_back() {
        // Two windows of one program with one title. The area was on the one
        // that closed. The other was open all along and is passed by, and a
        // third that appears afterwards is taken.
        let already: HashSet<isize> = [11].into_iter().collect();
        let sibling_only = [open(11, "notes.txt")];
        assert!(window_again("notes.txt", &already, &sibling_only, |_| true).is_none());
        let reopened = [open(11, "notes.txt"), open(31, "notes.txt")];
        let found = window_again("notes.txt", &already, &reopened, |_| true);
        assert_eq!(found.map(|open| open.window), Some(31));
    }

    /// `source` with its test module cut off and its comments dropped, so that
    /// a call which survives only in a comment or only in a test does not
    /// count as made. A comment is dropped wherever it starts: on a line of
    /// its own, after code on the same line, or as a block.
    ///
    /// The third version kept a comment that followed code on its line, and
    /// the fourth review left `wake();` only in such a comment with every
    /// test green.
    ///
    /// It does not understand string literals. A comment marker inside one
    /// cuts the rest of that line, or up to the next block end. That can only
    /// remove text, so it can make a guard fail and never make one pass.
    ///
    /// What it cannot see is code made dead another way: a call left inside
    /// `if false { }` still reads as made. This guards against a line going
    /// missing, which is how these calls were lost in three reviews' drills.
    /// It is not a proof that the line runs.
    ///
    /// The test module is everything from the line `mod tests {` on. The first
    /// version looked for that line directly under `#[cfg(test)]`, found it in
    /// one of three files, and returned the other two whole, comments and
    /// tests included. The second review commented a guarded call out in each
    /// and the test stayed green.
    fn production(source: &str) -> String {
        let code = source
            .split_once("\nmod tests {")
            .map_or(source, |(before, _)| before);
        let mut kept = String::with_capacity(code.len());
        let mut rest = code;
        loop {
            let (before, comment, is_block) = match (rest.split_once("//"), rest.split_once("/*")) {
                (None, None) => break,
                (Some((before, comment)), Some((sooner, block))) => {
                    if sooner.len() < before.len() {
                        (sooner, block, true)
                    } else {
                        (before, comment, false)
                    }
                }
                (Some((before, comment)), None) => (before, comment, false),
                (None, Some((before, block))) => (before, block, true),
            };
            kept.push_str(before);
            rest = if is_block {
                comment.split_once("*/").map_or("", |(_, after)| after)
            } else {
                // The line break stays, so the next line is still its own.
                comment.find('\n').map_or("", |end| &comment[end..])
            };
        }
        kept.push_str(rest);
        kept
    }

    /// The body of one function in `code`: from its signature to the first
    /// closing brace at the start of a line.
    fn body<'a>(code: &'a str, signature: &str) -> &'a str {
        let Some((_, after)) = code.split_once(signature) else {
            panic!("`{signature}` not found: renamed? An unfound function must not read as a pass")
        };
        let Some((body, _)) = after.split_once("\n}") else {
            panic!("`{signature}` has no end")
        };
        body
    }

    #[test]
    fn the_source_helpers_cannot_be_satisfied_by_a_comment_or_a_test() {
        let call = "sticky::init(app.handle());";
        // Commented out, in a file with no test module at all.
        assert!(!production(&format!("fn setup() {{\n    // {call}\n}}\n")).contains(call));
        // Present only in a test module, under either spelling of its header.
        for header in [
            "#[cfg(test)]\nmod tests {",
            "#[cfg(test)]\n#[allow(dead_code)]\nmod tests {",
        ] {
            let source = format!("fn setup() {{}}\n{header}\n    fn t() {{ {call} }}\n}}\n");
            assert!(!production(&source).contains(call), "{header}");
        }
        // Inside a block comment, on one line or across several.
        assert!(!production(&format!("fn setup() {{\n    /* {call} */\n}}\n")).contains(call));
        assert!(
            !production(&format!("fn setup() {{\n    /*\n    {call}\n    */\n}}\n")).contains(call)
        );
        // In a comment that follows code on the same line. The code stays and
        // so does the line after it.
        let trailing = production(&format!(
            "fn setup() {{\n    other(); // {call}\n    last();\n}}\n"
        ));
        assert!(!trailing.contains(call));
        assert!(trailing.contains("other();") && trailing.contains("\n    last();"));
        // In a line comment that a block comment marker follows, and the
        // other way round: whichever starts first decides.
        assert!(!production(&format!("fn setup() {{\n    // {call} /* x */\n}}\n")).contains(call));
        assert!(
            !production(&format!("fn setup() {{\n    /* // */ /* {call} */\n}}\n")).contains(call)
        );
        assert!(production(&format!("fn setup() {{\n    /* // */ {call}\n}}\n")).contains(call));
        // Present in production code: found.
        assert!(production(&format!("fn setup() {{\n    {call}\n}}\n")).contains(call));
        // A body ends at its own closing brace, not at the next function's.
        let two = "fn one() {\n    a();\n}\n\nfn two() {\n    b();\n}\n";
        assert!(body(two, "fn one() {").contains("a();"));
        assert!(!body(two, "fn one() {").contains("b();"));
    }

    /// The single lines without which the feature is dead in the app while
    /// every test of this module's own logic stays green. Two reviews removed
    /// them one at a time, ten and then six more, and the suite passed each
    /// time. Each is asserted in production source, comments and tests cut
    /// away, inside the function it has to be in.
    #[test]
    fn every_line_the_feature_hangs_on_is_where_it_must_be() {
        let lib = production(include_str!("lib.rs"));
        assert!(
            lib.contains("sticky::init(app.handle());"),
            "startup hands this module the app, or nothing the follower decides is written"
        );
        let placement = production(include_str!("placement.rs"));
        assert!(
            placement.contains(
                "MenuAction::SetSticky(sticky) => crate::sticky::set(app, area, sticky),"
            ),
            "the menu row does something"
        );
        assert!(
            placement.contains("crate::sticky::reanchor(app, id, Rect::new(x, y, width, height));"),
            "a hand move takes a new anchor"
        );
        let overlay = production(include_str!("overlay.rs"));
        assert!(
            body(&overlay, "pub(crate) fn dismiss_area(").contains("crate::sticky::forget(id);"),
            "a dismissed area drops its link"
        );

        let this = production(include_str!("sticky.rs"));
        for (signature, line, why) in [
            (
                "fn stick(app: &AppHandle, id: AreaId) -> bool {",
                "wake();",
                "sticking starts the follower thread, or nothing ever follows",
            ),
            (
                "fn stick(app: &AppHandle, id: AreaId) -> bool {",
                "stick_in(store.inner(), &LINKS, link)",
                "sticking records the link",
            ),
            (
                "fn release(app: &AppHandle, id: AreaId) -> bool {",
                "release_in(store.inner(), &LINKS, id)",
                "freeing an area drops its link",
            ),
            (
                "pub(crate) fn reanchor(app: &AppHandle, id: AreaId, placed: Rect) {",
                "reanchor_in(store.inner(), &LINKS, id, placed, see);",
                "a hand move takes the new anchor, or the area snaps back",
            ),
            (
                "fn app_place(changes: &[Change]) -> Vec<AreaId> {",
                "write(&mut lock(&store), changes, &overlay::monitor_rects())",
                "the follower's decisions reach the store",
            ),
            (
                "fn app_place(changes: &[Change]) -> Vec<AreaId> {",
                "tell_the_page(app);",
                "the page is told, or the store moves and the screen does not",
            ),
            (
                "fn tell_the_page(app: &AppHandle) {",
                "app.run_on_main_thread(move || {",
                "the set is sent from the main thread, in line with every other one",
            ),
            (
                "fn tell_the_page(app: &AppHandle) {",
                "overlay::emit_areas(&on_main)",
                "and it is sent at all",
            ),
            (
                "fn app_settle(id: AreaId) {",
                "overlay::refresh_magnification(app, id);",
                "a moved Upscale area re-takes its still",
            ),
            (
                "fn app_settle(id: AreaId) {",
                "placement::reread_in_place_ocr(app, id);",
                "a moved OCR area reads again",
            ),
            (
                "fn handle(message: &MSG) {",
                "reattach();",
                "the once-a-second check looks for a closed window coming back",
            ),
            (
                "fn prune() {",
                "prune_in(&LINKS, outlet.still_sticky);",
                "links of areas that are gone are dropped",
            ),
            (
                "fn reattach() {",
                "let joins = joins_of(&waiting, &open, |link, candidate| {",
                "a closed window coming back is decided by the rule the tests hold",
            ),
            (
                "fn reattach() {",
                "join_in(&mut lock(&LINKS), &joins)",
                "and the joins are made, with the taken window noted for the others",
            ),
            (
                "pub(crate) fn can_stick(bounds: Rect) -> bool {",
                "window_under(bounds).is_some()",
                "the menu greys its row out by asking for the window sticking would take",
            ),
            (
                "fn link_under(id: AreaId, bounds: Rect) -> Option<Link> {",
                "window_under(bounds)?;",
                "and sticking takes that same window, so the two cannot disagree",
            ),
        ] {
            assert!(body(&this, signature).contains(line), "{why}");
        }
    }

    #[test]
    fn pruning_keeps_the_links_of_sticky_areas_and_drops_the_rest() {
        let (store, first, second) = store_with_two();
        let store = Mutex::new(store);
        let links: Mutex<Vec<Link>> = Mutex::new(Vec::new());
        stick_in(
            &store,
            &links,
            link_for(first, Rect::new(140, 260, 200, 80)),
        );
        stick_in(
            &store,
            &links,
            link_for(second, Rect::new(600, 500, 100, 50)),
        );
        // The second area is freed behind this module's back.
        lock(&store).set_sticky(second, Sticky::Free);
        let mut asked: Vec<AreaId> = Vec::new();
        prune_in(&links, |linked| {
            asked = linked.to_vec();
            let store = lock(&store);
            linked
                .iter()
                .copied()
                .filter(|&id| store.get(id).is_some_and(|area| area.sticky.is_sticky()))
                .collect()
        });
        assert_eq!(asked, vec![first, second], "every link is asked about");
        let left: Vec<AreaId> = lock(&links).iter().map(|link| link.area).collect();
        assert_eq!(left, vec![first]);
        // With no links there is nothing to ask.
        let none: Mutex<Vec<Link>> = Mutex::new(Vec::new());
        prune_in(&none, |_| panic!("asked about no links"));
    }

    #[test]
    fn a_link_cannot_be_added_between_the_question_and_the_answer() {
        // The ordering the second review found: the menu thread added a link
        // while the follower was between listing the links and dropping the
        // ones not vouched for, and the new link was dropped unasked. The
        // links are now held for the whole of both steps. Asserted from
        // inside the question: another thread cannot take the lock there.
        let (store, first, _) = store_with_two();
        let store = Mutex::new(store);
        let links: Mutex<Vec<Link>> = Mutex::new(Vec::new());
        stick_in(
            &store,
            &links,
            link_for(first, Rect::new(140, 260, 200, 80)),
        );
        prune_in(&links, |linked| {
            assert!(
                links.try_lock().is_err(),
                "the links are held while the store is asked"
            );
            linked.iter().copied().collect()
        });
        assert_eq!(lock(&links).len(), 1);
    }

    #[test]
    fn sticking_and_releasing_hold_the_links_while_they_tell_the_store() {
        // The same property for the two steps the menu thread takes, checked
        // by another thread trying the links while this one holds the store.
        // If a step took the links only for its own half, the second thread
        // would get in between.
        let (store, first, _) = store_with_two();
        let store = Mutex::new(store);
        let links: Mutex<Vec<Link>> = Mutex::new(Vec::new());
        let bounds = Rect::new(140, 260, 200, 80);
        for release in [false, true] {
            let held = lock(&store);
            std::thread::scope(|scope| {
                let step = scope.spawn(|| {
                    if release {
                        release_in(&store, &links, first);
                    } else {
                        stick_in(&store, &links, link_for(first, bounds));
                    }
                });
                // The step is now waiting for the store, which this thread
                // holds. It must already hold the links.
                let blocked = wait_for(Duration::from_secs(2), || links.try_lock().is_err());
                drop(held);
                let _ = step.join();
                assert!(
                    blocked,
                    "release {release}: the links are held across the store"
                );
            });
        }
        assert!(lock(&links).is_empty(), "stuck, then released");
    }

    #[test]
    fn areas_of_one_closed_window_rejoin_together_and_two_windows_never_share_one() {
        let ids: Vec<AreaId> = {
            let mut store = AreaStore::new();
            (0..3)
                .filter_map(|_| store.create(AreaType::Default, Rect::new(0, 0, 10, 10)))
                .collect()
        };
        assert_eq!(ids.len(), 3);
        let waiting = |area: AreaId, was: isize| Waiting {
            area,
            was,
            program: "editor.exe".to_owned(),
            title: "notes.txt".to_owned(),
            open_at_close: HashSet::new(),
        };
        // Two areas were on window 70, one on window 71. Same program, same
        // title, both closed.
        let links = [
            waiting(ids[0], 70),
            waiting(ids[1], 71),
            waiting(ids[2], 70),
        ];
        let chosen = |open: &[Open]| -> Vec<Option<isize>> {
            rejoin(&links, open, |_, _| true)
                .into_iter()
                .map(|found| found.map(|open| open.window))
                .collect()
        };
        // One window reopens: the areas of window 70 take it together, and the
        // area of window 71 waits. It must not pile onto the same window.
        assert_eq!(
            chosen(&[open(90, "notes.txt")]),
            vec![Some(90), None, Some(90)]
        );
        // Both reopen: each closed window's areas get a window of their own.
        assert_eq!(
            chosen(&[open(90, "notes.txt"), open(91, "notes.txt")]),
            vec![Some(90), Some(91), Some(90)]
        );
        // Nothing fitting is open: everyone waits.
        assert_eq!(chosen(&[open(90, "other.txt")]), vec![None, None, None]);
        // Another program under the same title is nobody's window.
        let refused: Vec<Option<isize>> = rejoin(&links, &[open(90, "notes.txt")], |_, _| false)
            .into_iter()
            .map(|found| found.map(|open| open.window))
            .collect();
        assert_eq!(refused, vec![None, None, None]);
    }

    /// Several once-a-second checks in a row, through the functions the check
    /// itself uses. Review round 4 found the areas of the second closed
    /// window joining the first one's window on the check after.
    #[test]
    fn areas_of_two_closed_windows_never_share_a_window_on_a_later_check() {
        let ids: Vec<AreaId> = {
            let mut store = AreaStore::new();
            (0..3)
                .filter_map(|_| store.create(AreaType::Default, Rect::new(0, 0, 10, 10)))
                .collect()
        };
        assert_eq!(ids.len(), 3);
        let closed = |area: AreaId, was: isize| Link {
            window: was,
            gone: true,
            ..link_for(area, Rect::new(140, 260, 200, 80))
        };
        // Two areas were on window 70 and one on window 71. Same program,
        // same title, both closed.
        let mut links = vec![closed(ids[0], 70), closed(ids[1], 71), closed(ids[2], 70)];
        let check = |links: &mut [Link], open: &[Open]| {
            let joins = joins_of(&waiting_of(links), open, |_, _| true);
            join_in(links, &joins)
        };
        let on = |links: &[Link]| -> Vec<(isize, bool)> {
            links.iter().map(|link| (link.window, link.gone)).collect()
        };

        // One window reopens. The areas of window 70 take it together.
        let one = [open(90, "notes.txt")];
        assert!(check(&mut links, &one));
        assert_eq!(on(&links), vec![(90, false), (71, true), (90, false)]);
        // The check after, and the one after that: window 90 is taken, and
        // the area of window 71 goes on waiting.
        assert!(!check(&mut links, &one));
        assert!(!check(&mut links, &one));
        assert_eq!(on(&links), vec![(90, false), (71, true), (90, false)]);
        // The second window reopens, and now it is the waiting area's.
        let both = [open(90, "notes.txt"), open(91, "notes.txt")];
        assert!(check(&mut links, &both));
        assert_eq!(on(&links), vec![(90, false), (91, false), (90, false)]);
    }

    /// A join is made only for a link that is still waiting. An area freed
    /// between the decision and the join took no window, so the window is not
    /// counted as taken, and an area that was on the same closed window is
    /// never told to pass its own window by.
    #[test]
    fn only_a_join_that_was_made_marks_its_window_as_taken() {
        let ids: Vec<AreaId> = {
            let mut store = AreaStore::new();
            (0..3)
                .filter_map(|_| store.create(AreaType::Default, Rect::new(0, 0, 10, 10)))
                .collect()
        };
        assert_eq!(ids.len(), 3);
        let closed = |area: AreaId, was: isize| Link {
            window: was,
            gone: true,
            ..link_for(area, Rect::new(140, 260, 200, 80))
        };
        let join = |area: AreaId, was: isize| Join {
            area,
            was,
            window: 90,
            pid: 7,
        };
        // The area the join was decided for is gone from the links.
        let mut links = vec![closed(ids[1], 71)];
        assert!(!join_in(&mut links, &[join(ids[0], 70)]));
        assert!(links[0].open_at_close.is_empty());
        // Made for one area of window 70. The other area of window 70, still
        // waiting, may take window 90 too. The area of window 71 may not.
        let mut links = vec![closed(ids[0], 70), closed(ids[1], 71), closed(ids[2], 70)];
        assert!(join_in(&mut links, &[join(ids[0], 70)]));
        assert!(links[0].open_at_close.is_empty() && !links[0].gone);
        assert!(links[1].open_at_close.contains(&90));
        assert!(links[2].open_at_close.is_empty() && links[2].gone);
    }

    #[test]
    fn the_window_under_the_area_is_the_topmost_one_that_can_be_followed() {
        let inside = Point::new(500, 500);
        let overlay = Candidate {
            click_through: true,
            ..window("SomeOverlay")
        };
        let elsewhere = Candidate {
            rect: Rect::new(2000, 0, 400, 300),
            ..window("Elsewhere")
        };
        // Topmost first: an overlay, a window somewhere else, then two that
        // both contain the point. The upper of those two is the answer.
        let windows = vec![
            (1, overlay),
            (2, elsewhere),
            (3, window("Editor")),
            (4, window("Browser")),
            (5, window("Progman")),
        ];
        assert_eq!(first_under(windows.clone(), inside, OWN), Some(3));
        // Nothing followable under the point: the desktop alone is no answer.
        let bare = vec![(5, window("Progman"))];
        assert_eq!(first_under(bare, inside, OWN), None);
        assert_eq!(first_under(windows, Point::new(-50, -50), OWN), None);
    }
}
