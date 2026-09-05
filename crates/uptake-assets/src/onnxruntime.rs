//! The ONNX Runtime build UP-TAKE ships, pinned by digest.
//!
//! # Why this file exists
//!
//! [`ADR-0032`] decision 2: the runtime DLL is acquired by *"a documented,
//! checksummed step … pinned SHA-256, verified before load, HTTPS only"*, and
//! [`ADR-0035`] then decided it ships **inside the installer** rather than
//! being fetched on first run. Between those two records the step itself was
//! never written: until 2026-09-02 nothing in this repository referred to
//! `onnxruntime.dll` except the code that looked for one. This module is the
//! pinned half of that step; `scripts/acquire-onnxruntime.py` is the fetching
//! half, and `src-tauri/src/ocr.rs` is the verifying-before-load half.
//!
//! # Why the DLL is pinned and the ZIP is pinned too
//!
//! They are different promises. The **archive** digest is what the acquisition
//! script checks, so a tampered download is refused before anything is
//! extracted. The **DLL** digest is what the application checks at load time,
//! so a file replaced on disk *after* installation is refused as well. A
//! bundled file can still be swapped by anything with write access to Program
//! Files, which is why `ADR-0032` decision 2 was never only about transit.
//!
//! # The notices are pinned for the same reason, and it is not paranoia
//!
//! ONNX Runtime is MIT, and MIT requires the copyright and permission notice to
//! travel with *"copies or substantial portions of the Software"*. Shipping the
//! DLL in an installer is a copy, so `LICENSE` must travel, and because that
//! repository bundles other people's code its `ThirdPartyNotices.txt` must
//! travel too. **`cargo deny` cannot see either**: it walks the crate graph, and
//! a DLL is not a crate. Nothing else in this repository would notice their
//! absence, which is exactly why they are digest-pinned here rather than left as
//! files somebody is trusted to copy.
//!
//! [`ADR-0032`]: ../../../Projects/UP-TAKE/DECISIONS/ADR-0032-onnx-runtime-is-loaded-not-downloaded.md
//! [`ADR-0035`]: ../../../Projects/UP-TAKE/DECISIONS/ADR-0035-assets-ship-in-the-installer.md

use crate::manifest::{Asset, AssetKind, AssetManifest, ManifestError, Sha256Digest};

/// The release this pin describes.
///
/// **Moving it is a deliberate change with its own review**, the same
/// discipline `ADR-0032` decision 4 applies to the `ort` pin: every digest
/// below moves with it, and so does the compatibility claim in
/// [`VERIFIED_AGAINST_ORT`].
pub const VERSION: &str = "1.29.0";

/// Where the official Windows x64 release is published.
///
/// HTTPS, and GitHub's own release host rather than a mirror. `Asset::validate`
/// enforces the scheme; this constant is what makes the *origin* auditable.
pub const ARCHIVE_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.29.0/onnxruntime-win-x64-1.29.0.zip";

/// The archive's SHA-256, probed on 2026-09-02 by downloading it.
///
/// ⚠️ **Nothing polls this URL, and that is intended behaviour rather than a
/// fault.** `scripts/write-model-notice.py` says the same about its own
/// upstream and it is worth saying here too: if Microsoft ever replaced this
/// release asset in place, no probe in either repository would notice. The
/// acquisition script would go red the next time it ran, and that red is the
/// whole notification.
///
/// **The exposure is smaller here than for the models, and the difference is
/// the host rather than an argument.** A GitHub release asset is effectively
/// immutable once published: replacing one requires deleting and re-uploading
/// it under the same tag. `ADR-0034`'s PaddleOCR archives sit on
/// `paddleocr.bj.bcebos.com`, an ordinary object store with no such property,
/// which is why those needed a workspace probe (`CL-32`) and this does not.
pub const ARCHIVE_SHA256: &str = "c9b4b7086b529ad814f428c1bad028e20a25d7dc0699836775faace4ab5b78b2";

/// The archive's length in bytes, from the same download.
pub const ARCHIVE_SIZE: u64 = 79_645_520;

/// The DLL's name once installed, beside the executable.
pub const RUNTIME_FILE_NAME: &str = "onnxruntime.dll";

/// The DLL's SHA-256, as extracted from the pinned archive.
pub const RUNTIME_SHA256: &str = "69d8e6d3879a3b4001cdc74c8ed9ccc7e7f799a5b847059738323404519ec471";

