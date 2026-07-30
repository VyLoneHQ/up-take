//! The frozen screen: full-monitor stills held while PLACEMENT is frozen
//! (roadmap task 1.9d, [ADR-0026]).
//!
//! # What this is for
//!
//! Some things on screen do not wait to be selected — a video frame, a
//! notification sliding away, a hover state that dies the moment you reach for
//! the mouse. Freeze-on-demand captures the monitors to stills and shows those,
//! so the moment can be selected at leisure ([ADR-0014] §4).
//!
//! # The semantics, all decided rather than inferred
//!
//! [ADR-0026] settles what [ADR-0014] §4 left open, and each of these is a
//! decision this module implements rather than a choice made here:
//!
//! * **`Ctrl+Space` toggles frozen↔live, and only in PLACEMENT.** This is not a
//!   system-wide screen freeze.
//! * **The default is live, and it resets to live on every entry to PLACEMENT**
//!   ([`thaw`] is called on the way in). Freezing is always something the user
//!   asked for during *this* visit — which is what keeps ADR-0014's promise that
//!   the desktop never freezes while you place an area.
//! * **The toggle always fires**, whatever type is armed or none. Freezing is
//!   only *useful* for types that consume pixels at creation, but usefulness is
//!   not a gate: a key that silently does nothing is worse than one that does
//!   something harmless.
//! * **Freezing changes no area type's behaviour.** It is a view state. A
//!   Screenshot area arms, drags, releases and copies identically either way.
//! * **Each freeze re-captures** ([ADR-0014] §4), so toggling off and on gives
//!   the current moment rather than the first one.
//!
//! # Why there is no freshness bound here, unlike `precapture`
//!
//! [`crate::precapture`] bounds its held frame's age, because the user is
//! selecting on **live** pixels while a stale frame waits to be cropped — the
//! image and the screen can silently disagree. Frozen is the opposite case:
//! the user is selecting *on the still itself*, so the pixels they see are by
//! construction the pixels they get, however long they take. [ADR-0022] §5 names
//! this — the frozen source "carries no staleness question at all" — and it is
//! the reason this module has no clock in it.
//!
//! **That is a real invariant and not a convenience.** If a future change ever
//! displays something other than what [`crop`] serves, this reasoning collapses
//! and a bound has to come back.
//!
//! # Why the crop is not implemented here
//!
//! It is [`RgbaBitmap::crop_screen`], shared with `precapture`. 1B's exit gate
//! requires every path that can produce a Screenshot's pixels to produce
//! identical results for the same rectangle, and the cheapest way to hold that
//! is for the paths to be the same code. See that function's docs.
//!
//! [ADR-0014]: the private planning repo's
//! `DECISIONS/ADR-0014-capture-and-render-over-live-content.md`
//! [ADR-0022]: the private planning repo's
//! `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`
//! [ADR-0026]: the private planning repo's
//! `DECISIONS/ADR-0026-freeze-on-demand-trigger.md`

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

/// One monitor's still: where it came from, the pixels a crop is cut out of,
/// and the PNG the WebView displays.
///
/// The rectangle is not decoration: a bitmap does not know its own position, and
/// without it a screen-space crop cannot be computed at all.
///
/// # Why both representations, and why the PNG is made now
///
/// The same reason [`crate::captures::CaptureStore`] holds both: a crop needs
/// raw RGBA and an `<img>` needs PNG, and neither cheaply produces the other.
/// Encoding happens **at freeze time**, on the thread that already spawned for
/// the captures, rather than inside the URI-scheme handler — that handler runs
/// on the WebView2 UI thread, and a full-monitor PNG encode there would stall
/// the very repaint it is feeding.
///
/// The cost is memory, and it is the largest this feature carries: a 1440p
/// monitor is ~14.7 MB raw plus its PNG, and a 4K one ~33 MB. Four monitors
/// frozen is therefore well past `quality-bars.md` §1's 80 MB idle-RAM row —
/// **which is why [`thaw`] runs on every state transition and not only on the
/// toggle.** Frozen is a transient state by construction; if it ever becomes a
/// resting one, this is the number that has to be revisited first.
struct Still {
    rect: Rect,
    bitmap: RgbaBitmap,
    png: Vec<u8>,
}

/// The stills currently displayed, one per frozen monitor. Empty means live.
///
/// **Emptiness is the state**, rather than a separate `bool` that could disagree
/// with it. A frozen screen with no stills is not a state this feature has: if
/// every capture failed there is nothing to show, and continuing to report
/// "frozen" would leave the user looking at a live desktop the app believes is
/// frozen — the two-flags-one-fact defect that keeps showing up in this project's
/// findings ledger.
static STILLS: Mutex<Vec<Still>> = Mutex::new(Vec::new());

