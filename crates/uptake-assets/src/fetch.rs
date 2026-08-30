//! The seam bytes arrive through, and how progress is reported.
//!
//! No HTTP client lives here -- see the crate header for why. What lives here is
//! the shape a transport must present, chosen so that the verification in
//! [`crate::verify`] cannot be bypassed by any implementation of it.

use crate::manifest::Asset;

/// How far along one asset, or a whole manifest, is.
///
/// `total` is from the manifest rather than from a response header, so progress
/// is against what we *pinned*, not against what a server claimed. A server that
/// reports a different length is a failure to report, not a progress bar that
/// silently rescales.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes verified so far.
    pub done_bytes: u64,
    /// Bytes expected in total.
    pub total_bytes: u64,
    /// Assets fully installed so far.
    pub done_assets: usize,
    /// Assets in the manifest.
    pub total_assets: usize,
}

impl Progress {
    /// Completion as a fraction in `0.0..=1.0`.
    ///
    /// A zero total reports `1.0` -- nothing to do is done, not undefined. The
    /// alternative is a `NaN` reaching a progress bar, which renders as an
    /// empty or frozen widget and reads to a user as a hang.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 1.0;
        }
        // Saturating rather than wrapping: a caller that reports more bytes than
        // the total has a bug, and a fraction above 1.0 would drive a progress
        // bar past its end rather than making the bug visible.
        (self.done_bytes.min(self.total_bytes) as f64 / self.total_bytes as f64) as f32
    }

    /// Whether every asset is installed.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.done_assets >= self.total_assets
    }
}

/// Why an asset could not be fetched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FetchError {
    /// The transport failed: no route, TLS refused, connection dropped, a non-
    /// success status. One variant, carrying the transport's own words.
    ///
    /// Deliberately not an enum of causes. This crate has no transport, so the
    /// variants would be invented from imagination rather than from what
    /// actually goes wrong -- the same reasoning `uptake-ocr`'s `EngineError`
    /// records, and `#[non_exhaustive]` keeps the door open for structured
    /// variants once a real implementation has taught us what they are.
    #[error("could not fetch {url}: {reason}")]
    Transport {
        /// What was being fetched.
        url: String,
        /// The transport's description.
        reason: String,
    },
    /// The caller asked to stop.
    #[error("the download was cancelled")]
    Cancelled,
}

/// Somewhere bytes come from.
///
/// # Why `&mut dyn FnMut` and not a returned reader
///
/// A transport hands chunks to a sink rather than returning a stream, so the
/// verification in [`crate::install`] sits **between** the transport and the
/// disk. If this returned a reader, a caller could read it straight into a file
/// and skip the verifier, and the crate's invariant would depend on everyone
/// remembering not to.
///
/// # What this shape does and does NOT enforce
///
/// ⚠️ **This section said the arrangement made verification unskippable
/// “by construction”. That overclaimed, and the independent review of
/// `PR #77` drew the line precisely.** Three things were being run together:
///
/// | Property | Enforced by the type? |
/// | --- | --- |
/// | Bytes reach the disk only through the sink, so they are hashed | **yes** -- there is no other channel |
/// | The transport stops when the sink refuses | **no** -- documented only |
/// | Memory stays bounded by the chunk size | **no** -- documented only |
///
/// Only the first is structural. An implementation is free to buffer the entire
/// response and call the sink once at the end: verification still happens and
/// the invariant still holds, but the early abort below buys nothing and peak
/// memory is the whole response. An implementation that ignores the sink's
/// `Err` and keeps feeding chunks is likewise not prevented -- though
/// [`crate::verify::Verifier`] refuses at `finish` regardless, so the worst
/// outcome is wasted work rather than an unverified install.
///
/// **That residual is a property of every `Fetcher`, and the only reviewer of
/// it is whoever writes one.** It is stated here rather than left implicit
/// because a rule an implementor must remember is exactly the class this
/// project keeps finding unenforced.
pub trait Fetcher {
    /// Fetches `asset`, handing every chunk to `sink` in arrival order.
    ///
    /// An implementation **must** stop and return the sink's error if `sink`
    /// returns one -- that is how an over-long response is cut off at the byte
    /// that crosses the line rather than buffered to exhaustion. **Nothing
    /// checks this**; see the table above. Feeding chunks after a refusal
    /// cannot produce an unverified install, but it can produce an unbounded
    /// one.
    ///
    /// An implementation **should** call `sink` incrementally as bytes arrive
    /// rather than once with the whole body. Also unchecked, and also the
    /// difference between bounded and unbounded memory.
    ///
    /// # Errors
    ///
    /// [`FetchError`] if the bytes could not be obtained. A sink error is
    /// reported as [`FetchError::Transport`] carrying the sink's message, since
    /// from the transport's side an aborted read is what happened.
    fn fetch(
        &mut self,
        asset: &Asset,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), FetchError>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn progress(done: u64, total: u64) -> Progress {
        Progress {
            done_bytes: done,
            total_bytes: total,
            done_assets: 0,
            total_assets: 1,
        }
    }

    #[test]
    fn a_fraction_runs_from_zero_to_one() {
        assert!((progress(0, 100).fraction() - 0.0).abs() < f32::EPSILON);
        assert!((progress(50, 100).fraction() - 0.5).abs() < 1e-6);
        assert!((progress(100, 100).fraction() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn nothing_to_do_reports_complete_rather_than_nan() {
        // A NaN here renders as a frozen progress bar, which a user reads as a
        // hang. This is the whole reason the zero case is special-cased.
        let fraction = progress(0, 0).fraction();
        assert!(fraction.is_finite(), "fraction was {fraction}");
        assert!((fraction - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn over_reporting_is_clamped_rather_than_exceeding_one() {
        let fraction = progress(150, 100).fraction();
        assert!(
            (fraction - 1.0).abs() < f32::EPSILON,
            "fraction was {fraction}"
        );
    }

    #[test]
    fn a_huge_total_does_not_lose_precision_into_a_wrong_fraction() {
        // u64 -> f32 directly would round badly at this scale; the computation
        // goes through f64 for that reason.
        let half = progress(4_000_000_000, 8_000_000_000).fraction();
        assert!((half - 0.5).abs() < 1e-6, "fraction was {half}");
    }

    #[test]
    fn completion_counts_assets_not_bytes() {
        // Bytes can be at 100% while the last file is still being renamed into
        // place, so "done" is the asset count.
        let mut state = progress(100, 100);
        state.done_assets = 0;
        state.total_assets = 2;
        assert!(!state.is_complete());
        state.done_assets = 2;
        assert!(state.is_complete());
    }
}
