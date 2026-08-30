//! What must be acquired, and what each file must hash to.
//!
//! **The manifest is data, and that is the point.** ADR-0034 decided that the
//! PP-OCRv4 ONNX models are produced by *our* conversion of PaddleOCR's official
//! release rather than taken from a third party, and ADR-0032 decided the same
//! for ONNX Runtime. Both records require the identical discipline -- *"a
//! documented, checksummed step: pinned SHA-256, verified before load, HTTPS
//! only"* -- so both are described here by one type rather than by two
//! mechanisms that could drift apart.
//!
//! Nothing in this module performs a download or touches the filesystem.

use std::fmt;

/// A SHA-256 digest, as 32 bytes.
///
/// A fixed-size array rather than a `String`, so a malformed digest cannot exist
/// past parsing. Comparing hex strings would make a length mistake or a case
/// mistake into a silent mismatch at verification time, which reads as "the
/// download is corrupt" and sends the reader looking in the wrong place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Wraps 32 raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parses the 64-character lowercase or uppercase hex form.
    ///
    /// Returns `None` for anything else -- wrong length, non-hex characters,
    /// embedded whitespace. **There is no lenient path**: a digest that is
    /// almost right is not a digest, and accepting one would mean a typo in a
    /// pinned hash becomes a runtime "file is corrupt" rather than a manifest
    /// error a test catches.
    #[must_use]
    pub fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_value(pair[0])?;
            let low = hex_value(pair[1])?;
            bytes[index] = (high << 4) | low;
        }
        Some(Self(bytes))
    }

    /// The raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One hex character's value, or `None`.
fn hex_value(character: u8) -> Option<u8> {
    match character {
        b'0'..=b'9' => Some(character - b'0'),
        b'a'..=b'f' => Some(character - b'a' + 10),
        b'A'..=b'F' => Some(character - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// What an asset is for, which decides where it is installed and what breaks
/// without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    /// ONNX Runtime itself. ADR-0032: loaded at run time from a path we choose.
    Runtime,
    /// A model the OCR engine loads. ADR-0034: converted by us from PaddleOCR's
    /// official release.
    Model,
    /// The character dictionary the recogniser's classes index into.
    Dictionary,
}

/// One file that must be acquired before OCR can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// The file name it is installed as, with no directory part.
    pub file_name: String,
    /// Where to get it. **Must be `https://`** -- see [`Asset::validate`].
    pub url: String,
    /// What the bytes must hash to. Verified before the file is usable.
    pub digest: Sha256Digest,
    /// How many bytes to expect.
    ///
    /// Not a security property -- the digest is that -- but it is what makes a
    /// progress bar possible and what lets a wrong-file download be reported as
    /// "this is not the file we expected" rather than as a hash mismatch after
    /// a 200 MB transfer.
    pub size_bytes: u64,
    /// What it is for.
    pub kind: AssetKind,
}

/// Why a manifest is not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// An asset's URL is not HTTPS.
    #[error("{file_name} is served over {scheme}, and only https is allowed")]
    InsecureUrl {
        /// Which asset.
        file_name: String,
        /// What it asked for.
        scheme: String,
    },
    /// An asset's file name would escape the install directory.
    #[error("{file_name} is not a plain file name")]
    UnsafeFileName {
        /// Which asset.
        file_name: String,
    },
    /// Two assets install to the same name.
    #[error("two assets both install as {file_name}")]
    DuplicateFileName {
        /// The colliding name.
        file_name: String,
    },
    /// An asset declares a zero length.
    #[error("{file_name} declares a size of zero bytes")]
    EmptyAsset {
        /// Which asset.
        file_name: String,
    },
}