/// Bumped by every [`freeze`], and carried in each still's URL.
///
/// **Cache-busting, and it is load-bearing rather than tidy.** WebView2 caches
/// by URL, so a second freeze re-using `frozen-0.png` would redisplay the
/// *first* freeze's pixels — a still of a moment the user deliberately replaced,
/// with nothing on screen to say so. Exactly the defect the pin store's own
/// version counter exists for, and ADR-0014 §4's "each freeze re-captures the
/// current moment" is the promise it would quietly break.
static VERSION: AtomicU64 = AtomicU64::new(0);

/// Whether a freeze is in flight, so a second toggle cannot start another.
///
/// A freeze takes ~420 ms on the four-monitor rig, which is comfortably long
/// enough for a user to press the toggle again — and without this, the second
/// press read [`is_frozen`] as `false` (the first freeze has not stored
/// anything yet) and started a **second concurrent freeze**. Eight capture
/// threads, two `VERSION` bumps, and the first freeze's emitted URLs already
/// stale by the time the frontend fetched them, which serves a 404 for every
/// still and leaves a `frozen` badge over a monitor showing live pixels.
static FREEZING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Bumped by every [`thaw`], so a freeze that was overtaken cannot publish.
///
/// **A freeze takes ~420 ms and a state transition takes none.** Press
/// `Ctrl+Space` and then `Esc` inside that window — which is exactly what a user
/// does when the freeze feels unresponsive — and without this the capture
/// threads land *after* [`set_state`](crate::overlay)'s thaw has run: the stills
/// are stored into a state that is no longer Placement, ~60 MB of them, with no
/// toggle left on screen to dismiss them. It is the failure the `thaw`-on-every-
/// transition rule exists to prevent, arriving by the one route that rule cannot
/// see.
///
/// Read and compared **under the stills lock**, which is what makes the check
/// race-free rather than merely narrow: `thaw` clears and bumps under the same
/// lock, so a freeze either publishes before the thaw or observes it. Same
/// shape as `precapture`'s generation and for the same reason.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Whether the warm capture path (roadmap 1.9f) is enabled. **Default off.**
///
/// # Why this is a setting and not simply the behaviour
///
/// Measured on the four-monitor rig 2026-07-30 and recorded in ADR-0026's second
/// amendment: holding a session per monitor costs **+0.62 pp of one core** with
/// the desktop mostly still and **+0.94 pp with video playing**, against the
/// 0.87 pp `quality-bars.md` §1 leaves after the click-through poll — so the
/// video case misses §1's target — plus ~175 MiB of private commit. The release
/// condition for making it the default was *"cheap at idle"*, and 71 % of the
/// remaining budget is not that. Whoever wants the instant freeze pays for it;
/// whoever does not, does not.
///
/// # How it is set, and why that is temporary
///
/// Task **1.14** owns the settings UI and does not exist yet, so this reads
/// `UPTAKE_WARM_CAPTURE` once at startup. When 1.14 lands, the env read is
/// replaced by the stored setting and **this static stays the one place the
/// answer lives** — the point of routing every reader through
/// [`warm_capture_enabled`] rather than checking the variable at each site.
static WARM_CAPTURE: AtomicBool = AtomicBool::new(false);

/// Reads `UPTAKE_WARM_CAPTURE` and reports what it decided.
///
/// **The report is not decoration — it is the `I-11` fix.** A warm path that is
/// enabled but never becomes warm behaves *exactly* like one that was never
/// switched on: every capture falls back, everything works, slowly, forever. So
/// the setting states itself at startup and [`freeze`] states how many stills
/// the warm path actually served, rather than a reader inferring either from the
/// absence of a complaint.
pub(crate) fn init_warm_capture() {
    let enabled = std::env::var("UPTAKE_WARM_CAPTURE")
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "on"));
    WARM_CAPTURE.store(enabled, Ordering::SeqCst);
    if enabled {
        eprintln!(
            "freeze: warm capture ENABLED (UPTAKE_WARM_CAPTURE) — sessions are held \
             while Placement is visible; expect higher idle CPU and ~175 MB more RAM"
        );
    }
}

/// Whether the warm capture path is enabled. The only reader of [`WARM_CAPTURE`].
pub(crate) fn warm_capture_enabled() -> bool {
    WARM_CAPTURE.load(Ordering::SeqCst)
}

