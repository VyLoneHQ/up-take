//! The host half of watch areas (roadmap `1.58`, `ADR-0048`): reading each
//! watching area's region on a timer and raising a Windows notification when
//! what it shows changes, or stops changing.
//!
//! The rule for *what counts* as a change is `uptake_core::watch`, which needs
//! no screen and is tested there. This module takes the readings and says so.
//!
//! # When it reads, and when it does not
//!
//! `ADR-0048` admits an area that reads its own region repeatedly on four
//! conditions, and three of them are held here:
//!
//! - **Switched on per area, off by default.** Only an area of type
//!   [`AreaType::Watch`] whose [`Watch`] is not [`Watch::Off`] is read, and
//!   every area starts `Off` (`uptake_core::area`).
//! - **Visibly marked while it reads.** The page draws the mark from the same
//!   area payload this module reads the choice from, so the two cannot
//!   disagree. Nothing is read while the overlay is hidden, because the mark
//!   is hidden with it, nor while the area is a paused sticky area, which the
//!   page draws only in Placement.
//! - **Local.** A reading is reduced to a few hundred brightness averages,
//!   compared and dropped. Nothing is written, logged or sent; the
//!   notification says which moment it was, never what was read.
//!
//! The fourth, *the user placed it*, is true of every area.
//!
//! # Cost
//!
//! ⚠️ **Not measured yet, and the interval is a first value.** Each reading is
//! one `uptake_capture::capture_region`, which builds a capture session per
//! call (about 100 to 350 ms, mostly waiting, per that crate's notes), every
//! [`INTERVAL`] for each watching area. Roadmap `1.58` requires the rate and
//! its cost to be measured against `quality-bars.md` section 1 before the
//! default is chosen; that measurement is owed.
//!
//! # The thread
//!
//! One thread, started the first time any area is switched to watching and
//! never stopped. With nothing to watch it waits on a condition variable and
//! costs nothing; [`wake`] is what moves it. While something is watched but
//! the overlay is hidden, it looks once per [`INTERVAL`] whether that changed.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use uptake_core::area::{AreaId, AreaStore, AreaType};
use uptake_core::geometry::Rect;
use uptake_core::sticky::Sticky;
use uptake_core::watch::{Detector, Event, Fingerprint, Watch};

use crate::overlay_state::OverlayState;
use crate::strings::{Text, text};

/// How often a watching area reads its region. A first value; see the module
/// docs' *Cost*.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// Whether the thread exists, and whether it has been woken since it last
/// looked.
struct Bell {
    started: bool,
    rung: bool,
}

static BELL: Mutex<Bell> = Mutex::new(Bell {
    started: false,
    rung: false,
});
static RINGING: Condvar = Condvar::new();

fn bell() -> std::sync::MutexGuard<'static, Bell> {
    BELL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Tells the watcher that what it should watch may have changed, starting it
/// the first time. Called when an area's [`Watch`] is set.
pub fn wake(app: &AppHandle) {
    let mut bell = bell();
    bell.rung = true;
    if !bell.started {
        let app = app.clone();
        match std::thread::Builder::new()
            .name("uptake-watch".into())
            .spawn(move || run(&app))
        {
            Ok(_) => bell.started = true,
            Err(error) => crate::diagnostics::trouble(
                "watch: could not start the watcher, so no watch area will notify",
                &error,
            ),
        }
    }
    RINGING.notify_one();
}

/// Waits until [`wake`] rings, or `timeout` passes when there is one.
fn wait(timeout: Option<Duration>) {
    let mut bell = bell();
    match timeout {
        None => {
            while !bell.rung {
                bell = RINGING.wait(bell).unwrap_or_else(PoisonError::into_inner);
            }
        }
        Some(timeout) if !bell.rung => {
            bell = RINGING
                .wait_timeout(bell, timeout)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Some(_) => {}
    }
    bell.rung = false;
}

/// One area to read: where, and what for.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    id: AreaId,
    bounds: Rect,
    watch: Watch,
}

/// Every area that is switched to watching and in sight now. Takes the store
/// lock for the copy and nothing else.
fn targets(app: &AppHandle) -> Vec<Target> {
    let store = app.state::<Mutex<AreaStore>>();
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    store
        .iter()
        .filter(|area| {
            area.kind == AreaType::Watch && area.watch.is_on() && area.sticky != Sticky::Paused
        })
        .map(|area| Target {
            id: area.id,
            bounds: area.bounds,
            watch: area.watch,
        })
        .collect()
}

fn overlay_showing(app: &AppHandle) -> bool {
    let state = app.state::<Mutex<OverlayState>>();
    let state = *state.lock().unwrap_or_else(PoisonError::into_inner);
    state != OverlayState::Hidden
}

fn run(app: &AppHandle) {
    // Each area's detector, with the target it was made for: an area that
    // moved, was resized or was given another choice starts again from a fresh
    // baseline rather than reporting its own move as a change.
    let mut detectors: HashMap<AreaId, (Target, Detector)> = HashMap::new();
    loop {
        let targets = targets(app);
        if targets.is_empty() {
            detectors.clear();
            wait(None);
            continue;
        }
        if !overlay_showing(app) {
            detectors.clear();
            wait(Some(INTERVAL));
            continue;
        }
        detectors.retain(|id, _| targets.iter().any(|target| target.id == *id));
        for target in targets {
            let entry = detectors
                .entry(target.id)
                .or_insert_with(|| (target, Detector::new()));
            if entry.0 != target {
                *entry = (target, Detector::new());
            }
            let reading = match crate::output::capture_for_watch(app, target.bounds) {
                Ok(bitmap) => Fingerprint::of(&bitmap),
                Err(error) => {
                    crate::diagnostics::trouble("watch: a reading could not be taken", &error);
                    None
                }
            };
            if let Some(event) = reading.and_then(|reading| entry.1.observe(reading))
                && target.watch.notifies(event)
            {
                notify(app, event);
            }
        }
        wait(Some(INTERVAL));
    }
}

/// Raises the Windows notification for `event`. Says which moment it was and
/// never what the area read.
fn notify(app: &AppHandle, event: Event) {
    use tauri_plugin_notification::NotificationExt;
    let body = match event {
        Event::Changed => text(Text::WatchChanged),
        Event::Settled => text(Text::WatchSettled),
    };
    if let Err(error) = app
        .notification()
        .builder()
        .title(text(Text::WatchTitle))
        .body(body)
        .show()
    {
        crate::diagnostics::trouble("watch: the notification could not be shown", &error);
    }
}
