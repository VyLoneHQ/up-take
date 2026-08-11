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
//!
//! ## `UPTAKE_DEV_PACING`
//!
//! Task 1.17(a)'s gesture instrumentation: the achieved poll rate and the
//! emit→painted round trip, plus the sampled IPC probe that produces the second
//! number.
//!
//! **A pair of lines per gesture is the intent and not a guarantee**, and the
//! difference is what `I-11` is about. A drag observed in a single poll
//! iteration carries `ticks == 0` and prints nothing, and a drag abandoned by
//! `Esc` or by toggling the overlay away is flushed with a `cut short by
//! deactivate` label rather than reported normally. Read
//! [`announce_pacing`]'s line at startup before reading anything into silence:
//! it is the only thing in this module that distinguishes *the probe is off*
//! from *the probe is on and saw nothing worth printing*.
//!
//! **It is opt-in for two separate reasons, and only one of them is noise.** A
//! line per gesture is a line per *drag*, so an ordinary dev session that places
//! a few areas gets a running commentary it did not ask for — that is the
//! cosmetic half. The load-bearing half is that the probe adds an IPC round trip
//! on every eighth selection frame, ~27 a second, **to the exact path it
//! measures**: leaving it on by default means every dev build paces its drags
//! while carrying the weight of watching itself, and the numbers drift towards
//! measuring the measurement.
//!
//! Off by default; the two `quality-bars.md` §1 drag rows are re-measured by
//! turning it on deliberately.
//!
//! ```text
//! UPTAKE_DEV_PACING=1 pnpm tauri dev
//! ```
//!
//! **On startup this prints one line either way** — armed or not — so a rig
//! operator can tell at a glance whether the variable reached the process. That
//! is the whole of `I-11`'s prescribed fix, and it is a positive signal rather
//! than a louder probe.
//!
//! ## `UPTAKE_DEV_MONITOR_PERTURB`
//!
//! **The route to a check no operator can perform.** `6e25555` widened the
//! monitor cache from bare rectangles to whole monitors so that a scale change
//! at *identical bounds* would drive the warm-session resync, and nothing
//! verifies it. The owed rig check asked for a DPI change while PLACEMENT is
//! visible, and `UT-F-50` records that this is impossible: PLACEMENT installs
//! the global mouse hook and takes focus, so no Windows display UI is reachable
//! while the state under test is active. An unplug does not substitute, because
//! an unplug changes the bounds and would have passed under the old code.
//!
//! This injects a scale-only difference at the cache and drives the real
//! `sync_bounds` path. Enter PLACEMENT, wait, and watch for the warm-session
//! resync line; its absence is the failure.
//!
//! ```text
//! UPTAKE_DEV_MONITOR_PERTURB=20 pnpm tauri dev
//! ```
//!
//! **What it does not cover, stated because the gap is the interesting part:**
//! Windows raising the change and Tauri reporting a new scale factor are not
//! exercised. A green here means a scale-only difference drives the resync, and
//! nothing more.
//!
//! ## `UPTAKE_DEV_REPORT`, and it is NOT in this module
//!
//! **The fifth switch lives in [`crate::output`], not here, and this section
//! exists so a reader of this file finds it.** This module is
//! `#[cfg(debug_assertions)]` at its declaration in `lib.rs`, so nothing in it
//! exists in a release build, and `UPTAKE_DEV_REPORT` is a switch whose entire
//! purpose is a release build. It forces `output::report`'s per-action line on,
//! which is otherwise compiled out of release while the over-budget lines beside
//! it are not, so a release log records only the actions that MISSED the bar
//! (`I-42`, `UT-F-60`).
//!
//! ```text
//! UPTAKE_DEV_REPORT=1 <a release build>
//! ```
//!
//! Added here by an independent review, which found the variable documented only
//! at its own definition: this header is what `click_through.rs`, `hotkey.rs`
//! and `lib.rs` point a rig operator at, so a switch absent from it is a switch
//! nobody finds. Nothing detects an omission from a prose index, which is the
//! same weakness as any rule an author has to remember.

use std::env;
use std::sync::{LazyLock, OnceLock};
use std::thread::{self, ThreadId};
use std::time::Duration;

use tauri::AppHandle;

/// Environment variable holding the re-show delay, in seconds.
const RESHOW_VAR: &str = "UPTAKE_DEV_RESHOW";

/// Environment variable that, when set, skips registering the single-instance
/// guard so M-9 can still be reproduced with two dev instances.
const ALLOW_MULTIPLE_VAR: &str = "UPTAKE_DEV_ALLOW_MULTIPLE";

/// Environment variable that, when set, turns on the gesture pacing and
/// emit→painted instrumentation.
const PACING_VAR: &str = "UPTAKE_DEV_PACING";

/// Environment variable holding the monitor-cache perturbation delay, in
/// seconds. See [`schedule_monitor_perturb`].
const MONITOR_PERTURB_VAR: &str = "UPTAKE_DEV_MONITOR_PERTURB";

/// Whether gesture instrumentation is on this run.
///
/// Read once and cached: the probe half of this is consulted on **every**
/// selection frame — up to ~220 a second — and `env::var` allocates a `String`
/// per call. Caching also means the answer cannot change mid-gesture, so a
/// summary line always describes a gesture that was measured end to end.
pub fn pacing_enabled() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| env::var(PACING_VAR).is_ok());
    *ENABLED
}

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