impl Asset {
    /// Checks the two properties that are this type's job to guarantee.
    ///
    /// # HTTPS only
    ///
    /// ADR-0032 decision 2 says so in as many words, and the reason is
    /// `architecture.md` section 4: the model file is *"arbitrary code"*, and the
    /// runtime is a larger surface than the model. A pinned digest already
    /// defeats a tampered file, so this is defence in depth rather than the
    /// primary control -- but it costs nothing, and a plain-HTTP URL in a
    /// manifest is a mistake nobody would notice from behaviour, because the
    /// download would simply succeed.
    ///
    /// # A plain file name
    ///
    /// The name is joined to an install directory, so `..`, a separator, or an
    /// absolute path would write outside it. That is the classic archive-
    /// extraction escape, and it is worth refusing here even though every name
    /// in the built-in manifest is a literal this repository controls: the
    /// manifest is data, and data acquires new sources.
    ///
    /// # Errors
    ///
    /// [`ManifestError`] naming which property failed and for which file.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !self.url.starts_with("https://") {
            let scheme = self
                .url
                .split_once("://")
                .map_or_else(|| "no scheme".to_owned(), |(scheme, _)| scheme.to_owned());
            return Err(ManifestError::InsecureUrl {
                file_name: self.file_name.clone(),
                scheme,
            });
        }
        if !is_plain_file_name(&self.file_name) {
            return Err(ManifestError::UnsafeFileName {
                file_name: self.file_name.clone(),
            });
        }
        if self.size_bytes == 0 {
            return Err(ManifestError::EmptyAsset {
                file_name: self.file_name.clone(),
            });
        }
        Ok(())
    }
}

/// Whether a name is a single path component with no traversal.
///
/// Rejects an empty name, `.`, `..`, anything containing `/` or `\`, and
/// anything Windows would read as a drive or UNC prefix. Both separators are
/// rejected on every platform deliberately: a manifest written on Windows must
/// not become an escape when the same file is read on Linux.
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
}

/// Everything that must be present before OCR can run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AssetManifest {
    /// The assets, in the order they should be fetched.
    pub assets: Vec<Asset>,
}

impl AssetManifest {
    /// Builds a manifest and checks every asset.
    ///
    /// # Errors
    ///
    /// The first [`ManifestError`] found, including a duplicate install name
    /// across two otherwise-valid assets.
    pub fn new(assets: Vec<Asset>) -> Result<Self, ManifestError> {
        for asset in &assets {
            asset.validate()?;
        }
        for (index, asset) in assets.iter().enumerate() {
            if assets[..index]
                .iter()
                .any(|earlier| earlier.file_name == asset.file_name)
            {
                return Err(ManifestError::DuplicateFileName {
                    file_name: asset.file_name.clone(),
                });
            }
        }
        Ok(Self { assets })
    }

