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
use uptake_core::geometry::{Point, Rect};
use windows_capture::encoder::ImageFormat;

/// One monitor's still: where it came from, the pixels a crop is cut out of,
/// and the encoded image the WebView displays.
///
/// The rectangle is not decoration: a bitmap does not know its own position, and
/// without it a screen-space crop cannot be computed at all.
///
/// # The two fields are not interchangeable, and that is now load-bearing
///
/// `bitmap` is **lossless RGBA and is what the user actually receives** —
/// [`crop`] cuts every Screenshot from it. `encoded` is only what the WebView
/// paints while the user selects, and since [ADR-0027] it is **JPEG**, which is
/// lossy.
///
/// **That asymmetry is the entire justification for a lossy display format.**
/// If any export path is ever changed to derive from `encoded`, this decision
/// silently becomes a defect that degrades what the user gets, and it is
/// [ADR-0027]'s first *Revisit if*. The field is named `encoded` rather than
/// `png` for the same reason: it held JPEG bytes under a name that said PNG for
/// exactly one commit, and a name that lies is how this becomes invisible.
///
/// # Why both representations, and why the encode happens now
///
/// The same reason [`crate::captures::CaptureStore`] holds both: a crop needs
/// raw RGBA and an `<img>` needs an encoded image, and neither cheaply produces
/// the other. Encoding happens **at freeze time**, on the thread that already
/// spawned for the captures, rather than inside the URI-scheme handler — that
/// handler runs on the WebView2 UI thread, and a full-monitor encode there would
/// stall the very repaint it is feeding.
///
/// The cost is memory, and it is the largest this feature carries: a 1440p
/// monitor is ~14.7 MB raw plus its encoded copy, and a 4K one ~33 MB. Four
/// monitors frozen is therefore well past `quality-bars.md` §1's 80 MB idle-RAM
/// row — **which is why [`thaw`] runs on every state transition and not only on
/// the toggle.** Frozen is a transient state by construction; if it ever becomes
/// a resting one, this is the number that has to be revisited first. ADR-0027
/// helped here rather than hurting: the encoded half fell from 33.9 MB to 8.9 MB
/// across four monitors on the worst screen this project tests.
///
/// [ADR-0027]: the private planning repo's
/// `DECISIONS/ADR-0027-jpeg-for-the-freeze-display-path.md`
struct Still {
    rect: Rect,
    bitmap: RgbaBitmap,
    encoded: Vec<u8>,
    /// The MIME type of `encoded`, **recorded when it was encoded** rather than
    /// re-derived when it is served.
    ///
    /// The two are the same today, because [`DISPLAY_FORMAT`] is written once at
    /// startup and never again. They stop being the same the moment roadmap 1.14
    /// wires a stored setting to it — which [`display_format`] describes as "a
    /// call site rather than a redesign", and this field is the part that claim
    /// was missing: a still encoded before the switch would otherwise be served
    /// under the new format's `Content-Type`, and a JPEG announced as a PNG
    /// renders as a broken image. Storing the answer beside the bytes makes the
    /// header describe what was actually produced, whatever the setting does
    /// afterwards.
    content_type: &'static str,
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

/// The clock the `Ctrl+Space` → painted probe runs on.
///
/// Its own epoch rather than `placement`'s, deliberately: that one is reached
/// through `probe_enabled`, which gates on `UPTAKE_DEV_PACING` — the variable
/// **`I-11`** records as producing no output under two launch mechanisms. An
/// instrument wired to a switch nobody can show is on is `I-11` again, so this
/// one is gated on `debug_assertions` alone and has no switch to fail.
///
/// The cost that buys is one IPC round trip per freeze. The poll probe samples
/// one frame in eight precisely because it fires at ~220 Hz; a freeze is a
/// discrete event a user asks for, so there is nothing here to sample down.
#[cfg(debug_assertions)]
static PROBE_EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// The in-flight `Ctrl+Space` → painted probe, in nanoseconds since
/// [`PROBE_EPOCH`]. Zero means none, which is why a stamp is forced to 1.
#[cfg(debug_assertions)]
static PAINT_PROBE: AtomicU64 = AtomicU64::new(0);

/// Starts the `Ctrl+Space` → painted clock.
///
/// Called at the **keypress**, before any capture, because that is what
/// `quality-bars.md` §1's row measures — the user pressed a key and is waiting
/// for a view. Stamping later would measure a stage rather than the promise.
#[cfg(debug_assertions)]
pub(crate) fn stamp_paint_probe() {
    let now = u64::try_from(PROBE_EPOCH.elapsed().as_nanos()).unwrap_or(u64::MAX);
    // Zero is the sentinel for "no probe", so a stamp landing exactly on the
    // epoch is nudged rather than lost.
    PAINT_PROBE.store(now.max(1), Ordering::SeqCst);
}

/// Release builds never stamp, so nothing echoes and nothing is recorded.
#[cfg(not(debug_assertions))]
pub(crate) const fn stamp_paint_probe() {}

/// Takes the in-flight probe, if there is one, and clears it.
///
/// **Taking rather than reading** is what keeps the probe attached to the one
/// payload carrying the new stills. A freeze emits state once; any later emit —
/// an arming change, an area added — would otherwise re-report the same
/// keypress against a paint it had nothing to do with, and that number would
/// look like an improvement.
#[cfg(debug_assertions)]
pub(crate) fn take_paint_probe() -> Option<u64> {
    match PAINT_PROBE.swap(0, Ordering::SeqCst) {
        0 => None,
        probe => Some(probe),
    }
}

#[cfg(not(debug_assertions))]
pub(crate) const fn take_paint_probe() -> Option<u64> {
    None
}

/// Reports one completed `Ctrl+Space` → painted round trip.
///
/// # What this measures, and what it does not
///
/// Keypress → capture → encode → IPC → Svelte → **every still decoded** → the
/// following frame painted. The decode is the reason this exists: `72–78 ms`
/// was capture-through-encode, and §1's row is about pixels on screen. A
/// `requestAnimationFrame` pair alone resolves as soon as the DOM has updated,
/// while four full-monitor stills are still decoding — a comfortable number that
/// excludes the one cost nobody has measured, which is `UT-F-41`'s failure
/// exactly. So the frontend awaits `img.decode()` on every still first.
///
/// It still **excludes DWM's final composite**, like the poll probe, so it is a
/// lower bound on what the eye sees rather than a claim about photons.
#[cfg(debug_assertions)]
pub(crate) fn record_paint_latency(probe: u64) {
    let now = u64::try_from(PROBE_EPOCH.elapsed().as_nanos()).unwrap_or(u64::MAX);
    #[expect(
        clippy::cast_precision_loss,
        reason = "milliseconds for a log line; one freeze's nanoseconds are far below 2^53"
    )]
    let elapsed_ms = now.saturating_sub(probe) as f64 / 1_000_000.0;
    // The bars are printed beside the figure so a rig operator reads a verdict
    // rather than a number they have to go and look up.
    let verdict = if elapsed_ms < 100.0 {
        "meets the < 100 ms target"
    } else if elapsed_ms < 200.0 {
        "OVER the 100 ms target, inside the 200 ms hard fail"
    } else {
        "HARD FAIL - over 200 ms"
    };
    eprintln!("freeze: Ctrl+Space->painted {elapsed_ms:.1} ms - {verdict}");
}

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
/// # Why this is still `false` after the third amendment said it becomes the
/// default
///
/// ADR-0026's third amendment (2026-08-04) decided the warm path becomes the
/// default **and held that flip behind a measurement whose numbers were written
/// down in advance**: the still row at or under **+0.25 pp** and the video row
/// under **+0.40 pp**, taken with the same instrument and the same two conditions
/// that produced the figures above. Narrowing to one monitor is expected to be
/// roughly a quarter of four, and *expected* is the whole problem — that is an
/// arithmetic model of an unmeasured configuration, which is `F-39` and
/// `UT-F-53` in one step. DWM's share was never resolvable and the per-monitor
/// cost is not known to be equal, so one monitor is not one quarter of four in
/// any guaranteed way.
///
/// **So the narrowing ships with the gate still on.** Flipping this line is a
/// one-word change and it is deliberately not made here: it is the rig's to
/// authorise, and if the measurement misses, the default is re-decided on the
/// number rather than on this comment.
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

