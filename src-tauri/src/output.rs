//! The output pipeline (roadmap task 1.9): `capture_region` → PNG → clipboard,
//! and a separate, explicit save-to-file action.
//!
//! Every decision here was settled before this module was written — see
//! `PRODUCT-VISION.md` §8 in the private planning repo — so nothing below is a
//! product choice: **clipboard only by default**, publishing both `CF_DIBV5`
//! and the registered `"PNG"` format (Office and most desktop apps read DIB;
//! browsers, Discord and Figma prefer PNG); saving is a **separate, explicit**
//! action that writes to `Pictures\UP-TAKE\`, never invoked implicitly by a
//! copy.
//!
//! # Why this never runs on the mouse-hook thread
//!
//! Both actions are reached from the area menu (`placement.rs`), whose
//! release handling normally runs synchronously inside the `WH_MOUSE_LL`
//! callback on the event-loop thread. A capture is ~100–300 ms even warm
//! (`uptake_capture` crate docs, F-29) — far past what a low-level hook
//! callback can spend before Windows silently removes it
//! (`LowLevelHooksTimeout`, the same failure class as F-33). [`copy_to_clipboard`]
//! and [`save_to_file`] are therefore designed to be called from a **freshly
//! spawned thread**, never from the hook callback itself; `placement.rs` owns
//! that spawn. Any thread may call `uptake_capture::capture_region` (its own
//! contract), and the clipboard is opened against the overlay's `HWND` — a
//! plain field read on tao's side, safe from any thread — so nothing below is
//! thread-affine.

use std::fs;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use tauri::{AppHandle, Manager};
use uptake_core::area::AreaId;
use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::{Point, Rect};

use windows_capture::encoder::{ImageEncoder, ImageEncoderPixelFormat, ImageFormat};
use windows_sys::Win32::Foundation::{GlobalFree, HWND, SYSTEMTIME};
use windows_sys::Win32::Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER, LCS_GM_IMAGES};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

use crate::captures::CaptureStore;

/// The value Windows defines for `LCS_sRGB` (`wingdi.h`) — not exported by
/// `windows-sys`'s `Gdi` module under any name (only the `LCS_GM_*` rendering
/// intents are), so pinned here as the documented, ABI-stable FourCC it is.
const LCS_S_RGB: u32 = 0x7352_4742;

/// §1's selection→clipboard budget: a soft target and a hard fail (ms).
const BUDGET_TARGET_MS: u128 = 300;
const BUDGET_HARD_FAIL_MS: u128 = 600;

/// Environment variable that turns [`report`]'s per-action line on in a release
/// build. See [`init_report_verbosity`].
const REPORT_VAR: &str = "UPTAKE_DEV_REPORT";

/// Stands in for a [`REPORT_VAR`] value that is set but is not valid Unicode.
///
/// A sentinel rather than `""`, because the armed line quotes the value it read
/// and `""` would announce a value the variable does not hold. It is set, so it
/// counts as on like any other value; what a reader needs is that it could not be
/// shown, not a plausible-looking empty string.
const NOT_UNICODE: &str = "<set, but not valid Unicode>";

/// Whether every action prints a timing line, or only the ones over budget.
///
/// Defaults to the build kind, so a debug build behaves exactly as it did
/// before this switch existed and [`REPORT_VAR`] can only ever turn the line
/// **on**. There is no way to make a release build quieter than its default,
/// which is deliberate: the failure this switch exists for is a log that says
/// less than its reader thinks, so a route to less is a route to that failure.
static REPORT_EVERY_ACTION: AtomicBool =
    AtomicBool::new(report_line_default(cfg!(debug_assertions)));

/// The starting value of [`REPORT_EVERY_ACTION`], as a function of the build
/// kind rather than as a `cfg!` read inline.
///
/// **It is split out to be assertable from either build.** Written inline, the
/// only test available is `flag == cfg!(debug_assertions)`, which is `true ==
/// true` in a debug build and passes whatever the code says. That is `I-22`'s
/// shape and the 2026-08-05 review's finding against this repository's own
/// suite: a test pointed away from the thing it is named for.
///
/// ⚠️ **This function is checkable in either build. The STATIC BELOW IS NOT, and
/// an earlier revision of this comment claimed otherwise.** It said *"both
/// directions are checkable in the build CI runs"*, which is true of
/// `report_line_default` and false of [`REPORT_EVERY_ACTION`], the value
/// [`report`] actually reads. Replacing the initializer with a bare
/// `AtomicBool::new(true)`, a release build that starts loud and ships a timing
/// line on every grab, which is the one outcome `I-42`'s constraint column
/// forbids by name, leaves this function correct and every debug assertion
/// green. No test in a debug build can distinguish *true because debug* from
/// *true always*.
///
/// **What closes it is the release test job, not a cleverer assertion.** CI runs
/// `cargo test --release --all-features` for this reason (`.github/workflows/ci.yml`),
/// added by the review that found this: the guarantee is about a release build,
/// so the check has to run in one. Found by mutation, not by reading.
const fn report_line_default(debug_build: bool) -> bool {
    debug_build
}

/// The pixels an export should use for `area`: its pinned capture if it has one,
/// otherwise a fresh capture of `bounds`.
///
/// # A pinned area exports what it is showing, not what is under it
///
/// **This is the fix for a defect found on the rig 2026-07-27.** Both export
/// actions used to capture live, unconditionally. While areas could not move that
/// was indistinguishable from correct — a passive `Screenshot` pins the instant it
/// is created, so its rectangle still held the same pixels. Task 1.17(a) added
/// move and resize in LIVING, and from then on Copy on a moved Screenshot area
/// returned **the desktop underneath its new position**, cropped to the area's
/// size: a screenshot of whatever the area happened to be sitting on.
///
/// The failure is quiet, which is what makes it bad. The result is a plausible
/// image of the right dimensions, so nothing looks broken until you look at what
/// you pasted.
///
/// A `Default` area has no capture and falls through to the live path, which is
/// the only thing Copy can mean for a bare claimed rectangle.
fn export_source(
    app: &AppHandle,
    area: AreaId,
    bounds: Rect,
    split: &mut Split,
) -> Result<(RgbaBitmap, Vec<u8>), String> {
    if let Some(pinned) = crate::captures::pinned_capture(app, area) {
        // Capture and encode both stay at 0 ms, which is the honest reading: this
        // path does neither. It also keeps the §1 budget lines meaningful — a
        // pinned export is not a measurement of the capture pipeline.
        // **`bounds` is deliberately NOT recorded here, and that is the whole
        // point of the field.** `bounds` is the area's rectangle *now*; `pinned`
        // is the capture taken when the area was created, and 1.17(a) made areas
        // movable and resizable afterwards (see this function's own docs above).
        // So on a moved or resized area the two describe different pixels, and a
        // line naming one beside the byte length of the other is the
        // misattribution `UT-F-56` exists to end, reintroduced by its own fix.
        //
        // The stored capture's *size* is recoverable from `pinned.0`; its origin
        // is not stored at all, so there is no rectangle to print. `stage_line`
        // renders `None` by omitting the field rather than inventing one.
        split.source = Source::Pinned;
        split.encoded_bytes = pinned.1.len();
        return Ok(pinned);
    }
    capture(bounds, split)
}

/// Publishes an area's image to the clipboard alone.
///
/// PRODUCT-VISION §8: an area's source is still on screen, so re-copying is
/// one gesture — the justification ShareX/Snipping Tool have for also
/// writing a file (a screenshot is transient) does not transfer here, and the
/// cost (an area copied forty times is forty stray PNGs) stays. Nothing is
/// written to disk by this path.
pub(crate) fn copy_to_clipboard(app: &AppHandle, area: AreaId, bounds: Rect) {
    let started = Instant::now();
    let mut split = Split::default();
    let outcome = export_source(app, area, bounds, &mut split).and_then(|(bitmap, png)| {
        // Both buffers are built *before* the clipboard is opened. The
        // clipboard is a global system resource — every other process's
        // access blocks while it is held — so no per-pixel work belongs
        // inside the bracket, and a 4K-sized area is a 33 MB conversion.
        let publish = Instant::now();
        let result =
            dibv5_bytes(&bitmap).and_then(|dib| publish_clipboard(overlay_hwnd(app)?, &dib, &png));
        split.publish_ms = publish.elapsed().as_millis();
        result
    });
    // Acknowledge the action on screen. Only on success: a flash after a
    // failed Copy would be a lie the user has no way to check.
    if outcome.is_ok() {
        crate::overlay::emit_flash(app, area);
    }
    report("copy", started, &split, outcome);
}

/// Copies the whole monitor under the cursor to the clipboard, with no placement
/// gesture at all (roadmap task **1.9e**, [ADR-0014] section 4).
///
/// # What this deliberately does not do
///
/// It does not summon, show, or change the overlay's state. The feature is
/// defined as the path with no gesture, so bringing the overlay up first would
/// reintroduce the thing it exists to remove. It follows that the grab works
/// from the tray, with nothing on screen, which is the normal case rather than
/// an edge one.
///
/// It writes no file. `PRODUCT-VISION` section 8 is clipboard only by default,
/// and Save is a separate explicit action; a grab that also littered
/// `Pictures\UP-TAKE\` would be that decision reversed by a new entry point.
///
/// # It takes the COLD capture path, always, and that is a decision
///
/// [`crate::freeze`]'s warm sessions are held **only while Placement is
/// visible**, which is exactly the state this feature does not require, so for
/// the usage 1.9e is defined by there is normally no warm session to hand over.
/// (This said *intended* to be held, which was accurate for one day and is not
/// now — see below. A skim that stops at this paragraph should not come away
/// with the superseded reading.)
///
/// ✅ **That invariant was intended and NOT enforced when this was written, and
/// it is enforced now** — `I-29`, found by this branch's own independent review
/// and fixed 2026-08-09, before the warm default flips (founder-sequenced).
/// `placement::resync_warm_off_thread` spawns a detached worker, and it used to
/// call `freeze::sync_warm_sessions(true, …)` with `is_placement` hard-coded, so
/// leaving Placement between a monitor crossing and that worker's next pass ran
/// `warm::start` *after* `apply`'s `warm::stop` and left sessions held with the
/// overlay hidden. The worker now reads the state on both sides of the rebuild
/// and stops what it built if Placement went while it was blocked
/// (`freeze::resync_guarded`).
/// **This function did not depend on the invariant either way** — it never reads
/// a warm frame on any path — so the race cost held sessions rather than wrong
/// pixels. The reasoning below is why it does not read one, and it stands
/// unchanged.
///
/// The grab could still use a warm session on the rarer in-Placement press, and
/// does not, for two reasons:
///
/// 1. **One behaviour, one number.** A path that is fast in one state and slow
///    in another produces rig figures that cannot be read without knowing which
///    state produced them, and `UT-F-47` is this project's record of exactly
///    that being got wrong.
/// 2. **A warm frame has no freshness bound here.** [`crate::freeze`] can go
///    without one because the user selects *on the still they are looking at*;
///    a grab hands back pixels the user never saw, which is
///    [`crate::precapture`]'s situation, and that path is bounded at 750 ms by
///    [ADR-0022] section 3 for this reason. Reaching for the warm frame without
///    settling that bound would be the same hazard with no rule attached.
///
/// **The honest consequence, stated rather than discovered on the rig:** this
/// inherits `UT-F-45` whole. The pixels are the desktop roughly 350 ms after the
/// key, because a WGC device, item, pool and session are all built before the
/// compositor is asked for a frame. For a grab of a page you are reading that is
/// a delay; for a grab of something disappearing it is the wrong moment. Making
/// it instant needs a warm session held **outside** Placement, which is a
/// standing cost in every state and is a decision this function is not entitled
/// to take.
///
/// [ADR-0014]: the private planning repo's
/// `DECISIONS/ADR-0014-capture-and-render-over-live-content.md`
/// [ADR-0022]: the private planning repo's
/// `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`
pub(crate) fn grab_monitor(app: &AppHandle) {
    // Read here, on the caller's thread, before the spawn. Same reason
    // `overlay::toggle_freeze` reads it before spawning: this is the cursor at
    // the moment the key was pressed, and the worker starts late enough that the
    // pointer can already have crossed onto another monitor. Reading it inside
    // the thread would grab whichever screen the mouse drifted to.
    let cursor = crate::placement::real_cursor(app);
    let app = app.clone();
    // Spawned for the reason the module docs give: a capture is 100-300 ms even
    // warm, and this handler runs on the event-loop thread, which is also the
    // thread the mouse hook's callback runs on.
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut split = Split::default();
        let outcome = grab_target(cursor).and_then(|monitor| {
            let (bitmap, png) = capture(monitor, &mut split)?;
            let publish = Instant::now();
            let result = dibv5_bytes(&bitmap)
                .and_then(|dib| publish_clipboard(overlay_hwnd(&app)?, &dib, &png));
            split.publish_ms = publish.elapsed().as_millis();
            result
        });
        report("grab", started, &split, outcome);
    });
}

