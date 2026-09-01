//! PP-OCRv4 behind the [`crate::engine::Engine`] seam -- roadmap 1.11.
//!
//! `architecture.md` section 3.2's pipeline, one module per stage:
//!
//! ```text
//! RGBA frame -> preprocess -> detection -> boxes -> recognition -> reading order
//! ```
//!
//! | Stage | Module | Needs a model? |
//! | --- | --- | --- |
//! | resize + normalise | [`preprocess`] | no |
//! | probability map -> quads | [`detect`] | no |
//! | geometry the above rests on | [`quad`] | no |
//! | crop, rectify, CTC decode | [`recognise`] | no |
//! | reading order, whitespace | [`reading_order`] | no |
//! | running the two sessions | this file | **yes** |
//!
//! **Five of the six stages are pure**, which is the point of the split: the
//! arithmetic that decides where a box sits and what the model said is tested in
//! CI with no ONNX Runtime and no model file, and only session management here
//! needs either. That is what makes 1.11 reviewable without a 25 MB fixture.

pub mod detect;
pub mod preprocess;
pub mod quad;
pub mod reading_order;
pub mod recognise;

use std::path::{Path, PathBuf};

use ort::session::Session;
use ort::value::TensorRef;
use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

use crate::engine::{Engine, EngineError, Recognition, TextBlock};
use detect::{DetectorOptions, ProbabilityMap};
use reading_order::Placed;
use recognise::{CharacterDictionary, DecodedText};

/// Where the models and the runtime live, and how the pipeline is tuned.
///
/// Paths rather than bytes: `architecture.md` section 3.2 says *"models load
/// once at startup and stay resident"*, and the loading happens on the worker
/// thread inside [`crate::service::Service::spawn`]'s closure, so this struct
/// crosses the thread boundary and the 15 MB of weights do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddleConfig {
    /// The DB text-detection model, ONNX.
    pub detection_model: PathBuf,
    /// The CRNN text-recognition model, ONNX.
    pub recognition_model: PathBuf,
    /// The character dictionary the recogniser's classes index into.
    pub dictionary: PathBuf,
    /// ONNX Runtime itself.
    ///
    /// `None` means "take whatever `ORT_DYLIB_PATH` names", which the host must
    /// then have set. **`None` does NOT mean unchecked** -- if the variable is
    /// also unset, [`PaddleEngine::load`] fails with `Unavailable` rather than
    /// letting `ort` fall back to a bare library name and panic. Naming the path
    /// explicitly is what an installed UP-TAKE does, since ADR-0032 makes the
    /// DLL a file **we** place, and it also cross-checks the host's variable
    /// against this engine's expectation.
    pub runtime_library: Option<PathBuf>,
}

/// Tuning that is not a path. Separated so a caller can take the defaults
/// without naming them, which is the common case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaddleOptions {
    /// DB post-processing thresholds.
    pub detector: DetectorOptions,
    /// Cap on the detector's input side length.
    pub limit_side_len: u32,
    /// Recognitions below this confidence are dropped.
    ///
    /// PP-OCR's `drop_score`. A low-confidence line is usually an artifact --
    /// a button border, a window edge -- and pasting it into the user's
    /// clipboard as though it were text is worse than omitting it.
    pub drop_score: f32,
}

impl Default for PaddleOptions {
    fn default() -> Self {
        Self {
            detector: DetectorOptions::default(),
            limit_side_len: preprocess::DEFAULT_LIMIT_SIDE_LEN,
            drop_score: 0.5,
        }
    }
}

/// PP-OCRv4, loaded and resident.
///
/// Constructed on the worker thread and never moved off it -- see
/// [`crate::engine::Engine`]'s note on `&mut self`.
#[derive(Debug)]
pub struct PaddleEngine {
    detection: Session,
    recognition: Session,
    dictionary: CharacterDictionary,
    options: PaddleOptions,
}