/// Whether a freeze covers **every** monitor rather than the cursor's.
///
/// **Default off, which means the cursor's monitor** — [ADR-0026]'s third
/// amendment, 2026-08-04, inverting [ADR-0014] §4. The setting used to narrow
/// and now widens, and that direction is the decision rather than a detail:
///
/// * **Disclosure.** A whole-desktop freeze holds full-bleed captures of screens
///   the user was not looking at. Narrowing the freeze is what discharges that;
///   narrowing only the sessions does not, because the concern is the stills.
/// * **Honesty at the boundary.** `I-14` records that a selection straddling two
///   monitors while frozen returns *live* pixels under a display that says
///   frozen. With the other monitors visibly live the user can see what they will
///   get. **This does not fix `I-14`** — it stops it being invisible.
/// * **Cost.** Fewer monitors is less work, but the cost argument belongs to the
///   *sessions* and is measured there, not asserted here.
///
/// # Task 1.14 owns the real setting
///
/// Same shape and same reason as [`WARM_CAPTURE`]: read once at startup from
/// `UPTAKE_FREEZE_ALL_MONITORS` until the settings UI exists, with every reader
/// routed through [`freeze_all_monitors_enabled`] so 1.14 replaces one line.
/// This is the **fourth** setting 1.14 has inherited.
///
/// [ADR-0026]: the private planning repo's
/// `DECISIONS/ADR-0026-freeze-on-demand-trigger.md`
/// [ADR-0014]: the private planning repo's
/// `DECISIONS/ADR-0014-capture-and-render-over-live-content.md`
static FREEZE_ALL_MONITORS: AtomicBool = AtomicBool::new(false);

/// Reads `UPTAKE_FREEZE_ALL_MONITORS` and reports what it decided.
///
/// **Prints the value it loaded, never a name written here** — `UT-F-46` is this
/// project's record of `init_display_format` announcing `staying on png` while
/// the default had been JPEG for a day, inside the function whose own doc said
/// it existed to prevent that. The scope is the thing a rig operator is most
/// likely to misattribute a number to, so it says which one it is on every run
/// rather than only when something was set.
pub(crate) fn init_freeze_scope() {
    let all = std::env::var("UPTAKE_FREEZE_ALL_MONITORS")
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "on"));
    FREEZE_ALL_MONITORS.store(all, Ordering::SeqCst);
    eprintln!(
        "freeze: scope is {} (UPTAKE_FREEZE_ALL_MONITORS)",
        if freeze_all_monitors_enabled() {
            "EVERY monitor"
        } else {
            "the cursor's monitor"
        }
    );
}

/// Whether a freeze covers every monitor. The only reader of
/// [`FREEZE_ALL_MONITORS`].
pub(crate) fn freeze_all_monitors_enabled() -> bool {
    FREEZE_ALL_MONITORS.load(Ordering::SeqCst)
}

/// **The** scope decision: which monitors this freeze and its warm sessions
/// cover.
///
/// # Why this is a type and not a rule written twice
///
/// The first cut of the third amendment had [`monitors_in_scope`] answer the
/// question for the freeze and [`sync_warm_sessions`] answer it again, in its
/// own `if`, against [`uptake_capture::warm::Scope`] — and the doc comment
/// claimed they were "two call sites reading one answer". **They were two
/// implementations of one rule, and the comment asserting otherwise is
/// `bug_003`'s recorded shape.** Caught in PR #42's independent review.
///
/// The rule is stated once here. Both callers convert; neither decides.
///
/// **This is the decision the amendment says the two halves must share.**
/// Narrowing the sessions without narrowing the freeze buys the freeze nothing,
/// because [`freeze`] captures every monitor in parallel and the user waits for
/// the last one to land: one warm monitor and three cold ones costs what the
/// three cold ones cost, ~255–353 ms, which is exactly the number `I-13` is
/// about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every monitor — the widened setting.
    AllMonitors,
    /// The monitor containing this point, widening to every monitor if it is on
    /// none.
    AtPoint(Point),
}