/// The monitor a grab should capture: the one containing `cursor`.
///
/// # Why a failure here is an error and not a fallback
///
/// [`crate::freeze::monitors_in_scope`] answers the same question for a freeze
/// and falls back to **every** monitor when the cursor is unreadable or lands in
/// a dead zone, because a freeze can cover several screens and doing nothing
/// would be indistinguishable from the key not arriving.
///
/// A grab cannot borrow that. There is exactly one clipboard, so "every monitor"
/// has no meaning here, and the natural substitutes (the primary display, the
/// first enumerated one) would hand back a screenshot of a monitor the user was
/// not pointing at. **That is this project's see-one-thing-get-another failure**,
/// which `F-38` and the moved-area export defect above are both instances of,
/// and it is quiet: the result is a plausible image of the right size.
///
/// So the grab declines and says why. The cursor being on no monitor at all is
/// close to unreachable in practice, since Windows keeps the pointer on a
/// display, and the remaining case is a cursor read that failed.
///
/// # It asks the CAPTURE crate which monitors exist, not the window manager
///
/// `I-31`. This used to call `overlay::fresh_monitor_rects`, which reads tao's
/// list, while [`uptake_capture::capture_region`] clamps the region it is given
/// against the capture crate's own `EnumDisplayMonitors`. Two enumerations, one
/// decision: whenever they disagreed the grab would pick a rectangle from one
/// list and receive pixels clamped to the other, which is a screenshot of
/// something nobody asked for and is not detectable from the image.
///
/// It is also the cheaper call. `WebviewWindow::available_monitors()` resolves
/// in **`tauri-runtime-wry` 2.11.4** to `window_getter!` → `send_user_message`
/// (`src/lib.rs:197-211`, `:2089`), which posts to the event-loop proxy and
/// blocks on `rx.recv()` with no timeout from any thread that is not the
/// event-loop thread — exactly what this worker is.
/// [`uptake_capture::enumerate_monitors`] is a direct `EnumDisplayMonitors` on
/// the calling thread. **Not tao's**, which has no such indirection; that
/// function's own docs record why the distinction is spelled out.
///
/// **No fallback if the enumeration fails, deliberately.** A `CaptureError` here
/// means `EnumDisplayMonitors` failed, and the reasoning above applies with more
/// force to a guess made without a monitor list than to one made with a
/// disagreeing one.
fn grab_target(cursor: Option<Point>) -> Result<Rect, String> {
    let monitors = uptake_capture::enumerate_monitors().map_err(|error| error.to_string())?;
    monitor_at(&monitors, cursor)
}

/// The rule [`grab_target`] applies, with the enumeration taken as an argument.
///
/// Split out because the enumeration is a Win32 call, so leaving this inside
/// [`grab_target`] would have made the only reachable test one that asserts a
/// rectangle contains its own centre — backlog `I-1`'s sweep defect and
/// `UT-F-44`'s tautology in one: true for any input, red for none.
///
/// **The containment scan is [`uptake_core::geometry::index_at`]'s and no longer
/// this function's** (`I-30`). What is left here is the *policy*, which is
/// genuinely the grab's: a cursor that could not be read and a cursor in a dead
/// zone are both refusals, where a freeze widens to every monitor for the second.
/// `SPECS/quality-bars.md` §2 puts the property-test goal on `uptake-core`, so a
/// copy of the rule living in `src-tauri` was coverage that looked equivalent and
/// was not.
fn monitor_at(
    monitors: &[uptake_capture::MonitorInfo],
    cursor: Option<Point>,
) -> Result<Rect, String> {
    let cursor = cursor.ok_or_else(|| "the cursor position could not be read".to_string())?;
    uptake_core::geometry::index_at(monitors.iter().map(|monitor| monitor.bounds), cursor)
        .map(|index| monitors[index].bounds)
        .ok_or_else(|| {
            format!(
                "the cursor at ({}, {}) is on none of the {} monitor(s) enumerated",
                cursor.x,
                cursor.y,
                monitors.len()
            )
        })
}

/// Writes an area's image to `Pictures\UP-TAKE\`, creating the directory on first
/// use. A separate, explicit action (PRODUCT-VISION §8) — does not also touch the
/// clipboard.
pub(crate) fn save_to_file(app: &AppHandle, area: AreaId, bounds: Rect) {
    let started = Instant::now();
    let mut split = Split::default();
    let outcome = export_source(app, area, bounds, &mut split).and_then(|(_bitmap, png)| {
        let publish = Instant::now();
        let result = write_file(app, &png);
        split.publish_ms = publish.elapsed().as_millis();
        result
    });
    if outcome.is_ok() {
        crate::overlay::emit_flash(app, area);
    }
    report("save", started, &split, outcome);
}

/// Where the selection→clipboard time actually goes, per stage (ms).
///
/// **Owed to task 1.9c by [ADR-0022].** Task 1.9's rig pass timed only the
/// total, and the ADR's premise — that capture is ~65–70 % of it — was derived
/// by *subtracting* the Copy and Save figures from each other rather than
/// measured. 1.9c is a capture-side fix and would be worthless if the encode or
/// the clipboard publish turned out to dominate instead, so the split is
/// recorded rather than inferred.
///
/// [ADR-0022]: the private planning repo's
/// `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`
#[derive(Default)]
struct Split {
    /// `uptake_capture::capture_region` — the stage 1.9c removes from the
    /// measured interval.
    capture_ms: u128,
    /// PNG encode.
    encode_ms: u128,
    /// Whatever the action does with the bytes: the DIB conversion plus the
    /// clipboard bracket for Copy, the file write for Save.
    publish_ms: u128,
    /// The rectangle the pixels came from, and the size of what was encoded.
    ///
    /// # Both exist because a line without them cost a rig pass
    ///
    /// `UT-F-56`, 2026-08-08. Task 1.9e's first hardware run produced five grabs
    /// at 372, 516, 466, 338 and 340 ms across at least three different screens,
    /// and **nothing in the log said which was which**, so §1's row for the
    /// gesture could not be filled in and the pass has to be repeated. The
    /// freeze's own per-monitor line has printed both fields since 1.9g and is
    /// readable for exactly that reason.
    ///
    /// **The bar these feed is content-dependent**, which is what makes this
    /// mandatory rather than nice: `quality-bars.md` §1 footnote 3 requires a run
    /// to state the screen it ran against, and the encoded byte length is how a
    /// run states it without trusting the operator's memory. `UT-F-47` is the
    /// finding that rule comes from, and this field is that finding's second
    /// instance being closed rather than recorded again.
    ///
    /// `bounds` is `None` only where nothing was captured, which is a failure
    /// before the rectangle is known.
    bounds: Option<Rect>,
    /// Length of the encoded image, in bytes. Zero when nothing was encoded.
    encoded_bytes: usize,
    /// Where the pixels came from. Reported on every line, because
    /// `capture 0 ms` is ambiguous on its own — it is what both the fast path
    /// and a pinned export produce — and "which path ran" is the first question
    /// asked of any 1.9c latency number.
    source: Source,
}

/// Which of the routes to a Screenshot's pixels actually ran.
#[derive(Default, Clone, Copy)]
enum Source {
    /// A live `capture_region` with no fast path attempted: every action that is
    /// not a create.
    ///
    /// **This used to say "the pinned export" too, and that was wrong** — a
    /// pinned export captures nothing, so reporting it as a live capture named
    /// the one path that does no work as the path that does the most. Harmless
    /// while no rectangle was printed beside it and actively misleading once one
    /// was (`UT-F-56`'s fix), which is what surfaced it. See [`Self::Pinned`].
    #[default]
    Live,
    /// The area's stored capture, taken when it was created and re-encoded by
    /// nothing. Capture and encode are both 0 ms because this path performs
    /// neither, and **no rectangle is reported**: the area may have moved or
    /// been resized since, so its current bounds do not describe these pixels.
    Pinned,
    /// Cropped out of the frame held since mouse-down (task 1.9c).
    Held,
    /// Cropped out of the frozen still the user was looking at (task 1.9d).
    ///
    /// Distinct from [`Self::Held`] in the log even though both are crops of a
    /// full-monitor frame, because they answer different questions: `Held` may
    /// legitimately decline on staleness and `Frozen` never can, so a run where
    /// the two are conflated cannot be read.
    Frozen,
    /// The fast path was attempted and declined; the reason is why.
    Fell(crate::precapture::Fallback),
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live => write!(f, "live capture"),
            Self::Pinned => write!(f, "pinned capture, not re-taken"),
            Self::Held => write!(f, "held frame"),
            Self::Frozen => write!(f, "frozen still"),
            Self::Fell(reason) => write!(f, "live capture — fell back: {reason}"),
        }
    }
}

/// Captures `bounds` and encodes it as PNG, returning both the raw bitmap
/// (needed for the DIB clipboard format, which is not decoded back out of the
/// PNG) and the encoded bytes, and recording each stage's cost in `split`.
fn capture(bounds: Rect, split: &mut Split) -> Result<(RgbaBitmap, Vec<u8>), String> {
    let grab = Instant::now();
    let captured = uptake_capture::capture_region(bounds).map_err(|error| error.to_string())?;
    split.capture_ms = grab.elapsed().as_millis();
    let encode = Instant::now();
    let png = encode_png(&captured.bitmap)?;
    split.encode_ms = encode.elapsed().as_millis();
    // What the capture crate reports it took, never what was asked for: it clamps
    // to the virtual desktop, and the log would otherwise name a rectangle that
    // was not captured. `freeze::capture_still` records the same distinction for
    // the same reason.
    split.bounds = Some(captured.rect);
    split.encoded_bytes = png.len();
    Ok((captured.bitmap, png))
}