impl PaddleEngine {
    /// Loads both models and the dictionary.
    ///
    /// # Errors
    ///
    /// [`EngineError::Unavailable`] if the runtime, either model, or the
    /// dictionary cannot be loaded. Every one of those is fatal to the service
    /// rather than to one frame, which is what `Unavailable` means.
    ///
    /// # Why the runtime is checked before `ort` is touched
    ///
    /// ⚠️ **`ort`'s `load-dynamic` feature PANICS when the library is missing.**
    /// It resolves the dylib lazily, inside a `OnceLock` initialiser, with
    /// `.expect("Failed to load ONNX Runtime dylib")` -- read at
    /// `ort-2.0.0-rc.13/src/lib.rs:234` rather than assumed. So the first call
    /// that touches the ONNX API unwinds, and no `Result` anywhere in this
    /// crate ever sees it.
    ///
    /// *(That citation said `:224` until 2026-08-30 and was wrong by ten lines:
    /// 224 opens the `match` that picks the path, 234 is the `.expect` that
    /// panics. The claim was right and the reference was not, and the wrong
    /// number had already reached four documents before an independent review
    /// read the vendored source and caught it.)*
    ///
    /// **ADR-0032 decision 3 says a failure to find or load the runtime is "an
    /// ordinary run-time error, surfaced as `EngineError::Unavailable`", and
    /// that it "is already built and tested". The first half is the intent; the
    /// second half is not true by itself** -- what is built and tested is
    /// [`crate::service`]'s handling of an engine that *returns* that error, and
    /// a panic takes the other path, [`crate::service::StopReason::Panicked`].
    /// The service survives both (a `Drop` guard, not `catch_unwind`), so this
    /// is a wrong *outcome*, not a lost session.
    ///
    /// The check below closes the common case -- no runtime installed -- by
    /// looking for the file before any ONNX call happens, so it reports
    /// `Unavailable` exactly as the decision says. **It runs on every path,
    /// including `runtime_library: None`**, which is the one that would
    /// otherwise reach `ort`'s fallback to a bare library name and panic there.
    /// *(It was skipped on that path until 2026-08-30 -- found by an independent
    /// review, which noted this comment claimed the common case was closed while
    /// the config shape that reproduces it went unchecked.)*
    ///
    /// **It does not close every case:** a present-but-corrupt or
    /// wrong-architecture DLL still panics inside `ort`, and only `ort` gaining
    /// a fallible initialiser fixes that.
    pub fn load(config: &PaddleConfig, options: PaddleOptions) -> Result<Self, EngineError> {
        // Unconditional, and that is the point: every way of reaching `ort`
        // without a resolvable runtime ends in a panic, so every way of reaching
        // it has to pass through here first.
        resolve_runtime(
            config.runtime_library.as_deref(),
            std::env::var_os(DYLIB_PATH_VAR)
                .map(PathBuf::from)
                .as_deref(),
            &|path: &Path| path.is_file(),
        )?;

        let dictionary_text = std::fs::read_to_string(&config.dictionary).map_err(|error| {
            EngineError::Unavailable(format!(
                "could not read the character dictionary at {}: {error}",
                config.dictionary.display()
            ))
        })?;
        // The LITERAL reading is what the emptiness guard is asked about, and the
        // order is load-bearing: `from_ppocr_dictionary` appends a space
        // unconditionally, so a dictionary built from an empty file is never
        // `is_empty()` and this guard would be dead code that reports the
        // failure much later, as a class-count mismatch of 2 against 6625.
        if CharacterDictionary::from_lines(&dictionary_text).is_empty() {
            return Err(EngineError::Unavailable(format!(
                "the character dictionary at {} is empty",
                config.dictionary.display()
            )));
        }
        // PP-OCR's own reading: the file's lines, then an appended space, then
        // the blank at index 0. See `CharacterDictionary::from_ppocr_dictionary`
        // for the arithmetic and the citation -- reading the file literally is
        // one class short of every PP-OCRv4 recogniser (UP-TAKE `I-333`).
        let dictionary = CharacterDictionary::from_ppocr_dictionary(&dictionary_text);

        let detection = open_session(&config.detection_model, "detection")?;
        let recognition = open_session(&config.recognition_model, "recognition")?;

        Ok(Self {
            detection,
            recognition,
            dictionary,
            options,
        })
    }

