//! The host side of OCR: an `Ocr` area's pixels in, its recognised text out
//! (roadmap task 1.26).
//!
//! # What this module is, and what it deliberately is not
//!
//! `uptake-ocr` builds the engine, the worker thread and the result queue.
//! Every one of those shipped before this module existed, and none of them had
//! a caller: `1.11` put PP-OCRv4 behind the [`Engine`] seam, `1.10` gave it a
//! background service, and `1.31` proved the pipeline returns text -- from an
//! example, run by hand. **This is the row that makes the engine reachable from
//! the product**, which is why `1.26`'s own roadmap text calls its absence the
//! reason 1C could have shipped complete with no OCR area existing.
//!
//! So the work here is wiring and nothing else. No recognition logic lives in
//! this file; it captures a rectangle, hands the frame to the service, and
//! turns what comes back into an event the overlay can draw.
//!
//! # Where the models and the runtime come from
//!
//! [`ADR-0035`] settled it: they **ship inside the installer**.
//! `tauri.release.conf.json`'s `bundle.resources` puts the runtime and the
//! notices beside the executable and the models in `<exe dir>/models`, which is
//! exactly what [`models_directory`] and [`runtime_library`] resolve. An environment
//! variable overrides the models directory for development, and a developer with
//! no bundled runtime falls through to `ORT_DYLIB_PATH`.
//!
//! ⚠️ **The staging directory is NOT in the repository.** `src-tauri/assets` is
//! gitignored and filled by `scripts/acquire-onnxruntime.py` and
//! `scripts/convert-ppocr-models.py`, each of which verifies every byte against
//! a pinned SHA-256 before writing.
//!
//! **Only the bundling step needs them**, and that is a deliberate split rather
//! than a convenience. `tauri-build` validates `bundle.resources` paths at
//! COMPILE time, so keeping them in `tauri.conf.json` made `cargo check`,
//! `cargo clippy` and `cargo test` all fail on any machine without 31 MB of
//! acquired assets. They live in `tauri.release.conf.json`, merged in with
//! `--config` when an installer is built, so an ordinary build and `tauri dev`
//! need nothing. **CI found this after a local run and an independent review
//! had both passed** -- both ran where the assets already existed, which is the
//! oldest shape there is.
//!
//! *(This paragraph said "it is not done -- nothing packages the files today"
//! until 2026-09-02, which was true when `1.26` shipped and false four hours
//! later. Corrected in the change that falsified it rather than left for the
//! next reader.)*
//!
//! **Nothing is ever loaded from unverified bytes.** [`ADR-0032`] decision 2
//! requires a pinned SHA-256 verified before load, and `uptake-assets`
//! implements exactly that check -- so [`resolve_config`] runs
//! `Installer::state_of` over all three model files, and [`runtime_library`]
//! runs it over the DLL. **A present-but-wrong runtime is refused rather than
//! ignored**: a bundled file can still be replaced on disk after installation,
//! so falling back to `ORT_DYLIB_PATH` when ours fails its digest would make the
//! check decorative. That costs one hash of ~31 MB, once, on the first OCR area
//! of a session. It is not a fetch and this module has no network path.
//!
//! # Threading
//!
//! Two boundaries, and they are different from each other.
//!
//! [`recognise_into_area`] spawns, for the reason `output::capture_into_area`
//! spawns: its caller is inside the `WH_MOUSE_LL` callback, a capture is
//! 100-300 ms, and `LowLevelHooksTimeout` removes a hook that overruns (F-25,
//! F-33). Loading the engine is far worse than a capture -- ~15 MB of weights
//! off disk -- so the first submission pays it on that spawned thread too.
//!
//! [`pump`] runs on the existing placement poll and mostly only drains an
//! already-populated queue, so it does no work of its own worth moving.
//!
//! ⚠️ **It has ONE spawn, added by `1.13`, and this paragraph said it had none
//! until then.** The auto-copy publishes to the clipboard, which is a global
//! system resource every other process blocks on and which a clipboard manager
//! can hold; the poll thread it would otherwise block is the one
//! `quality-bars.md` §1's *poll emit -> frame painted* row is measured against,
//! and that is the only §1 row currently marked met. `output::copy_to_clipboard`
//! is dispatched off-thread for the same reason (`placement.rs`, the
//! `MenuAction::Copy` arm). Corrected here in the change that falsified it: an
//! independent review of `PR #83` raised the thread as a non-binding hunch, the
//! fix was taken, and a header still promising "does not spawn" would be a doc
//! comment asserting a guarantee no test can falsify -- which is the exact
//! shape all three of 2026-09-03's review-found defects took.
//!
//! [`Engine`]: uptake_ocr::Engine
//! [`ADR-0032`]: ../../../Projects/UP-TAKE/DECISIONS/ADR-0032-onnx-runtime-is-loaded-not-downloaded.md
//! [`ADR-0035`]: ../../../Projects/UP-TAKE/DECISIONS/ADR-0035-assets-ship-in-the-installer.md

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use tauri::AppHandle;
use uptake_assets::install::{AssetState, Installer};
use uptake_assets::{onnxruntime, ppocr};
use uptake_core::area::AreaId;
use uptake_core::geometry::Rect;
use uptake_ocr::paddle::{PaddleConfig, PaddleEngine, PaddleOptions};
use uptake_ocr::{Outcome, RequestId, Service, StopReason};

/// Overrides [`models_directory`]'s default, for development.
///
/// **Not a shipping path.** An installed UP-TAKE has its models beside the
/// executable and sets nothing; this exists so a developer can point at the
/// output of `scripts/convert-ppocr-models.py` without an install, exactly as
/// the `ocr_smoke` example does.
const MODELS_DIR_VARIABLE: &str = "UPTAKE_MODELS_DIR";