/// Says at startup whether gesture pacing is armed — **the positive signal
/// `I-11` asks for**, and the reason it is a separate call rather than a louder
/// probe.
///
/// `UPTAKE_DEV_PACING` produced no output at all across nine drags on the
/// 2026-07-28 rig pass, and nothing could distinguish *the variable did not
/// reach the process*, *no gesture was ever counted*, and *the probe is working
/// and the drags were not drags*. All three look like silence. An instrument
/// whose only output is the measurement it is asked for cannot report that it is
/// alive, which is `I-11`'s point and the `F-17`/`F-33`/`UT-F-41` family's
/// shape: a check that says nothing when something is wrong is indistinguishable
/// from one that is switched off.
///
/// So this prints on **both** paths — armed and not armed. A line only when
/// enabled would leave the disabled case silent, and the disabled case is
/// exactly the one a rig operator mistakes for a broken probe.
pub fn announce_pacing() {
    if pacing_enabled() {
        // **What this promises is bounded deliberately, and the first version
        // was not.** It said "one line per completed drag", which is false: a
        // drag observed in a single poll iteration carries `ticks == 0`, and
        // both report sites suppress the line at zero. It then named the wrong
        // cause for a missing line. An armed signal that overclaims is `I-11`'s
        // own defect one level up — it converts "no measurement" into "the
        // measurement is zero" — so it now states the floor a drag has to clear
        // and lists both reasons for silence instead of one.
        eprintln!(
            "dev-harness: gesture pacing ARMED ({PACING_VAR}) — a drag spanning \
             at least two poll iterations prints `poll: gesture ran …`. Silence \
             after a drag means EITHER it was shorter than two iterations OR it \
             was abandoned before the loop saw it end — it does not mean the \
             probe is off, which is the one thing this line rules out."
        );
    } else {
        eprintln!(
            "dev-harness: gesture pacing off ({PACING_VAR} unset) — no \
             `poll: gesture …` lines will be printed, and their absence measures \
             nothing"
        );
    }
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

/// Schedules a synthetic **scale-only** monitor-cache change if
/// [`MONITOR_PERTURB_VAR`] is set.
///
/// Enter PLACEMENT and wait. That the overlay owns the input is irrelevant
/// here, which is the whole point: `UT-F-50` records that the owed DPI check
/// cannot be performed by hand *because* PLACEMENT takes the mouse and the
/// keyboard, so no Windows display UI is reachable while the state under test
/// is active. This needs no UI at all.
///
/// Read `overlay::dev_perturb_cached_scale` for what a green result is worth:
/// the gate and the resync behind it are the real code on the real path, and
/// Windows raising the change is **not** exercised.
///
/// **What to watch for.** A `freeze: warm sessions held for…` line, printed by
/// `sync_warm_sessions` on the `refresh_monitor_cache` → resync path. Its
/// *absence* is the failure — and it is exactly the absence that revealed the
/// powering-a-monitor-off substitute had never run at all.
pub fn schedule_monitor_perturb(app: &AppHandle) {
    let Some(delay) = perturb_delay() else {
        return;
    };
    let app = app.clone();
    thread::spawn(move || {
        eprintln!(
            "dev-harness: perturbing one monitor's cached scale in {} s — be in \
             PLACEMENT, and watch for a warm-session resync line",
            delay.as_secs()
        );
        thread::sleep(delay);
        let Some((bounds, was, now)) = crate::overlay::dev_perturb_cached_scale() else {
            // Said out loud rather than returning quietly. An empty cache means
            // the perturbation never happened, and a silent no-op would leave
            // the operator reading the missing resync line as a failure of the
            // code under test — which is `UT-F-50`'s own defect, one layer up.
            eprintln!(
                "dev-harness: the monitor cache is empty, so nothing was perturbed \
                 and this check did NOT run"
            );
            return;
        };
        eprintln!(
            "dev-harness: cached scale for {}x{} at ({}, {}) set {was} -> {now}, bounds \
             untouched — driving sync_bounds",
            bounds.size.width, bounds.size.height, bounds.origin.x, bounds.origin.y
        );
        if let Err(error) = crate::overlay::sync_bounds(&app) {
            eprintln!("dev-harness: sync_bounds failed after the perturbation: {error}");
        }
    });
}

/// The configured perturbation delay, or `None` when the harness is off. Same
/// parse-strictly-or-say-so rule as [`reshow_delay`], for the same reason.
fn perturb_delay() -> Option<Duration> {
    let raw = env::var(MONITOR_PERTURB_VAR).ok()?;
    match raw.trim().parse::<u64>() {
        Ok(seconds) if seconds > 0 => Some(Duration::from_secs(seconds)),
        _ => {
            eprintln!(
                "dev-harness: ignoring {MONITOR_PERTURB_VAR}={raw:?} — expected a positive integer"
            );
            None
        }
    }
}

// (A `log_conversion` helper printed the Rust side of the CSS→physical region
// conversion here until ADR-0016 deleted that conversion with the rest of the
// per-region click-through machinery. The lesson it embodied — print both
// sides of an IPC boundary before trusting either — is recorded in ADR-0011
// and survives in the frontend's own conversion fail-safes.)