    /// Runs the detector and returns the boxes it found, in frame coordinates.
    fn detect(&mut self, frame: &RgbaBitmap) -> Result<Vec<detect::DetectedBox>, EngineError> {
        let Some(input) = preprocess::detector_input(frame, self.options.limit_side_len) else {
            return Ok(Vec::new());
        };
        let shape: Vec<i64> = input.shape().iter().map(|&value| value as i64).collect();
        let tensor = TensorRef::from_array_view((shape, input.tensor.as_slice()))
            .map_err(|error| EngineError::Inference(format!("detector input rejected: {error}")))?;

        let outputs = self
            .detection
            .run(ort::inputs![tensor])
            .map_err(|error| EngineError::Inference(format!("detection failed: {error}")))?;
        let (output_shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|error| EngineError::Inference(format!("detector output: {error}")))?;

        // DB emits [batch, 1, height, width]; the last two dimensions are the
        // map. Read them from the tensor rather than assuming they match the
        // input: a model whose stride differs would otherwise be indexed wrong,
        // silently and plausibly.
        let dimensions = output_shape.as_ref();
        let (map_height, map_width) = match dimensions {
            [.., height, width] => (*height as usize, *width as usize),
            _ => {
                return Err(EngineError::Inference(format!(
                    "detector returned a {}-dimensional output; expected at least 2",
                    dimensions.len()
                )));
            }
        };

        let map = ProbabilityMap {
            data,
            width: map_width,
            height: map_height,
        };
        // The map may be a different size from the tensor we sent, so the ratio
        // home is computed against the MAP, not against the resize. One rule,
        // defined and tested in `preprocess`; this used to be a second copy of
        // it, and the copy `DetectorInput` carried was the dead one.
        let (scale_x, scale_y) =
            preprocess::scale_to_source(frame.width(), frame.height(), map_width, map_height);

        Ok(detect::boxes_from_map(
            &map,
            self.options.detector,
            scale_x,
            scale_y,
            frame.width() as f32,
            frame.height() as f32,
        ))
    }

    /// Runs the recogniser over one crop.
    fn recognise_crop(
        &mut self,
        frame: &RgbaBitmap,
        quad: &quad::Quad,
    ) -> Result<Option<DecodedText>, EngineError> {
        let Some(crop) = recognise::rectify(frame, quad) else {
            return Ok(None);
        };
        let shape: Vec<i64> = crop.shape().iter().map(|&value| value as i64).collect();
        let tensor = TensorRef::from_array_view((shape, crop.tensor.as_slice()))
            .map_err(|error| EngineError::Inference(format!("crop rejected: {error}")))?;

        let outputs = self
            .recognition
            .run(ort::inputs![tensor])
            .map_err(|error| EngineError::Inference(format!("recognition failed: {error}")))?;
        let (output_shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|error| EngineError::Inference(format!("recogniser output: {error}")))?;

        // CRNN emits [batch, timesteps, classes]. The class count comes from the
        // tensor rather than from the dictionary, so a dictionary that does not
        // match the model is caught here instead of shifting every character.
        let dimensions = output_shape.as_ref();
        let Some(&class_count) = dimensions.last() else {
            return Err(EngineError::Inference(
                "recogniser returned a rank-0 output".to_owned(),
            ));
        };
        let class_count = class_count as usize;
        if class_count != self.dictionary.class_count() {
            return Err(EngineError::Unavailable(format!(
                "the model emits {class_count} classes but the dictionary describes {}; \
                 they are not a matching pair",
                self.dictionary.class_count()
            )));
        }

        Ok(Some(recognise::ctc_decode(
            data,
            class_count,
            &self.dictionary,
        )))
    }
}

/// The environment variable `ort` reads to find ONNX Runtime.
const DYLIB_PATH_VAR: &str = "ORT_DYLIB_PATH";

