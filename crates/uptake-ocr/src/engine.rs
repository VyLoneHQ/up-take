//! The seam a recogniser plugs into, and the types that cross it.
//!
//! Task 1.11 puts PP-OCRv4 behind [`Engine`]. Nothing here knows that, and that
//! is the point: the thread in [`crate::service`] is written against this trait,
//! so the recogniser can be replaced, stubbed in a test, or deferred entirely
//! while the threading contract is proven.

use std::fmt;

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

/// One run of recognition over one frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recognition {
    /// The blocks found, already in reading order.
    ///
    /// **Ordering is the engine's job, not the caller's.** `architecture.md`
    /// section 3.2 puts *"reading-order sort, whitespace normalisation"* inside
    /// the OCR pipeline, before the result leaves it. A caller that had to sort
    /// would need the layout heuristics the engine already has, and two sorts
    /// that disagree is a defect nobody would see until the text came out
    /// scrambled on a two-column screenshot.
    pub blocks: Vec<TextBlock>,
}

impl Recognition {
    /// The whole recognition as one string, blocks separated by newlines.
    ///
    /// What roadmap 1.13 puts on the clipboard. Kept here rather than at the
    /// call site so the join rule has one definition.
    #[must_use]
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether anything was found. A frame of blank wall is not an error.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// One recognised run of text and where it sat in the frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBlock {
    /// The text, already whitespace-normalised by the engine.
    pub text: String,
    /// Where it sat, in frame-local coordinates.
    ///
    /// **Frame-local, never screen coordinates.** The engine is handed a bitmap
    /// and has no idea where on the desktop it came from; making it guess would
    /// put a coordinate translation inside the one component that cannot see the
    /// monitor layout. `geometry.rs`'s own header calls coordinate maths this
    /// project's number one bug source, which is the argument for keeping this
    /// conversion at the call site that already knows the area's bounds.
    pub bounds: Rect,
}

/// A recogniser. Task 1.11 implements this with PP-OCRv4.
///
/// # Why `&mut self`
///
/// A real engine holds session state that inference mutates -- ONNX Runtime's
/// `Session::run` takes `&mut` in `ort` 2.x, and an engine that pooled scratch
/// buffers would want the same. Taking `&self` here would force interior
/// mutability on every implementor to buy a sharing property nothing needs: the
/// engine lives on exactly one thread by construction ([`crate::service`] owns
/// it and never lends it out), so `&mut` is both honest and free.
///
/// # Why loading is not on this trait
///
/// There is no `load()` method, and the omission is deliberate. *"Models load
/// once at startup and stay resident"* means loading happens **once, on the
/// worker thread, before the first request** -- so it belongs in the closure
/// that constructs the engine, which [`crate::service::Service::spawn`] runs on
/// that thread. A `load()` on the trait would be a second lifecycle for callers
/// to sequence, and the failure mode is an engine that answers before it is
/// ready.
pub trait Engine: Send {
    /// Recognise the text in one frame.
    ///
    /// Called only from the worker thread, one frame at a time. An engine may
    /// take seconds; nothing here is expected to be fast, and the caller is
    /// insulated from that by the thread rather than by a promise made here.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when the frame cannot be recognised. A frame with
    /// **no text in it is not an error** -- that is an empty [`Recognition`],
    /// and conflating the two would make "the wall is blank" indistinguishable
    /// from "the model failed to load".
    fn recognise(&mut self, frame: &RgbaBitmap) -> Result<Recognition, EngineError>;
}

/// Why a recogniser could not answer.
///
/// # Why this is a string and not an enum of causes
///
/// The variants would be the union of every future engine's failure modes, and
/// this crate has no engine yet -- so the enum would be invented from
/// imagination rather than from what actually goes wrong, which is how a type
/// ends up with variants nobody constructs and a catch-all everybody uses. It
/// carries a message and stays `#[non_exhaustive]` so structured variants can be
/// added later **without breaking a caller that matched on it** (`UT-F-53`'s
/// lesson applied to a type rather than to a measurement: do not model what has
/// not been observed).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The engine could not be constructed -- typically a missing or unreadable
    /// model. Distinct from [`EngineError::Inference`] because it is fatal to
    /// the whole service rather than to one request.
    #[error("the OCR engine could not start: {0}")]
    Unavailable(String),
    /// One frame could not be recognised. The engine is still usable.
    #[error("recognition failed: {0}")]
    Inference(String),
}

impl EngineError {
    /// Whether this killed the engine rather than one request.
    ///
    /// The service reads this to decide whether to keep the thread alive. Named
    /// rather than left as a `matches!` at the call site, because "is this
    /// fatal" is a policy question and policy stated once cannot drift.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// A no-op engine that finds nothing, for wiring a caller before 1.11 lands.
///
/// **Not a test double** -- tests build their own with the behaviour they need.
/// This exists so the host can construct a [`crate::service::Service`] and prove
/// the plumbing end to end while the recogniser is still a decision. It reports
/// success with an empty result, which is the honest answer for an engine that
/// genuinely did not find any text.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEngine;

impl Engine for NoEngine {
    fn recognise(&mut self, _frame: &RgbaBitmap) -> Result<Recognition, EngineError> {
        Ok(Recognition::default())
    }
}

impl fmt::Display for Recognition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text())
    }
}
