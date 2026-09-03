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
    /// The blocks found, grouped into visual lines, each line ordered left to
    /// right, the lines themselves in reading order.
    ///
    /// **Ordering and grouping are the engine's job, not the caller's.**
    /// `architecture.md` section 3.2 (in the workspace repository, not this
    /// one) puts *"reading-order sort, whitespace normalisation"* inside the
    /// OCR pipeline, before the result leaves it. A caller that had to sort
    /// would need the layout heuristics the engine already has, and two sorts
    /// that disagree is a defect nobody would see until the text came out
    /// scrambled on a two-column screenshot.
    ///
    /// **This is the SINGLE representation, and that is the point.** An earlier
    /// version of this fix kept a public flat `blocks` beside a private
    /// `line_starts` index, and its own doc comment claimed the pairing
    /// therefore "holds by construction". **It did not**: the independent
    /// review of `PR #82` mutated the public field and showed that pushing a
    /// block merged it onto a stale line, while truncating made whole lines
    /// vanish from [`Recognition::text`] with no error at all. Two containers
    /// that must agree, one of them writable, is a convention wearing a type
    /// system's clothes. Grouping is now unrepresentable-if-wrong instead.
    lines: Vec<Vec<TextBlock>>,
}

impl Recognition {
    /// Builds a recognition from blocks already grouped into visual lines and
    /// ordered left to right within each.
    ///
    /// Empty lines are dropped, so no line ever renders as a blank one.
    #[must_use]
    pub fn from_lines(lines: Vec<Vec<TextBlock>>) -> Self {
        Self {
            lines: lines.into_iter().filter(|line| !line.is_empty()).collect(),
        }
    }

    /// Every block, in reading order, flattened across lines.
    ///
    /// An accessor rather than a public field: see the note on
    /// [`Recognition::lines`] for what a writable one cost.
    pub fn blocks(&self) -> impl Iterator<Item = &TextBlock> {
        self.lines.iter().flatten()
    }

    /// The blocks grouped by visual line, for a caller that needs the layout
    /// rather than the text.
    #[must_use]
    pub fn lines(&self) -> &[Vec<TextBlock>] {
        &self.lines
    }

    /// The whole recognition as one string: blocks on one visual line joined
    /// with a space, lines separated by newlines.
    ///
    /// What roadmap 1.13 puts on the clipboard. Kept here rather than at the
    /// call site so the join rule has one definition.
    ///
    /// ⚠️ **This joined EVERY block with a newline until 2026-09-03**, which is
    /// `UP-TAKE I-350`. It was invisible at ordinary text sizes because the
    /// detector's unclip step merges a whole line into a single box when the
    /// inter-word gaps are small relative to the glyphs, so one block happened
    /// to equal one line. Measured on ground-truth cards: one sentence at 14,
    /// 28 and 56 px each produced ONE block and read correctly; the same
    /// sentence at 96 px produced FOUR and came out one word per line.
    ///
    /// **There is no fallible indexing here any more, and that is deliberate.**
    /// The first version of this fix sliced a flat `blocks` by a parallel index
    /// and silently skipped a range that did not resolve. Round 1 of the review
    /// showed that branch had no test able to drive it, and that when the
    /// public field made it reachable it hid total content loss. `UT-F-75` is
    /// this project's record of a guard whose refusal path is never driven, so
    /// the guard is gone along with the shape that needed it.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether anything was found. A frame of blank wall is not an error.
    ///
    /// Equivalent to having no blocks, because [`Recognition::from_lines`]
    /// drops empty lines: a non-empty `lines` always carries at least one
    /// block.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A block at a nominal position. Only `text` matters to `text()`; the
    /// bounds are carried for realism and are not read by the assembly, which is
    /// itself the point -- the LINE structure is decided upstream, where the
    /// subpixel edges live, and `text()` consumes the decision rather than
    /// re-making it.
    fn block(text: &str, x: i32, y: i32) -> TextBlock {
        TextBlock {
            text: text.to_owned(),
            bounds: Rect::new(x, y, 40, 20),
        }
    }

    #[test]
    fn blocks_on_one_line_are_joined_with_a_space() {
        // `UP-TAKE I-350`'s regression test. This returned "The\nquick\nbrown\nfox"
        // until 2026-09-03, and it was measured rather than supposed: one
        // sentence rendered at 96 px produces four detector boxes, where the
        // same sentence at 56 px and below produces one.
        let recognition = Recognition::from_lines(vec![vec![
            block("The", 0, 0),
            block("quick", 50, 0),
            block("brown", 100, 0),
            block("fox", 150, 0),
        ]]);
        assert_eq!(recognition.text(), "The quick brown fox");
    }

    #[test]
    fn separate_lines_are_joined_with_a_newline() {
        let recognition = Recognition::from_lines(vec![
            vec![block("first", 0, 0)],
            vec![block("second", 0, 40)],
        ]);
        assert_eq!(recognition.text(), "first\nsecond");
    }

    #[test]
    fn words_stay_together_and_lines_stay_apart_in_one_result() {
        // The shape a real paragraph produces at a size where unclip does not
        // merge the words: several boxes per line, several lines.
        let recognition = Recognition::from_lines(vec![
            vec![block("hello", 0, 0), block("there", 50, 0)],
            vec![block("general", 0, 40), block("kenobi", 60, 40)],
        ]);
        assert_eq!(recognition.text(), "hello there\ngeneral kenobi");
    }

    #[test]
    fn blocks_are_flattened_in_reading_order() {
        // `blocks()` documents "in reading order" and NOTHING held it to that:
        // round 2 of `PR #82`'s review reversed the line order inside it and all
        // 598 tests stayed green, because the only assertion touching it counted
        // blocks rather than reading them. A guarantee in a doc comment that no
        // test can falsify is the same defect as round 1's, one method along.
        let recognition = Recognition::from_lines(vec![
            vec![block("hello", 0, 0), block("there", 50, 0)],
            vec![block("general", 0, 40), block("kenobi", 60, 40)],
        ]);
        let order: Vec<&str> = recognition.blocks().map(|b| b.text.as_str()).collect();
        assert_eq!(order, ["hello", "there", "general", "kenobi"]);
        // And the two views agree, which is the property the restructure exists
        // to make true rather than merely claim.
        assert_eq!(recognition.text(), "hello there\ngeneral kenobi");
    }

    #[test]
    fn an_empty_recognition_is_an_empty_string() {
        assert_eq!(Recognition::from_lines(Vec::new()).text(), "");
        assert!(Recognition::from_lines(Vec::new()).is_empty());
    }

    #[test]
    fn an_empty_line_contributes_no_break() {
        // An empty line must not become a stray blank line, and must not leave a
        // `line_starts` entry pointing at the next line's first block -- which
        // would put a newline in the middle of a line.
        let recognition = Recognition::from_lines(vec![
            vec![block("alpha", 0, 0)],
            Vec::new(),
            vec![block("beta", 0, 40)],
        ]);
        assert_eq!(recognition.text(), "alpha\nbeta");
        assert_eq!(recognition.blocks().count(), 2);
        assert_eq!(
            recognition.lines().len(),
            2,
            "the empty line must not survive"
        );
    }

    #[test]
    fn display_matches_text() {
        let recognition =
            Recognition::from_lines(vec![vec![block("one", 0, 0), block("two", 50, 0)]]);
        assert_eq!(recognition.to_string(), recognition.text());
    }
}