/// Starts or stops the held sessions to match `is_placement`.
///
/// Called from the one point every state transition funnels through, beside
/// [`thaw`] and for the same reason: warm sessions exist only while Placement is
/// visible, so "start on entry" and "never held outside Placement" are one rule,
/// and writing it once means a state added later cannot forget it. Holding four
/// full-monitor sessions into Living would be this feature's version of the
/// undismissable-stills defect — an ongoing CPU and RAM cost for a state that
/// cannot freeze.
///
/// A no-op when the setting is off, and [`uptake_capture::warm::stop`] is safe
/// with nothing running, so the disabled path costs a bool read and the enabled
/// path cannot leak.
pub(crate) fn sync_warm_sessions(is_placement: bool) {
    if !warm_capture_enabled() {
        return;
    }
    if is_placement {
        let held = uptake_capture::warm::start();
        // Reports what is *warm*, not only what is held, because `start` keeps
        // sessions that already cover the desktop — so a Placement → Placement
        // transition prints `4 warm` while a fresh entry prints `0 warm` and
        // stays that way for ~330 ms. A fixed "not warm yet" would have been
        // wrong on one of those two paths, and `I-11` is this project's row
        // about a probe whose output cannot distinguish the states it reports.
        #[cfg(debug_assertions)]
        eprintln!(
            "freeze: warm sessions held for {held} monitor(s) — {} warm",
            uptake_capture::warm::status().warm
        );
        let _ = held;
    } else {
        uptake_capture::warm::stop();
    }
}

/// Releases [`FREEZING`] however [`freeze`] leaves — including by panic, which
/// would otherwise wedge the feature off for the life of the process.
struct FreezingGuard;

impl Drop for FreezingGuard {
    fn drop(&mut self) {
        FREEZING.store(false, Ordering::SeqCst);
    }
}

fn stills() -> std::sync::MutexGuard<'static, Vec<Still>> {
    STILLS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The URL a frozen still is served at. The only thing that builds one.
///
/// Shares [`crate::captures::SCHEME`] rather than registering a second protocol:
/// one scheme means one handler, one `img-src` entry for whoever adds the CSP
/// this app still lacks, and one place the Windows `http://<scheme>.localhost`
/// form is got right (see [`crate::captures::pin_url`], where getting it wrong
/// cost a session).
///
/// The `frozen-` prefix is what keeps the two namespaces apart: an area's URL is
/// `<id>-<version>.png` and an id is a number, so no area can ever produce a
/// path that starts with `frozen-`.
#[must_use]
fn still_url(index: usize, version: u64) -> String {
    let path = format!("frozen-{index}-{version}.png");
    if cfg!(windows) {
        format!("http://{}.localhost/{path}", crate::captures::SCHEME)
    } else {
        format!("{}://localhost/{path}", crate::captures::SCHEME)
    }
}

/// The PNG for one frozen still, if `version` is still the live freeze.
///
/// A version mismatch is `None` rather than the current bytes, for the same
/// reason the pin store refuses one: the only way to ask for a stale version is
/// to hold a stale URL, and answering it with fresh pixels would hide a caching
/// bug instead of surfacing it.
pub(crate) fn still_png(index: usize, version: u64) -> Option<Vec<u8>> {
    // Version read *under* the stills lock, not beside it. `VERSION` and
    // `STILLS` are one fact in two variables, and reading them separately lets a
    // freeze land between the two reads — see `stills_for_display`, where the
    // same split had the worse consequence.
    let stills = stills();
    if version != VERSION.load(Ordering::SeqCst) {
        return None;
    }
    stills.get(index).map(|still| still.png.clone())
}

/// Every frozen still as `(rect, url)`, for the frontend to lay out.
///
/// Physical virtual-desktop pixels, unconverted — the WebView owns its own
/// scale factor (ADR-0011), and pre-converting here is the exact mistake that
/// ADR made a rule about.
///
/// # Why the version is read under the lock
///
/// `VERSION` and `STILLS` are **one fact stored in two variables**, and this
/// function is where reading them apart does visible damage: load the version,
/// have a freeze land, then lock the stills, and every URL returned names the
/// previous freeze while the stills are the new one. [`still_png`] answers a
/// stale version with `None` — correctly — so the frontend renders a `frozen`
/// badge over a monitor whose image 404'd, which is a live desktop labelled
/// frozen. Holding the lock across both makes the pair atomic; [`freeze`]
/// publishes them the same way.
pub(crate) fn stills_for_display() -> Vec<(Rect, String)> {
    let stills = stills();
    let version = VERSION.load(Ordering::SeqCst);
    stills
        .iter()
        .enumerate()
        .map(|(index, still)| (still.rect, still_url(index, version)))
        .collect()
}

/// Whether the screen is currently frozen.
pub(crate) fn is_frozen() -> bool {
    !stills().is_empty()
}

