//! Streaming verification: the gate every downloaded byte passes through.
//!
//! ADR-0032 decision 2 and ADR-0034 both require *"pinned SHA-256, verified
//! before load"*. This module is the "verified before" half, and it is written
//! as a **streaming** verifier rather than a hash-the-finished-file function for
//! two reasons that are not about speed:
//!
//! 1. **The size check can fail early.** A response that is already longer than
//!    the manifest says is wrong at that byte, not 200 MB later, and reporting
//!    it as an over-length response is a better diagnostic than a hash mismatch.
//! 2. **The caller cannot forget.** A verifier that is fed the bytes as they
//!    arrive and consumed by [`Verifier::finish`] makes "download, then verify"
//!    one operation instead of two, and it is the second one that gets skipped.
//!
//! Nothing here touches the network or the filesystem.

use sha2::{Digest, Sha256};

use crate::manifest::Sha256Digest;

/// Why a stream of bytes is not the file the manifest describes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VerifyError {
    /// More bytes arrived than the manifest declared.
    ///
    /// Reported at the byte that crossed the line rather than at the end: the
    /// stream is already wrong, and continuing to buffer an unbounded response
    /// from a source that has already broken its contract is how a verifier
    /// becomes a memory-exhaustion bug.
    #[error("expected {expected} bytes but the stream is already longer ({received} so far)")]
    TooLong {
        /// What the manifest said.
        expected: u64,
        /// How many bytes had arrived when the limit was crossed.
        received: u64,
    },
    /// The stream ended early.
    #[error("expected {expected} bytes but the stream ended after {received}")]
    TooShort {
        /// What the manifest said.
        expected: u64,
        /// What arrived.
        received: u64,
    },
    /// The right number of bytes, the wrong bytes.
    #[error("checksum mismatch: expected {expected}, computed {actual}")]
    DigestMismatch {
        /// The pinned digest.
        expected: Sha256Digest,
        /// What the bytes actually hash to.
        actual: Sha256Digest,
    },
}

/// Hashes a stream and holds it to a declared length and digest.
///
/// Construct with [`Verifier::new`], feed every chunk to [`Verifier::update`],
/// and finish with [`Verifier::finish`]. **`finish` consumes the verifier**, so
/// a caller cannot accidentally use a half-checked stream: there is no way to
/// hold a `Verifier` that has been asked for its verdict.
#[derive(Debug, Clone)]
pub struct Verifier {
    hasher: Sha256,
    expected_digest: Sha256Digest,
    expected_bytes: u64,
    received: u64,
}

impl Verifier {
    /// A verifier for one asset's declared size and digest.
    #[must_use]
    pub fn new(expected_digest: Sha256Digest, expected_bytes: u64) -> Self {
        Self {
            hasher: Sha256::new(),
            expected_digest,
            expected_bytes,
            received: 0,
        }
    }

    /// Feeds the next chunk.
    ///
    /// # Errors
    ///
    /// [`VerifyError::TooLong`] as soon as the total exceeds the declared size.
    /// **The chunk is hashed anyway before the check**, so a caller that ignores
    /// the error and calls [`Verifier::finish`] still gets a refusal rather than
    /// a digest over a truncated prefix -- an error that only fires when it is
    /// handled is not a control.
    pub fn update(&mut self, chunk: &[u8]) -> Result<(), VerifyError> {
        self.hasher.update(chunk);
        self.received = self.received.saturating_add(chunk.len() as u64);
        if self.received > self.expected_bytes {
            return Err(VerifyError::TooLong {
                expected: self.expected_bytes,
                received: self.received,
            });
        }
        Ok(())
    }

    /// How many bytes have been fed so far. For a progress figure.
    #[must_use]
    pub const fn received(&self) -> u64 {
        self.received
    }

    /// Checks the length and the digest, consuming the verifier.
    ///
    /// # Errors
    ///
    /// [`VerifyError::TooShort`] or [`VerifyError::TooLong`] if the length is
    /// wrong, and [`VerifyError::DigestMismatch`] if the bytes are.
    ///
    /// **Length is checked first**, deliberately: "the server sent 4 bytes of
    /// HTML error page" is a far more useful report than "checksum mismatch",
    /// and it is the overwhelmingly common real failure.
    pub fn finish(self) -> Result<(), VerifyError> {
        if self.received < self.expected_bytes {
            return Err(VerifyError::TooShort {
                expected: self.expected_bytes,
                received: self.received,
            });
        }
        if self.received > self.expected_bytes {
            return Err(VerifyError::TooLong {
                expected: self.expected_bytes,
                received: self.received,
            });
        }
        let computed = self.hasher.finalize();
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(&computed);
        let actual = Sha256Digest::from_bytes(bytes);
        if actual == self.expected_digest {
            Ok(())
        } else {
            Err(VerifyError::DigestMismatch {
                expected: self.expected_digest,
                actual,
            })
        }
    }
}