/// Verifies ONNX Runtime is present and reachable, before `ort` can panic on it.
///
/// # Why this cannot just set the variable itself
///
/// The obvious implementation is `std::env::set_var(DYLIB_PATH_VAR, runtime)`.
/// It is not available here and **should not be**: since the 2024 edition
/// `set_var` is `unsafe`, because the process environment is global mutable
/// state that other threads may be reading, and this crate is
/// `#![forbid(unsafe_code)]`. The engine is constructed on the OCR worker thread
/// while the UI thread is running, which is precisely the racy case that rule
/// exists for.
///
/// So the variable is the **host's** to set, once, at startup, before any thread
/// exists to race with. This function's job is to make that requirement
/// impossible to miss: it fails with an actionable message rather than letting
/// `ort` unwind three frames deeper with `Failed to load ONNX Runtime dylib`.
///
/// # Why it takes the environment AND the filesystem as arguments
///
/// So every branch can be **tested**. Reading `var_os` inside would make each
/// arm reachable only by mutating the process environment, which needs `unsafe`
/// (see above) and is order-dependent across parallel tests; calling `is_file`
/// inside would make the existence check reachable only on a machine that
/// already had a runtime installed at a path the test chose. Both are passed in,
/// so the whole decision is a pure function of its arguments.
///
/// *(The environment parameter was added 2026-08-30 after an independent review
/// inverted this function's central comparison -- `==` to `!=` -- and all 70
/// tests still passed, because the only test touching it failed at the
/// file-exists guard and never reached the match. The `exists` parameter was
/// added the same day, by round 2 of the same review: the round-1 fix had moved
/// the existence check into `load`, where NO test in the suite could drive it,
/// and the two tests that previously covered it had to be loosened to accept
/// several messages because the author could no longer force which branch fired.
/// That was a coverage regression introduced by a fix, which is this project's
/// most reliable defect shape -- `PR #73` ran seven consecutive rounds of it.)*
///
/// # A runtime merely on the library search path is deliberately NOT accepted
///
/// `ort` falls back to a bare `onnxruntime.dll` resolved by the OS loader when
/// `ORT_DYLIB_PATH` is unset, and that fallback can succeed on a machine with a
/// system-wide install. **This function refuses that case, and the refusal is
/// the point rather than a side effect.** `ADR-0032` decision 1 chose
/// `load-dynamic` so the runtime is *"a file we place, check and ship
/// deliberately"*; a DLL picked up from `PATH` is by definition not that, it is
/// whatever the machine happened to offer, and loading it is a search-order
/// hijacking surface on precisely the component `architecture.md` section 4
/// calls a larger attack surface than the model.
///
/// ⚠️ **It is still a behaviour change, and the round-1 commit did not say so.**
/// A host that worked by having the runtime on the search path, with no
/// configuration at all, now fails to load with an actionable message instead.
/// That is intended under `ADR-0032`; it was not disclosed, and an independent
/// review had to point out that a previously-documented configuration had been
/// removed silently.
fn resolve_runtime(
    configured: Option<&Path>,
    from_environment: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf, EngineError> {
    let resolved = resolve_runtime_path(configured, from_environment)?;
    if exists(&resolved) {
        Ok(resolved)
    } else {
        Err(EngineError::Unavailable(format!(
            "ONNX Runtime not found at {}",
            resolved.display()
        )))
    }
}

/// Which path the runtime should be loaded from, before asking whether it is
/// there. Split out so the two questions fail with distinct messages.
fn resolve_runtime_path(
    configured: Option<&Path>,
    from_environment: Option<&Path>,
) -> Result<PathBuf, EngineError> {
    match (configured, from_environment) {
        (Some(wanted), Some(actual)) if wanted == actual => Ok(wanted.to_path_buf()),
        (Some(wanted), Some(actual)) => Err(EngineError::Unavailable(format!(
            "{DYLIB_PATH_VAR} points at {} but this engine was configured for {}; \
             they must agree, because ONNX Runtime is loaded once per process",
            actual.display(),
            wanted.display()
        ))),
        (Some(wanted), None) => Err(EngineError::Unavailable(format!(
            "{DYLIB_PATH_VAR} is not set; the host must set it to {} before starting \
             the OCR service, because this crate forbids the unsafe code that \
             setting it here would need",
            wanted.display()
        ))),
        // No configured path, but the host set the variable: a complete
        // instruction, and `ort` will use exactly this file.
        (None, Some(actual)) => Ok(actual.to_path_buf()),
        // Neither. `ort` would fall back to a bare "onnxruntime.dll" on the
        // library search path and panic if it is not there -- the exact outcome
        // this function exists to convert into a returned error.
        (None, None) => Err(EngineError::Unavailable(
            "no ONNX Runtime location is known: ORT_DYLIB_PATH is unset and no \
             runtime_library was configured. The host must set ORT_DYLIB_PATH at \
             startup"
                .to_owned(),
        )),
    }
}

/// Opens one ONNX session, naming which model failed.
fn open_session(path: &Path, role: &str) -> Result<Session, EngineError> {
    if !path.is_file() {
        return Err(EngineError::Unavailable(format!(
            "the {role} model is not at {}",
            path.display()
        )));
    }
    Session::builder()
        .and_then(|mut builder| builder.commit_from_file(path))
        .map_err(|error| {
            EngineError::Unavailable(format!(
                "the {role} model at {} could not be loaded: {error}",
                path.display()
            ))
        })
}

impl Engine for PaddleEngine {
    fn recognise(&mut self, frame: &RgbaBitmap) -> Result<Recognition, EngineError> {
        let detected = self.detect(frame)?;
        let mut placed = Vec::with_capacity(detected.len());

        for found in detected {
            let Some(decoded) = self.recognise_crop(frame, &found.quad)? else {
                continue;
            };
            if decoded.confidence < self.options.drop_score {
                continue;
            }
            let text = reading_order::normalise_whitespace(&decoded.text);
            if text.is_empty() {
                continue;
            }
            let (min_x, min_y, max_x, max_y) = found.quad.bounds();
            placed.push(Placed {
                top: min_y,
                bottom: max_y,
                left: min_x,
                payload: TextBlock {
                    text,
                    bounds: rect_from_bounds(min_x, min_y, max_x, max_y),
                },
            });
        }

        let blocks = reading_order::sort_into_reading_order(placed)
            .into_iter()
            .map(|item| item.payload)
            .collect();
        Ok(Recognition { blocks })
    }
}

/// Converts subpixel bounds into the integer [`Rect`] a [`TextBlock`] carries.
///
/// This is the single place the pipeline leaves floating point, which is
/// deliberate: `geometry.rs` calls coordinate maths this project's number one
/// bug source, and rounding in more than one place is how two coordinates that
/// should agree stop agreeing. The rectangle is grown outward -- floor the
/// origin, ceil the far edge -- so it always contains the text it describes
/// rather than clipping it by a fraction of a pixel.
fn rect_from_bounds(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Rect {
    let left = min_x
        .floor()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i32;
    let top = min_y
        .floor()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i32;
    let width = (max_x.ceil() - min_x.floor())
        .max(0.0)
        .min(f32::from(i16::MAX)) as u32;
    let height = (max_y.ceil() - min_y.floor())
        .max(0.0)
        .min(f32::from(i16::MAX)) as u32;
    Rect::new(left, top, width, height)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn path(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    /// A filesystem where every path exists. Injected, so the existence check is
    /// reachable from a test on a machine with no ONNX Runtime installed.
    fn everything_exists(_: &Path) -> bool {
        true
    }

    /// A filesystem where nothing exists.
    fn nothing_exists(_: &Path) -> bool {
        false
    }

    #[test]
    fn resolve_runtime_accepts_a_configured_path_the_environment_agrees_with() {
        let wanted = path("C:/runtime/onnxruntime.dll");
        assert_eq!(
            resolve_runtime(Some(&wanted), Some(&wanted), &everything_exists).unwrap(),
            wanted
        );
    }

    #[test]
    fn resolve_runtime_refuses_when_the_environment_names_a_different_file() {
        // ONNX Runtime loads once per process, so a disagreement means the
        // engine would run against a library it did not choose. This is the
        // comparison an independent review inverted on 2026-08-30 with every
        // test still passing, because nothing reached it.
        let error = resolve_runtime(
            Some(&path("C:/ours/onnxruntime.dll")),
            Some(&path("C:/somewhere-else/onnxruntime.dll")),
            &everything_exists,
        )
        .unwrap_err();
        assert!(error.is_fatal());
        let message = error.to_string();
        assert!(message.contains("somewhere-else"), "message: {message}");
        assert!(message.contains("ours"), "message: {message}");
    }

    #[test]
    fn resolve_runtime_refuses_a_configured_path_with_the_variable_unset() {
        let error = resolve_runtime(
            Some(&path("C:/ours/onnxruntime.dll")),
            None,
            &everything_exists,
        )
        .unwrap_err();
        assert!(error.is_fatal());
        assert!(
            error.to_string().contains("ORT_DYLIB_PATH is not set"),
            "message: {error}"
        );
    }

    #[test]
    fn resolve_runtime_takes_the_environments_path_when_none_is_configured() {
        let from_environment = path("C:/host/onnxruntime.dll");
        assert_eq!(
            resolve_runtime(None, Some(&from_environment), &everything_exists).unwrap(),
            from_environment
        );
    }

    #[test]
    fn resolve_runtime_refuses_when_neither_names_a_runtime() {
        // THE REGRESSION THIS TEST EXISTS FOR. Until 2026-08-30 `load` skipped
        // the check entirely when `runtime_library` was None, so this exact
        // configuration reached `ort`, hit its fallback to a bare library name,
        // and PANICKED -- the outcome the check exists to prevent, on the config
        // shape that reproduces it most directly.
        let error = resolve_runtime(None, None, &everything_exists).unwrap_err();
        assert!(error.is_fatal());
        let message = error.to_string();
        assert!(
            message.contains("no ONNX Runtime location"),
            "message: {message}"
        );
        assert!(message.contains("ORT_DYLIB_PATH"), "message: {message}");
    }

    #[test]
    fn a_resolved_path_that_is_not_on_disk_is_reported_as_not_found() {
        // THE CHECK ROUND 2 FOUND UNREACHABLE. The round-1 fix moved this into
        // `load`, where no test in the suite could drive it: getting there needs
        // the ambient ORT_DYLIB_PATH to agree with a test-chosen path, and this
        // crate cannot set an environment variable because it forbids the unsafe
        // code that would need. Two tests had to be loosened to accept several
        // messages as a result -- a coverage regression introduced by a fix.
        // Injecting the predicate is what makes it testable again.
        let wanted = path("C:/ours/onnxruntime.dll");
        let error = resolve_runtime(Some(&wanted), Some(&wanted), &nothing_exists).unwrap_err();
        assert!(error.is_fatal());
        assert!(
            error.to_string().contains("ONNX Runtime not found"),
            "message: {error}"
        );
    }

    #[test]
    fn the_path_is_decided_before_the_disk_is_consulted() {
        // Ordering, pinned: a disagreement between config and environment is
        // reported as a disagreement even when NEITHER file exists. Reversing
        // the two would report "not found" and send the reader looking for a
        // missing file rather than for a misconfiguration.
        let error = resolve_runtime(
            Some(&path("C:/ours/onnxruntime.dll")),
            Some(&path("C:/theirs/onnxruntime.dll")),
            &nothing_exists,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("must agree"),
            "the disk was consulted before the paths were reconciled: {error}"
        );
    }

    #[test]
    fn a_runtime_merely_on_the_library_search_path_is_refused_deliberately() {
        // ADR-0032 decision 1 chose load-dynamic so the runtime is "a file we
        // place, check and ship deliberately". A DLL the OS loader happens to
        // find on PATH is not that, and accepting it would be a search-order
        // hijacking surface on the component architecture.md section 4 calls a
        // larger attack surface than the model itself.
        //
        // This test exists because the refusal is a BEHAVIOUR CHANGE the round-1
        // commit made silently: `ort` really does fall back to a bare library
        // name, and a host relying on that used to work. `everything_exists`
        // models exactly that machine -- a system-wide runtime present and
        // loadable -- and the answer is still a refusal.
        let error = resolve_runtime(None, None, &everything_exists).unwrap_err();
        assert!(
            error.is_fatal(),
            "a search-path runtime must be refused even where one would load"
        );
    }

    #[test]
    fn a_missing_runtime_is_reported_as_unavailable_rather_than_panicking() {
        // ADR-0032 decision 3, made executable. Without the pre-check in
        // `load`, this call reaches `ort`'s lazy initialiser and unwinds with
        // "Failed to load ONNX Runtime dylib" instead of returning.
        let config = PaddleConfig {
            detection_model: PathBuf::from("no-such-det.onnx"),
            recognition_model: PathBuf::from("no-such-rec.onnx"),
            dictionary: PathBuf::from("no-such-dict.txt"),
            runtime_library: Some(PathBuf::from("no-such-onnxruntime.dll")),
        };
        let error = PaddleEngine::load(&config, PaddleOptions::default()).unwrap_err();
        assert!(error.is_fatal(), "a missing runtime must be fatal");

        // WHICH guard fires depends on ORT_DYLIB_PATH, which this crate cannot
        // set (unsafe) and this test must not depend on: unset in a normal test
        // run, so `resolve_runtime` refuses before the file check; set on a
        // machine with a real runtime, so the file check refuses instead. The
        // property pinned here holds either way -- `load` RETURNS an
        // `Unavailable` for a runtime it cannot use, and does not unwind.
        let message = error.to_string();
        assert!(
            message.contains("ONNX Runtime not found")
                || message.contains("ORT_DYLIB_PATH is not set")
                || message.contains("must agree"),
            "load failed for an unrelated reason: {message}"
        );
    }

    #[test]
    fn a_missing_dictionary_is_reported_before_any_model_is_opened() {
        // ⚠️ REWRITTEN 2026-08-30. This used to assert that the dictionary
        // error came back, on the reasoning that `runtime_library: None` meant
        // the runtime pre-check did not fire. That was TRUE and was the bug --
        // `None` is now checked like every other path, so `resolve_runtime`
        // refuses first when the environment is also unset. What the test pins
        // now is the property that survives either ordering: `load` returns a
        // fatal error and never reaches `ort`.
        let config = PaddleConfig {
            detection_model: PathBuf::from("no-such-det.onnx"),
            recognition_model: PathBuf::from("no-such-rec.onnx"),
            dictionary: PathBuf::from("no-such-dict.txt"),
            runtime_library: None,
        };
        let error = PaddleEngine::load(&config, PaddleOptions::default()).unwrap_err();
        assert!(error.is_fatal());
        let message = error.to_string();
        assert!(
            message.contains("character dictionary") || message.contains("ONNX Runtime location"),
            "load reached neither guard cleanly: {message}"
        );
    }

    #[test]
    fn bounds_become_a_rectangle_that_contains_the_text() {
        let rect = rect_from_bounds(10.4, 20.6, 50.2, 32.1);
        assert_eq!(rect.origin.x, 10);
        assert_eq!(rect.origin.y, 20);
        // Grown outward, never inward: 10.4 -> 10 and 50.2 -> 51 is 41 wide.
        assert_eq!(rect.size.width, 41);
        assert_eq!(rect.size.height, 13);
    }

    #[test]
    fn a_degenerate_bound_yields_an_empty_rather_than_a_negative_rectangle() {
        let rect = rect_from_bounds(30.0, 30.0, 10.0, 10.0);
        assert_eq!(rect.size.width, 0);
        assert_eq!(rect.size.height, 0);
    }

    #[test]
    fn the_default_options_are_the_reference_implementations() {
        let options = PaddleOptions::default();
        assert!((options.detector.threshold - 0.3).abs() < f32::EPSILON);
        assert!((options.detector.box_threshold - 0.6).abs() < f32::EPSILON);
        assert!((options.detector.unclip_ratio - 1.5).abs() < f32::EPSILON);
        assert!((options.drop_score - 0.5).abs() < f32::EPSILON);
        assert_eq!(options.limit_side_len, preprocess::DEFAULT_LIMIT_SIDE_LEN);
    }
}