/// What a freeze cost, for the caller to log.
///
/// The stage figures are **maxima across monitors, not sums**, and they may come
/// from different monitors. Every monitor is captured on its own thread, so what
/// the user waits for is the last one to land; a sum would report a cost nobody
/// experiences. They exist to answer one question — which stage dominates — and
/// are reported rather than asserted: the measured split is what decides whether
/// this feature's latency has anywhere left to go.
/// Why a [`freeze`] produced nothing. Both are correct outcomes, not errors —
/// they are distinguished because a log that conflates them cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Skipped {
    /// A freeze was already capturing; this toggle did nothing.
    InFlight,
    /// The screen left Placement mid-capture, so the stills were discarded.
    Retired,
}

impl std::fmt::Display for Skipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InFlight => write!(f, "a freeze is already in flight"),
            Self::Retired => write!(f, "the screen left Placement mid-capture"),
        }
    }
}

pub(crate) struct FreezeReport {
    /// How many monitors were actually captured — see [`freeze`].
    pub count: usize,
    /// How many of those came from a **warm** session rather than a fresh
    /// capture.
    ///
    /// **The positive signal `I-11` says to build in.** With the setting on, a
    /// warm path that never becomes warm is indistinguishable from one that was
    /// never enabled: every monitor silently falls back and the feature works,
    /// slowly. `0/4` here is the difference, and it is the first thing to read
    /// on a rig pass — a fast freeze with `warm 0` means something else got
    /// faster and the warm path is still doing nothing.
    pub warm_served: usize,
    /// Wall-clock for the whole freeze, capture through encode.
    pub elapsed_ms: u128,
    /// The slowest single monitor's capture.
    pub slowest_capture_ms: u128,
    /// The slowest single monitor's PNG encode.
    pub slowest_encode_ms: u128,
}

/// Captures `monitors` and holds the stills, replacing any already held.
///
/// [`FreezeReport::count`] is **not** always `monitors.len()`: a monitor WGC and
/// GDI both decline is skipped rather than failing the whole freeze, because
/// freezing three of four screens is more useful than freezing none. The count is
/// what the caller logs.
///
/// # What this does *not* do: capture the moment the user asked for
///
/// **The pixels are the desktop as it was ~350 ms after the keypress, not at
/// it** — measured on the rig 2026-07-29 against an on-screen stopwatch, and
/// recorded as `UT-F-45`. Nothing here is slow in the sense of being fixable by
/// tuning: [`uptake_capture::capture_region`] builds a D3D11 device, a capture
/// item, a frame pool and a session *before* WGC starts looking, so the setup
/// happens ahead of the frame rather than behind it.
///
/// ADR-0014 §4 justifies this feature with "a video frame, a notification sliding
/// away" — moments this path misses. What it delivers is the weaker and still
/// useful *stop a slowly-changing screen so it can be selected at leisure*, and
/// the ADRs now say so rather than the reader inferring the stronger claim.
///
/// The route to the stronger one is a **warm capture session held open while
/// PLACEMENT is visible**, which makes the toggle a readback rather than a
/// capture. That is a settings-gated follow-up whose default is owed a
/// measurement (roadmap 1.9f), not a change to this function.
///
/// # Threading
///
/// **One thread per monitor, so the freeze costs one capture rather than one
/// per monitor.** It still blocks its caller for that time — roughly 183–313 ms
/// on the dev rig plus the PNG encode — so it must not run on the event-loop
/// thread or inside the `WH_MOUSE_LL` callback; the caller spawns. That is the
/// same constraint `precapture` documents at length and the failure class F-33
/// found the hard way.
///
/// The serial version of this measured **1139–1367 ms on the four-monitor rig**
/// (2026-07-29), which is four full first-frame latencies end to end and reads
/// as a hang rather than a beat. [`uptake_capture::capture_region`] has always
/// spawned every monitor's shot before waiting on any — a multi-monitor region
/// costs one first-frame latency — and this function was the one caller not
/// getting that, because it asked for a monitor at a time. Each `capture_region`
/// call already runs its WGC session on its own pump thread with its own message
/// queue, so calling four of them concurrently is the same resource shape that
/// one straddling capture already produces, not a new one.
///
/// The overlay is permanently excluded from capture ([ADR-0019]), so a freeze
/// never captures UP-TAKE's own chrome and re-freezing cannot compound it.
///
/// [ADR-0019]: the private planning repo's
/// `DECISIONS/ADR-0019-overlay-excluded-from-capture.md`
pub(crate) fn freeze(monitors: &[Rect]) -> Result<FreezeReport, Skipped> {
    // Claimed before any work: `InFlight` means this call must do nothing at
    // all — not even capture and discard, which would cost four WGC sessions to
    // reach the same place.
    if FREEZING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(Skipped::InFlight);
    }
    let _releases_the_claim = FreezingGuard;
    let generation = GENERATION.load(Ordering::SeqCst);
    let started = Instant::now();
    // Scoped rather than detached: the stills must all be in hand before the
    // state is emitted, and a scope makes "every thread has finished" a property
    // of the type rather than something the caller has to remember to join.
    let captured: Vec<(Still, u128, u128, bool)> = std::thread::scope(|scope| {
        let handles: Vec<_> = monitors
            .iter()
            .map(|monitor| scope.spawn(move || capture_still(*monitor)))
            .collect();
        handles
            .into_iter()
            // A panicked capture thread is treated exactly as a failed one: the
            // monitor is dropped and the rest of the freeze stands. Joining in
            // spawn order is what keeps the stills' indices — and therefore
            // their URLs — matching the monitor list.
            .filter_map(|handle| handle.join().unwrap_or(None))
            .collect()
    });
    let slowest_capture_ms = captured.iter().map(|(_, capture, _, _)| *capture).max();
    let slowest_encode_ms = captured.iter().map(|(_, _, encode, _)| *encode).max();
    let warm_served = captured.iter().filter(|(_, _, _, warm)| *warm).count();
    let captured: Vec<Still> = captured.into_iter().map(|(still, _, _, _)| still).collect();
    let count = captured.len();
    // Published under one lock, because the version and the stills are one fact:
    // a reader that catches the bump without the new stills builds URLs the
    // store will refuse. See `stills_for_display`.
    //
    // The version is bumped even when nothing was captured: a failed freeze must
    // still invalidate the previous freeze's URLs, or the WebView would happily
    // redisplay an older still for a freeze that produced none.
    publish(captured, generation)?;
    Ok(FreezeReport {
        count,
        warm_served,
        elapsed_ms: started.elapsed().as_millis(),
        slowest_capture_ms: slowest_capture_ms.unwrap_or_default(),
        slowest_encode_ms: slowest_encode_ms.unwrap_or_default(),
    })
}