/// The SHA-256 of a slice, for verifying a file already on disk.
///
/// Used to answer *"is the installed copy still the one we verified?"* without
/// re-downloading it -- which is what makes a second launch cheap and what
/// catches a file edited or truncated after installation.
#[must_use]
pub fn digest_of(bytes: &[u8]) -> Sha256Digest {
    let computed = Sha256::digest(bytes);
    let mut out = [0_u8; 32];
    out.copy_from_slice(&computed);
    Sha256Digest::from_bytes(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The SHA-256 of the empty input, from the standard test vectors.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    /// The SHA-256 of "abc", from the standard test vectors.
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn parse(text: &str) -> Sha256Digest {
        Sha256Digest::parse_hex(text).unwrap()
    }

    #[test]
    fn the_hash_matches_the_published_test_vectors() {
        // Pins this crate's SHA-256 against FIPS 180-4's own vectors rather than
        // against whatever `sha2` happens to produce -- a test that only checks
        // self-consistency would pass on a broken hash.
        assert_eq!(digest_of(b"").to_string(), EMPTY_SHA256);
        assert_eq!(digest_of(b"abc").to_string(), ABC_SHA256);
    }

    #[test]
    fn a_stream_that_matches_both_length_and_digest_verifies() {
        let mut verifier = Verifier::new(parse(ABC_SHA256), 3);
        verifier.update(b"abc").unwrap();
        assert_eq!(verifier.received(), 3);
        assert!(verifier.finish().is_ok());
    }

    #[test]
    fn chunking_does_not_change_the_verdict() {
        // The bytes arrive from a socket in whatever sizes the network chose.
        for chunk_size in 1..=3 {
            let mut verifier = Verifier::new(parse(ABC_SHA256), 3);
            for chunk in b"abc".chunks(chunk_size) {
                verifier.update(chunk).unwrap();
            }
            assert!(
                verifier.finish().is_ok(),
                "failed at chunk size {chunk_size}"
            );
        }
    }

    #[test]
    fn an_empty_chunk_is_harmless() {
        let mut verifier = Verifier::new(parse(ABC_SHA256), 3);
        verifier.update(b"").unwrap();
        verifier.update(b"abc").unwrap();
        verifier.update(b"").unwrap();
        assert!(verifier.finish().is_ok());
    }

    #[test]
    fn the_right_length_with_the_wrong_bytes_is_a_digest_mismatch() {
        let mut verifier = Verifier::new(parse(ABC_SHA256), 3);
        verifier.update(b"abd").unwrap();
        let error = verifier.finish().unwrap_err();
        match error {
            VerifyError::DigestMismatch { expected, actual } => {
                assert_eq!(expected.to_string(), ABC_SHA256);
                assert_ne!(actual.to_string(), ABC_SHA256);
            }
            other => panic!("expected a digest mismatch, got {other}"),
        }
    }

    #[test]
    fn a_truncated_stream_is_reported_as_short_rather_than_as_a_bad_hash() {
        // The common real failure -- a dropped connection -- must not read as
        // "the file was tampered with".
        let mut verifier = Verifier::new(parse(ABC_SHA256), 3);
        verifier.update(b"ab").unwrap();
        let error = verifier.finish().unwrap_err();
        assert!(
            matches!(
                error,
                VerifyError::TooShort {
                    expected: 3,
                    received: 2
                }
            ),
            "got {error}"
        );
    }

    #[test]
    fn an_over_long_stream_fails_at_the_byte_that_crosses_the_line() {
        // Not at the end: continuing to buffer an unbounded response from a
        // source that has already broken its contract is a memory bug.
        let mut verifier = Verifier::new(parse(ABC_SHA256), 3);
        verifier.update(b"ab").unwrap();
        let error = verifier.update(b"cd").unwrap_err();
        assert!(
            matches!(
                error,
                VerifyError::TooLong {
                    expected: 3,
                    received: 4
                }
            ),
            "got {error}"
        );
    }

    #[test]
    fn ignoring_the_too_long_error_still_refuses_at_finish() {
        // The property that makes `update`'s error a control rather than a
        // suggestion. A caller that swallows it does not get a pass.
        let mut verifier = Verifier::new(parse(ABC_SHA256), 3);
        let _ = verifier.update(b"abcd");
        let error = verifier.finish().unwrap_err();
        assert!(matches!(error, VerifyError::TooLong { .. }), "got {error}");
    }

    #[test]
    fn an_empty_expected_file_verifies_against_the_empty_digest() {
        let verifier = Verifier::new(parse(EMPTY_SHA256), 0);
        assert!(verifier.finish().is_ok());
    }

    #[test]
    fn bytes_arriving_for_a_zero_length_asset_are_refused() {
        let mut verifier = Verifier::new(parse(EMPTY_SHA256), 0);
        let error = verifier.update(b"x").unwrap_err();
        assert!(
            matches!(error, VerifyError::TooLong { expected: 0, .. }),
            "got {error}"
        );
    }

    #[test]
    fn digest_of_agrees_with_the_streaming_verifier() {
        // One rule, two entry points: an installed file re-checked with
        // `digest_of` must be judged identically to the stream that installed
        // it, or a reinstall loop is possible where each half disagrees.
        let payload: Vec<u8> = (0..5000_u32).map(|value| (value % 251) as u8).collect();
        let computed = digest_of(&payload);
        let mut verifier = Verifier::new(computed, payload.len() as u64);
        for chunk in payload.chunks(97) {
            verifier.update(chunk).unwrap();
        }
        assert!(verifier.finish().is_ok());
    }
}
