//! The OCR model manifest: what UP-TAKE must acquire before OCR can run.
//!
//! # ONE of these digests is ours and TWO are Baidu's, and the split is the point
//!
//! ⚠️ **This header said "These digests are OURS" of all three until
//! 2026-09-04.** That stopped being true when [`ADR-0036`] made the detector
//! PaddlePaddle's own ONNX build of PP-OCRv6. Corrected here rather than left
//! standing, because the whole value of the section is knowing *whose bytes*
//! each hash covers.
//!
//! [`ADR-0034`] chose to convert PaddleOCR's official release to ONNX ourselves
//! rather than take **a third party's** conversion, specifically so that the
//! SHA-256 pinned here means what [`ADR-0032`] decision 2 says a checksum means.
//! A hash over bytes someone else produced proves only that a file arrived
//! intact; a hash over bytes *we* produced pins our own artifact.
//!
//! | File | Whose bytes | Acquired by |
//! | --- | --- | --- |
//! | [`RECOGNITION_FILE_NAME`] | **Baidu's**, unmodified | `acquire-ppocr-recogniser.py` |
//! | [`DICTIONARY_FILE_NAME`] | PaddleOCR's, copied byte for byte | the same script |
//! | [`DETECTION_FILE_NAME`] | **Baidu's**, unmodified | `acquire-ppocr-detector.py` |
//!
//! **`ADR-0034`'s objection was to an unaccountable converter, and it still
//! holds.** The model's own author publishing ONNX under its own name is a
//! different provenance class, which that record never considered -- the
//! artifact predated it by eighty-two days. [`ADR-0036`] has the argument.
//!
//! ⚠️ **THE RECOGNISER IS NO LONGER CONVERTED EITHER.** This paragraph said its
//! digest came from `convert-ppocr-models.py` on 2026-09-01, which was true
//! until [`ADR-0037`] took the recogniser to Baidu's own published ONNX on
//! 2026-09-05, superseding `ADR-0034` for it as `ADR-0036` had for the
//! detector. **Nothing in UP-TAKE's OCR path is converted here now**, and that
//! script is deleted.
//!
//! The one artifact still OURS is the dictionary: PP-OCRv6 keeps its character
//! list inside the recogniser's `inference.yml` rather than publishing it
//! standalone, so [`DICTIONARY_SHA256`] pins this repository's extraction. The
//! characters are upstream's, in upstream's order; the container is ours.
//!
//! # Where the files are served from is NOT settled here
//!
//! [`ppocr_models`] takes a base URL rather than hardcoding one, and that is a
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
//! [`ADR-0036`]: https://github.com/VyLoneHQ/up-take

use crate::manifest::{Asset, AssetKind, AssetManifest, ManifestError, Sha256Digest};

/// The detection model: **PP-OCRv6 small**'s Differentiable Binarization network.
///
/// # Baidu's artifact, not ours, and that is `ADR-0036`
///
/// ⚠️ **This paragraph used to say every OTHER pin in this file was a digest
/// over bytes this repository produced by conversion.** That stopped being
/// true on 2026-09-05: `ADR-0037` took the recogniser to Baidu's published
/// ONNX as well, so both models are now downloaded and verified byte for byte,
/// the way `acquire-onnxruntime.py` handles the runtime. Only the dictionary
/// is still ours, and only because upstream does not publish it separately.
///
/// `ADR-0034` rejected taking *a third party's* conversion, on the ground that
/// such a checksum "proves nothing about provenance". **That objection stands.**
/// The model's own author publishing under its own name is a different thing,
/// and `ADR-0034` never considered it -- the artifact predated that record by
/// eighty-two days.
///
/// ⚠️ **A republish upstream reads as CORRUPT, not as UPDATED.** The digest
/// cannot tell "Baidu changed the file" from "somebody substituted it", and it
/// must not: that is the whole point of pinning. If verification starts failing
/// on a clean download, read `ADR-0036` before touching this constant.
pub const DETECTION_FILE_NAME: &str = "PP-OCRv6_small_det.onnx";
/// SHA-256 of [`DETECTION_FILE_NAME`] as **Baidu publishes it**.
///
/// Observed identical on two independent downloads, 2026-09-04.
pub const DETECTION_SHA256: &str =
    "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e";
/// Size of [`DETECTION_FILE_NAME`] in bytes.
pub const DETECTION_SIZE: u64 = 9_880_512;

/// Where [`DETECTION_FILE_NAME`] is fetched from.
///
/// PaddlePaddle's own HuggingFace organisation. Named here rather than in the
/// script for the reason every other pin is: the acquisition step reads it out
/// of this source, so there is exactly one statement of where the bytes come
/// from and a change to it cannot be made in only one place.
pub const DETECTION_URL: &str =
    "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/main/inference.onnx";

/// The recognition model: PP-OCRv6 small, 18710 output classes.
pub const RECOGNITION_FILE_NAME: &str = "PP-OCRv6_small_rec.onnx";
/// SHA-256 of [`RECOGNITION_FILE_NAME`] as this repository's script produces it.
pub const RECOGNITION_SHA256: &str =
    "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634";
/// Size of [`RECOGNITION_FILE_NAME`] in bytes.
pub const RECOGNITION_SIZE: u64 = 21_159_378;