/// Stores `captured` as the frozen stills, unless `generation` has been retired.
///
/// A separate function because it is the whole of the overtaken-freeze fix and
/// the only part of a freeze that can be driven without a desktop: `freeze`
/// reads the generation at entry, so a test calling `thaw` around it can never
/// reproduce the interleaving that matters. Here the stale generation is an
/// argument, which is the interleaving, stated directly.
///
/// # Why the version is not bumped on the retired path
///
/// Nothing was published, so nothing needs invalidating — and bumping would
/// retire the URLs of whatever the *thaw* left behind, which is a live screen.
fn publish(captured: Vec<Still>, generation: u64) -> Result<(), Skipped> {
    // One lock across the check and both writes. `thaw` takes the same lock to
    // clear and bump, so a freeze either publishes before it or observes it —
    // the check is race-free rather than merely narrow.
    let mut stills = stills();
    if GENERATION.load(Ordering::SeqCst) != generation {
        return Err(Skipped::Retired);
    }
    // Published under that same lock, because the version and the stills are one
    // fact: a reader that catches the bump without the new stills builds URLs
    // the store will refuse. See `stills_for_display`.
    //
    // The version is bumped even when nothing was captured: a failed freeze must
    // still invalidate the previous freeze's URLs, or the WebView would happily
    // redisplay an older still for a freeze that produced none.
    VERSION.fetch_add(1, Ordering::SeqCst);
    *stills = captured;
    Ok(())
}

/// Captures and encodes one monitor, with each stage's cost.
///
/// Returns `None` for a monitor that could not be captured *or* could not be
/// encoded. A still that cannot be encoded is dropped rather than kept, so the
/// display and the crop source cannot disagree about which monitors are frozen:
/// keeping it would mean a monitor whose pixels a drag would use but which shows
/// live content — the see-one-thing-get-another failure this feature exists to
/// avoid.
fn capture_still(monitor: Rect) -> Option<(Still, u128, u128, bool)> {
    let capture_started = Instant::now();
    // The warm path first, when it is on and has something to hand over.
    //
    // **`None` here is ordinary and must stay cheap.** A session is not warm for
    // its first ~330 ms (measured on the rig 2026-07-30), so a `Ctrl+Space`
    // pressed straight after entering Placement takes the cold path — which is
    // the pre-1.9f behaviour, not a failure. The window is close enough to
    // `UT-F-45`'s own ~350 ms lateness to be worth naming in those terms: for
    // that first third of a second the feature is exactly as late as it was
    // before, and no message says so because there is nothing the user could do.
    let warm = warm_capture_enabled()
        .then(|| uptake_capture::warm::capture_monitor(monitor))
        .flatten();
    let served_warm = warm.is_some();
    let shot = match warm {
        Some(shot) => shot,
        None => match uptake_capture::capture_region(monitor) {
            Ok(shot) => shot,
            Err(error) => {
                eprintln!("freeze: could not capture {monitor:?}: {error}");
                return None;
            }
        },
    };
    let capture_ms = capture_started.elapsed().as_millis();
    let encode_started = Instant::now();
    let png = match crate::output::encode_png(&shot.bitmap) {
        Ok(png) => png,
        Err(error) => {
            eprintln!("freeze: could not encode {monitor:?}: {error}");
            return None;
        }
    };
    Some((
        Still {
            // What the capture crate reports it took, never what was asked for:
            // it clamps to the virtual desktop, and trusting the request would
            // offset every crop by the clamp distance.
            rect: shot.rect,
            bitmap: shot.bitmap,
            png,
        },
        capture_ms,
        encode_started.elapsed().as_millis(),
        served_warm,
    ))
}