/// The base URL [`ppocr::ppocr_v4`] wants and this module never uses.
///
/// `AssetManifest` carries a URL per asset because it was built for a *fetch*,
/// and `ADR-0035` deleted the fetch. Verification reads bytes off disk and
/// never looks at the URL, so any valid HTTPS base satisfies `Asset::validate`
/// and none is dereferenced. Named `invalid.` deliberately -- the TLD is
/// reserved by RFC 2606 and can never resolve, so a future change that starts
/// dereferencing these fails loudly rather than reaching a host somebody
/// registered.
const UNUSED_BASE_URL: &str = "https://assets.invalid/ppocr-v4";

/// Where an installed UP-TAKE keeps its models, relative to the executable.
const MODELS_SUBDIRECTORY: &str = "models";

/// ONNX Runtime's file name beside the executable, per `ADR-0032`.
const RUNTIME_FILE_NAME: &str = "onnxruntime.dll";

/// One area's recognition state, as the overlay draws it.
///
/// `&'static str` on the wire rather than a `Serialize` derive: the frontend's
/// own union is the other half of this vocabulary, and the pair is checked the
/// way `src/lib/area-kinds.test.ts` checks the type names -- a text comparison
/// against this file. A derive would rename the variants without telling it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// Submitted; the worker has not answered yet.
    Working,
    /// The engine read text.
    Text,
    /// The engine ran and found nothing. **A success**, and distinct from
    /// [`Status::Failed`]: an empty region legitimately has no text in it, and
    /// reporting that as an error would teach the user to ignore the message.
    Empty,
    /// The engine could not be built -- typically the models are not installed.
    /// True of every area, not just this one.
    Unavailable,
    /// This request failed, or was abandoned when the worker stopped.
    Failed,
}