/// Resolves the scope from the setting and the cursor. The only place the rule
/// lives.
///
/// `cursor` is `None` when the cursor could not be read at all, which resolves
/// to [`Scope::AllMonitors`] exactly as a dead zone does. The alternative is a
/// `Ctrl+Space` that captures nothing because a cursor read failed, and a freeze
/// that does nothing is indistinguishable from the hotkey not arriving.
#[must_use]
pub(crate) fn scope_for(cursor: Option<Point>) -> Scope {
    match cursor {
        Some(point) if !freeze_all_monitors_enabled() => Scope::AtPoint(point),
        _ => Scope::AllMonitors,
    }
}

impl Scope {
    /// This scope as the capture crate's, for [`uptake_capture::warm::start`].
    fn for_warm(self) -> uptake_capture::warm::Scope {
        match self {
            Self::AllMonitors => uptake_capture::warm::Scope::AllMonitors,
            Self::AtPoint(point) => uptake_capture::warm::Scope::AtPoint(point),
        }
    }
}

/// The monitors a freeze covers, given the desktop and a resolved [`Scope`].
///
/// # The two sides do NOT read one monitor list, and pretending they do would be
/// the same defect again
///
/// This narrows `all`, which the caller takes from [`crate::overlay::MONITOR_CACHE`].
/// The warm side narrows a **fresh** `crate::monitors::enumerate()` inside
/// `uptake_capture::warm::start`, because that is where a session's bounds have
/// to come from. They agree whenever the cache is current, which is what
/// `sync_bounds` exists to keep true, and they can disagree for the window
/// between a display change and the cache catching up. What is shared is the
/// **rule** ([`Scope`]), not the list — and a monitor the two disagree about
/// falls back to the cold path rather than serving wrong pixels, because
/// `warm::capture_monitor` matches by centre containment and returns `None` on a
/// miss.
#[must_use]
pub(crate) fn monitors_in_scope(all: &[Rect], scope: Scope) -> Vec<Rect> {
    let Scope::AtPoint(cursor) = scope else {
        return all.to_vec();
    };
    match all.iter().find(|bounds| bounds.contains(cursor)) {
        Some(bounds) => vec![*bounds],
        // The dead zone. See `Scope::AtPoint`: every monitor, not none.
        None => all.to_vec(),
    }
}

/// Which format the **display** path encodes stills in. 0 = PNG, 1 = JPEG, 2 = BMP.
///
/// An integer rather than an enum because it lives in an atomic, read once per
/// monitor per freeze.
///
/// **Defaults to JPEG ([ADR-0027], 2026-08-03), decided on measurement.** Warm
/// path, four-monitor rig, against the defined test screens, as `Ctrl+Space` →
/// painted: JPEG **42.0-53.5 ms** (PLAIN) / **162.2 ms** (DENSE), against PNG's
/// **122.1 / 535.4**. So `quality-bars.md` §1's *frozen view painted* row is met
/// at the floor for the first time and never hard fails, where PNG missed the
/// target at the floor and was 2.7× over the hard fail at the ceiling.
///
/// [ADR-0027]: the private planning repo's
/// `DECISIONS/ADR-0027-jpeg-for-the-freeze-display-path.md`
static DISPLAY_FORMAT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// The display encode format, its MIME type, and the name to print.
///
/// # Why this is switchable at all
///
/// It was built to let the rig settle 1.9g's format question, and it stays for
/// two reasons now that [ADR-0027] has settled it: re-measuring the decision,
/// and **the founder's requirement that the user keep the option of PNG**
/// (2026-08-03). That is a *settings* surface, not an environment variable —
/// **roadmap 1.14 owns it**, and this static is the one place the choice lives
/// so wiring a stored setting to it is a call site rather than a redesign, the
/// same shape `init_warm_capture` documents.
///
/// **Lossy is only defensible because this is the display path alone.**
/// [`crop`] cuts the user's actual screenshot from [`Still::bitmap`], the
/// lossless RGBA, and never from these bytes. Change that and this switch
/// becomes a way to silently degrade what the user receives — which is
/// [ADR-0027]'s first *Revisit if*, and the reason `encode_for_display` is a
/// separate function from `encode_png` rather than a parameter on it.
pub(crate) fn display_format() -> (ImageFormat, &'static str, &'static str) {
    match DISPLAY_FORMAT.load(Ordering::SeqCst) {
        1 => (ImageFormat::Jpeg, "image/jpeg", "jpeg"),
        2 => (ImageFormat::Bmp, "image/bmp", "bmp"),
        _ => (ImageFormat::Png, "image/png", "png"),
    }
}

/// Reads `UPTAKE_FREEZE_FORMAT` once, at startup, and **states what it chose**.
///
/// Same shape and the same reason as [`init_warm_capture`]: a format that
/// silently fell back to the default would make a rig pass measure PNG while its
/// operator wrote "JPEG" beside the number, which is `UT-F-46`'s defect exactly.
/// So an unrecognised value is refused out loud rather than absorbed.
pub(crate) fn init_display_format() {
    let Ok(raw) = std::env::var("UPTAKE_FREEZE_FORMAT") else {
        // The default states itself too. A reader of a rig log must be able to
        // tell which format produced a number without knowing what the default
        // was on the day the build was made.
        eprintln!(
            "freeze: display stills encode as {} (default, ADR-0027) — the DISPLAY path only; \
             crops still come from the lossless bitmap",
            display_format().2
        );
        return;
    };
    let chosen = match raw.trim().to_ascii_lowercase().as_str() {
        "png" => 0,
        "jpeg" | "jpg" => 1,
        "bmp" => 2,
        _ => {
            // The format is READ BACK rather than named, because naming it is
            // how this line was wrong: it said "staying on png" while the
            // default had become JPEG, so a mistyped variable would have told a
            // rig operator PNG and handed them a JPEG number. That is `UT-F-46`
            // exactly — the defect this function's own doc says it exists to
            // prevent — and it survived because the sentence was written when
            // PNG was still the default and was not re-read when 4c7440d
            // changed it. No branch here may name a format it did not load.
            eprintln!(
                "freeze: ignoring UPTAKE_FREEZE_FORMAT={raw:?} — expected png, jpeg or bmp; \
                 staying on {}",
                display_format().2
            );
            return;
        }
    };
    DISPLAY_FORMAT.store(chosen, Ordering::SeqCst);
    eprintln!(
        "freeze: display stills encode as {} (UPTAKE_FREEZE_FORMAT) — the DISPLAY path only; \
         crops still come from the lossless bitmap",
        display_format().2
    );
}