/// Returns to live, dropping every still.
///
/// Called on the toggle's way out **and on every entry to PLACEMENT**, which is
/// what makes ADR-0026's "reset to live" true rather than merely intended.
pub(crate) fn thaw() {
    // Both under the one lock, so a freeze publishing concurrently either gets
    // in first or sees the bump and discards. See [`GENERATION`].
    let mut stills = stills();
    GENERATION.fetch_add(1, Ordering::SeqCst);
    stills.clear();
}

/// The pixels for `bounds`, cropped out of the frozen still that contains it.
///
/// `None` when the screen is live, or when `bounds` does not lie wholly inside
/// any single still — a rectangle straddling two monitors, which is the same
/// case `precapture` calls a straddle and answers the same way: fall back to an
/// ordinary capture rather than return a short image.
///
/// **Does not consume the still.** `precapture::take` consumes, because a held
/// frame belongs to one drag and reusing it would silently serve stale pixels.
/// Frozen is the opposite: the still is what the user is *looking at*, and it
/// stays until they unfreeze. Consuming it here would mean the second drag on a
/// frozen screen quietly captured live pixels while the display still showed the
/// freeze — the exact see-one-thing-get-another defect this feature exists to
/// prevent.
pub(crate) fn crop(bounds: Rect) -> Option<RgbaBitmap> {
    stills()
        .iter()
        .find_map(|still| still.bitmap.crop_screen(still.rect.origin, bounds))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "architecture §5 bans both outside tests; inside them a failed \
              setup should abort the test loudly. `expect` earns its place in \
              the URL tests: the path is derived from the URL rather than \
              written out, because a hard-coded prefix is exactly what let \
              F-38's round-trip test pass over an unresolvable URL"
)]
mod tests {
    use uptake_core::geometry::Size;

    use super::*;

    /// A bitmap whose every pixel encodes its own coordinates, so a crop taken
    /// from the wrong offset produces different bytes rather than
    /// coincidentally-equal ones.
    ///
    /// A flat fill would let an off-by-anything crop pass every assertion here —
    /// the same weakness the 1.9c review found in the full-size-crop test, where
    /// the coordinate pattern is what makes an axis swap fail.
    fn patterned(size: Size) -> RgbaBitmap {
        let mut bitmap = RgbaBitmap::transparent(size).unwrap();
        let pixels = bitmap.pixels_mut();
        for y in 0..size.height {
            for x in 0..size.width {
                let at = ((y * size.width + x) * 4) as usize;
                pixels[at] = (x % 251) as u8;
                pixels[at + 1] = (y % 251) as u8;
                pixels[at + 2] = ((x ^ y) % 251) as u8;
                pixels[at + 3] = 255;
            }
        }
        bitmap
    }

    /// Installs stills directly, standing in for completed captures — the tests
    /// below are about the decision and the arithmetic, neither of which needs a
    /// desktop.
    fn hold(stills_to_set: Vec<(Rect, RgbaBitmap)>) {
        *stills() = stills_to_set
            .into_iter()
            .map(|(rect, bitmap)| Still {
                rect,
                bitmap,
                // These tests are about the crop decision and the arithmetic,
                // neither of which reads the PNG. A real encode here would buy
                // nothing and make every test depend on the encoder.
                png: Vec::new(),
            })
            .collect();
    }

    /// The URL a still is served at must parse back to the same still through
    /// the **scheme handler's own parser**, not through a copy of it here.
    ///
    /// This is the F-38 lesson pinned as a test: `pin_url` had a round-trip
    /// test that passed while the URL was unresolvable, because it trimmed a
    /// hard-coded prefix and so checked the function against the same wrong
    /// assumption the function was built on. Driving `parse_frozen_path` is
    /// what makes this an independent check rather than a mirror.
    #[test]
    fn a_still_url_parses_back_through_the_scheme_handler() {
        let url = still_url(2, 7);
        let path = url
            .rsplit_once('/')
            .map(|(_, tail)| format!("/{tail}"))
            .expect("a still url always has a path");
        assert_eq!(crate::captures::parse_frozen_path(&path), Some((2, 7)));
    }

