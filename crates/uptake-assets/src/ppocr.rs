//! The PP-OCRv4 manifest: what UP-TAKE must acquire before OCR can run.
//!
//! # These digests are OURS, and that is the whole point
//!
//! [`ADR-0034`] chose option A -- we convert PaddleOCR's official release to
//! ONNX ourselves rather than take a third party's conversion -- specifically so
//! that the SHA-256 pinned here means what [`ADR-0032`] decision 2 says a
//! checksum means. A hash over bytes someone else produced proves only that a
//! file arrived intact. A hash over bytes *we* produced pins our own artifact.
//!
//! The two model digests below were produced by
//! `scripts/convert-ppocr-models.py` on 2026-09-01, from upstream archives that
//! script verified against their own pinned digests first. The dictionary is
//! PaddleOCR's file copied byte for byte.
//!
//! # Where the files are served from is NOT settled here
//!
//! [`ppocr_v4`] takes a base URL rather than hardcoding one, and that is a
//! deliberate refusal rather than a convenience. Publishing converted model
//! artifacts to a public location is an outward-facing supply-chain step of the
//! same class `ADR-0032` and `ADR-0034` both stopped for, and it has not been
//! taken: **nothing is published at any URL today.** Roadmap `1.12` also still
//! owes the decision both ADRs deferred to it -- whether these files ship inside
//! the installer or are fetched on first run -- and that answer decides whether
//! a URL is needed at all.
//!
//! So this module states the part `1.31` owns and can prove: *what* to acquire,
//! and *what it must hash to*. The base URL is the caller's, and
//! [`Asset::validate`] holds it to HTTPS.
//!
//! [`ADR-0032`]: https://github.com/VyLoneHQ/up-take
//! [`ADR-0034`]: https://github.com/VyLoneHQ/up-take

use crate::manifest::{Asset, AssetKind, AssetManifest, ManifestError, Sha256Digest};

/// The detection model: PP-OCRv4's Differentiable Binarization network.
pub const DETECTION_FILE_NAME: &str = "ch_PP-OCRv4_det.onnx";
/// SHA-256 of [`DETECTION_FILE_NAME`] as this repository's script produces it.
pub const DETECTION_SHA256: &str =
    "69ce850fec741a2a4568c7c924bb025c9d4f1129e5f96ab428c799ccc5ef2275";
/// Size of [`DETECTION_FILE_NAME`] in bytes.
pub const DETECTION_SIZE: u64 = 4_729_474;

/// The recognition model: PP-OCRv4's CRNN, 6625 output classes.
pub const RECOGNITION_FILE_NAME: &str = "ch_PP-OCRv4_rec.onnx";
/// SHA-256 of [`RECOGNITION_FILE_NAME`] as this repository's script produces it.
pub const RECOGNITION_SHA256: &str =
    "ad7dd55f6759fa02333bff6eb179a4f51be5b89cbe6f710249c95f47d0211350";
/// Size of [`RECOGNITION_FILE_NAME`] in bytes.
pub const RECOGNITION_SIZE: u64 = 10_812_334;

/// The character dictionary, copied from PaddleOCR unchanged.
pub const DICTIONARY_FILE_NAME: &str = "ppocr_keys_v1.txt";
/// SHA-256 of [`DICTIONARY_FILE_NAME`]. PaddleOCR's own file, at tag `v2.7.0`.
pub const DICTIONARY_SHA256: &str =
    "28b2362ad4ab2dc38769aa72feb535e3a9ddb3fd2a7585a05920e6393b1dc7f7";
/// Size of [`DICTIONARY_FILE_NAME`] in bytes.
pub const DICTIONARY_SIZE: u64 = 26_249;

/// How many classes [`RECOGNITION_FILE_NAME`] emits on its last axis.
///
/// Recorded here as well as in `uptake-ocr` because it is the number that ties
/// the model to the dictionary: 6623 lines in [`DICTIONARY_FILE_NAME`], plus the
/// space PaddleOCR appends, plus the CTC blank. Reading the dictionary literally
/// gives 6624 and the engine refuses the pair -- UP-TAKE `I-333`. If a future
/// conversion changes this number, the two files below stop being a matching set
/// and both digests move together.
pub const RECOGNITION_CLASS_COUNT: usize = 6625;