/// Starts or stops the held sessions to match `is_placement`.
///
/// Called from the point every state transition funnels through, beside
/// [`thaw`] and for the same reason (and, since the third amendment, from two
/// more places — see `cursor` below): warm sessions exist only while Placement is
/// visible, so "start on entry" and "never held outside Placement" are one rule,
/// and writing it once means a state added later cannot forget it. Holding four
/// full-monitor sessions into Living would be this feature's version of the
/// undismissable-stills defect — an ongoing CPU and RAM cost for a state that
/// cannot freeze.
///
/// A no-op when the setting is off, and [`uptake_capture::warm::stop`] is safe
/// with nothing running, so the disabled path costs a bool read and the enabled
/// path cannot leak.
///
/// # `cursor` and the third caller
///
/// Since [ADR-0026]'s third amendment the held set is **the cursor's monitor**,
/// so this function's answer changes when the pointer crosses a monitor edge —
/// which is not a state transition and which neither of the original two callers
/// can see. `placement`'s active-monitor poll is the third, and without it the
/// narrowing would warm whichever monitor the cursor happened to be on when
/// Placement opened and leave the user's actual target cold. That is the shape
/// the amendment warns about in as many words: a change that provably does
/// nothing.
///
/// **That third caller no longer comes through this function.** It reaches
/// [`resync_warm_sessions`] instead, because it runs on a detached worker and so
/// is the one caller that cannot be trusted with an `is_placement` it was handed
/// earlier (`I-29`). The two callers left here — `overlay::apply` and
/// `overlay::sync_bounds` — both read the state and act on it in the same breath.
///
/// `None` means the cursor could not be read, which widens rather than narrows —
/// see [`monitors_in_scope`].
///
/// [ADR-0026]: the private planning repo's
/// `DECISIONS/ADR-0026-freeze-on-demand-trigger.md`
pub(crate) fn sync_warm_sessions(is_placement: bool, cursor: Option<Point>) {
    // Written **before** the gate, and before either branch runs, because this
    // is the fact and the sessions are the consequence. Two reasons it is not
    // inside the `if`. (1) With the gate off nothing is held, but the flag still
    // has to be true so that flipping the gate on mid-Placement — which 1.14's
    // settings UI makes an ordinary thing to do — does not leave a worker
    // reading `false` about a Placement that is plainly visible. (2) Storing it
    // ahead of `stop` is what makes the departure path safe: a worker that reads
    // `true` has read it before this store, so an `apply(false)` after that read
    // is still going to reach the `stop` below. See [`resync_guarded`], which is
    // the whole of the reasoning about that interleaving.
    PLACEMENT_VISIBLE.store(is_placement, Ordering::SeqCst);
    if !warm_capture_enabled() {
        return;
    }
    if is_placement {
        // The scope the *freeze* will ask for, because it is literally the same
        // value: `scope_for` is the only place the rule lives, and this converts
        // rather than decides. A session held for a monitor the freeze will not
        // capture is pure cost; a monitor the freeze captures without a session
        // is the cold path this feature exists to remove.
        let held = uptake_capture::warm::start(scope_for(cursor).for_warm());
        report_held(held, None);
    } else {
        uptake_capture::warm::stop();
    }
}

/// Whether Placement was visible as of the last state transition.
///
/// **This exists because a warm-session rebuild outlives the moment that asked
/// for it.** `placement::resync_warm_off_thread` runs the rebuild on a detached
/// worker — it has to, because a rebuild blocks for up to a second and the
/// caller is the poll thread that owns `quality-bars.md` §1's 8 ms drag row —
/// and until 2026-08-09 that worker passed a hard-coded `true` for
/// `is_placement`. The comment justifying it read *"this branch is already
/// inside the `placing` guard — reached only in Placement"*, which is true of
/// the **caller** and says nothing about the **worker**. Leave Placement between
/// a monitor crossing and the worker's next pass and [`uptake_capture::warm::start`]
/// ran after [`apply`][crate::overlay]'s [`uptake_capture::warm::stop`], leaving
/// sessions held over a hidden overlay until the next Placement exit — recorded
/// as `I-29`, found in the independent review of up-take #44.
///
/// So the worker reads the state rather than being told it, and the flag is an
/// `AtomicBool` rather than the `Mutex<OverlayState>` for one reason: the worker
/// has no `AppHandle`, and threading one onto a detached thread to answer a
/// yes/no question is a larger change to this path than the fix is.
///
/// **Not a substitute for the argument on the other callers.** `sync_warm_sessions`
/// keeps its parameter, because `apply` and `sync_bounds` both know the state
/// authoritatively at the moment they call and this static is downstream of
/// them. Only the worker, which knows it *later*, reads it back.
static PLACEMENT_VISIBLE: AtomicBool = AtomicBool::new(false);

/// Whether Placement is up, as the last transition left it. The only reader of
/// [`PLACEMENT_VISIBLE`].
pub(crate) fn placement_visible() -> bool {
    PLACEMENT_VISIBLE.load(Ordering::SeqCst)
}

/// What a deferred resync did.
///
/// **The rig operator's signal is the `eprintln!`, not this**, and saying
/// otherwise was the first version of this comment. What the return value buys is
/// the tests: [`resync_guarded`]'s three outcomes are asserted by name, which is
/// how the mutations that delete either guard are caught. Its one production
/// caller discards it, which `#[must_use]` makes a deliberate act rather than an
/// oversight.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resync {
    /// Placement had already gone before the rebuild started, so nothing was
    /// built. The ordinary outcome of a crossing that lost its race cheaply.
    Skipped,
    /// The rebuild ran and Placement is still up. Carries what `start` held.
    Held(usize),
    /// Placement went **while** the rebuild was blocked, so what it built was
    /// stopped again. The interleaving `I-29` describes.
    Undone,
}