/// The DLL's length in bytes.
///
/// **16.1 MB, and that number has a consequence.** `ADR-0035` set the installer
/// target at `< 35 MB` with a 40 MB hard fail, against a measured 29.2 MB that
/// predates this pin.
///
/// **Do not read a total out of this comment.** It said "~31.7 MB" and
/// designated itself "the figure to read when the installer bar is next
/// measured", which was exact at `3e58ad3` and went 5.15 MB stale the moment
/// `ADR-0036` swapped in a larger detector -- landing on the wrong side of the
/// 35 MB target with nothing announcing it. Found by round 3 of `PR #88`'s
/// independent review.
///
/// The total is `RUNTIME_SIZE + DETECTION_SIZE + RECOGNITION_SIZE +
/// DICTIONARY_SIZE`, every term a pinned constant in this crate, and
/// `installer_payload_bytes()` below sums them so the number cannot be
/// hand-copied and cannot drift again.
pub const RUNTIME_SIZE: u64 = 16_149_344;

/// The bytes this crate's pins put in the installer, before compression.
///
/// Exists because a hand-written total in a doc comment drifted (see
/// [`RUNTIME_SIZE`]). Derived, so it moves when a pin moves.
///
/// **Every pinned file the release config bundles is counted, notices
/// included.** An earlier version summed only the runtime and the three model
/// files and so under-reported by 344,343 bytes -- `PR #88` round 4, F6. The
/// two notice files are not incidental: they are the MIT and Apache-2.0
/// obligations `ADR-0032` and `ADR-0034` put in the installer, `cargo deny`
/// cannot see them because it walks the crate graph, and a budget that ignores
/// them is a budget that would let them be dropped to save space.
#[must_use]
pub const fn installer_payload_bytes() -> u64 {
    RUNTIME_SIZE
        + LICENCE_SIZE
        + NOTICES_SIZE
        + crate::ppocr::DETECTION_SIZE
        + crate::ppocr::RECOGNITION_SIZE
        + crate::ppocr::DICTIONARY_SIZE
}

/// ONNX Runtime's MIT licence text, as it travels in the installer.
pub const LICENCE_FILE_NAME: &str = "LICENSE-onnxruntime.txt";

/// The licence file's SHA-256.
pub const LICENCE_SHA256: &str = "c250d6278f0b47a6439fb7592b08b58a55eb9f535aa49a1db63211c3f982b674";

/// The licence file's length in bytes.
pub const LICENCE_SIZE: u64 = 1_094;

/// ONNX Runtime's bundled third-party notices, as they travel in the installer.
///
/// **The file a project forgets**, because nothing about the `ort` crate
/// mentions it and it is not discoverable from the Rust side at all.
pub const NOTICES_FILE_NAME: &str = "ThirdPartyNotices-onnxruntime.txt";

/// The notices file's SHA-256.
pub const NOTICES_SHA256: &str = "4c5b864d8974c94b37461f38163facef79a1bb5dea461667ee9e5be6a8e73f83";

/// The notices file's length in bytes.
pub const NOTICES_SIZE: u64 = 343_249;

/// The `ort` release this runtime was observed to work with.
///
/// **Observed, not inferred.** `ort` resolves `ORT_API_VERSION = 17` in this
/// build because no `api-*` feature is enabled, and ONNX Runtime supports older
/// API versions from newer runtimes -- but that is a claim about someone else's
/// compatibility policy, which is the class `D-29` says to probe rather than
/// assert. Probed 2026-09-02 by loading both PP-OCRv4 models through this exact
/// DLL and reading text off a rendered image.
pub const VERIFIED_AGAINST_ORT: &str = "2.0.0-rc.13";