/// Task 1.9c's fast path: crop the frame held since mouse-down, or capture.
///
/// **The fallback is the same `capture` call, unchanged.** That is deliberate
/// and is what keeps 1B's exit-gate row — "the paths that can produce a
/// Screenshot's pixels produce identical results" — something the code enforces
/// rather than something a test has to chase: there is one capture path and one
/// crop, and the crop is a byte-for-byte copy of a sub-rectangle
/// (`RgbaBitmap::crop`, which refuses anything that does not fit rather than
/// clamping). The two differ only in *when* the pixels were taken, which is the
/// semantic ADR-0022 §3 bounded at 200 ms and nothing else.
///
/// Every decline is logged with its reason. A silent fall back to the slow path
/// would show up as nothing worse than "1.9c did not help much", which is the
/// hardest kind of regression to notice.
fn capture_or_crop(bounds: Rect, split: &mut Split) -> Result<(RgbaBitmap, Vec<u8>), String> {
    // The frozen still wins when the screen is frozen, and this ordering is
    // ADR-0026 decision 6: **what you see at release is what you get.** The
    // frame source is resolved here, at mouse-up, rather than at mouse-down —
    // so a user who freezes part-way through a drag gets the pixels they are
    // now looking at. Resolving it earlier would hand back the pre-capture's
    // live pixels while the screen showed a still, which is precisely the
    // divergence 1B's exit-gate row 2 exists to catch.
    //
    // There is no staleness test on this branch and that is not an oversight:
    // the user is selecting *on* the displayed image, so it cannot disagree
    // with what they see however long they take (ADR-0022 §5).
    if let Some(bitmap) = crate::freeze::crop(bounds) {
        split.source = Source::Frozen;
        let encode = Instant::now();
        let png = encode_png(&bitmap)?;
        split.encode_ms = encode.elapsed().as_millis();
        split.bounds = Some(bounds);
        split.encoded_bytes = png.len();
        return Ok((bitmap, png));
    }
    match crate::precapture::take(bounds) {
        Ok(bitmap) => {
            // Capture stays at 0 ms, and that is the honest reading rather than
            // a flattering one: the §1 row measures selection release → the
            // clipboard, and on this path no capture happens inside it. The
            // pre-capture's own cost is real but was paid during the drag,
            // before the interval starts.
            split.source = Source::Held;
            let encode = Instant::now();
            let png = encode_png(&bitmap)?;
            split.encode_ms = encode.elapsed().as_millis();
            split.bounds = Some(bounds);
            split.encoded_bytes = png.len();
            Ok((bitmap, png))
        }
        Err(reason) => {
            split.source = Source::Fell(reason);
            capture(bounds, split)
        }
    }
}

/// Captures `bounds` for a newly created area: publishes to the clipboard
/// (PRODUCT-VISION §8 — clipboard only) **and** pins the PNG for the area to
/// render (ADR-0014 §6, the Snipaste pin).
///
/// # The seam task 1.9c changed
///
/// [ADR-0022] settles that §1's selection→clipboard budget is met by *holding a
/// full-monitor frame and cropping it*, not by making capture faster. Task 1.9b
/// confined frame acquisition to [`capture`] so that 1.9c would be a seam change
/// and not surgery, and it was: this function's only change was calling
/// [`capture_or_crop`] instead, and nothing below ever assumed the pixels were
/// captured *now*. The pre-capture itself is `crate::precapture`.
///
/// # Threading
///
/// Spawns, and the spawn is not optional. The only caller is
/// `placement::finish_gesture`, which runs inside the `WH_MOUSE_LL` callback,
/// and a capture is 100–300 ms even warm — far past `LowLevelHooksTimeout`, the
/// failure class F-33 found the hard way.
///
/// [ADR-0022]: the private planning repo's
/// `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`
pub(crate) fn capture_into_area(app: &AppHandle, id: AreaId, bounds: Rect) {
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut split = Split::default();
        let outcome = capture_or_crop(bounds, &mut split).and_then(|(bitmap, png)| {
            let publish = Instant::now();
            // The pin is stored *and announced* before the clipboard is touched:
            // it is the thing the user can see, and a clipboard failure should
            // still leave a visible capture on screen rather than an empty area.
            //
            // **The announcement used to come after the clipboard work, which
            // silently voided that promise.** `dibv5_bytes(&bitmap)?` and
            // `overlay_hwnd(&app)?` both return early from this closure, so
            // either failure stored the bytes and never told the frontend they
            // existed — an area left permanently blank, with the pixels sitting
            // in the store, and a comment below asserting the opposite. Ordering
            // the emit first makes the promise structural instead of a claim.
            let version = {
                let store = app.state::<Mutex<CaptureStore>>();
                let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
                guard.insert(id, bitmap.clone(), png.clone())
            };
            if let Err(error) = crate::overlay::emit_pin(&app, id, version) {
                eprintln!("output: pinned the capture but could not announce it: {error}");
            }
            // Recorded before the `?`s below, so a failure *inside* publishing is
            // not reported as "publish 0 ms".
            let published = dibv5_bytes(&bitmap)
                .and_then(|dib| publish_clipboard(overlay_hwnd(&app)?, &dib, &png));
            split.publish_ms = publish.elapsed().as_millis();
            published
        });
        report("capture", started, &split, outcome);
    });
}

/// The magnified capture a scroll has asked for but no worker has served yet.
///
/// **A slot, not a queue, and that is the whole coalescing strategy.** A scroll
/// burst is ten notches in a few hundred milliseconds and a capture is 100-300
/// ms, so a queue would serve nine magnifications the user has already scrolled
/// past. Overwriting means the worker always picks up the newest request and
/// the intermediate ones are dropped rather than rendered.
static PENDING_MAGNIFY: Mutex<Option<(AreaId, Rect, u64)>> = Mutex::new(None);

/// Bumped by every scroll and by every return to natural size, so a capture can
/// tell whether the zoom it was taken for is still the zoom on screen.
///
/// **The slot alone is not enough, because a request leaves it before the
/// capture starts.** A worker holding the last request of a burst is 100-300 ms
/// from publishing, and in that window the user can scroll again or scroll all
/// the way back out. Without this, the first case paints a magnification the
/// user has already scrolled past and the second re-pins an area that
/// [`clear_magnification`] has just emptied — an area stuck showing a still of
/// the screen with no way back short of scrolling in and out again.
static MAGNIFY_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Whether a magnify worker is running. One at a time, for correctness rather
/// than for thrift: `CaptureStore::insert` issues versions in completion order,
/// so two concurrent captures of one area publish in whichever order they
/// happen to finish, and a slower capture of an *older* zoom would overwrite a
/// newer one on screen.
static MAGNIFY_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Captures `source` and pins it as `id`'s contents, magnifying the area (§3.4).
///
/// `source` is `Zoom::source_rect`'s output: a rectangle *inside* the area,
/// which the frontend then stretches to fill it. The magnification is entirely
/// in the ratio between the two rectangles, so this function does no scaling of
/// its own — the resampling belongs to the compositor, which does it on the GPU.
///
/// # Why this is not [`capture_into_area`]
///
/// That function is the Snipaste pin and publishes to the clipboard as well
/// (PRODUCT-VISION §8). Zooming is not a capture the user asked to keep: it is
/// how the area *renders*, it happens on every notch of a scroll, and putting
/// each intermediate magnification on the clipboard would destroy whatever the
/// user had there. What the two share is [`CaptureStore`] and the pin event,
/// which is the part worth reusing.
///
/// It also resolves its pixels differently. [`capture_into_area`] prefers the
/// pre-capture ([`crate::precapture`]), which holds a frame from the drag that
/// created the area — right for a capture taken *at* that moment, and stale by
/// any amount for a scroll that happens later. This path takes the frozen still
/// when the screen is frozen (ADR-0026 decision 6: what you see is what you
/// get) and captures live otherwise.
///
/// # Threading
///
/// Returns immediately. The only caller is the mouse hook, and a capture is far
/// past `LowLevelHooksTimeout` — the F-33 failure class, and the same reason
/// [`capture_into_area`] spawns.
pub(crate) fn magnify_into_area(app: &AppHandle, id: AreaId, source: Rect) {
    let generation = MAGNIFY_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    *lock_pending() = Some((id, source, generation));
    // Lost the race to an already-running worker, which will take the slot
    // above rather than the request it was spawned for. Nothing else to do.
    if MAGNIFY_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            let taken = lock_pending().take();
            let Some((id, source, generation)) = taken else {
                MAGNIFY_IN_FLIGHT.store(false, Ordering::Release);
                // **The re-check is not belt and braces.** A request landing
                // between the `take` above and the release on the line above
                // sees `MAGNIFY_IN_FLIGHT` still true, returns without
                // spawning, and would then sit in the slot unserved until the
                // user happened to scroll again — leaving the area showing the
                // magnification before last.
                if lock_pending().is_some()
                    && MAGNIFY_IN_FLIGHT
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                break;
            };
            magnify_once(&app, id, source, generation);
        }
    });
}

fn lock_pending() -> std::sync::MutexGuard<'static, Option<(AreaId, Rect, u64)>> {
    PENDING_MAGNIFY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn magnify_once(app: &AppHandle, id: AreaId, source: Rect, generation: u64) {
    let started = Instant::now();
    let mut split = Split::default();
    let outcome = frozen_or_live(source, &mut split).and_then(|(bitmap, png)| {
        // Checked *after* the capture and before the publish, which is the only
        // place it can be checked: the capture is the slow part, so it is the
        // part the user scrolls through. A superseded result is dropped, not
        // published — the newer request is already in the slot and this worker
        // loops straight back round to it.
        if MAGNIFY_GENERATION.load(Ordering::Acquire) != generation {
            return Ok(());
        }
        let version = {
            let store = app.state::<Mutex<CaptureStore>>();
            let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
            guard.insert(id, bitmap, png)
        };
        crate::overlay::emit_pin(app, id, version)
    });
    report("magnify", started, &split, outcome);
}

/// The frozen still if there is one, a live capture otherwise.
///
/// [`capture_or_crop`]'s first and last branches with the pre-capture left out
/// of the middle — see [`magnify_into_area`] for why that branch is wrong here.
fn frozen_or_live(source: Rect, split: &mut Split) -> Result<(RgbaBitmap, Vec<u8>), String> {
    if let Some(bitmap) = crate::freeze::crop(source) {
        split.source = Source::Frozen;
        let encode = Instant::now();
        let png = encode_png(&bitmap)?;
        split.encode_ms = encode.elapsed().as_millis();
        split.bounds = Some(source);
        split.encoded_bytes = png.len();
        return Ok((bitmap, png));
    }
    capture(source, split)
}

/// Drops an area's pinned pixels and tells the frontend they are gone.
///
/// The caller is the scroll that returns an area to natural size. There the
/// area must show the live screen underneath rather than a capture of it:
/// §3.4's floor is *"a way back to normal"*, and an area left rendering its
/// last still would be a way back to a photograph of normal.
pub(crate) fn clear_magnification(app: &AppHandle, id: AreaId) {
    // Two steps, and both are needed. Emptying the slot stops a request that
    // has not started; bumping the generation invalidates one that is already
    // mid-capture, which the slot cannot reach.
    let mut pending = lock_pending();
    if pending.is_some_and(|(pending_id, _, _)| pending_id == id) {
        *pending = None;
    }
    MAGNIFY_GENERATION.fetch_add(1, Ordering::AcqRel);
    crate::captures::forget(app, id);
    drop(pending);
    if let Err(error) = crate::overlay::emit_unpin(app, id) {
        eprintln!("output: dropped the magnification but could not announce it: {error}");
    }
}

/// Encodes RGBA8 pixels as PNG via the same WIC-backed encoder
/// `uptake-capture`'s own hardware-verification driver uses
/// (`examples/grab.rs`) — reused rather than adding a second PNG codec to vet.
pub(crate) fn encode_png(bitmap: &RgbaBitmap) -> Result<Vec<u8>, String> {
    encode_as(bitmap, ImageFormat::Png, "PNG")
}

/// Encodes for the **freeze display path**, in whatever format
/// [`crate::freeze::display_format`] selected.
///
/// Separate from [`encode_png`] on purpose. Copy and Save keep PNG
/// unconditionally, because there the bytes *are* the product and a lossy
/// export would be a defect; here the bytes are only what the WebView paints
/// while the user selects, and the crop comes from the lossless bitmap. **One
/// function serving both would make that distinction a comment instead of a
/// type.**
pub(crate) fn encode_for_display(bitmap: &RgbaBitmap) -> Result<Vec<u8>, String> {
    let (format, _, name) = crate::freeze::display_format();
    encode_as(bitmap, format, name)
}

fn encode_as(bitmap: &RgbaBitmap, format: ImageFormat, name: &str) -> Result<Vec<u8>, String> {
    ImageEncoder::new(format, ImageEncoderPixelFormat::Rgba8)
        .map_err(|error| format!("could not create the {name} encoder: {error}"))?
        .encode(bitmap.pixels(), bitmap.width(), bitmap.height())
        .map_err(|error| format!("could not encode {name}: {error}"))
}

