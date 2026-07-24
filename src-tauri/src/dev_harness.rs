//! Debug-only instrumentation for driving the overlay by hand on real
//! hardware. Compiled out of release builds entirely.
//!
//! **This exists because it has twice been the only thing that found a bug.**
//! Both defects on the coordinate path in July 2026 — the stale regions across
//! a hide/show and the CSS↔physical scale mismatch — were found by re-showing
//! the overlay from a background thread with both sides of the IPC boundary
//! printing their own numbers. CI, unit tests and a clean `pnpm tauri dev` boot
//! passed over both (friction F-15).
//!
//! It has now been written twice, because the first copy was deleted with the
//! branch it lived on. Hence the shape here: **opt-in via an environment
//! variable and committed to `main`**, rather than a local edit to be
//! reconstructed from a status note each time. Off by default, so an ordinary
//! `pnpm tauri dev` is unaffected.
//!
//! ```text
//! UPTAKE_DEV_RESHOW=45 pnpm tauri dev
//! ```
//!
//! ## What the re-show timer is actually for
//!
//! It calls [`overlay::show`] **from a spawned thread**, which the hotkey does
//! not: `WM_HOTKEY` is dispatched on the event-loop thread, so a hotkey summon
//! never exercises the off-event-loop path (see `hotkey.rs`). Off the event
//! loop, the `Moved`/`Resized` events a reposition raises arrive while the
//! window is still hidden and `sync_bounds` returns early — historically the
//! path where a display change taken while hidden left state stale (the
//! region re-anchoring bug of task 1.3's follow-up; that machinery is gone
//! since ADR-0016, but the path itself still deserves the exercise). A bug
//! there would pass every dev-boot test and fail in release.
//!
//! Combined with a display change made *during* the wait, this reproduces the
//! full hide → rearrange → show sequence with no hands on the keyboard.
//!
//! ## `UPTAKE_DEV_ALLOW_MULTIPLE`
//!
//! Task 1.5's single-instance guard exits a second launch before it reaches
//! `hotkey::install`, which was the only way M-9 (another app already holding
//! `Win+Shift+U`) had been reproduced — a second UP-TAKE instance standing in
//! for the "other app". This variable skips registering the guard so two dev
//! instances can run side by side again, one holding the hotkey for the other
//! to collide with, exactly as before 1.5.
//!
//! ```text
//! UPTAKE_DEV_ALLOW_MULTIPLE=1 pnpm tauri dev
//! ```

use std::env;
use std::sync::OnceLock;
use std::thread::{self, ThreadId};
use std::time::Duration;

use tauri::AppHandle;

/// Environment variable holding the re-show delay, in seconds.
const RESHOW_VAR: &str = "UPTAKE_DEV_RESHOW";

/// Environment variable that, when set, skips registering the single-instance
/// guard so M-9 can still be reproduced with two dev instances.
const ALLOW_MULTIPLE_VAR: &str = "UPTAKE_DEV_ALLOW_MULTIPLE";

/// Whether the single-instance guard should be skipped this run.
///
/// (An earlier `UPTAKE_DEV_FORCE_CLICKTHROUGH` toggle lived here too. It was
/// deleted with ADR-0016: the window is unconditionally click-through now, so
/// there is nothing left to force.)
pub fn single_instance_disabled() -> bool {
    env::var(ALLOW_MULTIPLE_VAR).is_ok()
}

/// The thread that ran `setup`, i.e. the event-loop thread.
static MAIN_THREAD: OnceLock<ThreadId> = OnceLock::new();

/// Records the event-loop thread so [`log_summon`] can compare against it.
pub fn record_main_thread() {
    let _ = MAIN_THREAD.set(thread::current().id());
}

/// Reports which thread a summon arrived on, and the overlay's origin before
/// the show.
///
/// This turns the central question of task 1.4 into an observation instead of
/// an argument. Reading the dependency sources says the hotkey handler runs on
/// the event-loop thread; that conclusion decides whether `overlay::show`'s
/// `reconvert_regions` call is load-bearing for this path, so it is worth one
/// printed line rather than trust in a source-reading.
pub fn log_summon(caller: &str, origin: Option<(i32, i32)>) {
    if env::var(RESHOW_VAR).is_err() {
        return;
    }
    let current = thread::current().id();
    let on_event_loop = MAIN_THREAD.get() == Some(&current);
    let origin = match origin {
        Some((x, y)) => format!("({x}, {y})"),
        None => "unreadable".to_string(),
    };
    eprintln!(
        "dev-harness: summon via {caller} on {current:?} — event-loop thread: {on_event_loop} · \
         overlay origin before show: {origin}"
    );
}

/// Schedules a re-show of the overlay if [`RESHOW_VAR`] is set.
///
/// Called at the end of `overlay::hide`. Rearrange or unplug a monitor during
/// the delay, and the overlay comes back through the off-event-loop-thread
/// path with the display configuration changed underneath it.
pub fn schedule_reshow(app: &AppHandle) {
    let Some(delay) = reshow_delay() else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        eprintln!(
            "dev-harness: re-showing the overlay in {} s — change the display configuration now",
            delay.as_secs()
        );
        std::thread::sleep(delay);
        log_summon("dev-harness timer", crate::overlay::current_origin(&app));
        // Deliberately *not* `run_on_main_thread`: calling from this thread is
        // the entire point (it exercises `show`'s off-event-loop `reconvert_regions`
        // path). `summon` reaches `show` through the state machine and logs its
        // own failures. See the module docs.
        crate::overlay::summon(&app);
    });
}

/// The configured delay, or `None` when the harness is off.
///
/// An unparseable or zero value is treated as off and said so, rather than
/// silently falling back to a default — a harness that runs on a different
/// schedule than the operator believes is worse than one that does not run.
fn reshow_delay() -> Option<Duration> {
    let raw = env::var(RESHOW_VAR).ok()?;
    match raw.trim().parse::<u64>() {
        Ok(seconds) if seconds > 0 => Some(Duration::from_secs(seconds)),
        _ => {
            eprintln!("dev-harness: ignoring {RESHOW_VAR}={raw:?} — expected a positive integer");
            None
        }
    }
}

// (A `log_conversion` helper printed the Rust side of the CSS→physical region
// conversion here until ADR-0016 deleted that conversion with the rest of the
// per-region click-through machinery. The lesson it embodied — print both
// sides of an IPC boundary before trusting either — is recorded in ADR-0011
// and survives in the frontend's own conversion fail-safes.)