/// The manifest for PP-OCRv4, served from `base_url`.
///
/// `base_url` is joined with a single `/` and each asset's file name. It must be
/// HTTPS; [`Asset::validate`] enforces that, and this function surfaces the
/// error rather than asserting.
///
/// **Nothing is published at any URL yet** -- see this module's header. A caller
/// today is a test or a developer pointing at a local server.
///
/// # Errors
///
/// [`ManifestError`] if the resulting manifest is not valid: a non-HTTPS base,
/// or a base that makes two assets collide.
pub fn ppocr_v4(base_url: &str) -> Result<AssetManifest, ManifestError> {
    let base = base_url.trim_end_matches('/');
    let entry = |file_name: &str, digest: &str, size: u64, kind: AssetKind| Asset {
        file_name: file_name.to_owned(),
        url: format!("{base}/{file_name}"),
        // Every digest here is a literal in this file, checked by
        // `every_pinned_digest_is_well_formed` below, so a parse failure is a
        // typo caught in CI rather than a run-time condition to model.
        digest: Sha256Digest::parse_hex(digest)
            .unwrap_or_else(|| unreachable!("{file_name}'s pinned digest is malformed")),
        size_bytes: size,
        kind,
    };

    AssetManifest::new(vec![
        entry(
            DETECTION_FILE_NAME,
            DETECTION_SHA256,
            DETECTION_SIZE,
            AssetKind::Model,
        ),
        entry(
            RECOGNITION_FILE_NAME,
            RECOGNITION_SHA256,
            RECOGNITION_SIZE,
            AssetKind::Model,
        ),
        entry(
            DICTIONARY_FILE_NAME,
            DICTIONARY_SHA256,
            DICTIONARY_SIZE,
            AssetKind::Dictionary,
        ),
    ])
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const BASE: &str = "https://example.invalid/models/v4";

    #[test]
    fn every_pinned_digest_is_well_formed() {
        // `ppocr_v4` would otherwise reach `unreachable!` at run time on a typo
        // in a 64-character constant, which is the least testable place for a
        // hand-copied hash to be wrong.
        for digest in [DETECTION_SHA256, RECOGNITION_SHA256, DICTIONARY_SHA256] {
            assert!(
                Sha256Digest::parse_hex(digest).is_some(),
                "{digest} is not 64 hex characters"
            );
        }
    }

    #[test]
    fn the_manifest_names_three_assets_and_validates() {
        let manifest = ppocr_v4(BASE).unwrap();
        assert_eq!(manifest.assets.len(), 3);
        assert_eq!(
            manifest.total_bytes(),
            DETECTION_SIZE + RECOGNITION_SIZE + DICTIONARY_SIZE
        );
    }

    #[test]
    fn the_two_models_and_the_one_dictionary_are_classified_apart() {
        let manifest = ppocr_v4(BASE).unwrap();
        assert_eq!(manifest.of_kind(AssetKind::Model).count(), 2);
        assert_eq!(manifest.of_kind(AssetKind::Dictionary).count(), 1);
        // No runtime here on purpose: ADR-0032's DLL is a separate acquisition
        // whose source has not been decided, and inventing an entry for it would
        // put a digest in this file that nothing produced.
        assert_eq!(manifest.of_kind(AssetKind::Runtime).count(), 0);
    }

    #[test]
    fn a_trailing_slash_on_the_base_does_not_double_up() {
        let manifest = ppocr_v4("https://example.invalid/models/v4/").unwrap();
        assert!(
            manifest
                .assets
                .iter()
                .all(|asset| !asset.url.contains("//models")),
            "a trailing slash produced a doubled separator"
        );
        assert_eq!(
            manifest.assets[0].url,
            format!("https://example.invalid/models/v4/{DETECTION_FILE_NAME}")
        );
    }

    #[test]
    fn a_plain_http_base_is_refused_rather_than_downgraded() {
        let error = ppocr_v4("http://example.invalid/models").unwrap_err();
        assert!(
            matches!(error, ManifestError::InsecureUrl { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn the_recognisers_class_count_matches_the_dictionary_arithmetic() {
        // 6623 lines in ppocr_keys_v1.txt, + 1 appended space, + 1 CTC blank.
        // Stated as arithmetic rather than as a bare constant so a future change
        // to either side has to restate which half moved. UP-TAKE I-333.
        const DICTIONARY_LINES: usize = 6623;
        assert_eq!(RECOGNITION_CLASS_COUNT, DICTIONARY_LINES + 1 + 1);
    }
}