/// The stage breakdown a reader of a rig log actually parses.
///
/// (Five lines describing [`report`] sat at the head of this comment until
/// 2026-08-11, left behind when that function's own doc was moved and this one
/// was written above it. `report` was left undocumented and this function
/// claimed to log a budget overrun, which it does not. They are separated now.)
///
/// # A pure function because the alternative was unverifiable
///
/// This was inlined in [`report`], which writes to stderr and returns nothing,
/// so the only way to check the line was to run the app and read a console.
/// That is how `UT-F-56` shipped: the line was missing the two fields the bar it
/// serves depends on, and no test could have said so. Splitting it out is what
/// lets the assertions below name the fields, so a later edit that drops one
/// goes red instead of costing another rig pass.
///
/// **The rectangle and the byte length lead**, because they are the run's
/// description of its own conditions and the timings are meaningless without
/// them (`quality-bars.md` §1 footnote 3, `UT-F-47`).
fn stage_line(split: &Split) -> String {
    let where_from = match split.bounds {
        Some(rect) => format!(
            "{}x{} at ({}, {}) — ",
            rect.size.width, rect.size.height, rect.origin.x, rect.origin.y
        ),
        // Reached only when the capture itself failed, so there is no rectangle
        // to name. Printing a placeholder would be worse than the gap: a reader
        // scanning for the monitor would find one.
        None => String::new(),
    };
    format!(
        "{where_from}capture {} ms, encode {} ms, publish {} ms, {} bytes ({})",
        split.capture_ms, split.encode_ms, split.publish_ms, split.encoded_bytes, split.source
    )
}

/// Reads [`REPORT_VAR`] once, at startup, and **states what a reader of this
/// run's log may conclude from silence**.
///
/// # Why a switch rather than deleting the gate
///
/// `report`'s per-action line was `cfg(debug_assertions)` and its budget lines
/// were not, so a release build logged **only the actions that missed the bar**.
/// The rig pass of 2026-08-11 took four grabs, changed the clipboard four times,
/// and produced one line: the three that MET the 300 ms target are precisely the
/// three the log discarded, and the survivor at 488 ms was the worst of them
/// (`I-42`, `UT-F-60`). §1's grab row is filled from these logs, so the coverage
/// of the instrument was a function of the bar the instrument exists to set:
/// move the row to 250 ms and three of that day's four vanish; move it to 600 ms
/// and all four do.
///
/// Deleting the `cfg` gate would fix the sample and ship a line on every grab in
/// a shipped product, which is noise a user cannot turn off, so `P-5`'s reasoning
/// applies and the instrument gets removed later by someone who is right to. A
/// `UPTAKE_DEV_*` switch is the route this repository already uses for exactly
/// that trade (`UPTAKE_DEV_PACING`, `UPTAKE_DEV_RESHOW`,
/// `UPTAKE_DEV_MONITOR_PERTURB`), and it is the one the backlog row prescribes.
///
/// **It cannot live in `dev_harness` with its four siblings.** That module is
/// `#[cfg(debug_assertions)]` at its declaration in `lib.rs`, so it does not
/// exist in the one build whose numbers mean anything. The shape it borrows
/// instead is `freeze::init_display_format`: read once at setup, in release,
/// and say what was chosen.
///
/// # It prints on both paths, and the OFF line is the load-bearing one
///
/// `I-11` is this project's standing example of a dev variable whose silence was
/// indistinguishable from working, and the backlog row attaches it to this fix as
/// a warning rather than as a precedent: whatever is added must say that it is on.
/// So this prints armed **and** unarmed, like [`crate::dev_harness::announce_pacing`].
///
/// The difference is which line matters. There, the armed line is the useful one.
/// Here it is the **unarmed** line, because the default state is the one that
/// misleads: it names the threshold and says outright that an action UNDER it
/// prints nothing. Had that sentence been on the console of the 2026-08-11 pass,
/// the operator would have read three missing lines as three fast grabs rather
/// than as three grabs that did not happen.
///
/// # Where the announcement itself is invisible, which is `I-11` again
///
/// This writes to stderr, and `main.rs`'s `windows_subsystem = "windows"`
/// attribute means a release build allocates **no console of its own**. An
/// **inherited** console still receives the output, so a release build launched
/// from a terminal, which is what a rig pass does, prints everything here.
/// Measured twice by independent review on 2026-08-11, with a purpose-built
/// `windows_subsystem = "windows"` binary, from two shells and through three
/// capture forms. `main.rs:12` says release stderr is invisible *in a release
/// build*, which is too broad and is wrong on this point; `hotkey.rs:163` says it
/// of an **installed** build, which is the sink-less case below and is correct.
/// Neither is corrected here: they are outside this change.
///
/// ⚠️ **ONE CAPTURE FORM SILENTLY PRODUCES AN EMPTY FILE, and it is the one a
/// PowerShell operator reaches for first.** Measured, not reasoned about:
///
/// ```text
/// bash    exe 2>&1 | cat                        line present
/// bash    exe 2> file                           line present
/// pwsh    & exe 2>&1 | Write-Host               line present
/// pwsh    & exe 2> file                         FILE CREATED, 0 BYTES
/// pwsh    Start-Process -RedirectStandardError  line present
/// ```
///
/// So on Windows PowerShell, capture with `Start-Process -RedirectStandardError`
/// or a pipe, never with `2> file`. The workspace opened `I-43` for this on the
/// same day and out of the same review, and it had already **cost a rig step**:
/// the operator was handed the broken form, ran it, and got an empty log with no
/// error to explain it. An announcement that lands in a 0-byte file is `I-11`
/// reproduced inside the fix written to escape `I-11`, which is why this table is
/// here rather than in a commit message.
///
/// **Launched from Explorer or the installed shortcut there is no console at all,
/// and then this announcement disappears along with everything it announces.**
/// That restores exactly the ambiguity it exists to remove, and no line can fix
/// it because the missing thing is the sink. Roadmap 1.15 (structured logging) is
/// where that stops being true. State it rather than let a rig operator find it.
pub(crate) fn init_report_verbosity() {
    // What no test covers is the ENV READ, and only the read: the three-way match
    // below maps `VarError` onto the argument, and the decision it feeds is
    // [`apply_report_verbosity`], which is tested including this function's
    // `NOT_UNICODE` case. `std::env::set_var` is `unsafe` in edition 2024 because
    // a concurrent `getenv` is UB, and `cargo test` runs its tests on parallel
    // threads, so covering the read itself would buy one line with a soundness
    // hazard in every other test in the binary.
    //
    // ⚠️ **This comment said the read was "the only part no test covers" while
    // the correction beneath it added an untested arm.** Round 2 of the review
    // found the sentence describing the body it no longer matched.
    //
    // `NotUnicode` would be folded into `None` by `.ok()` and then announced as
    // *unset*, which is a line describing a state it did not load: the rule this
    // function's own comment below states, broken one level up from where it was
    // applied. It is named instead. It is deliberately NOT rendered as `""`,
    // which is what the first cut did and which announces a value the variable
    // does not hold. The `env::var(..).is_ok()` siblings in `dev_harness` absorb
    // this case silently; they are not release code.
    let raw = match std::env::var(REPORT_VAR) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => Some(NOT_UNICODE.to_string()),
    };
    eprintln!("{}", apply_report_verbosity(raw.as_deref()));
}

/// Applies the variable and returns the line to print. Separated from the env
/// read so the switch's behaviour is testable; see [`init_report_verbosity`].
fn apply_report_verbosity(raw: Option<&str>) -> String {
    if raw.is_some() {
        REPORT_EVERY_ACTION.store(true, Ordering::SeqCst);
    }
    // Read BACK rather than named. `init_display_format` printed `staying on
    // png` while the default had moved to JPEG (`UT-F-46`'s SHAPE, not that
    // finding: `UT-F-46` is the idle condition that could not be produced on a
    // real desktop. The workspace records the format defect as "`UT-F-46`
    // exactly", meaning the analogy, and this comment dropped the word that
    // made it one), because the sentence
    // knew what it had decided instead of asking. No branch below may describe a
    // state it did not load.
    verbosity_line(
        REPORT_EVERY_ACTION.load(Ordering::SeqCst),
        cfg!(debug_assertions),
        raw,
    )
}

/// The startup line, as a pure function so its wording is testable.
///
/// Split out for `stage_line`'s reason, one finding later: `UT-F-56` shipped
/// because a line that writes to stderr and returns nothing can only be checked
/// by running the app and reading a console, and the field it was missing was the
/// one the bar depended on. The claim this line makes is the whole value of
/// `I-42`'s fix, so it is asserted rather than eyeballed.
///
/// The threshold is **interpolated from [`BUDGET_TARGET_MS`]**, never typed. A
/// sentence naming 300 while the constant said something else would be a
/// censorship notice that misreports what is censored.
fn verbosity_line(every_action: bool, debug_build: bool, raw: Option<&str>) -> String {
    match (every_action, debug_build) {
        (true, true) => format!(
            "output: every action prints a timing line (debug build), so {REPORT_VAR} is not \
             consulted. A RELEASE build without it prints only actions over {BUDGET_TARGET_MS} ms."
        ),
        (true, false) => format!(
            "output: every action prints a timing line, ARMED by {REPORT_VAR}={:?}. Any value \
             counts as on, including \"0\" and the empty string, the same rule as \
             UPTAKE_DEV_PACING.",
            // `<unset>` is unreachable through `init_report_verbosity`, which
            // only sets the flag when the variable is present. If it is ever
            // printed, the flag was set by something else and that is worth
            // seeing rather than papering over with a plausible default.
            raw.unwrap_or("<unset>")
        ),
        (false, _) => format!(
            "output: only actions OVER {BUDGET_TARGET_MS} ms print a line ({REPORT_VAR} unset). An \
             action UNDER it prints NOTHING, so counting lines counts the misses and not the \
             actions: set {REPORT_VAR}=1 before a measurement pass. {BUDGET_TARGET_MS} ms is the \
             SELECTION budget and every action is compared against it, so for a grab this is a \
             cutoff and not a target it can meet."
        ),
    }
}

/// Logs the outcome, a budget overrun against §1's selection to clipboard
/// numbers, and, when [`REPORT_EVERY_ACTION`] is set, the action itself.
///
/// Instrumented inline rather than asserted after the fact (F-29's lesson): a
/// capture alone measured ~190 to 230 ms warm, leaving little room before the
/// 300 ms target and less before the 600 ms hard fail.
///
/// **Every action here is affected, not only `grab`.** `copy`, `save` and
/// `capture` reach this function too, so the censored sample `I-42` measured on
/// grabs was equally censoring the other three.
fn report(action: &str, started: Instant, split: &Split, outcome: Result<(), String>) {
    let elapsed = started.elapsed().as_millis();
    // The split rides along with every budget line, not just the debug one:
    // the whole point (ADR-0022) is that "it was 320 ms" is not actionable and
    // "capture 210, encode 25, publish 80" is.
    let stages = stage_line(split);
    for line in report_lines(
        action,
        elapsed,
        &stages,
        outcome,
        REPORT_EVERY_ACTION.load(Ordering::SeqCst),
    ) {
        eprintln!("{line}");
    }
}

/// Which lines an action produces, as a pure function of its outcome and the
/// verbosity flag.
///
/// # This is the site the whole change is about, and it was the one site with
/// no test
///
/// Split out after an independent review drilled it. Inverting the flag check
/// inside `report`, so that armed prints nothing and unarmed prints everything,
/// which is `I-42` exactly and inverted, left the suite green in **both**
/// profiles: no test calls `report`, and `eprintln!` returns nothing to assert on.
/// A switch whose only job is to decide whether a line appears had every part
/// tested except whether the line appears.
///
/// So the decision is a value now. `report` prints what this returns and makes
/// no decisions of its own.
fn report_lines(
    action: &str,
    elapsed: u128,
    stages: &str,
    outcome: Result<(), String>,
    every_action: bool,
) -> Vec<String> {
    let Err(error) = outcome else {
        let mut lines = Vec::new();
        if elapsed > BUDGET_HARD_FAIL_MS {
            lines.push(format!(
                "output: {action} took {elapsed} ms — over the §1 hard-fail budget ({BUDGET_HARD_FAIL_MS} ms) — {stages}"
            ));
        } else if elapsed > BUDGET_TARGET_MS {
            lines.push(format!(
                "output: {action} took {elapsed} ms — over the §1 target ({BUDGET_TARGET_MS} ms), within the hard fail — {stages}"
            ));
        }
        // Was `#[cfg(debug_assertions)]`. The flag defaults to that same
        // answer, so a debug build is unchanged; a release build gains the
        // line only when the operator asked for it. See
        // [`init_report_verbosity`].
        if every_action {
            lines.push(format!(
                "output: {action} finished in {elapsed} ms — {stages}"
            ));
        }
        return lines;
    };
    vec![format!(
        "output: {action} failed after {elapsed} ms: {error} — {stages}"
    )]
}