/// The three files the installer must carry for ONNX Runtime.
///
/// Ordered runtime first: it is the one whose absence stops OCR working, so a
/// partial install reports the useful thing first.
///
/// # Errors
///
/// [`ManifestError`] if the manifest is not valid -- a non-HTTPS base or a name
/// collision. Both are typos caught by this module's own tests rather than
/// run-time conditions to model.
pub fn onnxruntime() -> Result<AssetManifest, ManifestError> {
    let entry = |file_name: &str, digest: &str, size: u64, kind: AssetKind| Asset {
        file_name: file_name.to_owned(),
        // Every one of these files comes out of the SAME archive, so they share
        // its URL rather than each having one of their own. Nothing dereferences
        // this per file: the acquisition script fetches the archive once and
        // extracts three members from it, and the application only ever reads
        // these entries to verify bytes already on disk.
        url: ARCHIVE_URL.to_owned(),
        digest: Sha256Digest::parse_hex(digest)
            .unwrap_or_else(|| unreachable!("{file_name}'s pinned digest is malformed")),
        size_bytes: size,
        kind,
    };

    AssetManifest::new(vec![
        entry(
            RUNTIME_FILE_NAME,
            RUNTIME_SHA256,
            RUNTIME_SIZE,
            AssetKind::Runtime,
        ),
        entry(
            LICENCE_FILE_NAME,
            LICENCE_SHA256,
            LICENCE_SIZE,
            AssetKind::Notice,
        ),
        entry(
            NOTICES_FILE_NAME,
            NOTICES_SHA256,
            NOTICES_SIZE,
            AssetKind::Notice,
        ),
    ])
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
    fn every_pinned_digest_is_well_formed() {
        // A malformed digest would reach `unreachable!` inside `onnxruntime()`
        // at run time, which is a panic in an always-on tray app. Catching it
        // here makes it a typo CI finds.
        for (what, digest) in [
            ("archive", ARCHIVE_SHA256),
            ("runtime", RUNTIME_SHA256),
            ("licence", LICENCE_SHA256),
            ("notices", NOTICES_SHA256),
        ] {
            assert!(
                Sha256Digest::parse_hex(digest).is_some(),
                "{what}'s pinned digest is not 64 hex characters"
            );
        }
    }

    #[test]
    fn the_manifest_is_valid_and_carries_all_three_files() {
        let manifest = onnxruntime().expect("the pinned manifest must be valid");
        let names: Vec<&str> = manifest
            .assets
            .iter()
            .map(|asset| asset.file_name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![RUNTIME_FILE_NAME, LICENCE_FILE_NAME, NOTICES_FILE_NAME],
            "the installer carries the runtime and BOTH notice files, in this order"
        );
    }

    #[test]
    fn both_notices_travel_with_the_runtime() {
        // The licence obligation in one assertion. MIT requires the notice to
        // travel with a copy, and ONNX Runtime bundles other people's code, so
        // dropping either file is a licence defect that `cargo deny` cannot
        // see -- it walks the crate graph and a DLL is not a crate. This is the
        // only mechanical check that either file is even named.
        let manifest = onnxruntime().unwrap();
        let notices: Vec<&str> = manifest
            .of_kind(AssetKind::Notice)
            .map(|asset| asset.file_name.as_str())
            .collect();
        assert_eq!(
            notices.len(),
            2,
            "shipping the DLL without its LICENSE and ThirdPartyNotices is an MIT violation"
        );
    }

    #[test]
    fn the_archive_url_is_https_and_names_the_pinned_version() {
        // `Asset::validate` refuses a non-HTTPS asset URL, and every asset here
        // carries the ARCHIVE_URL, so a downgrade to http would fail
        // `the_manifest_is_valid_and_carries_all_three_files` above. What that
        // does NOT catch is the URL drifting off the version this file pins,
        // which would leave the script fetching one release and verifying
        // another's digests -- a refusal at best, and a confusing one.
        assert!(ARCHIVE_URL.starts_with("https://"));
        assert!(
            ARCHIVE_URL.contains(VERSION),
            "the archive URL must name the version this file pins"
        );
    }

    #[test]
    fn the_installer_payload_is_derived_and_not_hand_written() {
        // The whole point of the function: it cannot disagree with the pins.
        assert_eq!(
            installer_payload_bytes(),
            RUNTIME_SIZE
                + LICENCE_SIZE
                + NOTICES_SIZE
                + crate::ppocr::DETECTION_SIZE
                + crate::ppocr::RECOGNITION_SIZE
                + crate::ppocr::DICTIONARY_SIZE
        );
    }

    #[test]
    fn the_installer_payload_stays_inside_adr_0035s_hard_fail() {
        // ADR-0035 set a < 35 MB target and a 40 MB HARD FAIL. Until now both
        // were sentences in a decision record, and a doc comment carrying a
        // hand-copied total went 5.15 MB stale across ADR-0036's detector swap
        // -- landing past the target with nothing going red (PR #88 round 3,
        // F2). This is the check that would have caught it.
        //
        // The TARGET is deliberately not asserted here: a payload past the
        // target is a decision, and the hard fail is the line that must not
        // move without one.
        //
        // ⚠️ RAISED 2026-09-05 to 60 MB by ADR-0037, which is the decision
        // record this test's own message demanded. It fired at 47,264,179
        // bytes when both models became the PP-OCRv6 small tier, said "moving
        // this line needs a decision record, not a bigger constant", and the
        // record was written before this constant moved. That order is the
        // whole point of the check.
        const HARD_FAIL: u64 = 60_000_000;
        assert!(
            installer_payload_bytes() < HARD_FAIL,
            "installer payload is {} bytes, past ADR-0035's {} byte hard fail;              moving this line needs a decision record, not a bigger constant",
            installer_payload_bytes(),
            HARD_FAIL
        );
    }
}