/// Where [`RECOGNITION_FILE_NAME`] is fetched from.
///
/// Baidu's own published ONNX, same organisation and same shape as
/// [`DETECTION_URL`]. `ADR-0037` supersedes `ADR-0034` for the recogniser as
/// well, so nothing in UP-TAKE's OCR path is converted here any more.
pub const RECOGNITION_URL: &str =
    "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/main/inference.onnx";

/// Where the dictionary is DERIVED from, which is not the same as fetched.
///
/// PP-OCRv6 does not publish the character list as a standalone file the way
/// PP-OCRv4 did. It lives inside the model's own `inference.yml`, as the
/// `character_dict` block, so the acquisition step downloads that and extracts
/// it.
///
/// **[`DICTIONARY_SHA256`] is therefore a digest over OUR extraction, not over
/// a file Baidu published**, and that is the same distinction `ADR-0034` drew
/// about the converted models: a hash over bytes someone else produced proves
/// they arrived intact, and a hash over bytes we produced pins our own
/// artifact. The extraction is deterministic and the script re-derives it, so a
/// change upstream shows up as a digest mismatch rather than as a silently
/// different alphabet.
pub const DICTIONARY_URL: &str =
    "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/main/inference.yml";

/// The character dictionary, extracted from the recogniser's own `inference.yml`.
pub const DICTIONARY_FILE_NAME: &str = "ppocr_keys_v6_small.txt";
/// SHA-256 of [`DICTIONARY_FILE_NAME`] **as this repository extracts it** from
/// [`DICTIONARY_URL`]. Not a digest over a file Baidu publishes: see that
/// constant. The `v2.7.0` tag this line used to name belonged to PP-OCRv4's
/// standalone `ppocr_keys_v1.txt`, which `ADR-0037` stopped shipping.
pub const DICTIONARY_SHA256: &str =
    "f7aa897ca828a4c7c9e2739c30f9161a33306d532f020bcdb91dcfb664a5507e";
/// Size of [`DICTIONARY_FILE_NAME`] in bytes.
pub const DICTIONARY_SIZE: u64 = 74_946;

/// How many classes [`RECOGNITION_FILE_NAME`] emits on its last axis.
///
/// Recorded here as well as in `uptake-ocr` because it is the number that ties
/// the model to the dictionary: 18708 lines in [`DICTIONARY_FILE_NAME`], plus the
/// space PaddleOCR appends, plus the CTC blank. Reading the dictionary literally
/// gives 18709 and the engine refuses the pair -- UP-TAKE `I-333`. If a future
/// conversion changes this number, the RECOGNISER and the DICTIONARY stop being
/// a matching set and those two digests move together.
///
/// ⚠️ **The detector is not in that pair, and this said "the two files below"
/// when it was.** Detection is language- and dictionary-independent across
/// PP-OCRv4, v5 and v6 -- there is no `en_` detector in any of them -- which is
/// exactly why `ADR-0036` could swap it alone without touching this number.
pub const RECOGNITION_CLASS_COUNT: usize = 18710;

/// The manifest for UP-TAKE's OCR assets, served from `base_url`.
///
/// ⚠️ **This was `ppocr_v4` and its doc said "the manifest for PP-OCRv4"**
/// until 2026-09-05. `ADR-0036` moved the detector to PP-OCRv6 and `ADR-0037`
/// moved the recogniser, so the name described nothing this function returns.
/// Renamed rather than re-documented: `PR #88` round 3's F3 found the header
/// corrected and the name left standing a hundred lines below it, which is
/// the shape where a reader trusts whichever they read first.
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
pub fn ppocr_models(base_url: &str) -> Result<AssetManifest, ManifestError> {
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

    const BASE: &str = "https://example.invalid/models";

    #[test]
    fn every_pinned_digest_is_well_formed() {
        // `ppocr_models` would otherwise reach `unreachable!` at run time on a typo
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
        let manifest = ppocr_models(BASE).unwrap();
        assert_eq!(manifest.assets.len(), 3);
        assert_eq!(
            manifest.total_bytes(),
            DETECTION_SIZE + RECOGNITION_SIZE + DICTIONARY_SIZE
        );
    }

    #[test]
    fn the_two_models_and_the_one_dictionary_are_classified_apart() {
        let manifest = ppocr_models(BASE).unwrap();
        assert_eq!(manifest.of_kind(AssetKind::Model).count(), 2);
        assert_eq!(manifest.of_kind(AssetKind::Dictionary).count(), 1);
        // No runtime here on purpose: ADR-0032's DLL is a separate acquisition
        // whose source has not been decided, and inventing an entry for it would
        // put a digest in this file that nothing produced.
        assert_eq!(manifest.of_kind(AssetKind::Runtime).count(), 0);
    }

    #[test]
    fn a_trailing_slash_on_the_base_does_not_double_up() {
        let manifest = ppocr_models("https://example.invalid/models/").unwrap();
        assert!(
            manifest
                .assets
                .iter()
                .all(|asset| !asset.url.contains("//models")),
            "a trailing slash produced a doubled separator"
        );
        assert_eq!(
            manifest.assets[0].url,
            format!("https://example.invalid/models/{DETECTION_FILE_NAME}")
        );
    }

    #[test]
    fn a_plain_http_base_is_refused_rather_than_downgraded() {
        let error = ppocr_models("http://example.invalid/models").unwrap_err();
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
        const DICTIONARY_LINES: usize = 18708;
        assert_eq!(RECOGNITION_CLASS_COUNT, DICTIONARY_LINES + 1 + 1);
    }
}
