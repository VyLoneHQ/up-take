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
    /// A licence or third-party notice that must travel with a binary UP-TAKE
    /// distributes.
    ///
    /// **Not decoration, and not something the build can check for itself.**
    /// ADR-0032's licence section: MIT requires the notice to travel with a
    /// copy, ONNX Runtime bundles other people's code so it ships a
    /// `ThirdPartyNotices.txt` as well, and `cargo deny` walks the CRATE graph
    /// so it can see neither file. Giving notices a kind is what lets a test
    /// assert that they are in the manifest at all -- the only mechanical check
    /// that exists for an obligation nothing else in this repository can reach.
    Notice,
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
    /// absolute path would write outside it -- the classic archive-extraction
    /// escape. On Windows the check also refuses reserved device stems and
    /// trailing dots and spaces; see [`is_plain_file_name`] for why each.
    ///
    /// **There is no built-in manifest today**, so every name this sees will
    /// arrive from wherever `ADR-0034`'s conversion step eventually writes one.
    /// That is the argument for checking, not against it.
    ///
    /// *(This paragraph claimed the opposite -- that the check was
    /// belt-and-braces "even though every name in the built-in manifest is a
    /// literal this repository controls" -- until 2026-08-30. Round 1 of this
    /// PR's review falsified that, and the fix corrected the identical sentence
    /// in [`is_plain_file_name`]'s doc **nine lines below while leaving this
    /// one standing**. Round 2 found the survivor. Third partial sweep in one
    /// session; see UP-TAKE `I-334`.)*
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

/// Windows device stems, refused with or without an extension.
///
/// Opening `NUL` reaches a **device**, not a file in the directory: the write
/// succeeds, `is_file()` is false, reading back gives nothing, and the name is
/// absent from the listing. An asset written there is silently discarded and
/// re-fetched on every launch, because reading it back never matches the digest.
///
/// ⚠️ **THE "WHATEVER THE EXTENSION" HALF WAS FALSE AND IS CORRECTED.** This
/// said `NUL.onnx` "is the null device too". It is not. Measured on Windows 11
/// Pro 26200 (`PR #88` round 8, confirmed by a second probe): `NUL.onnx`,
/// `nul.onnx`, `CON.txt`, `COM1.txt`, `LPT9.onnx` and `NUL.tar.gz` each take 35
/// bytes, read 35 back, and appear in the directory listing. Only the BARE stem
/// reaches a device, and even that is path-form dependent -- `CON` hits the
/// device through a relative path and behaves as a file through an absolute
/// one, while `NUL` hits it either way.
///
/// **The extension forms are still refused, on the honest reason rather than
/// the false one.** The behaviour varies by path form, by API and by Windows
/// version; a flat rule costs nothing and does not turn on which runtime opened
/// the file. `scripts/rust_consts.py` states the same corrected version, and
/// `name-guard-cases.tsv` is what holds the two to it.
const RESERVED_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Whether a name is a single path component, with no traversal and no Windows
/// device name hiding inside it.
///
/// Rejects an empty name, `.`, `..`, anything containing a path separator or a
/// colon, a trailing dot or space, and any reserved device stem. Both
/// separators are rejected on every platform deliberately: a manifest written
/// on Windows must not become an escape when the same file is read on Linux,
/// and the device names are rejected everywhere for the same reason in reverse.
///
/// *(The device names, the trailing dot and the trailing space were added
/// 2026-08-30 by the independent review of `PR #77`. The original version
/// checked traversal only, and argued the check was belt-and-braces because
/// "every name in the built-in manifest is a literal this repository controls"
/// -- **which was false, because there is no built-in manifest at all yet.**
/// The reviewer said so, and the argument would have stayed false for as long
/// as the manifest's first real source was somewhere other than a Rust
/// literal.)*
fn is_plain_file_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.contains('/') || name.contains('\\') || name.contains(':') {
        return false;
    }
    // The Win32 path parser strips a trailing dot or space, so "model.onnx."
    // and "model.onnx " both open "model.onnx" -- two manifest entries that
    // look distinct and are not.
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name);
    !RESERVED_DEVICE_NAMES
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