/// The deferred half of [`sync_warm_sessions`], for a caller whose answer may
/// have gone stale since it was asked.
///
/// # Why two reads and not one
///
/// The obvious fix for `I-29` is a single check before the rebuild, and it does
/// not close the race — it narrows it. `start` holds `SESSIONS` across a spawn
/// loop that blocks on each pump's handshake, and a user can press `Esc` inside
/// that window. A check that runs before it cannot see a departure inside it.
///
/// ⚠️ **The "up to a second" figure this argument is usually quoted with is
/// ASSERTED, not measured.** It comes from ADR-0026's third amendment,
/// consequence 2, which states it without a measurement beside it, and
/// `quality-bars.md` §1 has no row for a rebuild at all. The direction is safe —
/// a shorter block only narrows a race this already handles either way — but do
/// not re-quote the number as though someone timed it. Raised by the independent
/// review of this change; `UT-F-47` is the project's row on unstated
/// preconditions like it.
///
/// So the state is read on **both** sides of the rebuild:
///
/// * `false` before — nothing is built, the cheap common case.
/// * `false` after — what was just built is stopped again, which is the only
///   outcome that costs a wasted rebuild and the only one that was previously a
///   leak.
/// * `true` after — the sessions are kept. Reading `true` means the read
///   happened before any `sync_warm_sessions(false, …)` stored its flag; that
///   function stores the flag **before** it calls `stop`, so any departure that
///   has not been seen here has not yet stopped anything and still will. No
///   interleaving ends with sessions held and no `stop` coming.
///
/// # The second read is still not enough on its own, and the rest is not here
///
/// **That reasoning covers one direction and the first version of this function
/// asserted it as if it covered both.** Deciding to stop and then stopping has a
/// gap of its own, and a re-entry inside that gap gets its correct, freshly built
/// sessions destroyed by a decision taken before they existed: Placement visible,
/// nothing held, nothing scheduled to rebuild, and the next freeze silently on
/// the cold path — `quality-bars.md` §1 measures that at 525–595 ms against
/// 269–279 ms warm. It is the same defect facing the other way, and it is worse
/// to notice, because no state is wrong, only absent.
///
/// So the undo is not a bare `stop`. It is [`uptake_capture::warm::stop_if`],
/// which re-evaluates the condition **while holding the lock that mutates the
/// session list**, so it and [`uptake_capture::warm::start`] cannot interleave:
/// a `start` that lands first makes the condition false and its sessions
/// survive; a `stop_if` that lands first empties the list and the `start`
/// rebuilds. Found by the independent review of this change, which is what an
/// independent review is for.
///
/// # Why a seam, and why the flag read is NOT a parameter
///
/// `start` and `stop_if` need a live WGC session and a desktop, so an inline
/// version is testable only on the rig — and the rig is where this project's
/// warm-path checks have historically not run (`I-22`). Taking them as closures
/// makes all three outcomes assertable off-desktop, including [`Resync::Undone`],
/// which is the one the fix is for and the one a hardware pass is least likely to
/// hit on purpose.
///
/// **The visibility read was a fourth parameter and is not any more.** The review
/// disconnected the whole fix by passing `|| true` at the single call site, with
/// all 233 tests still green: `I-17`'s exact shape, in a change whose own comment
/// claimed to have applied `I-17`'s rule to both ends. Reading
/// [`placement_visible`] here, and **constructing the `stop_if` condition here**,
/// puts both inside the region the tests drive, so a test moves the real flag and
/// watches this function respond rather than watching a fake stand in for it.
fn resync_guarded(
    start: impl FnOnce() -> usize,
    stop_unless_placement: impl FnOnce(fn() -> bool) -> bool,
) -> Resync {
    if !placement_visible() {
        return Resync::Skipped;
    }
    let held = start();
    if stop_unless_placement(|| !placement_visible()) {
        Resync::Undone
    } else {
        Resync::Held(held)
    }
}

/// Rebuilds the held sessions for `cursor`, **if Placement is still up**.
///
/// The entry point for `placement`'s detached resync worker, and the reason it
/// is a separate function from [`sync_warm_sessions`] rather than a `None`
/// argument to it: this one does not take `is_placement`, because taking it is
/// what `I-29` was. A caller that can be told the state is a caller that can be
/// told a stale one.
pub(crate) fn resync_warm_sessions(cursor: Option<Point>) -> Resync {
    if !warm_capture_enabled() {
        return Resync::Skipped;
    }
    let scope = scope_for(cursor);
    let outcome = resync_guarded(
        || uptake_capture::warm::start(scope.for_warm()),
        uptake_capture::warm::stop_if,
    );
    match outcome {
        // The scope is printed beside the count, and it is not decoration.
        // Placement can exit and re-enter while this worker is inside `start`,
        // and the rebuild it then reports as `Held` was built for the PREVIOUS
        // visit's cursor. The count alone cannot show that; the point can, because
        // a reader can compare it with the `placement: drag at (x, y)` lines
        // around it. `UT-F-56` is this project's row about a per-sample figure
        // that does not name its own conditions.
        Resync::Held(held) => report_held(held, Some(scope)),
        // Printed rather than left to be inferred, because this line is the
        // evidence that `I-29`'s window is real and is being closed. A rig pass
        // that never prints it has not exercised the fix.
        Resync::Undone => {
            #[cfg(debug_assertions)]
            eprintln!(
                "freeze: warm sessions rebuilt for a Placement that had gone — stopped again \
                 ({scope:?}; I-29, the rebuild outlived the crossing that asked for it)"
            );
        }
        Resync::Skipped => {}
    }
    outcome
}