    #[test]
    fn a_still_url_is_not_mistaken_for_an_area_pin() {
        // The two namespaces share one scheme, so a frozen path reaching the
        // area parser would 404 as "missing capture" and send the next reader
        // looking in the wrong store entirely.
        let url = still_url(0, 1);
        let path = url
            .rsplit_once('/')
            .map(|(_, tail)| format!("/{tail}"))
            .expect("a still url always has a path");
        assert_eq!(crate::captures::parse_path(&path), None);
    }

    #[test]
    fn a_stale_version_is_refused_rather_than_answered_with_current_pixels() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![(Rect::new(0, 0, 8, 8), patterned(Size::new(8, 8)))]);
        let live = VERSION.load(Ordering::SeqCst);
        assert!(still_png(0, live.wrapping_sub(1)).is_none());
    }

    #[test]
    fn live_until_something_is_frozen() {
        let _guard = crate::precapture::frame_store_guard();
        thaw();
        assert!(!is_frozen());
        assert!(crop(Rect::new(0, 0, 10, 10)).is_none());
    }

    #[test]
    fn thawing_returns_to_live() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        assert!(is_frozen());
        thaw();
        assert!(!is_frozen());
    }

    #[test]
    fn crops_from_the_still_holding_the_rectangle() {
        let _guard = crate::precapture::frame_store_guard();
        let frame = patterned(Size::new(64, 48));
        // A monitor at a negative origin, which is the case that has produced
        // real defects in this project (F-15) rather than a tidy 0,0 one.
        hold(vec![(Rect::new(-1920, -200, 64, 48), frame)]);
        let cropped = crop(Rect::new(-1910, -190, 8, 6)).unwrap();
        assert_eq!(cropped.size(), Size::new(8, 6));
        // Top-left pixel of the crop is (10, 10) of the frame, by the pattern.
        assert_eq!(&cropped.pixels()[0..3], &[10, 10, 10 ^ 10]);
    }

    #[test]
    fn declines_a_rectangle_that_straddles_two_stills() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![
            (Rect::new(0, 0, 64, 48), patterned(Size::new(64, 48))),
            (Rect::new(64, 0, 64, 48), patterned(Size::new(64, 48))),
        ]);
        // Spans the seam: wholly inside neither, and clamping to either would
        // hand back half a screenshot.
        assert!(crop(Rect::new(60, 10, 10, 10)).is_none());
        // ...while a rectangle wholly inside the second still is served.
        assert!(crop(Rect::new(70, 10, 10, 10)).is_some());
    }

    /// The toggle's in-flight guard, driven through `freeze` itself.
    ///
    /// An empty monitor list is what makes this testable without a desktop: it
    /// captures nothing, so the claim, the publish and the release are the only
    /// things that run. The claim is what the test is about, and the second half
    /// matters as much as the first — a guard that never releases would pass an
    /// "it refuses" assertion while wedging the feature off permanently.
    #[test]
    fn a_freeze_started_while_one_is_in_flight_does_nothing() {
        let _guard = crate::precapture::frame_store_guard();
        assert!(
            FREEZING
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            "the flag must start clear, or this test is asserting nothing"
        );
        assert_eq!(
            freeze(&[]).err(),
            Some(Skipped::InFlight),
            "a freeze must not start while another is in flight"
        );
        FREEZING.store(false, Ordering::SeqCst);
        assert!(freeze(&[]).is_ok(), "the claim must be released");
    }

    /// A freeze overtaken by a state transition must publish nothing.
    ///
    /// The generation is passed to [`publish`] rather than read inside it, which
    /// is what makes the interleaving expressible: on the rig this is
    /// `Ctrl+Space` followed by `Esc`, where the capture threads are still
    /// running when the overlay leaves Placement and `set_state` calls `thaw`.
    #[test]
    fn a_freeze_overtaken_by_a_thaw_publishes_nothing() {
        let _guard = crate::precapture::frame_store_guard();
        let generation = GENERATION.load(Ordering::SeqCst);
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        let version = VERSION.load(Ordering::SeqCst);
        // The transition lands: `thaw` is what `set_state` calls, and it is the
        // bump rather than the clear that the publish below has to notice.
        thaw();
        assert_eq!(
            publish(Vec::new(), generation).err(),
            Some(Skipped::Retired),
            "a freeze whose generation was retired must discard its stills"
        );
        assert_eq!(
            VERSION.load(Ordering::SeqCst),
            version,
            "a discarded freeze must not bump the version either"
        );
        // ...and the same publish succeeds against the current generation, so
        // the assertion above is about the generation and not about `publish`
        // refusing everything.
        assert!(publish(Vec::new(), GENERATION.load(Ordering::SeqCst)).is_ok());
    }

    /// A freeze that captured nothing still invalidates the previous freeze's
    /// URLs, so a stale still cannot be redisplayed for it.
    #[test]
    fn a_freeze_that_captures_nothing_still_retires_the_old_urls() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        let stale = stills_for_display();
        assert_eq!(stale.len(), 1);
        let (_, url) = &stale[0];
        let (index, version) =
            crate::captures::parse_frozen_path(url.rsplit_once('/').expect("the url has a path").1)
                .expect("a frozen url parses");
        assert!(still_png(index, version).is_some());
        // A freeze over no monitors: captures nothing, and must still retire it.
        assert!(freeze(&[]).is_ok());
        // Re-install a still at the same index. Without this the assertion below
        // passes for the wrong reason — the stills are empty, so the lookup
        // fails on the *index* and the version is never consulted. Confirmed by
        // mutation: with the `VERSION` bump removed, the first cut of this test
        // stayed green. It is backlog I-1 / UT-F-44's shape, which is that a
        // test can only be trusted once the thing it names has been broken.
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        assert!(
            still_png(index, version).is_none(),
            "the previous freeze's url must not resolve after a re-freeze"
        );
    }

    #[test]
    fn a_crop_does_not_consume_the_still() {
        let _guard = crate::precapture::frame_store_guard();
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        let first = crop(Rect::new(4, 4, 16, 16)).unwrap();
        let second = crop(Rect::new(4, 4, 16, 16)).unwrap();
        assert_eq!(first.pixels(), second.pixels());
        assert!(is_frozen(), "the still must survive being cropped");
    }

    /// **1B exit-gate row 2, the half a unit test can carry.**
    ///
    /// The gate requires the paths that can produce a Screenshot's pixels to
    /// produce identical results for the same rectangle. This asserts that the
    /// frozen path and the held-pre-capture path, given the same frame at the
    /// same position, return **byte-identical** pixels for the same screen
    /// rectangle.
    ///
    /// **It drives both real entry points**, `freeze::crop` and
    /// `precapture::take`, rather than re-implementing either. An earlier cut of
    /// this test called `crop_screen` directly for the held side — which, now
    /// that both paths share that function, reduced to asserting a function
    /// equals itself and would have passed with `freeze::crop` cropping from
    /// entirely the wrong origin. Same shape as the sweep defect in backlog
    /// I-1, caught here by re-reading rather than by the suite.
    ///
    /// **What this does not prove, stated so nobody reads it as more than it
    /// is:** that two *real* captures of the same screen agree. Both sides are
    /// fed the same synthetic frame, so this is the *transformation* half of
    /// the gate row. The capture half needs hardware and belongs to 1.9d's rig
    /// pass.
    #[test]
    fn frozen_and_held_crops_are_byte_identical() {
        let _guard = crate::precapture::frame_store_guard();
        let monitor = Rect::new(-1920, -200, 64, 48);
        let wanted = Rect::new(-1900, -180, 12, 9);
        // Built twice rather than cloned: `patterned` is deterministic, so the
        // two frames are byte-identical inputs, and the test does not need
        // `RgbaBitmap: Clone` to exist for its own convenience.
        hold(vec![(monitor, patterned(Size::new(64, 48)))]);
        crate::precapture::install_for_test(monitor, patterned(Size::new(64, 48)));

        let from_frozen = crop(wanted).unwrap();
        let from_held = crate::precapture::take(wanted).unwrap();

        assert_eq!(from_frozen.size(), from_held.size());
        assert_eq!(
            from_frozen.pixels(),
            from_held.pixels(),
            "the frozen and held crop paths diverged for one rectangle"
        );
    }

    #[test]
    fn a_still_at_the_origin_still_needs_the_subtraction() {
        let _guard = crate::precapture::frame_store_guard();
        // Guards the degenerate case that would pass even if the subtraction
        // were dropped entirely: with the monitor at 0,0 screen space and frame
        // space coincide. Paired with the negative-origin test above, dropping
        // the subtraction fails one of the two.
        hold(vec![(
            Rect::new(0, 0, 64, 48),
            patterned(Size::new(64, 48)),
        )]);
        let cropped = crop(Rect::new(10, 10, 8, 6)).unwrap();
        assert_eq!(&cropped.pixels()[0..3], &[10, 10, 10 ^ 10]);
    }
}