/// Whether two install names would be ONE file on Windows.
///
/// # Why `eq_ignore_ascii_case` is not enough
///
/// NTFS folds case with its own upcase table, which covers far more than ASCII:
/// accented Latin, Greek, Cyrillic and more. `eq_ignore_ascii_case` is
/// ASCII-only *by name*, so a pair differing only outside ASCII -- `MODÈLE.onnx`
/// and `modèle.onnx`, say -- passes the duplicate check and then installs to one
/// file. The second overwrites the first; the first fails verification on every
/// later launch and is re-fetched forever, which reads as a network fault.
///
/// UP-TAKE `I-336`, raised by the independent review of `PR #77` and left open
/// because it was **unreachable while no manifest source existed**. Roadmap
/// `1.31` created one, so it is reachable now and fixed here rather than left
/// as a row whose trigger just arrived.
///
/// # What this does and does not claim
///
/// Rust's `to_lowercase` implements Unicode's full case folding, which is *not*
/// NTFS's upcase table -- the two agree on the cases that matter here and are
/// not the same algorithm, and NTFS's table is even version-dependent. So this
/// is deliberately a **conservative over-approximation**: it refuses some pairs
/// NTFS would keep distinct, and refusing a legitimate manifest entry costs a
/// developer one rename at build time, while accepting a colliding pair costs a
/// user a permanently re-downloading asset. **It is not a promise that every
/// NTFS collision is caught**, and pretending otherwise is what a comment
/// claiming "matches Windows" would do.
fn collides_on_windows(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
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
            // Case-INSENSITIVE, because Windows' filesystem is. "Model.onnx"
            // and "model.onnx" are two manifest entries that install to ONE
            // file: the second silently overwrites the first, and the first then
            // fails verification on every subsequent launch and is re-fetched
            // forever. Comparing case-sensitively let that pair through until
            // the independent review of `PR #77` pointed it out.
            if assets[..index]
                .iter()
                .any(|earlier| collides_on_windows(&earlier.file_name, &asset.file_name))
            {
                return Err(ManifestError::DuplicateFileName {
                    file_name: asset.file_name.clone(),
                });
            }
        }
        Ok(Self { assets })
    }

    /// Total bytes across every asset, for a whole-manifest progress figure.
    ///
    /// **Saturating, and that is a fix rather than a style choice.** A plain
    /// `sum()` over `u64` **panics in a debug build and silently WRAPS in a
    /// release build** once the total exceeds `u64::MAX` -- so a manifest
    /// carrying two absurd `size_bytes` values would report a *small* total in
    /// the binary that ships, and every progress fraction computed from it
    /// would be wrong in the direction that looks finished. Found and
    /// reproduced by the independent review of `PR #77`.
    ///
    /// Saturating is right for this particular figure because it drives a
    /// progress bar: `u64::MAX` bytes is visibly absurd, a wrapped small number
    /// is not, and [`crate::fetch::Progress::fraction`] already clamps.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.assets
            .iter()
            .fold(0_u64, |total, asset| total.saturating_add(asset.size_bytes))
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

    // ---------------------------------------------------------------------
    // The shared name-guard corpus (PR #88 rounds 6, 7 and 8)
    //
    // `rust_consts.plain_file_name` in Python duplicates `is_plain_file_name`
    // here, because the Rust guards what the PRODUCT installs and the Python
    // guards what the BUILD stages -- different languages, different processes,
    // neither can call the other. Three rounds of review each found a different
    // way for the two to diverge unnoticed:
    //
    //   round 6  the control read only the reserved-name LIST, not the rules
    //   round 7  it read rules with a line-anchored regex, so a rule in the
    //            trailing expression, behind an `else if`, or wrapped by
    //            rustfmt was invisible
    //   round 8  it stripped `!`, so it saw WHICH predicate a rule used and
    //            never whether the rule was that predicate or its negation
    //
    // Every fix was correct and every one was incomplete one shape down,
    // because reading Rust predicates with a Python regex is a parser problem
    // solved without a parser. The founder chose to replace the approach rather
    // than patch it a fourth time.
    //
    // So NOTHING PARSES ANYTHING NOW. This test runs the real guard over a
    // systematic corpus and writes the verdicts to a committed file. The Python
    // control reads that same file and asserts its own guard agrees, name for
    // name. A rule added to either side changes a verdict, and whichever side
    // did not get it goes red:
    //
    //   rule added to Rust, file not regenerated  -> THIS test fails
    //   rule added to Rust, file regenerated      -> the Python control fails
    //   rule added to Python only                 -> the Python control fails

    // ⚠️ The third line was FALSE for one thing when it was written, and round 9
    // drilled it: `RESERVED_DEVICE_NAMES` was restated on both sides and the
    // corpus drew its device cases from the RUST list, so a stem added to the
    // PYTHON list alone produced no corpus case and every control stayed green.
    // Worse, the version before this one carried a list-agreement test that
    // would have caught it, and replacing the approach deleted that check along
    // with the parser it sat beside.
    //
    // There is no second list now. `scripts/rust_consts.py` READS this one, so
    // "added to Python only" is not a state the device list can be in. The
    // three lines above are about RULES, and for rules they hold -- a predicate
    // added to either guard moves a verdict, which the drills exercise in both
    // directions.
    //
    // The residual, stated rather than left to be found: a rule whose effect is
    // invisible on every name in the corpus. That is why the corpus is
    // generated systematically over the interesting character classes and
    // positions rather than hand-listed.
    // ---------------------------------------------------------------------

    /// Where the generated verdicts live, relative to the crate root.
    const CASES_FILE: &str = "name-guard-cases.tsv";

    /// Characters probed beyond plain ASCII, and why each is here.
    ///
    /// The ASCII range is covered exhaustively in `corpus()` rather than
    /// sampled. These are the ones outside it worth naming.
    const BEYOND_ASCII: [char; 4] = [
        '\u{a0}',    // non-breaking space: whitespace that is not ASCII
        '\u{3000}',  // ideographic space: removed by `str::trim`, invisible to a byte scan
        '\u{e9}',    // a non-ASCII letter
        '\u{10000}', // outside the basic multilingual plane
    ];

    /// Every name both guards are checked against.
    ///
    /// Systematic rather than hand-listed: each interesting character is placed
    /// at the start, the end, and the middle of an otherwise ordinary name, and
    /// alone. A rule about any of those positions therefore moves a verdict.
    fn corpus() -> Vec<String> {
        let mut names: Vec<String> = Vec::new();

        // The structural cases.
        for fixed in ["", ".", "..", "...", "a", "model.onnx", "MODEL.ONNX"] {
            names.push(fixed.to_owned());
        }

        // EVERY ASCII code point, in every position, alone and in context, plus
        // the non-ASCII characters above.
        //
        // Exhaustive rather than sampled, and that is not thoroughness for its
        // own sake. The first version of this corpus hand-picked fourteen
        // characters, and its own drill found that a Rust rule rejecting '|'
        // and a Python rule rejecting a leading 'z' BOTH passed unnoticed,
        // because no name carried either. A rule invisible on every name is the
        // one residual this design has, so the corpus closes it by covering the
        // whole range a file name is written in.
        let probes: Vec<char> = (0u8..=127).map(char::from).chain(BEYOND_ASCII).collect();
        for character in probes {
            names.push(character.to_string());
            names.push(format!("{character}model.onnx"));
            names.push(format!("model.onnx{character}"));
            names.push(format!("mo{character}del.onnx"));
            names.push(format!("model{character}.onnx"));
        }

        // Every reserved device stem, bare and with extensions, in three cases.
        for reserved in RESERVED_DEVICE_NAMES {
            names.push((*reserved).to_owned());
            names.push(reserved.to_lowercase());
            names.push(format!("{reserved}.onnx"));
            names.push(format!("{}.onnx", reserved.to_lowercase()));
            names.push(format!("{reserved}x"));
            names.push(format!("x{reserved}"));
        }

        // Lengths, because a bound is a plausible future rule.
        for length in [1_usize, 2, 8, 64, 255, 256, 300] {
            names.push("a".repeat(length));
        }

        names.sort();
        names.dedup();
        names
    }

    /// TSV- and line-safe, and reversible. The Python side decodes the same way.
    fn escape(name: &str) -> String {
        let mut out = String::new();
        for character in name.chars() {
            match character {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                // '#' is escaped for the READER's sake, not the writer's:
                // the Python control skips lines beginning with '#' to drop
                // this file's header, and the corpus legitimately contains
                // "#" and "#model.onnx". PR #88 round 10 found the file
                // carrying 803 data rows while the control compared 801, so
                // a Python-only rule about '#' passed unnoticed. Escaping it
                // removes the collision rather than teaching the reader to
                // tell a datum from a comment.
                '#' => out.push_str("\\x23"),
                c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                    out.push_str(&format!("\\x{:02x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out
    }

    fn rendered_cases() -> String {
        let mut lines = vec![
            "# GENERATED by uptake-assets' the_name_guard_cases_file_is_current test.".to_owned(),
            "# Do not hand-edit. Regenerate with:".to_owned(),
            "#   UPTAKE_REGENERATE_NAME_GUARD=1 cargo test -p uptake-assets name_guard".to_owned(),
            "# Read by scripts/control-rust-consts.py, which asserts the Python guard agrees."
                .to_owned(),
            "# name\\tverdict  (verdict is `plain` or `refused`)".to_owned(),
            // DECLARED so the reader can prove it parsed all of it. Round
            // 10's finding was a reader that silently dropped two rows; a
            // count it must match turns any future drop into a refusal.
            format!("# cases: {}", corpus().len()),
        ];
        for name in corpus() {
            let verdict = if is_plain_file_name(&name) {
                "plain"
            } else {
                "refused"
            };
            lines.push(format!("{}\t{}", escape(&name), verdict));
        }
        lines.join("\n") + "\n"
    }

    #[test]
    fn the_name_guard_cases_file_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CASES_FILE);
        let rendered = rendered_cases();

        // `== "1"`, not `is_ok()`. PR #88 round 9 FINDING 2: `is_ok()` is
        // true for ANY value, so `UPTAKE_REGENERATE_NAME_GUARD=0` -- an
        // operator instruction meaning OFF -- made the staleness gate
        // report green and silently rewrite the committed file. Every
        // piece of documentation spells the switch `=1`; this now agrees
        // with them.
        if std::env::var("UPTAKE_REGENERATE_NAME_GUARD").as_deref() == Ok("1") {
            if let Err(error) = std::fs::write(&path, &rendered) {
                panic!("could not write {}: {error}", path.display());
            }
            return;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "{} is missing ({error}).\nRegenerate it: \
                 UPTAKE_REGENERATE_NAME_GUARD=1 cargo test -p uptake-assets name_guard",
                path.display()
            )
        });

        assert_eq!(
            committed.replace("\r\n", "\n"),
            rendered,
            "\n{} is stale. The guard's behaviour changed and the file the Python \
             control reads did not.\nRegenerate it: \
             UPTAKE_REGENERATE_NAME_GUARD=1 cargo test -p uptake-assets name_guard\n\
             Then expect scripts/control-rust-consts.py to go red until the Python \
             guard implements the same rule -- that is the point.",
            path.display()
        );
    }

    #[test]
    fn the_corpus_covers_both_verdicts_and_is_not_degenerate() {
        // A corpus that is all-refused or all-plain would let the control pass
        // against a guard that answers a constant, which is the vacuous-control
        // failure this repository keeps finding.
        let names = corpus();
        let refused = names.iter().filter(|n| !is_plain_file_name(n)).count();
        let plain = names.len() - refused;
        assert!(refused > 20, "only {refused} refused names in the corpus");
        assert!(plain > 5, "only {plain} accepted names in the corpus");
    }

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
    fn a_windows_device_stem_is_refused_with_or_without_an_extension() {
        // ⚠️ THE FOURTH SITE of a claim corrected in the other three, and the
        // one the correcting commit's own Enumeration command could not reach:
        // it grepped for "null device too" and this line says "is the null
        // device" (PR #88 round 9, FINDING 3).
        //
        // What is true: NUL reaches a device, and CON does through a relative
        // path. What is NOT: "the rule survives an extension". NUL.onnx,
        // AUX.dll and com9.txt are ordinary files -- measured, see
        // RESERVED_DEVICE_NAMES above.
        //
        // The names below are all still refused and the rule is genuinely flat,
        // because the behaviour is path-form and platform dependent and a flat
        // rule costs nothing. Only the stated reason was wrong.
        for name in [
            "CON",
            "con",
            "NUL",
            "nul.onnx",
            "AUX.dll",
            "PRN",
            "COM1",
            "com9.txt",
            "LPT1",
            "LpT9.onnx",
        ] {
            let error = asset(name, "https://example.test/m")
                .validate()
                .unwrap_err();
            assert!(
                matches!(error, ManifestError::UnsafeFileName { .. }),
                "{name:?} was accepted as a file name"
            );
        }
    }

    #[test]
    fn a_name_that_merely_starts_with_a_device_name_is_still_fine() {
        // The check is on the STEM, not a prefix match: "console.onnx" and
        // "computer.dll" are ordinary names and must not be rejected.
        for name in [
            "console.onnx",
            "computer.dll",
            "auxiliary.txt",
            "printer.onnx",
        ] {
            assert!(
                is_plain_file_name(name),
                "{name:?} was rejected, but only its prefix looks like a device"
            );
        }
    }

    #[test]
    fn a_trailing_dot_or_space_is_refused() {
        // Win32 strips both, so "model.onnx." and "model.onnx " open the same
        // file as "model.onnx" -- entries that look distinct and are not.
        for name in ["model.onnx.", "model.onnx ", "model.onnx  ", "x."] {
            assert!(!is_plain_file_name(name), "{name:?} was accepted");
        }
    }

    #[test]
    fn two_names_differing_only_by_case_are_refused_as_duplicates() {
        // Windows' filesystem is case-insensitive, so these install to ONE file:
        // the second overwrites the first, and the first then fails verification
        // on every launch and is re-fetched forever.
        let error = AssetManifest::new(vec![
            asset("Model.onnx", "https://example.test/a"),
            asset("model.onnx", "https://example.test/b"),
        ])
        .unwrap_err();
        assert!(
            matches!(error, ManifestError::DuplicateFileName { .. }),
            "got {error}"
        );
    }

    #[test]
    fn two_names_differing_only_outside_ascii_are_refused_as_duplicates() {
        // UP-TAKE I-336. `eq_ignore_ascii_case` is ASCII-only by name, so this
        // pair passed the duplicate check while NTFS would fold them to one
        // file. It was unreachable until roadmap 1.31 created a real manifest
        // source, which is why the row sat open rather than being wrong.
        let error = AssetManifest::new(vec![
            asset("MOD\u{c8}LE.onnx", "https://example.test/a"),
            asset("mod\u{e8}le.onnx", "https://example.test/b"),
        ])
        .unwrap_err();
        assert!(
            matches!(error, ManifestError::DuplicateFileName { .. }),
            "got {error}"
        );
    }

    #[test]
    fn names_that_differ_by_more_than_case_are_still_distinct() {
        // The fold is a conservative over-approximation, so this guards the
        // other direction: it must not start refusing ordinary distinct names.
        AssetManifest::new(vec![
            asset("ch_PP-OCRv4_det.onnx", "https://example.test/a"),
            asset("ch_PP-OCRv4_rec.onnx", "https://example.test/b"),
            asset("ppocr_keys_v1.txt", "https://example.test/c"),
        ])
        .unwrap();
    }

    #[test]
    fn total_bytes_saturates_instead_of_overflowing() {
        // A plain sum() PANICS in debug and silently WRAPS in release, so a
        // release build would report a small total for an absurd manifest and
        // every progress fraction from it would look nearly finished. Two
        // assets at two thirds of u64::MAX each is the smallest reproduction.
        let huge = u64::MAX / 3 * 2;
        let mut first = asset("a.onnx", "https://example.test/a");
        first.size_bytes = huge;
        let mut second = asset("b.onnx", "https://example.test/b");
        second.size_bytes = huge;

        let manifest = AssetManifest::new(vec![first, second]).unwrap();
        assert_eq!(
            manifest.total_bytes(),
            u64::MAX,
            "the total did not saturate"
        );
    }

    #[test]
    fn an_ordinary_total_is_still_exact() {
        // Saturation must not cost accuracy in the normal range.
        let mut first = asset("a.onnx", "https://example.test/a");
        first.size_bytes = 11_000_000;
        let mut second = asset("b.onnx", "https://example.test/b");
        second.size_bytes = 4_700_000;
        let manifest = AssetManifest::new(vec![first, second]).unwrap();
        assert_eq!(manifest.total_bytes(), 15_700_000);
    }

    #[test]
    fn an_empty_manifest_is_valid_and_totals_zero() {
        let manifest = AssetManifest::new(Vec::new()).unwrap();
        assert_eq!(manifest.total_bytes(), 0);
        assert!(manifest.assets.is_empty());
    }
}