// ---------------------------------------------------------------------------
// Clipboard: both CF_DIBV5 and the registered "PNG" format (PRODUCT-VISION §8).
// ---------------------------------------------------------------------------

/// The overlay window's handle, to own the clipboard with.
///
/// Resolving it is a `HashMap` lookup plus a plain field read on tao's side
/// (`Window::hwnd` returns `self.window.0` — no event-loop dispatch), so this
/// is safe from the spawned thread these actions run on.
fn overlay_hwnd(app: &AppHandle) -> Result<HWND, String> {
    Ok(crate::overlay::overlay_window(app)?
        .hwnd()
        .map_err(|error| format!("could not get the overlay window handle: {error}"))?
        .0)
}

/// Opens the clipboard against `owner`, empties it, publishes both formats,
/// and closes it.
///
/// **`owner` must not be null.** `OpenClipboard(NULL)` would associate the
/// open clipboard with the current task rather than a window, which is
/// convenient on a spawned thread but out of contract: `EmptyClipboard`'s
/// documented remarks state that a NULL window handle "sets the clipboard
/// owner to NULL. Note that this causes `SetClipboardData` to fail." It does
/// not fail in practice on Windows 11 — the first cut of this module shipped
/// that way and was verified working on the rig — but relying on documented-
/// to-fail behaviour is the shape F-25 and F-33 both took, so the overlay's
/// own `HWND` is passed instead. Nothing is delay-rendered, so being the
/// clipboard owner costs the overlay nothing: it receives
/// `WM_DESTROYCLIPBOARD`, which `DefWindowProc` ignores.
fn publish_clipboard(owner: HWND, dib: &[u8], png: &[u8]) -> Result<(), String> {
    // SAFETY: `OpenClipboard`/`CloseClipboard` bracket every clipboard call
    // below; `owner` is a live top-level window handle owned by this process.
    let opened = unsafe { OpenClipboard(owner) };
    if opened == 0 {
        return Err("could not open the clipboard".to_string());
    }
    let result = (|| {
        // SAFETY: the clipboard is open, per the check above.
        if unsafe { EmptyClipboard() } == 0 {
            return Err("could not empty the clipboard".to_string());
        }
        set_clipboard_data(CF_DIBV5, dib)?;
        set_clipboard_png(png)?;
        Ok(())
    })();
    // SAFETY: matches the successful `OpenClipboard` above, on every path.
    unsafe {
        CloseClipboard();
    }
    result
}

/// Allocates a movable global block, copies `data` into it, and hands it to
/// the clipboard under `format`.
///
/// `SetClipboardData` takes ownership of the handle **only on success** — the
/// system frees it when the clipboard is next emptied or another app closes
/// it. On failure the caller still owns it and must free it, which is exactly
/// what the `Err` arm below does; a leak-on-failure clipboard helper would be
/// the kind of bug that only shows up as slowly rising handle counts.
fn set_clipboard_data(format: u32, data: &[u8]) -> Result<(), String> {
    // SAFETY: `size` is `data.len()`, a valid allocation request; the handle
    // is either handed to `SetClipboardData` (which then owns it) or freed
    // below on every other path.
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len()) };
    if handle.is_null() {
        return Err(format!(
            "could not allocate clipboard memory for format {format}"
        ));
    }
    // SAFETY: `handle` was just allocated and is not yet shared with anything
    // else; the lock is released before `SetClipboardData`, which is required
    // — the system must be able to lock the handle itself.
    let locked = unsafe { GlobalLock(handle) };
    if locked.is_null() {
        unsafe {
            GlobalFree(handle);
        }
        return Err(format!(
            "could not lock clipboard memory for format {format}"
        ));
    }
    // SAFETY: `locked` points at an allocation of exactly `data.len()` bytes,
    // just locked above.
    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), locked.cast::<u8>(), data.len());
        GlobalUnlock(handle);
    }
    // SAFETY: `handle` is valid, unlocked, and sized to `data`.
    let published = unsafe { SetClipboardData(format, handle) };
    if published.is_null() {
        // Ours again — the system never took it.
        unsafe {
            GlobalFree(handle);
        }
        return Err(format!("could not publish clipboard format {format}"));
    }
    Ok(())
}

/// Predefined clipboard format for a `BITMAPV5HEADER`-based DIB. Not exported
/// by `windows-sys`'s `DataExchange` module (the predefined `CF_*` constants
/// live with the GDI bitmap types instead, and this one specifically is
/// absent from both) — the numeric value is stable ABI, documented in
/// `winuser.h` and unchanged since Windows 2000.
const CF_DIBV5: u32 = 17;

/// Builds a `BITMAPV5HEADER` DIB with a true alpha channel, as the packed
/// bytes `CF_DIBV5` expects: header immediately followed by the pixels.
///
/// DIBs are conventionally bottom-up for clipboard compatibility (a negative,
/// top-down height is valid GDI but not every consumer handles it), so the
/// rows are flipped here; `uptake_core::bitmap::RgbaBitmap` is top-down by
/// contract. `BI_BITFIELDS` with an explicit alpha mask — rather than plain
/// `BI_RGB` — is what makes a partially transparent capture (the dead zones
/// between mismatched monitors, `uptake-capture`'s crate docs) paste with its
/// transparency intact instead of forced opaque.
fn dibv5_bytes(bitmap: &RgbaBitmap) -> Result<Vec<u8>, String> {
    let width = i32::try_from(bitmap.width()).map_err(|_| "capture width overflows a DIB")?;
    let height = i32::try_from(bitmap.height()).map_err(|_| "capture height overflows a DIB")?;
    let pixels = bottom_up_bgra(bitmap);
    // Zero is documented as acceptable only for `BI_RGB` ("This may be set to
    // zero for BI_RGB bitmaps") and this DIB is `BI_BITFIELDS`, so a consumer
    // deriving the pixel extent from the header gets the real size.
    let size_image =
        u32::try_from(pixels.len()).map_err(|_| "capture overflows a DIB's image size")?;
    let header = BITMAPV5HEADER {
        bV5Size: u32::try_from(size_of::<BITMAPV5HEADER>()).unwrap_or_default(),
        bV5Width: width,
        // Positive: bottom-up, matching the flipped pixel order below.
        bV5Height: height,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5SizeImage: size_image,
        bV5XPelsPerMeter: 0,
        bV5YPelsPerMeter: 0,
        bV5ClrUsed: 0,
        bV5ClrImportant: 0,
        bV5RedMask: 0x00FF_0000,
        bV5GreenMask: 0x0000_FF00,
        bV5BlueMask: 0x0000_00FF,
        bV5AlphaMask: 0xFF00_0000,
        bV5CSType: LCS_S_RGB,
        bV5Endpoints: unsafe { std::mem::zeroed() },
        bV5GammaRed: 0,
        bV5GammaGreen: 0,
        bV5GammaBlue: 0,
        bV5Intent: LCS_GM_IMAGES as u32,
        bV5ProfileData: 0,
        bV5ProfileSize: 0,
        bV5Reserved: 0,
    };
    // SAFETY: `BITMAPV5HEADER` is a `repr(C)` plain-old-data struct with no
    // padding at 4-byte alignment; reading its own storage as bytes for its
    // own size cannot read out of bounds.
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            (&raw const header).cast::<u8>(),
            size_of::<BITMAPV5HEADER>(),
        )
    };
    let mut dib = Vec::with_capacity(header_bytes.len() + pixels.len());
    dib.extend_from_slice(header_bytes);
    dib.extend_from_slice(&pixels);
    Ok(dib)
}

/// `bitmap`'s pixels as bottom-up `B, G, R, A` — the DIB's on-the-wire order
/// for `bV5RedMask = 0x00FF0000` and friends, and its conventional row order.
fn bottom_up_bgra(bitmap: &RgbaBitmap) -> Vec<u8> {
    let width = bitmap.width() as usize;
    let height = bitmap.height() as usize;
    let src = bitmap.pixels();
    let mut out = vec![0_u8; src.len()];
    for row in 0..height {
        let src_row = &src[row * width * 4..(row + 1) * width * 4];
        let dest_start = (height - 1 - row) * width * 4;
        let dest_row = &mut out[dest_start..dest_start + width * 4];
        for (src_px, dest_px) in src_row.chunks_exact(4).zip(dest_row.chunks_exact_mut(4)) {
            dest_px[0] = src_px[2]; // B
            dest_px[1] = src_px[1]; // G
            dest_px[2] = src_px[0]; // R
            dest_px[3] = src_px[3]; // A
        }
    }
    out
}

/// Publishes the encoded PNG bytes under the registered `"PNG"` clipboard
/// format — the name browsers, Discord and Figma look for.
fn set_clipboard_png(png: &[u8]) -> Result<(), String> {
    let name: Vec<u16> = "PNG\0".encode_utf16().collect();
    // SAFETY: `name` is a valid, NUL-terminated UTF-16 string for the
    // duration of this call.
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    if format == 0 {
        return Err("could not register the PNG clipboard format".to_string());
    }
    set_clipboard_data(format, png)
}

// ---------------------------------------------------------------------------
// Save to file: Pictures\UP-TAKE\, timestamp naming, collision suffix.
// ---------------------------------------------------------------------------

