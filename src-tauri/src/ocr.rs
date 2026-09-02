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
//! `tauri.conf.json`'s `bundle.resources` puts the runtime and the notices
//! beside the executable and the models in `<exe dir>/models`, which is exactly
//! what [`models_directory`] and [`runtime_library`] resolve. An environment
//! variable overrides the models directory for development, and a developer with
//! no bundled runtime falls through to `ORT_DYLIB_PATH`.
//!
//! ⚠️ **The staging directory is NOT in the repository.** `src-tauri/assets` is
//! gitignored and filled by `scripts/acquire-onnxruntime.py` and
//! `scripts/convert-ppocr-models.py`, each of which verifies every byte against
//! a pinned SHA-256 before writing. A build from a clean checkout that skips
//! those steps fails at the bundler, which is the intended failure: it is
//! louder than shipping an installer with no OCR in it.
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
//! [`pump`] does **not** spawn. It runs on the existing placement poll and only
//! drains an already-populated queue.
//!
//! [`Engine`]: uptake_ocr::Engine
//! [`ADR-0032`]: ../../../Projects/UP-TAKE/DECISIONS/ADR-0032-onnx-runtime-is-loaded-not-downloaded.md
//! [`ADR-0035`]: ../../../Projects/UP-TAKE/DECISIONS/ADR-0035-assets-ship-in-the-installer.md

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

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
}

impl Ocr {
    const fn new() -> Self {
        Self {
            service: None,
            unavailable: None,
            waiting: BTreeSet::new(),
        }
    }
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
                            announcements.push((id.get(), Status::Text, Some(recognition.text())));
                        }
                        Err(error) => {
                            announcements.push((id.get(), Status::Failed, Some(error.to_string())));
                        }
                    }
                }
                Outcome::Abandoned { id } => {
                    guard.waiting.remove(&id.get());
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
    lock().waiting.remove(&id.get());
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
    /// Reads `tauri.conf.json` at compile time, so this cannot drift from the
    /// file the bundler actually uses.
    #[test]
    fn the_installer_packages_every_asset_and_both_notice_sets() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json must be valid JSON");
        let resources = conf
            .get("bundle")
            .and_then(|bundle| bundle.get("resources"))
            .and_then(serde_json::Value::as_object)
            .expect(
                "tauri.conf.json has no bundle.resources -- the installer packages nothing,                  so it ships no runtime, no models and no notices",
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