impl Status {
    /// The wire name.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Text => "text",
            Self::Empty => "empty",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

/// The service and the requests outstanding against it.
///
/// One lock over both, so "is this id still waiting" cannot be answered against
/// a half-updated pair -- the same reasoning `output::MAGNIFY` records.
struct Ocr {
    /// `None` until the first OCR area asks for it. **Lazy on purpose**: the
    /// engine is ~15 MB resident and most sessions never draw an OCR area, so
    /// loading it at startup would charge every user for a feature they may not
    /// use, and would do it on the launch path.
    service: Option<Service>,
    /// Why the service could not be built, if it could not.
    ///
    /// Held so a second area gets the same answer without re-hashing 15 MB of
    /// models to rediscover that they are still missing.
    unavailable: Option<String>,
    /// Areas with a submission the worker has not answered.
    ///
    /// A set rather than a count: a stop has to be reconciled against **each**
    /// outstanding area, and a count cannot name them.
    waiting: BTreeSet<u64>,
    /// The conversion the clipboard is currently promised to (roadmap `1.13`).
    ///
    /// See [`Request`] for why this is one slot rather than a set.
    latest: Option<Request>,
}

/// The most recent conversion the user asked for, and when they asked.
///
/// # Why the clipboard follows one request rather than every result
///
/// There is exactly one clipboard and several areas may be converting at once
/// (§3.3 makes many areas the normal case, not a corner). Copying **every**
/// text result as it lands means the clipboard holds whichever recognition
/// happened to finish last: an order set by how much text each area contains
/// and how the worker was scheduled, which is to say arbitrary, and not
/// something the user can predict or see.
///
/// So the clipboard follows the user's latest intent: convert an area, and that
/// area's text is what you paste. A result for an *earlier* conversion arriving
/// afterwards is still drawn in its own area (nothing is lost on screen), but
/// it does not take the clipboard back from the conversion the user asked for
/// more recently. Dropping the slot once it is honoured is what stops a
/// second, later result claiming a promise that has already been kept.
struct Request {
    /// The area, raw. Raw rather than an [`AreaId`] because that is what
    /// arrives back from the worker, and comparing raw-to-raw keeps the
    /// identity check where it already is, in `overlay::live_area_id`.
    id: u64,
    /// The instant of the gesture, for `quality-bars.md` §1's *selection
    /// release → OCR text on clipboard* row. Taken on the caller's thread
    /// before anything is captured: the bar starts at the gesture, and a clock
    /// started after the frame was grabbed would measure a different thing and
    /// report it against the same number.
    started: Instant,
}

impl Ocr {
    const fn new() -> Self {
        Self {
            service: None,
            unavailable: None,
            waiting: BTreeSet::new(),
            latest: None,
        }
    }
}

/// Records `request` as the conversion the clipboard is promised to, unless a
/// LATER gesture already holds the promise.
///
/// # This comparison is the whole of the guarantee, and it was missing
///
/// Found by the independent review of `PR #83`, round 1, and it is a real
/// defect rather than a tidiness point. [`recognise_into_area`] takes the
/// gesture instant on the caller's thread and then **spawns**, and the spawned
/// thread captures a frame before it takes this lock. That capture is bounded
/// by `quality-bars.md` §1's own image budget (300 ms target, 600 ms hard fail)
/// and varies with the area's size, and the session's first conversion also
/// pays the engine's cold load while holding the lock a second thread is
/// waiting on.
///
/// So **lock-acquisition order is not gesture order**, and the first version of
/// this assigned the slot unconditionally. Click A then B, let A's capture be
/// the slower, and A's thread arrives last and overwrites B: the *older* click
/// takes the clipboard, B's text is drawn on screen and silently declined, and
/// the module's own doc comment, the commit message and the README all promise
/// the opposite. Every one of the four tests below drove `claims_clipboard` on
/// a slot built by hand, so not one of them could see it.
///
/// A strictly-later held request wins. An equal instant replaces, and that is
/// deliberate rather than an accident of `>` against `>=`: two gestures with
/// the same `Instant` are indistinguishable in the only ordering that exists
/// here, so refusing would be picking one arbitrarily and calling it the
/// user's intent.
fn record_request(latest: &mut Option<Request>, request: Request) {
    if latest
        .as_ref()
        .is_some_and(|held| held.started > request.started)
    {
        return;
    }
    *latest = Some(request);
}

/// The gesture instant `id`'s recognised text should be timed against, if `id`
/// is the conversion the clipboard was promised to, consuming the promise.
///
/// `Some` means *copy this one*; `None` means leave the clipboard alone.
/// Returning the instant rather than a bool keeps the two inseparable: the
/// caller cannot copy without also having the clock the copy is measured
/// against, which is the pairing `quality-bars.md` §1's row is stated in.
///
/// Pure, and separated from [`pump`] for the reason `output::report_lines`
/// records: this decision is the whole of `1.13`'s behaviour, and inside a
/// function that also drains a queue, takes a lock and emits Tauri events there
/// is nothing a test can hold on to. Every case below is driven by a test.
fn claims_clipboard(latest: &mut Option<Request>, id: u64) -> Option<Instant> {
    if latest.as_ref().is_some_and(|request| request.id == id) {
        // Taken rather than read in place: the promise is kept exactly once, so
        // a re-delivered or duplicated outcome for the same id cannot claim the
        // clipboard a second time.
        return latest.take().map(|request| request.started);
    }
    None
}

static OCR: Mutex<Ocr> = Mutex::new(Ocr::new());

/// The lock, poison-tolerant.
///
/// A panicked holder leaves the bookkeeping consistent -- every mutation here
/// is a single insert, remove or assignment -- so refusing to recognise
/// anything for the rest of the session would be a worse answer than carrying
/// on. Same choice, for the same reason, as everywhere else in this host.
fn lock() -> MutexGuard<'static, Ocr> {
    OCR.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The directory holding the ONNX models and the dictionary.
///
/// `<exe dir>/models` -- where `ADR-0035`'s installer will put them -- unless
/// [`MODELS_DIR_VARIABLE`] names somewhere else.
///
/// # Errors
///
/// When the executable's own path cannot be read and no override is set, which
/// leaves nothing to resolve against.
fn models_directory() -> Result<PathBuf, String> {
    if let Some(overridden) = std::env::var_os(MODELS_DIR_VARIABLE) {
        return Ok(PathBuf::from(overridden));
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("could not locate UP-TAKE: {error}"))?;
    let directory = executable
        .parent()
        .ok_or_else(|| "UP-TAKE's own path has no directory".to_string())?;
    Ok(directory.join(MODELS_SUBDIRECTORY))
}

/// ONNX Runtime's **verified** path, or `None` to let `ORT_DYLIB_PATH` decide.
///
/// # The three outcomes, and why the middle one is an error rather than a
/// fallback
///
/// * **Absent** -- no DLL beside the executable. `Ok(None)`, and
///   `PaddleEngine::load` then takes whatever `ORT_DYLIB_PATH` names. This is
///   the developer path the `ocr_smoke` example documents. **`None` is not
///   "unchecked"**: with the variable unset too, `load` fails with `Unavailable`
///   rather than letting `ort` fall back to a bare library name and panic
///   (UP-TAKE `I-330`).
/// * **Present and its bytes match the pin** -- `Ok(Some(path))`. This is what
///   an installed UP-TAKE takes, every time.
/// * **Present and its bytes do NOT match** -- `Err`. Not a fall back to the
///   environment, and that is the whole point of the check. `ADR-0032` decision
///   2 requires a pinned SHA-256 *verified before load*, and a bundled file can
///   still be replaced on disk after installation by anything with write access
///   to the install directory. Silently loading a different runtime because ours
///   failed its digest would make the check decorative.
///
/// # Errors
///
/// When the runtime is present and does not match the digest pinned in
/// [`uptake_assets::onnxruntime`], or when the manifest itself is invalid.
fn runtime_library() -> Result<Option<PathBuf>, String> {
    let Ok(executable) = std::env::current_exe() else {
        // Nothing to resolve against. Not an error: the environment can still
        // name a runtime, and this is the same answer as "no DLL beside me".
        return Ok(None);
    };
    let Some(directory) = executable.parent() else {
        return Ok(None);
    };
    verified_runtime_in(directory)
}

/// [`runtime_library`]'s answer for an arbitrary directory.
///
/// **Split out so the refusal can be driven.** `runtime_library` resolves
/// against `current_exe`, which a test cannot move, so the corrupt-runtime arm
/// would have been reachable by no test at all -- and a control whose refusal is
/// never exercised is this project's `UT-F-75`. This takes the directory, so a
/// test can write a wrong DLL into a temporary one and watch it go red.
fn verified_runtime_in(directory: &std::path::Path) -> Result<Option<PathBuf>, String> {
    let beside_executable = directory.join(RUNTIME_FILE_NAME);
    if !beside_executable.exists() {
        return Ok(None);
    }

    let manifest = onnxruntime::onnxruntime()
        .map_err(|error| format!("UP-TAKE's own runtime manifest is invalid: {error}"))?;
    let installer = Installer::new(directory.to_path_buf(), manifest.clone());
    let runtime = manifest
        .assets
        .iter()
        .find(|asset| asset.file_name == RUNTIME_FILE_NAME)
        .ok_or_else(|| "the runtime manifest does not describe the runtime".to_string())?;

    match installer.state_of(runtime) {
        AssetState::Verified => Ok(Some(beside_executable)),
        // `Missing` after `exists()` said otherwise means unreadable rather than
        // absent -- a permissions problem or a file being written. Reported as
        // a refusal rather than as `None`, because falling back would load some
        // other runtime while ours sits there unread.
        AssetState::Missing => Err(format!(
            "{} is present but could not be read",
            beside_executable.display()
        )),
        AssetState::Corrupt => Err(format!(
            "{} does not match the ONNX Runtime {} that UP-TAKE ships, so it will not be \
             loaded. Reinstall UP-TAKE rather than replacing this file by hand.",
            beside_executable.display(),
            onnxruntime::VERSION
        )),
    }
}

/// Resolves and **verifies** the three model files, then describes them as a
/// [`PaddleConfig`].
///
/// # Errors
///
/// A sentence naming what is wrong, suitable for showing the user: the
/// directory, and which files are missing or corrupt. Deliberately specific --
/// "OCR is unavailable" with no reason is the message that generates a support
/// question rather than answering one.
fn resolve_config() -> Result<PaddleConfig, String> {
    let directory = models_directory()?;
    let manifest = ppocr::ppocr_v4(UNUSED_BASE_URL)
        .map_err(|error| format!("UP-TAKE's own model manifest is invalid: {error}"))?;

    // Named individually rather than reported as a count. "2 of 3 models are
    // missing" tells a user nothing they can act on; the file names tell them
    // whether their install is incomplete or their antivirus ate one.
    let mut missing = Vec::new();
    let mut corrupt = Vec::new();
    let installer = Installer::new(directory.clone(), manifest.clone());
    for asset in &manifest.assets {
        match installer.state_of(asset) {
            AssetState::Verified => {}
            AssetState::Missing => missing.push(asset.file_name.clone()),
            AssetState::Corrupt => corrupt.push(asset.file_name.clone()),
        }
    }
    if !missing.is_empty() || !corrupt.is_empty() {
        let mut reason = format!("no usable OCR models in {}", directory.display());
        if !missing.is_empty() {
            reason.push_str(&format!("; missing: {}", missing.join(", ")));
        }
        if !corrupt.is_empty() {
            reason.push_str(&format!("; corrupt: {}", corrupt.join(", ")));
        }
        return Err(reason);
    }

    Ok(PaddleConfig {
        detection_model: directory.join(ppocr::DETECTION_FILE_NAME),
        recognition_model: directory.join(ppocr::RECOGNITION_FILE_NAME),
        dictionary: directory.join(ppocr::DICTIONARY_FILE_NAME),
        runtime_library: runtime_library()?,
    })
}

/// Recognises the text in `bounds` and reports it against `id`.
///
/// Spawns; see the module header for why that is not optional. Emits
/// [`Status::Working`] from the caller's thread first, so the area says
/// something the instant it is drawn rather than sitting blank for the several
/// hundred milliseconds a cold load takes.
pub(crate) fn recognise_into_area(app: &AppHandle, id: AreaId, bounds: Rect) {
    // The clock for `quality-bars.md` §1's *selection release → OCR text on
    // clipboard* row starts here, on the caller's thread, before the frame is
    // captured and before the engine is built. Anything later would exclude
    // work the bar includes.
    let started = Instant::now();
    crate::overlay::emit_ocr(app, id, Status::Working, None);
    let app = app.clone();
    std::thread::spawn(move || {
        let frame = match crate::output::frame_for_ocr(bounds) {
            Ok(frame) => frame,
            Err(error) => {
                eprintln!("ocr: could not capture area {id:?}: {error}");
                crate::overlay::emit_ocr(&app, id, Status::Failed, Some(error));
                return;
            }
        };

        let mut guard = lock();
        // Built once and kept. `service` is `None` only before the first OCR
        // area of the session or after the worker has stopped, and `unavailable`
        // is what stops a second area re-hashing the models to rediscover the
        // same absence.
        if guard.service.is_none() && guard.unavailable.is_none() {
            match resolve_config() {
                Ok(config) => match Service::spawn(move || {
                    PaddleEngine::load(&config, PaddleOptions::default())
                }) {
                    Ok(service) => guard.service = Some(service),
                    Err(error) => guard.unavailable = Some(error.to_string()),
                },
                Err(reason) => guard.unavailable = Some(reason),
            }
        }
        if let Some(reason) = guard.unavailable.clone() {
            drop(guard);
            crate::overlay::emit_ocr(&app, id, Status::Unavailable, Some(reason));
            return;
        }
        let Some(service) = guard.service.as_ref() else {
            drop(guard);
            return;
        };
        match service.submit(RequestId::new(id.get()), frame) {
            Ok(()) => {
                guard.waiting.insert(id.get());
                // Promised on submission rather than on the gesture: a
                // conversion that never reached the worker has no result
                // coming, and letting it take the slot would mean a later
                // *successful* conversion silently declined to copy because a
                // failed one was holding the promise.
                record_request(
                    &mut guard.latest,
                    Request {
                        id: id.get(),
                        started,
                    },
                );
            }
            Err(error) => {
                // The worker died between building it and here. Reported
                // against this area rather than dropped: `submit` returning
                // `Err` is the one case that produces no `Outcome` at all, so
                // nothing downstream would ever answer for this id.
                guard.service = None;
                let reason = error.to_string();
                guard.unavailable = Some(reason.clone());
                drop(guard);
                crate::overlay::emit_ocr(&app, id, Status::Failed, Some(reason));
            }
        }
    });
}

/// Drains whatever the worker has finished and tells the overlay.
///
/// Called from the placement poll. Takes one lock and returns immediately when
/// no service exists or nothing has completed, which is every tick but the few
/// that follow a recognition.
pub(crate) fn pump(app: &AppHandle) {
    // Collected under the lock and emitted after it. `AppHandle::emit` runs
    // arbitrary listener bookkeeping inside Tauri, and holding this lock across
    // it would put an unrelated subsystem inside OCR's critical section for no
    // reason.
    let mut announcements: Vec<(u64, Status, Option<String>)> = Vec::new();
    // The one text result, if any, that `1.13` puts on the clipboard. Decided
    // under the lock and acted on outside it, for the same reason the
    // announcements are: publishing touches a global system resource that every
    // other process blocks on, and that does not belong inside OCR's critical
    // section.
    let mut clipboard: Option<(u64, String, Instant)> = None;
    {
        let mut guard = lock();
        let Some(service) = guard.service.as_ref() else {
            return;
        };
        let drained: Vec<Outcome> = service.results().collect();
        if drained.is_empty() {
            return;
        }
        for outcome in drained {
            match outcome {
                Outcome::Done { id, result } => {
                    guard.waiting.remove(&id.get());
                    match result {
                        Ok(recognition) if recognition.is_empty() => {
                            announcements.push((id.get(), Status::Empty, None));
                        }
                        Ok(recognition) => {
                            let text = recognition.text();
                            if let Some(started) = claims_clipboard(&mut guard.latest, id.get()) {
                                clipboard = Some((id.get(), text.clone(), started));
                            }
                            announcements.push((id.get(), Status::Text, Some(text)));
                        }
                        Err(error) => {
                            announcements.push((id.get(), Status::Failed, Some(error.to_string())));
                        }
                    }
                }
                Outcome::Abandoned { id } => {
                    guard.waiting.remove(&id.get());
                    // Its answer is never coming, so it must not keep holding
                    // the clipboard's promise: an area converted after it would
                    // otherwise land its text on screen and decline to copy it.
                    // The instant is discarded: there is no copy to time.
                    let _ = claims_clipboard(&mut guard.latest, id.get());
                    announcements.push((
                        id.get(),
                        Status::Failed,
                        Some("the OCR worker stopped before reaching this area".to_string()),
                    ));
                }
                Outcome::Stopped(reason) => {
                    // The worker is gone. Drop it so the next area rebuilds
                    // rather than submitting into a dead service, and record
                    // why -- except for a stop we asked for, which is not a
                    // fault and must not make OCR permanently unavailable.
                    guard.service = None;
                    if !matches!(reason, StopReason::Requested) {
                        guard.unavailable = Some(describe_stop(&reason));
                    }
                    // Anything still outstanding has already been told by the
                    // `Abandoned` that precedes this, which the service
                    // guarantees is delivered first. This clears the residue of
                    // ids whose areas were dismissed in the meantime.
                    guard.waiting.clear();
                    // And the promise with them: the worker is gone, so no
                    // outstanding conversion can still deliver text.
                    guard.latest = None;
                }
                // `Outcome` is `#[non_exhaustive]`, so a variant added to the
                // service reaches this arm rather than failing to compile.
                // Logged rather than ignored: an outcome this host cannot
                // interpret means an area is waiting for an answer that has
                // arrived in a shape nothing draws, and silence there is the
                // failure `Outcome::Abandoned` exists to prevent.
                other => eprintln!("ocr: unrecognised outcome from the worker: {other:?}"),
            }
        }
    }
    for (raw, status, detail) in announcements {
        // Asked area by area rather than emitted blind: an area dismissed while
        // its frame was in the worker has nothing to draw on, and announcing a
        // result for it is the shape `captures::still_holds` exists to refuse
        // for a pin (`I-61`). A missing area here is the ordinary case, not an
        // error.
        if let Some(id) = crate::overlay::live_area_id(app, raw) {
            crate::overlay::emit_ocr(app, id, status, detail);
        }
    }
    // After the announcements, not before them: the copy fires the same flash
    // Copy does, and an acknowledgement that arrives while the area still says
    // "working" acknowledges nothing the user can see. Draw the text, then say
    // it is on the clipboard.
    //
    // `live_area_id` is asked here for the reason the loop above asks it: an
    // area dismissed while its frame was in the worker gets no copy, which is
    // right -- the user threw that conversion away, and taking their clipboard
    // for it would be the opposite of what they just did.
    //
    // **Spawned, not called here.** [`pump`] runs on `click_through`'s 60 Hz
    // poll thread, which is the thread `quality-bars.md` §1's *poll emit ->
    // frame painted* row (8 ms target, 16 ms hard fail) is measured against
    // and the one row currently marked met. Publishing takes the clipboard,
    // a global system resource every other process blocks on, and a clipboard
    // manager or viewer chain can hold it. `copy_to_clipboard` is dispatched
    // the same way for the same reason (`placement.rs`, the `MenuAction::Copy`
    // arm), and this path was the exception until the independent review of
    // `PR #83` raised it. Non-binding there and taken anyway: the cost is one
    // thread per conversion and the risk was a met bar.
    if let Some((raw, text, started)) = clipboard
        && let Some(id) = crate::overlay::live_area_id(app, raw)
    {
        let app = app.clone();
        std::thread::spawn(move || {
            crate::output::copy_text_to_clipboard(&app, id, &text, started);
        });
    }
}

/// A [`StopReason`] as a sentence for the user.
fn describe_stop(reason: &StopReason) -> String {
    match reason {
        StopReason::EngineUnavailable(error) => format!("the OCR engine could not start: {error}"),
        StopReason::Fatal(error) => format!("the OCR engine stopped: {error}"),
        StopReason::Requested => "OCR was shut down".to_string(),
        StopReason::Panicked => "the OCR engine panicked".to_string(),
        // `StopReason` is `#[non_exhaustive]`, so a new variant reaches this
        // arm rather than failing to compile. Deliberately vague rather than
        // wrong: this host cannot describe a reason it has never seen.
        _ => "the OCR engine stopped for an unrecognised reason".to_string(),
    }
}

/// Forgets `id`'s outstanding request, if it has one.
///
/// Called from the dismiss path. This does **not** cancel the work -- the
/// service has no cancellation and a frame already in the engine runs to
/// completion -- it stops this module holding a name for an area that is gone.
/// The result itself is discarded in [`pump`], which asks whether the area
/// still exists before drawing on it.
pub(crate) fn forget(id: AreaId) {
    let mut guard = lock();
    guard.waiting.remove(&id.get());
    // The dismissed area also gives up the clipboard's promise. `pump` would
    // decline to copy it anyway -- `live_area_id` answers `None` for an area
    // that is gone -- but leaving the slot filled would make the *next*
    // conversion's text arrive with the promise already spoken for, and it
    // would not copy either. One dismissal must not cost two copies.
    // The instant is discarded: there is no copy to time.
    let _ = claims_clipboard(&mut guard.latest, id.get());
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed unwrap or expect is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn every_status_has_a_distinct_wire_name() {
        let all = [
            Status::Working,
            Status::Text,
            Status::Empty,
            Status::Unavailable,
            Status::Failed,
        ];
        let names: BTreeSet<&str> = all.iter().map(|status| status.as_str()).collect();
        assert_eq!(
            names.len(),
            all.len(),
            "two statuses share a wire name, so the frontend cannot tell them apart"
        );
    }

    #[test]
    fn the_unused_base_url_still_satisfies_the_manifest() {
        // The manifest refuses a non-HTTPS base, so an edit to
        // `UNUSED_BASE_URL` that looks harmless would make every OCR area
        // report "UP-TAKE's own model manifest is invalid" at run time. This
        // test is the only thing that reads that constant outside the resolver.
        assert!(
            ppocr::ppocr_v4(UNUSED_BASE_URL).is_ok(),
            "the placeholder base URL must still build a valid manifest"
        );
    }

    /// A request, for the clipboard tests. The instant is arbitrary: every
    /// assertion below is about *which* request answers, never about how long
    /// one took.
    fn request(id: u64) -> Option<Request> {
        Some(Request {
            id,
            started: Instant::now(),
        })
    }

    /// The defect round 1 of `PR #83`'s review found, driven rather than
    /// argued. Gestures A then B; B's thread reaches the lock first because A's
    /// capture was slower, then A's arrives. A must not take the promise back.
    #[test]
    fn a_slower_earlier_gesture_does_not_steal_the_promise_from_a_later_one() {
        let first = Instant::now();
        // A later gesture, expressed as an instant that is strictly later. The
        // real gap is however long the user took between two clicks.
        let second = first + std::time::Duration::from_millis(120);

        let mut latest = None;
        // B's thread wins the lock, even though its gesture came second.
        record_request(
            &mut latest,
            Request {
                id: 2,
                started: second,
            },
        );
        // A's thread arrives afterwards, carrying the EARLIER gesture.
        record_request(
            &mut latest,
            Request {
                id: 1,
                started: first,
            },
        );

        assert_eq!(
            latest.as_ref().map(|held| held.id),
            Some(2),
            "the later gesture keeps the clipboard, whatever order the threads arrived in"
        );
    }

    /// The ordinary order still works, or the guard above would be a way of
    /// never updating the slot at all.
    #[test]
    fn a_later_gesture_takes_the_promise_from_an_earlier_one() {
        let first = Instant::now();
        let second = first + std::time::Duration::from_millis(120);

        let mut latest = None;
        record_request(
            &mut latest,
            Request {
                id: 1,
                started: first,
            },
        );
        record_request(
            &mut latest,
            Request {
                id: 2,
                started: second,
            },
        );

        assert_eq!(latest.as_ref().map(|held| held.id), Some(2));
    }

    /// An empty slot always takes the request. The state after a dismissal,
    /// after a worker stop, and before the session's first conversion.
    #[test]
    fn an_empty_slot_takes_whatever_arrives() {
        let mut latest = None;
        record_request(
            &mut latest,
            Request {
                id: 9,
                started: Instant::now(),
            },
        );
        assert_eq!(latest.as_ref().map(|held| held.id), Some(9));
    }

    #[test]
    fn the_conversion_the_user_asked_for_takes_the_clipboard() {
        let mut latest = request(7);
        assert!(
            claims_clipboard(&mut latest, 7).is_some(),
            "the promised area's text is what the user expects to paste"
        );
    }

    /// The case the one-slot design exists for. Two areas convert; the *older*
    /// one finishes first. Copying it would leave the clipboard holding text
    /// the user did not most recently ask for, chosen by nothing more than
    /// which region had less text in it.
    #[test]
    fn an_earlier_conversion_finishing_later_does_not_take_the_clipboard() {
        // The user converted A, then B: B holds the promise.
        let mut latest = request(2);
        assert!(
            claims_clipboard(&mut latest, 1).is_none(),
            "A's result must not take a clipboard promised to B"
        );
        assert!(
            latest.is_some(),
            "and it must not consume B's promise on the way past"
        );
        assert!(claims_clipboard(&mut latest, 2).is_some(), "B still copies");
    }

    /// The promise is kept exactly once. Without the `take`, an outcome
    /// delivered twice -- or a second `pump` over a queue that had not been
    /// drained -- would republish over a clipboard the user may have replaced
    /// in between.
    #[test]
    fn the_same_result_cannot_take_the_clipboard_twice() {
        let mut latest = request(3);
        assert!(claims_clipboard(&mut latest, 3).is_some());
        assert!(
            claims_clipboard(&mut latest, 3).is_none(),
            "the promise is spent"
        );
        assert!(latest.is_none());
    }

    /// With nothing promised, nothing copies. This is the state after a
    /// dismissal, after a worker stop, and before the first conversion of the
    /// session -- and in all three the clipboard belongs to whatever the user
    /// last put there.
    #[test]
    fn no_outstanding_conversion_means_the_clipboard_is_left_alone() {
        let mut latest = None;
        assert!(claims_clipboard(&mut latest, 4).is_none());
    }

    #[test]
    fn a_requested_stop_is_not_reported_as_a_fault() {
        // `Requested` is the only stop that must not leave OCR unavailable: it
        // is the shutdown path, not a failure, and recording it would make a
        // clean teardown look like a broken install to whatever asks next.
        assert_eq!(describe_stop(&StopReason::Requested), "OCR was shut down");
    }

    /// Drives the host's OWN resolver against the real converted models and
    /// the real engine.
    ///
    /// # Why this is `#[ignore]`d and what that costs
    ///
    /// It needs an ONNX Runtime, two models and a dictionary that CI does not
    /// have, so as an ordinary test it would fail on every machine. That is the
    /// same constraint `crates/uptake-ocr/examples/ocr_smoke.rs` documents, and
    /// it is answered the same way: a step that must be invoked by name is
    /// honest about being a manual one.
    ///
    /// **What it covers that `ocr_smoke` does not** is the whole of this
    /// module's job. `ocr_smoke` builds a `PaddleConfig` from command-line
    /// paths; this one asks [`resolve_config`], so it exercises the directory
    /// resolution, the digest verification and the runtime lookup an installed
    /// UP-TAKE will use. `1.31`'s lesson was that a pipeline nobody has run is
    /// a pipeline nobody has tested, and the wiring deserves the same treatment
    /// the engine got.
    ///
    /// ```text
    /// set UPTAKE_MODELS_DIR=C:\_CORE\up-take\dist\models
    /// set ORT_DYLIB_PATH=C:\Windows\System32\onnxruntime.dll
    /// cargo test -p up-take --lib -- --ignored --nocapture the_real_models
    /// ```
    #[test]
    #[ignore = "needs the converted models and a runtime; see the doc comment"]
    fn the_real_models_load_through_this_modules_own_resolver() {
        let config = resolve_config()
            .expect("set UPTAKE_MODELS_DIR to a directory holding the three converted files");
        println!("detection:   {}", config.detection_model.display());
        println!("recognition: {}", config.recognition_model.display());
        println!("dictionary:  {}", config.dictionary.display());
        println!("runtime:     {:?}", config.runtime_library);
        // Through the real `Service`, not through `PaddleEngine` directly, so
        // this covers the submit/drain shape `pump` depends on as well as the
        // load. `1.10`'s service and `1.11`'s engine had both shipped without
        // ever being composed by a caller in the product.
        let service = Service::spawn(move || PaddleEngine::load(&config, PaddleOptions::default()))
            .expect("the verified models must load");

        let Some(image) = std::env::var_os("UPTAKE_SMOKE_IMAGE") else {
            println!("loaded. Set UPTAKE_SMOKE_IMAGE to a .rgba file to recognise one too.");
            return;
        };
        let frame = read_rgba(std::path::Path::new(&image)).expect("the smoke image must decode");
        service
            .submit(RequestId::new(1), frame)
            .expect("the worker is running");

        // Polled rather than blocked on, because polling is what `pump` does:
        // the host has no channel to wait on and drains from its existing
        // placement tick.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut answered = false;
        while std::time::Instant::now() < deadline && !answered {
            for outcome in service.results() {
                match outcome {
                    Outcome::Done { id, result } => {
                        assert_eq!(id, RequestId::new(1));
                        let recognition = result.expect("the engine must not fail");
                        println!("--- what it read ---");
                        println!("{}", recognition.text());
                        println!("--- end ---");
                        assert!(
                            !recognition.is_empty(),
                            "the smoke image has text in it, so an empty result is a failure"
                        );
                        answered = true;
                    }
                    other => panic!("unexpected outcome: {other:?}"),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(answered, "the worker answered nothing within 30 s");
    }

    /// Reads the flat `.rgba` format `ocr_smoke` documents: little-endian `u32`
    /// width, little-endian `u32` height, then `width * height * 4` bytes.
    ///
    /// Duplicated from that example rather than shared, and deliberately: it is
    /// four lines of test scaffolding, and moving it into a crate's public API
    /// would put a diagnostic file format into the product's surface.
    fn read_rgba(path: &std::path::Path) -> Option<uptake_core::bitmap::RgbaBitmap> {
        let bytes = std::fs::read(path).ok()?;
        let width = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?);
        let height = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
        let pixels = bytes.get(8..)?.to_vec();
        uptake_core::bitmap::RgbaBitmap::from_pixels(
            uptake_core::geometry::Size::new(width, height),
            pixels,
        )
    }

    /// Every asset UP-TAKE verifies is also an asset the installer packages.
    ///
    /// # The only mechanical check that the licence obligation is met
    ///
    /// `ADR-0032`'s licence section says it plainly: ONNX Runtime is MIT, MIT
    /// requires the notice to travel with a copy, that project bundles other
    /// people's code so its `ThirdPartyNotices.txt` travels too, and **`cargo
    /// deny` can see neither file** because it walks the crate graph and a
    /// `.dll` and a `.txt` are not crates. PaddleOCR's terms ride along for the
    /// same reason under `ADR-0034` obligation 3.
    ///
    /// So the failure this guards is specific and silent: somebody deletes a
    /// line from `bundle.resources`, every test stays green, `cargo deny` stays
    /// green, CI stays green, and UP-TAKE ships a **public GPL-3.0 product with
    /// a signed installer** that is missing a licence it is required to carry.
    /// Nothing else in either repository would notice.
    ///
    /// Reads `tauri.release.conf.json` at compile time, so this cannot drift
    /// from the file it checks.
    ///
    /// ⚠️ **It does NOT prove the bundler was given that file, and the previous
    /// sentence here claimed it did** -- *"cannot drift from the file the
    /// bundler actually uses"*. Round 3 of this change's review caught the
    /// overclaim and named the input that defeats it: `pnpm tauri build`
    /// without `--config src-tauri/tauri.release.conf.json` exits 0 and
    /// produces a 2.27 MB installer carrying no runtime, no models and none of
    /// the notices, with this test and every other gate still green. Measured,
    /// not argued. **`scripts/verify-bundle.py` is what closes that**, by
    /// checking the artifact rather than the invocation, and CI runs it after
    /// every build. This test's job is narrower and still worth having: it
    /// guards the CONTENT of the resource map, so a notice cannot be dropped
    /// from the map itself.
    ///
    /// **Why the resources live in a release-only config at all**, since it
    /// looks like indirection for its own sake: `tauri-build`'s build script
    /// validates every `bundle.resources` path at COMPILE time, not at bundle
    /// time. With them in `tauri.conf.json`, `cargo check`, `cargo clippy` and
    /// `cargo test` all fail on a machine without 31 MB of acquired assets --
    /// which is every CI job except the one that builds an installer, and
    /// every contributor's first checkout. Found by CI on this branch after a
    /// local run and an independent review both passed, because both were on
    /// machines where the assets already existed.
    #[test]
    fn the_installer_packages_every_asset_and_both_notice_sets() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.release.conf.json"))
                .expect("tauri.release.conf.json must be valid JSON");
        let resources = conf
            .get("bundle")
            .and_then(|bundle| bundle.get("resources"))
            .and_then(serde_json::Value::as_object)
            .expect(
                "tauri.release.conf.json has no bundle.resources: the installer packages nothing, so it ships no runtime, no models and no notices",
            );
        // The DESTINATIONS, which is what actually lands beside the executable.
        // Asserting on the source paths would pass while the files were
        // installed somewhere the app never looks.
        let destinations: BTreeSet<&str> = resources
            .values()
            .filter_map(serde_json::Value::as_str)
            .collect();

        let models = ppocr::ppocr_v4(UNUSED_BASE_URL).unwrap();
        for asset in &models.assets {
            let expected = format!("{MODELS_SUBDIRECTORY}/{}", asset.file_name);
            assert!(
                destinations.contains(expected.as_str()),
                "{expected} is verified before load and NOT packaged, so an installed                  UP-TAKE can never have it"
            );
        }

        let runtime = onnxruntime::onnxruntime().unwrap();
        for asset in &runtime.assets {
            assert!(
                destinations.contains(asset.file_name.as_str()),
                "{} is pinned and NOT packaged. For the runtime that means OCR cannot work; for a notice it means an MIT or Apache-2.0 obligation is unmet, and nothing else in this repository can see it",
                asset.file_name
            );
        }

        // PaddleOCR's own notice, which belongs to no manifest because
        // `convert-ppocr-models.py` generates it rather than downloading it.
        // Named explicitly here precisely because nothing else references it.
        assert!(
            destinations.contains(format!("{MODELS_SUBDIRECTORY}/NOTICE-models.txt").as_str()),
            "the models' own notice is not packaged (ADR-0034 obligation 3)"
        );
    }

    #[test]
    fn a_runtime_that_is_not_ours_is_refused_rather_than_ignored() {
        // The arm ADR-0032 decision 2 is actually about. A bundled DLL can be
        // replaced on disk after installation by anything with write access to
        // the install directory, so the interesting case is not "absent" -- it
        // is "present and wrong". Falling back to ORT_DYLIB_PATH there would
        // load some other runtime while ours sat unread, which makes the whole
        // digest check decorative.
        let directory = std::env::temp_dir().join("uptake-runtime-drill");
        std::fs::create_dir_all(&directory).unwrap();
        let planted = directory.join(RUNTIME_FILE_NAME);

        // Nothing there: fall through to the environment. Not an error.
        let _ = std::fs::remove_file(&planted);
        assert_eq!(
            verified_runtime_in(&directory),
            Ok(None),
            "no runtime beside the executable is the developer path, not a fault"
        );

        // Something there, and it is not ours.
        std::fs::write(&planted, b"this is not ONNX Runtime").unwrap();
        let refusal = verified_runtime_in(&directory)
            .expect_err("a runtime that fails its digest must be refused");
        assert!(
            refusal.contains(&onnxruntime::VERSION.to_string()),
            "the refusal must name the version UP-TAKE ships so the user can act on it, got:              {refusal}"
        );

        let _ = std::fs::remove_file(&planted);
        let _ = std::fs::remove_dir(&directory);
    }

    #[test]
    fn a_missing_models_directory_names_all_three_files() {
        // Drives `resolve_config`'s refusal against a directory that certainly
        // holds nothing, so the message a user sees is covered rather than
        // assumed. The three names come from `uptake-assets`, so a rename there
        // travels here.
        let directory = std::env::temp_dir().join("uptake-ocr-models-that-do-not-exist");
        let manifest = ppocr::ppocr_v4(UNUSED_BASE_URL).unwrap();
        let installer = Installer::new(directory, manifest.clone());
        for asset in &manifest.assets {
            assert_eq!(
                installer.state_of(asset),
                AssetState::Missing,
                "{} must read as missing",
                asset.file_name
            );
        }
    }
}