/// Writes `png` to `Pictures\UP-TAKE\UP-TAKE_YYYY-MM-DD_HH-MM-SS.png`,
/// creating the directory on first use and appending `_2`, `_3`, … on a
/// same-second collision.
fn write_file(app: &AppHandle, png: &[u8]) -> Result<(), String> {
    let pictures = app
        .path()
        .picture_dir()
        .map_err(|error| format!("could not resolve the Pictures folder: {error}"))?;
    let dir = pictures.join("UP-TAKE");
    fs::create_dir_all(&dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    let path = unique_path(&dir, &timestamp_name());
    fs::write(&path, png).map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// `UP-TAKE_YYYY-MM-DD_HH-MM-SS` from the local wall clock, via `GetLocalTime`
/// rather than a new dependency — this workspace already talks to Win32
/// directly for everything else in `src-tauri` (see `placement.rs`,
/// `overlay.rs`), and a timestamp is one more struct, not a reason to add a
/// date/time crate.
fn timestamp_name() -> String {
    // SAFETY: `GetLocalTime` fills a plain-old-data struct with no
    // preconditions.
    let time: SYSTEMTIME = unsafe {
        let mut time = std::mem::zeroed();
        GetLocalTime(&mut time);
        time
    };
    format!(
        "UP-TAKE_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

/// `dir/stem.png`, or `dir/stem_2.png`, `dir/stem_3.png`, … — the first name
/// that does not already exist. A `_2` suffix appended for a same-second
/// collision reads more naturally than `_1` on the first file redundantly
/// numbering something unique.
fn unique_path(dir: &std::path::Path, stem: &str) -> PathBuf {
    let plain = dir.join(format!("{stem}.png"));
    if !plain.exists() {
        return plain;
    }
    let mut suffix = 2_u32;
    loop {
        let candidate = dir.join(format!("{stem}_{suffix}.png"));
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]
mod tests {
    use super::*;

    /// The unarmed line is `I-42`'s actual deliverable, so it is the one with
    /// the most assertions on it. Each names a thing the 2026-08-11 rig operator
    /// would have needed to read three absent lines correctly: which threshold
    /// selects the sample, that meeting it produces silence, and what to set.
    #[test]
    fn the_unarmed_line_states_that_a_met_target_prints_nothing() {
        let line = verbosity_line(false, false, None);
        assert!(line.contains(&BUDGET_TARGET_MS.to_string()), "{line}");
        assert!(line.contains("NOTHING"), "{line}");
        assert!(line.contains("counts the misses"), "{line}");
        assert!(line.contains(REPORT_VAR), "{line}");
    }

    /// The threshold is interpolated rather than typed. A line that named a
    /// literal 300 would keep saying 300 after the constant moved, which is a
    /// censorship notice misreporting what it censors.
    #[test]
    fn the_unarmed_line_takes_its_threshold_from_the_budget_constant() {
        assert!(
            verbosity_line(false, false, None).contains(&format!("OVER {BUDGET_TARGET_MS} ms")),
            "the threshold is not interpolated from BUDGET_TARGET_MS"
        );
    }

    /// `I-11`'s requirement: the armed state says so, and it names the value it
    /// read, because any value counts as on and `UPTAKE_DEV_REPORT=0` is the
    /// trap that rule sets.
    #[test]
    fn the_armed_line_names_the_variable_and_the_value_it_read() {
        let line = verbosity_line(true, false, Some("0"));
        assert!(line.contains("ARMED"), "{line}");
        assert!(line.contains(&format!("{REPORT_VAR}=\"0\"")), "{line}");
        assert!(line.contains("Any value counts as on"), "{line}");
    }

    /// A value that is not valid Unicode arms the switch like any other, and the
    /// line says it could not be shown rather than showing a plausible `""`.
    ///
    /// The first cut of the `NotUnicode` arm passed `String::new()`, so the
    /// armed line read `UPTAKE_DEV_REPORT=""` for a variable holding no such
    /// thing. Round 2 of the review found it: the rule twenty lines above is
    /// that no branch may describe a state it did not load.
    #[test]
    fn a_non_unicode_value_arms_the_switch_and_is_not_rendered_as_empty() {
        let line = verbosity_line(true, false, Some(NOT_UNICODE));
        assert!(line.contains("ARMED"), "{line}");
        assert!(line.contains("not valid Unicode"), "{line}");
        assert!(
            !line.contains(&format!("{REPORT_VAR}=\"\"")),
            "a value that could not be read must not be shown as empty: {line}"
        );
    }

    /// A debug build must not claim the variable did anything, and must say what
    /// a release build would have done instead: the developer reading this line
    /// is the one who will later read a rig log they did not produce.
    #[test]
    fn the_debug_line_says_the_variable_is_not_consulted() {
        let line = verbosity_line(true, true, Some("1"));
        assert!(line.contains("not consulted"), "{line}");
        assert!(line.contains("RELEASE"), "{line}");
        assert!(!line.contains("ARMED"), "{line}");
    }

    /// The three cases are mutually distinguishable at a glance. A rig operator
    /// scanning one line of console output should not have to parse a sentence
    /// to tell which of them they are in.
    #[test]
    fn the_three_verbosity_lines_are_all_different() {
        let debug = verbosity_line(true, true, None);
        let armed = verbosity_line(true, false, Some("1"));
        let off = verbosity_line(false, false, None);
        assert_ne!(debug, armed);
        assert_ne!(armed, off);
        assert_ne!(debug, off);
    }

    /// The censorship itself, at the site that performs it.
    ///
    /// **This is the test the first cut of this change did not have**, and an
    /// independent review found the hole by inverting the flag check inside
    /// `report`: armed printing nothing and unarmed printing everything, which
    /// is `I-42` exactly and inverted, left the suite green in **both** profiles.
    /// Every part of the switch was tested except whether the line appears.
    #[test]
    fn an_under_budget_action_is_silent_unless_every_action_is_on() {
        let quiet = report_lines("grab", 120, "STAGES", Ok(()), false);
        assert!(
            quiet.is_empty(),
            "an under-budget action must produce no line at all: {quiet:?}"
        );
        let loud = report_lines("grab", 120, "STAGES", Ok(()), true);
        assert_eq!(loud.len(), 1, "{loud:?}");
        assert!(loud[0].contains("finished in 120 ms"), "{loud:?}");
    }

    /// The three grabs of 2026-08-11 that met the target, and the one that did
    /// not, as the function now sees them. The whole finding in one assertion:
    /// unarmed, four actions produce one line, and it is the worst of them.
    #[test]
    fn the_2026_08_11_sample_is_one_line_from_four_actions_when_unarmed() {
        let grabs = [281_u128, 297, 300, 488];
        let unarmed: Vec<String> = grabs
            .iter()
            .flat_map(|ms| report_lines("grab", *ms, "STAGES", Ok(()), false))
            .collect();
        assert_eq!(unarmed.len(), 1, "{unarmed:?}");
        assert!(unarmed[0].contains("488"), "{unarmed:?}");
        let armed: Vec<String> = grabs
            .iter()
            .flat_map(|ms| report_lines("grab", *ms, "STAGES", Ok(()), true))
            .collect();
        assert_eq!(armed.len(), 5, "four finished lines plus one budget line");
    }

    /// 300 ms exactly is UNDER the bar, because the comparison is `>`. Pinned
    /// because the boundary is where a censored sample is least visible.
    #[test]
    fn the_budget_comparison_is_strict_at_both_thresholds() {
        assert!(report_lines("grab", BUDGET_TARGET_MS, "S", Ok(()), false).is_empty());
        assert_eq!(
            report_lines("grab", BUDGET_TARGET_MS + 1, "S", Ok(()), false).len(),
            1
        );
        let hard = report_lines("grab", BUDGET_HARD_FAIL_MS + 1, "S", Ok(()), false);
        assert!(hard[0].contains("hard-fail"), "{hard:?}");
        // BUDGET_HARD_FAIL_MS exactly is the OTHER boundary, and pinning only
        // the target one is why `>` -> `>=` at the hard-fail line survived round
        // 2's mutation pass with the suite green in both profiles. The test was
        // named for a coverage it did not have.
        let at_hard = report_lines("grab", BUDGET_HARD_FAIL_MS, "S", Ok(()), false);
        assert_eq!(at_hard.len(), 1, "{at_hard:?}");
        assert!(
            at_hard[0].contains("over the §1 target") && !at_hard[0].contains("hard-fail"),
            "exactly the hard-fail budget is still WITHIN it: {at_hard:?}"
        );
    }

    /// A failure was never censored and must not become so. It is the one line
    /// that already survived a release build, which is why `I-42` could be read
    /// as "no failures appeared" rather than as "successes are invisible".
    #[test]
    fn a_failure_prints_whatever_the_flag_says() {
        for every_action in [false, true] {
            let lines = report_lines("grab", 12, "S", Err("no monitor".into()), every_action);
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert!(
                lines[0].contains("failed after 12 ms: no monitor"),
                "{lines:?}"
            );
        }
    }

    /// A release build starts quiet and a debug build starts loud. **Both
    /// directions, in whichever build this runs in.** See
    /// [`report_line_default`] for why that is worth a parameter, and note that
    /// the assertion CI would otherwise have gotten is vacuous.
    #[test]
    fn a_release_build_starts_quiet_and_a_debug_build_starts_loud() {
        assert!(
            !report_line_default(false),
            "a release build must start quiet"
        );
        assert!(
            report_line_default(true),
            "a debug build must keep its line"
        );
    }

    /// The wiring from the variable to the flag, and the one-directional rule:
    /// presence raises it, absence never lowers it.
    ///
    /// **This test owns [`REPORT_EVERY_ACTION`]**, which is process-wide, so the
    /// three states have to be walked in order inside a single test rather than
    /// split across three. Nothing else reads the flag (`report` is called by no
    /// test), so the mutation is contained here.
    ///
    /// It is strongest under `cargo test --release`, where the walk is
    /// `false → false → true`. In a debug build the flag starts on, so the third
    /// assertion is the only one carrying weight. Stated rather than left for a
    /// reader to work out, because a test that is weaker than it looks is the
    /// thing this whole change is about.
    #[test]
    fn the_variable_raises_the_flag_and_its_absence_never_lowers_it() {
        let default = report_line_default(cfg!(debug_assertions));
        assert_eq!(REPORT_EVERY_ACTION.load(Ordering::SeqCst), default);
        apply_report_verbosity(None);
        assert_eq!(
            REPORT_EVERY_ACTION.load(Ordering::SeqCst),
            default,
            "an absent variable moved the flag"
        );
        // "0" rather than "1": any value counts as on, which is the trap the
        // armed line exists to name, so it is the value worth pinning.
        apply_report_verbosity(Some("0"));
        assert!(REPORT_EVERY_ACTION.load(Ordering::SeqCst));
    }

    #[test]
    fn unique_path_returns_the_plain_name_when_nothing_collides() {
        let dir = std::env::temp_dir().join(format!("uptake-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = unique_path(&dir, "UP-TAKE_2026-07-25_12-00-00");
        assert_eq!(path, dir.join("UP-TAKE_2026-07-25_12-00-00.png"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unique_path_appends_a_collision_suffix_starting_at_2() {
        let dir = std::env::temp_dir().join(format!("uptake-test-collide-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let stem = "UP-TAKE_2026-07-25_12-00-00";
        fs::write(dir.join(format!("{stem}.png")), b"one").unwrap();
        let path = unique_path(&dir, stem);
        assert_eq!(path, dir.join(format!("{stem}_2.png")));
        fs::write(&path, b"two").unwrap();
        let path = unique_path(&dir, stem);
        assert_eq!(path, dir.join(format!("{stem}_3.png")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_dib_is_a_v5_header_followed_by_its_pixels_with_a_real_size_image() {
        let bitmap = RgbaBitmap::from_pixels(
            uptake_core::geometry::Size::new(3, 2),
            vec![0_u8; 3 * 2 * 4],
        )
        .unwrap();
        let dib = dibv5_bytes(&bitmap).unwrap();
        assert_eq!(dib.len(), 124 + 3 * 2 * 4);
        // bV5Size is the first field and must equal the header's own length,
        // which is how a consumer picks the header version.
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 124);
        // bV5SizeImage sits at offset 20; zero is only sanctioned for BI_RGB
        // and this DIB is BI_BITFIELDS.
        assert_eq!(
            u32::from_le_bytes(dib[20..24].try_into().unwrap()),
            3 * 2 * 4
        );
    }

    /// The three defined test screens, as pixels rather than as a web page.
    ///
    /// `examples/testscreen/index.html` is what the rig displays; this is the
    /// same three contents built directly, so the claim underneath the whole
    /// bracket can be checked without a desktop. The PRNG is mulberry32, the
    /// same one the page uses, for no reason except that two figures taken by
    /// different routes should not differ for an uninteresting reason.
    fn test_screen(kind: &str, size: uptake_core::geometry::Size) -> RgbaBitmap {
        let count = (size.width as usize) * (size.height as usize);
        let mut pixels = vec![0_u8; count * 4];
        let mut state: u32 = 0x05ee_d199;
        let mut next = || {
            state = state.wrapping_add(0x6d2b_79f5);
            let mut t = state;
            t = (t ^ (t >> 15)).wrapping_mul(t | 1);
            t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
            (t ^ (t >> 14)) as u8
        };
        // The control is 8x8 blocks of one random colour each, which is
        // full-entropy noise at 1/64 the resolution and therefore lands about
        // mid-decade between the two.
        //
        // **It was smooth vertical bands first, and that was wrong.** Measured
        // at 640x400: plain 1987 bytes, bands 2011, dense 896245. The bands
        // satisfied "between the two" by 24 bytes, which is 1.2 % of PLAIN and
        // nothing a reader of a rig log could tell apart. PNG filters rows
        // before it deflates them, so hard-edged vertical bands are nearly as
        // compressible as a flat field. A control that lands on top of one of
        // the endpoints does not falsify anything.
        //
        // ⚠️ **Those three figures are the BANDS ERA and 896245 is not what this
        // generator produces today** — `examples/testscreen/README.md` records
        // 895,515 for the same screen, the two disagreed from the commit that
        // introduced both (`494b26a`), and the row for it is `I-24`. **The cause
        // is the line below rather than a typo, and it was measured rather than
        // reasoned about:** `block_colours` is built unconditionally, before the
        // `match kind`, so on the dense path it consumes 12,000 draws of this
        // shared `next` before the pixel loop starts and the dense stream begins
        // somewhere else. Building it only for the blocks branch reproduces
        // **896245 exactly**, which is what dates the figure rather than merely
        // explaining it. The README is current; this paragraph is history and is
        // labelled as history instead of being edited to match, because the
        // numbers are the evidence for the argument they sit inside.
        //
        // Leave the unconditional build alone: `block_colours` before the loop is
        // what keeps this function one pass, and the two published figures
        // (`plain 1,987`, `blocks 23,065`) are both current under it. The one
        // thing that would be wrong is a fourth copy of the table here, which is
        // what `I-20` closed.
        const BLOCK: usize = 8;
        let width = size.width as usize;
        let blocks_across = width.div_ceil(BLOCK);
        let block_colours: Vec<[u8; 3]> = (0..blocks_across
            * (size.height as usize).div_ceil(BLOCK))
            .map(|_| [next(), next(), next()])
            .collect();
        for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            let (x, y) = (index % width, index / width);
            match kind {
                "plain" => pixel.copy_from_slice(&[0x80, 0x80, 0x80, 255]),
                "dense" => pixel.copy_from_slice(&[next(), next(), next(), 255]),
                _ => {
                    let colour = block_colours[(y / BLOCK) * blocks_across + (x / BLOCK)];
                    pixel.copy_from_slice(&[colour[0], colour[1], colour[2], 255]);
                }
            }
        }
        RgbaBitmap::from_pixels(size, pixels).unwrap()
    }

    /// **The acceptance check `quality-bars.md` §1 footnote 3 demands before any
    /// 1.9g figure may be quoted**, run against the encoder the freeze path
    /// actually uses rather than a reproduction of it.
    ///
    /// The bracket is only worth having if the two screens separate. If PLAIN
    /// and DENSE encoded to similar sizes, the screen would not be controlling
    /// the variable and the pair would be decoration. An order of magnitude is
    /// the spec's own bar and is not a tuned threshold: the real separation is
    /// far wider, and the assert is deliberately loose so that a genuine change
    /// in the encoder trips it rather than ordinary noise.
    #[test]
    fn the_defined_test_screens_separate_by_an_order_of_magnitude() {
        let size = uptake_core::geometry::Size::new(640, 400);
        let plain = encode_png(&test_screen("plain", size)).unwrap().len();
        let dense = encode_png(&test_screen("dense", size)).unwrap().len();
        assert!(
            dense > plain * 10,
            "PLAIN and DENSE do not separate: plain {plain} bytes, dense {dense} bytes. \
             The bracket is decoration unless these differ by orders of magnitude."
        );
    }

    /// **The falsifier for the self-description** (`D-29(e)`): a check that
    /// cannot come out the other way is worth nothing.
    ///
    /// The byte length is what makes a mislabelled run detectable, and that
    /// claim is only true if an *unlisted* screen is visibly neither. So the
    /// control is asserted to land strictly between the two, which is what a
    /// reader of the rig log is being asked to rely on.
    #[test]
    fn an_unlisted_screen_lands_visibly_between_the_two() {
        let size = uptake_core::geometry::Size::new(640, 400);
        let plain = encode_png(&test_screen("plain", size)).unwrap().len();
        let dense = encode_png(&test_screen("dense", size)).unwrap().len();
        let blocks = encode_png(&test_screen("blocks", size)).unwrap().len();
        // An order of magnitude clear of **both** ends, not merely on the
        // correct side of them. The first version of this asserted only
        // `blocks > plain && blocks < dense`, which a 24-byte margin satisfied
        // while the control sat 1.2 % from PLAIN — a green that could not have
        // gone red for the reason the check exists.
        assert!(
            blocks > plain * 10 && dense > blocks * 10,
            "the control does not land visibly between the listed screens: \
             plain {plain}, blocks {blocks}, dense {dense}. If an unlisted screen \
             cannot be told from a listed one, the reported byte length is \
             unfalsifiable and buys nothing."
        );
    }

    /// **The same falsifier, against the format the freeze path actually
    /// ships** — and the reason it is a second test rather than an edit to the
    /// two above.
    ///
    /// Both checks above call [`encode_png`] explicitly. That was right when
    /// they were written and stopped being right in the same branch: since
    /// [ADR-0027] the display path encodes **JPEG**, so the byte lengths printed
    /// beside every rig timing are JPEG lengths, and the property those tests
    /// prove was a property of a format the rig no longer measures. A falsifier
    /// aimed at the wrong format is `UT-F-52`'s defect one level up — the check
    /// is real, it just does not guard the number anyone reads.
    ///
    /// # The bar is deliberately weaker here, and the arithmetic is why
    ///
    /// **The measurement is in `examples/testscreen/README.md` and deliberately
    /// not repeated here** (backlog `I-20`, 2026-08-05). It sat in three files,
    /// hand-maintained, with nothing comparing the copies: this suite guards the
    /// *inequalities* and has never guarded a digit, so a stale table was what a
    /// later reader would have reasoned from. What that page reports at
    /// 2560×1440 is the span each format covers floor to ceiling, and the two
    /// spans are the whole of what the argument below needs.
    ///
    /// PNG spans a factor of **704**, JPEG a factor of **57**. An order of
    /// magnitude clear of *both* ends needs the span to exceed 10 × 10 = 100, so
    /// under JPEG **no control can satisfy the bar above** — it is not that this
    /// one is badly chosen. The measured control sits 12.0× above PLAIN and
    /// 4.7× below DENSE, and the best any control could do is √57 ≈ 7.5× each
    /// way.
    ///
    /// So this asserts **3× clear of both ends**: a floor reasoned from the span
    /// rather than fitted to the measurement (which clears it by 4× and 1.6×),
    /// and still far more than the 1.2 % margin that made `UT-F-52` worth
    /// recording. What it preserves is the only claim `quality-bars.md` §1
    /// footnote 3 actually makes — that an unlisted screen is *visibly* neither.
    ///
    /// ✅ **Settled by the founder 2026-08-04, and `quality-bars.md` §1
    /// footnote 3 now states the acceptance per format rather than once.** The
    /// endpoint requirement is unchanged at 10× for **the two compressed
    /// formats** — that is the bar, and JPEG passes it. Only the *control's*
    /// margin is per format, 10× in PNG and 3× in JPEG, because 10 × 10 exceeds
    /// JPEG's whole span and therefore cannot be met by any control at all.
    ///
    /// ⛔ **NOT "every format", and the correction is the point.** This comment
    /// said *every format* until independent review measured the third one.
    /// **BMP is uncompressed**, so PLAIN, BLOCKS and DENSE encode to **exactly
    /// the same length** — 1,024,054 bytes at 640×400, 14,745,654 at 2560×1440,
    /// a span of **1.000**. Under BMP the per-monitor byte length carries **zero
    /// information about the screen**, which is not a weaker bracket but the
    /// absence of one, and `UPTAKE_FREEZE_FORMAT=bmp` is a supported override
    /// the shipped binary accepts. A universal quantifier written without
    /// checking the third case is `F-30`'s shape in one sentence. **Never quote
    /// a 1.9g figure from a BMP run; the self-description is void there.**
    ///
    /// **Why that is not a bar rewritten to fit what was built** — the thing
    /// being measured, can a reader tell PLAIN from DENSE, is untouched at full
    /// strength; what moved is an auxiliary control's margin, and it moved for
    /// an arithmetic reason. **If a future format widens the span past 100, this
    /// goes back to 10× without discussion** — and that is now *asserted* below
    /// rather than promised here, because a conditional nobody re-reads is not a
    /// mechanism.
    ///
    /// # The figures, and which size they were taken at
    ///
    /// The README's table is **2560×1440** (it used to sit directly above this
    /// paragraph, which is why this said "the table above" until `I-20` moved
    /// it); this test asserts at **640×400**, where
    /// the span is **49.7×** and the control's margins are **3.6×** and
    /// **1.6×**. The smaller buffer is the *pessimistic* corner — the span grows
    /// with resolution (49.7 → 57.1 from 640×400 to 4K) — so the assertion is
    /// stricter than the cited table implies rather than looser. Said out loud
    /// because this is the file whose whole subject is numbers whose
    /// preconditions nobody stated (`UT-F-46`, `UT-F-47`).
    ///
    /// ⚠️ **3× is comfortable at the PLAIN end and thin at the DENSE end**, and
    /// a reader should know which. The measured margin is 4.66×, but the *floor
    /// this assertion permits* would put a control at ~1.1 MB against DENSE's
    /// 3.3 MB — same order, same digit count, differing in the leading digit,
    /// which an operator scanning a four-monitor log does not reliably see as
    /// different. Raising the floor re-opens the arithmetic this settled, so it
    /// is recorded rather than changed.
    ///
    /// [ADR-0027]: the private planning repo's
    /// `DECISIONS/ADR-0027-jpeg-for-the-freeze-display-path.md`
    #[test]
    fn the_bracket_still_separates_in_the_format_that_ships() {
        let size = uptake_core::geometry::Size::new(640, 400);
        let encode = |kind| encode_for_display(&test_screen(kind, size)).unwrap().len();
        let (plain, blocks, dense) = (encode("plain"), encode("blocks"), encode("dense"));
        let format = crate::freeze::display_format().2;
        // The endpoints are the claim §1 makes in its own words, and it survives
        // the format change: JPEG still separates PLAIN from DENSE by 57×.
        assert!(
            dense > plain * 10,
            "PLAIN and DENSE do not separate in {format}, the shipped display \
             format: plain {plain} bytes, dense {dense} bytes. The per-monitor \
             byte length cannot describe a run whose endpoints it cannot tell \
             apart."
        );
        assert!(
            blocks > plain * 3 && dense > blocks * 3,
            "the control does not land visibly between the listed screens in \
             {format}: plain {plain}, blocks {blocks}, dense {dense}. See this \
             test's own note before loosening the factor — 3× is already \
             reasoned from the span rather than fitted to it."
        );
        // **The promise above, enforced instead of remembered.** The 3× control
        // margin exists only because 10 × 10 = 100 exceeds this format's span;
        // the moment a format's span clears 100, that reason evaporates and the
        // margin must go back to 10×. Written as an assertion because a
        // conditional in a doc comment is precisely the obligation nobody
        // re-reads when the condition finally falls due.
        //
        // Not flaky: the span is 49.7× here and 57.1× at 4K, both far from 100.
        // Deliberately NOT auto-derived as `sqrt(span)` — at this size that
        // yields 7.05 against a measured `dense/blocks` of 4.66, so a clever
        // derivation turns the suite red today for no defect.
        assert!(
            dense < plain * 100,
            "{format}'s span is now {:.0}×, past 100 — so a control CAN sit an \
             order of magnitude clear of both ends, and the 3× margin above has \
             lost the arithmetic that justified it. Restore 10× and update \
             quality-bars.md §1 footnote 3. plain {plain}, dense {dense}.",
            dense as f64 / plain as f64
        );
    }

    /// **Task 1.9g's cheap falsifier, and it needs no rig.**
    ///
    /// The measured freeze is `encode 218 ms of a 224 ms freeze`, and
    /// `289.6 − 218 = 71.6 ms` is under §1's 100 ms target — so if the encode
    /// goes near-free the row passes and the stronger rewrite (hand the WebView
    /// raw frames, deleting the encode *and* the decode from the display path)
    /// is unnecessary. This measures whether it can.
    ///
    /// **The roadmap's proposed first move does not exist.** Both the 1.9g row
    /// and `STATUS.md`'s next action say to drop *the `image` crate's* PNG
    /// compression to its fastest setting. This project does not depend on the
    /// `image` crate: `cargo tree -p up-take --target x86_64-pc-windows-msvc
    /// --edges normal` is 774 entries with zero matches for it. [`encode_png`]
    /// goes through `windows-capture`'s `ImageEncoder`, which wraps the WinRT
    /// `BitmapEncoder`, and that wrapper calls `CreateAsync` with **no encoding
    /// options at all** — there is no compression knob on the path we use.
    ///
    /// So the question becomes a *format* question, which costs nothing to ask
    /// because `Bmp` and `Jpeg` are already in the enum. Printed rather than
    /// asserted: this is a measurement that informs a design decision, and a
    /// threshold invented here would be a bar describing whatever was built.
    ///
    /// # What the numbers said, and the objection they remove
    ///
    /// PNG is 68-294 ms and strongly content-dependent. BMP is a flat 25 ms and
    /// is not, but costs 14.7 MB per monitor. **JPEG is the interesting one**:
    /// 26-37 ms, within a rounding error of BMP's speed, at 58 KB (PLAIN) to
    /// 3.3 MB (DENSE) — so it removes BMP's ~59 MB-per-freeze objection against
    /// §1's 80 MB idle-RAM row while keeping almost all of the speed.
    ///
    /// **Lossy is defensible here for one specific reason, and only that one.**
    /// This encode feeds the *display* path alone: [`crate::freeze::crop`] cuts
    /// the user's actual screenshot from `Still::bitmap`, the lossless RGBA, and
    /// never from these bytes. So artifacts would reach what the user *looks at*
    /// while selecting, and never what they get.
    ///
    /// **That is not the same as saying JPEG is fine, and the remaining risk is
    /// the one a table cannot settle.** Screen content is JPEG's worst case, not
    /// its best: ringing around high-contrast glyph edges and chroma
    /// subsampling smearing coloured text are exactly what a desktop full of
    /// small text produces. It is also unknown what quality level the WinRT
    /// encoder defaults to, because — as above — this wrapper passes it no
    /// options. **Judge it by eye on the rig against a text-heavy screen before
    /// adopting it**; that is a question for a person looking at a monitor, and
    /// nothing here answers it.
    ///
    /// Run with:
    /// `cargo test -p up-take --lib encode_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement, not a check: encodes four full-monitor buffers"]
    fn encode_cost_by_format_and_content() {
        // The rig's primary, so the figures are comparable with the freeze
        // lines rather than being a small-buffer proxy.
        let size = uptake_core::geometry::Size::new(2560, 1440);
        println!(
            "\n{:>7}  {:>7}  {:>12}  {:>10}",
            "screen", "format", "bytes", "ms"
        );
        for screen in ["plain", "blocks", "dense"] {
            let bitmap = test_screen(screen, size);
            for (label, format) in [
                ("png", ImageFormat::Png),
                ("bmp", ImageFormat::Bmp),
                ("jpeg", ImageFormat::Jpeg),
            ] {
                let started = std::time::Instant::now();
                let encoded = ImageEncoder::new(format, ImageEncoderPixelFormat::Rgba8)
                    .unwrap()
                    .encode(bitmap.pixels(), bitmap.width(), bitmap.height())
                    .unwrap();
                println!(
                    "{screen:>7}  {label:>7}  {:>12}  {:>10}",
                    encoded.len(),
                    started.elapsed().as_millis()
                );
            }
        }
        println!();
    }

    /// The six figures in `examples/testscreen/README.md` are still what this
    /// encoder produces.
    ///
    /// # Why this exists, and why it is not `#[ignore]`d
    ///
    /// `I-20` was closed by making that README the single home of the table.
    /// The first attempt at that shipped one hand-maintained copy fewer and an
    /// imperative sentence asking the next person to keep it current — which an
    /// independent review immediately falsified by finding a copy in `freeze.rs`
    /// that the sweep had missed. **A rule an agent has to remember had already
    /// failed before it was written down.**
    ///
    /// The backlog row rules out a `CL-` probe, correctly: `verify-claims.py`
    /// runs in the workspace repository and cannot reach this one. It does not
    /// rule out a check *here*, and this is the row's own second option — print
    /// it from the measurement and cite that — turned into an assertion.
    ///
    /// **This is not `F-35`'s check-that-cannot-fail-usefully.** It has a
    /// specific, named, expected failure: the WinRT/WIC encoder's default
    /// quality is not something this wrapper sets or knows, so a Windows codec
    /// update moves these numbers without anyone touching this repository. When
    /// it does, this goes red and names the file to correct, which is exactly
    /// the event the README says nothing else would catch.
    ///
    /// It runs in the ordinary suite rather than behind `--ignored` because
    /// three 2560×1440 buffers through two encoders measured **1.46 s** — a
    /// cost the sibling measurement pays only because it also times BMP, whose
    /// 14 MB of output buys no information here (all three screens encode
    /// identically, span 1.000).
    #[test]
    fn the_readme_table_is_what_the_encoder_still_produces() {
        let size = uptake_core::geometry::Size::new(2560, 1440);
        // Exactly the table in examples/testscreen/README.md, in its order.
        let expected = [
            ("plain", 17_895_usize, 58_225_usize),
            ("blocks", 316_483, 699_076),
            ("dense", 12_608_315, 3_304_252),
        ];
        for (screen, png, jpeg) in expected {
            let bitmap = test_screen(screen, size);
            for (label, format, want) in [
                ("PNG", ImageFormat::Png, png),
                ("JPEG", ImageFormat::Jpeg, jpeg),
            ] {
                let got = ImageEncoder::new(format, ImageEncoderPixelFormat::Rgba8)
                    .unwrap()
                    .encode(bitmap.pixels(), bitmap.width(), bitmap.height())
                    .unwrap()
                    .len();
                assert_eq!(
                    got, want,
                    "{screen} in {label} encodes to {got} bytes, not the {want}                      recorded in examples/testscreen/README.md. If the encoder                      changed under us, re-measure and correct THAT FILE -- it is                      the only home of this table, and every other mention of                      these numbers is a span or a ratio derived from it."
                );
            }
        }
    }

    /// The dev rig, in the same order and the same coordinates `freeze`'s tests
    /// use: a 2560x1440 primary, two 1920x1080 to its right, and a portrait
    /// 1080x1920 at a negative origin.
    ///
    /// Copied rather than shared because the two modules assert different rules
    /// against it and a shared fixture would couple them; the negative origin is
    /// the part that earns its keep in both.
    ///
    /// `MonitorInfo` rather than bare rectangles since `I-31`: the grab takes the
    /// capture crate's own enumeration now, and the handles are **deliberately
    /// not in position order**, so a scan that matched or sorted by handle would
    /// disagree with one that matches by containment.
    fn rig() -> Vec<uptake_capture::MonitorInfo> {
        use uptake_capture::MonitorInfo;
        use uptake_core::geometry::Rect;
        [
            (0x40, Rect::new(0, 0, 2560, 1440)),
            (0x10, Rect::new(2560, 0, 1920, 1080)),
            (0x30, Rect::new(4480, 0, 1920, 1080)),
            (0x20, Rect::new(-1080, 0, 1080, 1920)),
        ]
        .into_iter()
        .map(|(handle, bounds)| MonitorInfo::new(handle, bounds))
        .collect()
    }

    #[test]
    fn a_grab_takes_the_monitor_the_cursor_is_on() {
        use uptake_core::geometry::{Point, Rect};
        // Every monitor, not just a convenient one: picking the first would pass
        // against an implementation that always returns `monitors[0]`, which is
        // the shape `UT-F-44` records as a test that cannot go red.
        for monitor in rig() {
            let expected = monitor.bounds;
            let inside = Point::new(
                expected.origin.x + i32::try_from(expected.size.width).unwrap() / 2,
                expected.origin.y + i32::try_from(expected.size.height).unwrap() / 2,
            );
            assert_eq!(monitor_at(&rig(), Some(inside)), Ok(expected));
        }
        // The negative-origin monitor specifically, at a point that is *not* its
        // centre: a sign error in containment survives a centre-only test on a
        // symmetric layout.
        assert_eq!(
            monitor_at(&rig(), Some(Point::new(-1000, 1800))),
            Ok(Rect::new(-1080, 0, 1080, 1920))
        );
    }

    #[test]
    fn a_grab_declines_rather_than_guessing() {
        use uptake_core::geometry::Point;
        // A cursor that could not be read. The freeze widens to every monitor
        // here; a grab has one clipboard and no defensible widening, so it must
        // refuse. Asserted on the *behaviour*, not the message.
        assert!(monitor_at(&rig(), None).is_err());
        // The dead zone: below the portrait monitor's neighbours and outside all
        // four. Returning any monitor here is the see-one-thing-get-another
        // failure, so the refusal is the feature.
        assert!(monitor_at(&rig(), Some(Point::new(3000, 1300))).is_err());
        // And an empty enumeration, which is what `enumerate_monitors` returns
        // when `EnumDisplayMonitors` succeeds against an empty desktop.
        assert!(monitor_at(&[], Some(Point::new(0, 0))).is_err());
    }

    #[test]
    fn the_decline_message_names_what_was_looked_at() {
        use uptake_core::geometry::Point;
        // The only signal a rig operator gets when a grab does nothing, since
        // there is no on-screen acknowledgement yet. A message that said only
        // "no monitor" would leave them unable to tell a bad cursor read from a
        // bad monitor list.
        let error = monitor_at(&rig(), Some(Point::new(3000, 1300))).unwrap_err();
        assert!(error.contains("3000"), "{error}");
        assert!(error.contains("1300"), "{error}");
        // The count, with its noun. `contains('4')` was the first version and it
        // passed for 4, 14, 40 and a hardcoded literal alike: a check whose
        // falsifying input is hard to name is one of `A3`'s greens that could
        // not have been earned.
        assert!(error.contains("4 monitor(s)"), "{error}");
    }

    #[test]
    fn a_stage_line_names_the_monitor_and_the_byte_length() {
        use uptake_core::geometry::Rect;
        // UT-F-56: five grabs across three screens were indistinguishable in the
        // log because these two fields were absent. Each is asserted by the value
        // a reader would look for, so dropping one goes red here rather than on
        // the founder's next rig evening.
        let split = Split {
            capture_ms: 254,
            encode_ms: 38,
            publish_ms: 79,
            bounds: Some(Rect::new(0, 0, 2560, 1440)),
            encoded_bytes: 86_113,
            source: Source::Live,
        };
        let line = stage_line(&split);
        assert!(line.contains("2560x1440"), "no monitor size: {line}");
        assert!(line.contains("at (0, 0)"), "no monitor origin: {line}");
        assert!(line.contains("86113 bytes"), "no byte length: {line}");
        assert!(line.contains("capture 254 ms"), "{line}");
        assert!(line.contains("live capture"), "{line}");
    }

    #[test]
    fn a_stage_line_omits_the_rectangle_rather_than_inventing_one() {
        // The capture failed, so no rectangle is known. A placeholder would be
        // worse than the gap: a reader scanning the log for the monitor would
        // find one and believe it.
        let line = stage_line(&Split::default());
        assert!(!line.contains(" at ("), "invented a rectangle: {line}");
        assert!(line.contains("0 bytes"), "{line}");
    }

    #[test]
    fn bottom_up_bgra_flips_rows_and_swaps_red_and_blue() {
        // A 1x2 bitmap: top row red, bottom row green (top-down RGBA in).
        let bitmap = RgbaBitmap::from_pixels(
            uptake_core::geometry::Size::new(1, 2),
            vec![255, 0, 0, 255, 0, 255, 0, 128],
        )
        .unwrap();
        let out = bottom_up_bgra(&bitmap);
        // Bottom-up: the former bottom row (green) comes first now.
        assert_eq!(out, vec![0, 255, 0, 128, 0, 0, 255, 255]);
    }
}