    /// Total bytes across every asset, for a whole-manifest progress figure.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.assets.iter().map(|asset| asset.size_bytes).sum()
    }

    /// The assets of one kind.
    pub fn of_kind(&self, kind: AssetKind) -> impl Iterator<Item = &Asset> {
        self.assets.iter().filter(move |asset| asset.kind == kind)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn digest() -> Sha256Digest {
        Sha256Digest::parse_hex(ZEROS).unwrap()
    }

    fn asset(file_name: &str, url: &str) -> Asset {
        Asset {
            file_name: file_name.to_owned(),
            url: url.to_owned(),
            digest: digest(),
            size_bytes: 100,
            kind: AssetKind::Model,
        }
    }

    #[test]
    fn a_digest_round_trips_through_hex() {
        let text = "5f7217e0a89612e2f80d62f3c99a8bf5f7ae9cdc1ffd706be7dde07765627edf";
        let parsed = Sha256Digest::parse_hex(text).unwrap();
        assert_eq!(parsed.to_string(), text);
    }

    #[test]
    fn uppercase_hex_parses_to_the_same_digest() {
        let lower = "5f7217e0a89612e2f80d62f3c99a8bf5f7ae9cdc1ffd706be7dde07765627edf";
        assert_eq!(
            Sha256Digest::parse_hex(lower).unwrap(),
            Sha256Digest::parse_hex(&lower.to_uppercase()).unwrap()
        );
    }

    #[test]
    fn a_malformed_digest_is_refused_rather_than_truncated() {
        // Every one of these is a plausible typo in a pinned hash, and every one
        // must fail at the manifest rather than as a mismatch after a download.
        assert!(Sha256Digest::parse_hex("").is_none(), "empty");
        assert!(Sha256Digest::parse_hex(&ZEROS[..63]).is_none(), "too short");
        assert!(
            Sha256Digest::parse_hex(&format!("{ZEROS}0")).is_none(),
            "too long"
        );
        assert!(
            Sha256Digest::parse_hex(&format!("{}zz", &ZEROS[..62])).is_none(),
            "non-hex"
        );
        assert!(
            Sha256Digest::parse_hex(&format!("{} ", &ZEROS[..63])).is_none(),
            "trailing space"
        );
    }

    #[test]
    fn hex_parsing_maps_each_byte_to_the_right_position() {
        // A digest that is byte-reversed or nibble-swapped would still round
        // trip through Display, so pin the actual bytes.
        let parsed = Sha256Digest::parse_hex(&format!("0123456789abcdef{}", &ZEROS[..48])).unwrap();
        assert_eq!(parsed.as_bytes()[0], 0x01);
        assert_eq!(parsed.as_bytes()[1], 0x23);
        assert_eq!(parsed.as_bytes()[7], 0xef);
        assert_eq!(parsed.as_bytes()[8], 0x00);
    }

    #[test]
    fn https_is_accepted_and_plain_http_is_not() {
        assert!(
            asset("model.onnx", "https://example.test/m")
                .validate()
                .is_ok()
        );
        let error = asset("model.onnx", "http://example.test/m")
            .validate()
            .unwrap_err();
        assert!(
            matches!(error, ManifestError::InsecureUrl { ref scheme, .. } if scheme == "http"),
            "got {error}"
        );
    }

    #[test]
    fn a_url_with_no_scheme_at_all_is_refused() {
        let error = asset("model.onnx", "example.test/m")
            .validate()
            .unwrap_err();
        assert!(
            matches!(error, ManifestError::InsecureUrl { .. }),
            "got {error}"
        );
    }

    #[test]
    fn a_scheme_that_merely_starts_with_https_is_not_https() {
        // "httpsx://" starts with "https" but is not the scheme. The check uses
        // the full "https://" prefix for exactly this reason.
        let error = asset("model.onnx", "httpsx://example.test/m")
            .validate()
            .unwrap_err();
        assert!(
            matches!(error, ManifestError::InsecureUrl { .. }),
            "got {error}"
        );
    }

    #[test]
    fn a_file_name_that_would_escape_the_install_directory_is_refused() {
        for name in [
            "../escape.onnx",
            "..",
            ".",
            "",
            "sub/dir.onnx",
            r"sub\dir.onnx",
            "C:/absolute.onnx",
            r"C:\absolute.onnx",
        ] {
            let error = asset(name, "https://example.test/m")
                .validate()
                .unwrap_err();
            assert!(
                matches!(error, ManifestError::UnsafeFileName { .. }),
                "{name:?} was accepted, or failed for the wrong reason: {error}"
            );
        }
    }

    #[test]
    fn both_separators_are_refused_on_every_platform() {
        // A manifest authored on Windows must not become a traversal when the
        // same data is read on Linux, so neither separator is platform-gated.
        assert!(!is_plain_file_name("a/b"));
        assert!(!is_plain_file_name(r"a\b"));
    }

    #[test]
    fn an_ordinary_name_with_dots_is_still_a_plain_name() {
        assert!(is_plain_file_name("ch_PP-OCRv4_det_infer.onnx"));
        assert!(is_plain_file_name("onnxruntime.dll"));
        assert!(is_plain_file_name(".hidden"));
    }

    #[test]
    fn a_zero_length_asset_is_refused() {
        let mut empty = asset("model.onnx", "https://example.test/m");
        empty.size_bytes = 0;
        assert!(matches!(
            empty.validate().unwrap_err(),
            ManifestError::EmptyAsset { .. }
        ));
    }

    #[test]
    fn two_assets_installing_to_one_name_are_refused() {
        let error = AssetManifest::new(vec![
            asset("model.onnx", "https://example.test/a"),
            asset("model.onnx", "https://example.test/b"),
        ])
        .unwrap_err();
        assert!(
            matches!(error, ManifestError::DuplicateFileName { .. }),
            "got {error}"
        );
    }

    #[test]
    fn a_manifest_reports_its_total_and_filters_by_kind() {
        let mut runtime = asset("onnxruntime.dll", "https://example.test/r");
        runtime.kind = AssetKind::Runtime;
        runtime.size_bytes = 11_000_000;
        let mut model = asset("det.onnx", "https://example.test/d");
        model.size_bytes = 4_700_000;

        let manifest = AssetManifest::new(vec![runtime, model]).unwrap();
        assert_eq!(manifest.total_bytes(), 15_700_000);
        assert_eq!(manifest.of_kind(AssetKind::Runtime).count(), 1);
        assert_eq!(manifest.of_kind(AssetKind::Model).count(), 1);
        assert_eq!(manifest.of_kind(AssetKind::Dictionary).count(), 0);
    }

    #[test]
    fn an_empty_manifest_is_valid_and_totals_zero() {
        let manifest = AssetManifest::new(Vec::new()).unwrap();
        assert_eq!(manifest.total_bytes(), 0);
        assert!(manifest.assets.is_empty());
    }
}