/// The one line that reports the held set, so the two callers cannot describe it
/// differently.
///
/// Reports what is *warm*, not only what is held, because `start` keeps sessions
/// that already cover the scope — so a Placement → Placement transition prints
/// `1 warm` (`4 warm` under the widened setting) while a fresh entry prints
/// `0 warm` and stays that way for ~330 ms. A fixed "not warm yet" would have
/// been wrong on one of those two paths, and `I-11` is this project's row about a
/// probe whose output cannot distinguish the states it reports.
///
/// `unservable` is printed beside it because that is the whole reason the field
/// exists. `WarmStatus` documents it as the answer to "does `warm 3/4` mean one
/// session is still inside its ~330 ms warm-up, or is one display permanently on
/// the cold path for this visit", a question only a rig operator asks. Until
/// 2026-08-02 the count was computed, tested and never printed, so the one reader
/// it was built for could not see it: `I-11`'s shape again, one field over from
/// where it was fixed.
fn report_held(held: usize, scope: Option<Scope>) {
    #[cfg(debug_assertions)]
    {
        let status = uptake_capture::warm::status();
        // The scope is present on the deferred path and absent on the two
        // transition callers, which know it from the state they just applied.
        // Formatted as a suffix rather than a second line so a rig log stays one
        // line per event.
        let scope = scope.map_or_else(String::new, |scope| format!(" — {scope:?}"));
        eprintln!(
            "freeze: warm sessions held for {held} monitor(s) — {} warm, {} unservable{scope}",
            status.warm, status.unservable
        );
    }
    let _ = (held, scope);
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

/// One frozen still's encoded bytes **and the MIME type they were encoded as**,
/// if `version` is still the live freeze.
///
/// The content type comes from the still rather than from [`display_format`]:
/// see [`Still::content_type`] for why re-deriving it is a trap 1.14 walks into.
///
/// A version mismatch is `None` rather than the current bytes, for the same
/// reason the pin store refuses one: the only way to ask for a stale version is
/// to hold a stale URL, and answering it with fresh pixels would hide a caching
/// bug instead of surfacing it.
pub(crate) fn still_bytes(index: usize, version: u64) -> Option<(Vec<u8>, &'static str)> {
    // Version read *under* the stills lock, not beside it. `VERSION` and
    // `STILLS` are one fact in two variables, and reading them separately lets a
    // freeze land between the two reads — see `stills_for_display`, where the
    // same split had the worse consequence.
    let stills = stills();
    if version != VERSION.load(Ordering::SeqCst) {
        return None;
    }
    stills
        .get(index)
        .map(|still| (still.encoded.clone(), still.content_type))
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
/// previous freeze while the stills are the new one. [`still_bytes`] answers a
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

/// What one monitor's freeze cost, and how big the thing it produced was.
///
/// **The byte length is why this type exists**, and it is not a diagnostic
/// nicety. `quality-bars.md` §1's *frozen view painted* row is content-dependent
/// — encode and decode both scale with image complexity, in every format this
/// path can be set to — so a timing
/// without the content it was taken against is a number whose precondition
/// nobody stated. That is `UT-F-47`, where 1.9f's headline 72–78 ms turned out
/// to be the simple-screen best case quoted as *the* measurement, and the
/// 209–215 ms case had never been observed.
///
/// The encoded length is the property that correlates directly with encode
/// cost, the freeze path already computes it, and it was thrown away. Emitting
/// it beside every timing makes a mislabelled run **detectable rather than
/// trusted**: PLAIN and DENSE differ by orders of magnitude, so a run against an
/// unlisted screen lands between them and is visibly neither.
///
/// ⚠️ **How far apart they are depends on the format, and the margin is much
/// narrower than it was.** PNG spans a factor of **704** floor to ceiling, JPEG
/// — the default since [ADR-0027] — a factor of **57**; the six byte lengths
/// those come from are in `examples/testscreen/README.md` and are deliberately
/// not repeated here (`I-20`). The bracket still separates in both, but only PNG
/// leaves room for an unlisted screen to sit an order of magnitude clear of
/// *both* ends: 10 × 10 exceeds 57, so under JPEG no control can. Read the byte
/// length against the format the run actually used.
pub(crate) struct MonitorCost {
    /// The rect actually captured, as the capture crate reports it — never what
    /// was asked for, for the reason [`capture_still`] gives.
    pub rect: Rect,
    /// This monitor's capture.
    pub capture_ms: u128,
    /// This monitor's display encode, in whatever format [`display_format`]
    /// selected — **not** necessarily PNG since [ADR-0027].
    pub encode_ms: u128,
    /// The encoded image's length in bytes, in the display format — the
    /// `freeze: display stills encode as …` line at startup says which. See the
    /// type's own note: this is the run describing its own conditions, not a
    /// statistic.
    pub encoded_bytes: usize,
    /// Whether a **warm** session served this monitor rather than a fresh
    /// capture.
    pub served_warm: bool,
}

/// What a freeze cost, for the caller to log.
///
/// The stage figures are **maxima across monitors, not sums**, and they may come
/// from different monitors. Every monitor is captured on its own thread, so what
/// the user waits for is the last one to land; a sum would report a cost nobody
/// experiences. They exist to answer one question — which stage dominates — and
/// are reported rather than asserted: the measured split is what decides whether
/// this feature's latency has anywhere left to go.
///
/// [`FreezeReport::per_monitor`] carries the unaggregated figures, because a
/// maximum cannot describe the screen it was taken against and §1's row needs
/// exactly that. See [`MonitorCost`].
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
    /// The slowest single monitor's display encode — see [`MonitorCost`] for
    /// why the format is not assumed.
    pub slowest_encode_ms: u128,
    /// Every captured monitor's own figures, in the order they were captured —
    /// which is the order of the monitor list, and therefore of the still URLs.
    ///
    /// Monitors that could not be captured or encoded are **absent**, not zeroed,
    /// so this is `count` entries long and never `monitors.len()`.
    pub per_monitor: Vec<MonitorCost>,
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
/// on the dev rig plus the display encode — so it must not run on the event-loop
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
    let captured: Vec<(Still, MonitorCost)> = std::thread::scope(|scope| {
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
    let slowest_capture_ms = captured.iter().map(|(_, cost)| cost.capture_ms).max();
    let slowest_encode_ms = captured.iter().map(|(_, cost)| cost.encode_ms).max();
    let warm_served = captured.iter().filter(|(_, cost)| cost.served_warm).count();
    let (captured, per_monitor): (Vec<Still>, Vec<MonitorCost>) = captured.into_iter().unzip();
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
        per_monitor,
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
fn capture_still(monitor: Rect) -> Option<(Still, MonitorCost)> {
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
    // Read once, here, and carried on the still — so the bytes and the header
    // that describes them are decided in the same breath. See
    // [`Still::content_type`].
    let content_type = display_format().1;
    let encoded = match crate::output::encode_for_display(&shot.bitmap) {
        Ok(encoded) => encoded,
        Err(error) => {
            eprintln!("freeze: could not encode {monitor:?}: {error}");
            return None;
        }
    };
    let cost = MonitorCost {
        // What the capture crate reports it took, never what was asked for: it
        // clamps to the virtual desktop, and trusting the request would offset
        // every crop by the clamp distance.
        rect: shot.rect,
        capture_ms,
        encode_ms: encode_started.elapsed().as_millis(),
        encoded_bytes: encoded.len(),
        served_warm,
    };
    Some((
        Still {
            rect: shot.rect,
            bitmap: shot.bitmap,
            encoded,
            content_type,
        },
        cost,
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
                // neither of which reads the encoded bytes. A real encode here
                // would buy nothing and make every test depend on the encoder.
                //
                // **It is also load-bearing that this stays empty.** `crop` must
                // cut from `bitmap` and never from `encoded` — the one fact that
                // makes a lossy display format safe (ADR-0027) — and with no
                // bytes here, a `crop` rewritten to decode `encoded` cannot
                // produce pixels and every crop test below goes red. See
                // `the_crop_path_never_reads_the_encoded_bytes` for the check
                // that says so by name rather than by side effect.
                encoded: Vec::new(),
                content_type: "image/png",
            })
            .collect();
    }

    /// **The single fact [ADR-0027] rests on, asserted by name.**
    ///
    /// A lossy display format is defensible for exactly one reason: [`crop`]
    /// cuts what the user actually receives from [`Still::bitmap`], the lossless
    /// RGBA, and never from the encoded bytes the WebView paints. ADR-0027's
    /// first *Revisit if* is that fact ceasing to hold, at which point the
    /// decision silently becomes a defect that degrades every screenshot taken
    /// on a frozen screen.
    ///
    /// Until now that invariant was carried by doc comments plus a side effect —
    /// the test helper leaves `encoded` empty, so a `crop` that decoded it would
    /// fail the *other* tests for a reason none of them names. A reader chasing
    /// a red test would have had no way to learn what they had broken. This
    /// installs bytes that are **not** a decodable image and asserts the crop is
    /// still exact, so the failure names itself.
    ///
    /// What makes it fail: change `crop` to decode `still.encoded` instead of
    /// cutting `still.bitmap`. It cannot return these pixels from this input.
    ///
    /// [ADR-0027]: the private planning repo's
    /// `DECISIONS/ADR-0027-jpeg-for-the-freeze-display-path.md`
    #[test]
    fn the_crop_path_never_reads_the_encoded_bytes() {
        let _guard = crate::precapture::frame_store_guard();
        let bitmap = patterned(Size::new(64, 48));
        let expected = bitmap
            .crop_screen(
                uptake_core::geometry::Point::new(0, 0),
                Rect::new(8, 8, 16, 16),
            )
            .expect("the crop lies inside the still");
        *stills() = vec![Still {
            rect: Rect::new(0, 0, 64, 48),
            bitmap,
            // Not an image in any format. If the crop ever comes from here, it
            // cannot come out right — which is the point of choosing bytes that
            // no decoder accepts rather than a real encode of the same pixels.
            // A real encode would let a decoding `crop` return *nearly* these
            // pixels, and "nearly" is exactly the degradation this guards.
            encoded: b"not an image".to_vec(),
            content_type: "image/jpeg",
        }];
        let cropped = crop(Rect::new(8, 8, 16, 16)).expect("a contained crop returns pixels");
        assert_eq!(
            cropped, expected,
            "the crop must be a byte-exact cut of the lossless bitmap. If this \
             fails, ADR-0027's premise no longer holds and a lossy display \
             format is no longer safe — read that decision before changing \
             this test."
        );
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
        assert!(still_bytes(0, live.wrapping_sub(1)).is_none());
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
        assert!(still_bytes(index, version).is_some());
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
            still_bytes(index, version).is_none(),
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

    /// The dev rig's shape — see `warm`'s copy of this, and the same reason for
    /// real numbers: the dead zone only exists between monitors of unequal
    /// height.
    fn rig() -> Vec<Rect> {
        vec![
            Rect::new(0, 0, 2560, 1440),
            Rect::new(2560, 0, 1920, 1080),
            Rect::new(4480, 0, 1920, 1080),
            Rect::new(-1080, 0, 1080, 1920),
        ]
    }

    /// Sets the scope for one test and puts it back afterwards.
    ///
    /// `FREEZE_ALL_MONITORS` is process-global and libtest runs tests
    /// concurrently, which is `F-33` exactly: a test that mutates process-global
    /// state silently disabled another test, and the disabled test still passed.
    /// The lock is what makes these serial with each other.
    fn with_all_monitors<T>(all: bool, body: impl FnOnce() -> T) -> T {
        static SERIAL: Mutex<()> = Mutex::new(());
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let restore = FREEZE_ALL_MONITORS.swap(all, Ordering::SeqCst);
        let out = body();
        FREEZE_ALL_MONITORS.store(restore, Ordering::SeqCst);
        out
    }

    #[test]
    fn the_default_scope_is_the_cursors_monitor() {
        with_all_monitors(false, || {
            assert_eq!(
                monitors_in_scope(&rig(), scope_for(Some(Point::new(3000, 500)))),
                vec![Rect::new(2560, 0, 1920, 1080)]
            );
        });
    }

    #[test]
    fn the_setting_widens_rather_than_narrows() {
        // The direction is the decision (ADR-0026's third amendment), so it is
        // asserted rather than left to the reader of the `if`: with the setting
        // on, the same cursor that selects one monitor above selects all four.
        with_all_monitors(true, || {
            assert_eq!(
                monitors_in_scope(&rig(), scope_for(Some(Point::new(3000, 500)))),
                rig()
            );
        });
    }

    #[test]
    fn a_freeze_never_narrows_to_nothing() {
        // Both ways of having no monitor under the cursor, because they arrive
        // from different places and a freeze over an empty list publishes no
        // stills — a `Ctrl+Space` that does nothing, indistinguishable from the
        // hotkey never arriving.
        with_all_monitors(false, || {
            let dead = Point::new(3000, 1300);
            assert!(
                !rig().iter().any(|bounds| bounds.contains(dead)),
                "the fixture stopped having a dead zone, so this no longer tests it"
            );
            assert_eq!(monitors_in_scope(&rig(), scope_for(Some(dead))), rig());
            assert_eq!(monitors_in_scope(&rig(), scope_for(None)), rig());
        });
    }

    #[test]
    fn both_gates_are_off_until_a_rig_says_otherwise() {
        // ADR-0026's third amendment decided the warm path BECOMES the default
        // and held that flip behind a measurement whose numbers were written
        // down in advance: the still row at or under +0.25 pp and the video row
        // under +0.40 pp, taken with the same instrument and the same two
        // conditions that produced +0.62 / +0.94 pp.
        //
        // Both gates are one-word flips and nothing else asserts them, so
        // without this test a commit changing `new(false)` to `new(true)` on
        // either line passes the whole suite green. That is exactly the "rule an
        // agent has to remember" shape, and the amendment's own reason for
        // refusing the flip is that restoring a default on an arithmetic model
        // is `F-39` and `UT-F-53` in one step.
        //
        // Deleting this test is the honest way to ship the flip. Editing the
        // measurement into it is not.
        assert!(
            !warm_capture_enabled(),
            "the warm path is the default before the rig measured the narrowed              cost -- see ADR-0026's third amendment release condition"
        );
        assert!(
            !freeze_all_monitors_enabled(),
            "a freeze covers every monitor by default, which is ADR-0014 section              4 un-inverted -- the amendment made the setting WIDEN"
        );
    }

    // `I-29` — the deferred resync.
    //
    // `start` and `stop_if` need a live WGC session, so these drive
    // `resync_guarded` with fakes for those two. **The visibility flag is the
    // real one**, deliberately: it was a fourth closure parameter until the
    // independent review passed `|| true` for it at the single call site and
    // watched all 233 tests stay green. A fake there tests the fake.
    //
    // These therefore mutate a process-global and must be serial with each other.

    /// The lock is what makes the flag tests serial. Same shape as
    /// [`with_all_monitors`], and separate from it because the two globals are
    /// independent and pairing them would serialise tests that need not be.
    fn with_placement<T>(visible: bool, body: impl FnOnce() -> T) -> T {
        static SERIAL: Mutex<()> = Mutex::new(());
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let restore = PLACEMENT_VISIBLE.swap(visible, Ordering::SeqCst);
        let out = body();
        PLACEMENT_VISIBLE.store(restore, Ordering::SeqCst);
        out
    }

    #[test]
    fn a_resync_whose_placement_has_already_gone_builds_nothing() {
        with_placement(false, || {
            let started = std::cell::Cell::new(false);
            let outcome = resync_guarded(
                || {
                    started.set(true);
                    1
                },
                |_| panic!("stopped something that was never started"),
            );
            assert_eq!(outcome, Resync::Skipped);
            assert!(
                !started.get(),
                "the pre-check is gone: a crossing that lost its race still paid for a rebuild"
            );
        });
    }

    #[test]
    fn a_resync_inside_placement_keeps_what_it_built() {
        // The positive control. Without it every case here asserts a refusal, and
        // a guard that refused everything would pass the rest of this section --
        // which is `OS-F103`'s shape and the reason this test is not redundant.
        with_placement(true, || {
            let outcome = resync_guarded(
                || 3,
                |should_stop| {
                    assert!(
                        !should_stop(),
                        "the undo condition is true while Placement is visible, so a correct \
                         `stop_if` would tear down the sessions this resync just built"
                    );
                    false
                },
            );
            assert_eq!(outcome, Resync::Held(3));
        });
    }

    #[test]
    fn a_departure_during_the_rebuild_stops_what_the_rebuild_started() {
        // THE `I-29` CASE, and the one no pre-check alone can catch: Placement is
        // up when the worker looks and gone by the time `start` returns. The
        // departure is simulated by moving the REAL flag inside the fake `stop_if`,
        // which is where `start`'s blocking window sits in production.
        with_placement(true, || {
            let outcome = resync_guarded(
                || 1,
                |should_stop| {
                    PLACEMENT_VISIBLE.store(false, Ordering::SeqCst);
                    let stop = should_stop();
                    assert!(
                        stop,
                        "the undo condition is false after Placement has gone, so the sessions \
                         stay held over a hidden overlay -- `I-29` exactly"
                    );
                    stop
                },
            );
            assert_eq!(outcome, Resync::Undone);
        });
    }

    #[test]
    fn the_undo_asks_a_question_rather_than_carrying_an_answer() {
        // The review's finding, pinned: deciding to stop and THEN stopping leaves
        // a gap in which a re-entry's correct sessions are destroyed by a decision
        // taken before they existed. `resync_guarded` must therefore hand the
        // condition down unevaluated, so `warm::stop_if` can run it under the lock
        // that mutates the session list.
        //
        // This asserts the shape rather than the atomicity: the atomicity lives in
        // `uptake_capture::warm::stop_if` and is tested there. What can be caught
        // here is a refactor that evaluates the condition early and passes a bool.
        with_placement(true, || {
            let outcome = resync_guarded(
                || 1,
                |should_stop| {
                    // Placement is visible at entry, so the condition is false...
                    assert!(!should_stop());
                    // ...and must be re-derivable, not a value captured above.
                    PLACEMENT_VISIBLE.store(false, Ordering::SeqCst);
                    assert!(
                        should_stop(),
                        "the condition was evaluated once and frozen, so `stop_if` cannot \
                         re-check it under the session lock and the re-entry race stays open"
                    );
                    false
                },
            );
            assert_eq!(outcome, Resync::Held(1));
        });
    }

    #[test]
    fn the_state_the_worker_reads_is_written_even_when_the_gate_is_off() {
        // The gate being off must not stop the flag tracking Placement: 1.14
        // makes the warm setting flippable while Placement is up, and a worker
        // that then read a flag last written under a different gate would answer
        // about the wrong moment. Asserted here because `sync_warm_sessions`
        // returns early on that gate one line later, so the store is one `if`
        // away from being unreachable at any time.
        assert!(
            !warm_capture_enabled(),
            "the warm gate is on in the test process, so this test would call into WGC -- \
             see `both_gates_are_off_until_a_rig_says_otherwise`"
        );
        with_placement(false, || {
            sync_warm_sessions(true, None);
            assert!(placement_visible());
            sync_warm_sessions(false, None);
            assert!(!placement_visible());
        });
    }
}
