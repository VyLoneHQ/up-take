//! Measures what the real PP-OCRv4 pipeline reads against text of known content.
//!
//! Roadmap task `1.32`, `BACKLOG.md` `I-351`. The pipeline has returned text
//! since `1.31` and `1.26` puts that text in front of a user, and **nothing has
//! ever measured whether it is right**. `I-341` (no OCR accuracy bar exists)
//! cannot be answered before something does: a bar cannot be set on a pipeline
//! nobody has measured, and a number picked without one is a bar chosen to be
//! passed.
//!
//! # What it measures, and what each number does not mean
//!
//! Three figures per condition, because one would hide the trade this harness
//! exists to expose.
//!
//! - **CER**, the character error rate: Levenshtein distance from the expected
//!   string to the read one, over the expected length. `0.0` is perfect. It can
//!   exceed `1.0`, because inserting rubbish costs edits without bounding them.
//! - **Exact**, the share of cards read character for character. Harsh on
//!   purpose: a user pasting an invoice total cares about exactly this.
//! - **Empty**, the share of cards containing text that came back with none.
//!   Tracked apart from CER because it is the failure the founder actually hit
//!   at the rig (*"No text found" roughly half the time*) and because it is the
//!   one `drop_score` trades against. A card read as empty scores CER `1.0`,
//!   which is indistinguishable in that column from a card read as gibberish;
//!   for the user those are very different failures.
//!
//! **Every figure here is an UPPER BOUND on real screen text.** The cards are
//! rendered by `scripts/render-ocr-cards.py`, so they carry no subpixel
//! smearing from another renderer, no compression ringing and no downscaling.
//! That is the price of exact ground truth, and it is stated here rather than
//! discovered later: do not quote a number from this harness as "UP-TAKE's
//! accuracy".
//!
//! # One of `I-351`'s two axes, and only one
//!
//! That row asks for the pipeline run *"while varying `drop_score` **and the
//! sampling mode**"*. **This harness varies `drop_score` and cannot vary the
//! sampling mode**, because there is no knob for it: [`PaddleOptions`] has one
//! tunable field, and `recognise.rs`'s nearest-neighbour upsampling is a
//! literal in the code. Exposing it is a change to the engine's own API and is
//! not this row.
//!
//! **So the sampling-mode candidate is untouched, not eliminated**, and the
//! distinction matters because the first run makes it easy to conflate them.
//! Sweeping `drop_score` moved almost nothing and the false empties turned out
//! to be the detector, which is a finding about the EMPTIES. Roughly a fifth of
//! the cards are non-empty MISREADS, and that is exactly the population where
//! crop upsampling quality would show. Nothing here has looked at it. Do not
//! read *"the detector, not `drop_score`"* as closing the sampling-mode
//! question; it does not touch it.
//!
//! *(Written down because the independent review of `PR #84` predicted the
//! misreading, having had to pull the backlog row itself to notice that half
//! the ask was missing from the artifact.)*
//!
//! # Why it is an example and not a test
//!
//! Same constraint as `ocr_smoke.rs` and answered the same way: it needs a
//! runtime, two models and a dictionary that CI does not have, and it takes
//! minutes rather than milliseconds. A step that must be invoked by name is
//! honest about being a manual one.
//!
//! **Its pure parts ARE tested, and by `cargo test` rather than by hand.**
//! `crates/uptake-ocr/Cargo.toml` marks this example `test = true`, so the
//! module at the bottom runs in the same `cargo test --all-features` CI already
//! runs. Without that one line an example's `#[cfg(test)]` module compiles and
//! never runs, which would leave the metric that produces every figure here
//! unverified. An unverified metric is worse than no metric: it reports.
//!
//! # Usage
//!
//! ```text
//! python scripts/render-ocr-cards.py --out dist/cards
//!
//! cargo run --release -p uptake-ocr --example ocr_accuracy -- ^
//!     --models src-tauri/assets/models ^
//!     --runtime src-tauri/assets/onnxruntime.dll ^
//!     --cards dist/cards ^
//!     --drop-score 0.0 --drop-score 0.3 --drop-score 0.5 --drop-score 0.7
//! ```
//!
//! `--release` is not optional in practice: a debug build runs the two networks
//! roughly an order of magnitude slower, and the whole set is 192 cards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use uptake_assets::ppocr;
use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Size;
use uptake_ocr::Engine;
use uptake_ocr::paddle::{PaddleConfig, PaddleEngine, PaddleOptions, preprocess};

/// Bytes per pixel in the flat image format `ocr_smoke.rs` documents.
const BYTES_PER_PIXEL: usize = 4;

/// That format's header: two little-endian `u32`s.
const HEADER_BYTES: usize = 8;

/// One row of the manifest `render-ocr-cards.py` writes.
#[derive(Debug, Clone)]
struct Card {
    file: String,
    text_key: String,
    font: String,
    size_px: u32,
    polarity: String,
    text: String,
}

/// What one condition scored.
///
/// Accumulated rather than averaged as it goes: a running mean of ratios
/// weights a two-character card the same as a forty-character one, and the
/// digits card is deliberately shorter than the letters card.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    cards: u32,
    exact: u32,
    empty: u32,
    /// Total edit distance across every card in this condition.
    distance: u64,
    /// Total expected characters across every card in this condition.
    expected: u64,
}

impl Tally {
    fn add(&mut self, expected: &str, got: &str) {
        self.cards += 1;
        if expected == got {
            self.exact += 1;
        }
        if got.is_empty() {
            self.empty += 1;
        }
        self.distance += levenshtein(expected, got) as u64;
        self.expected += expected.chars().count() as u64;
    }

    /// Character error rate. `None` when nothing was expected, which cannot
    /// happen with the shipped card set but would be a division by zero if a
    /// blank card were ever added.
    fn cer(self) -> Option<f64> {
        if self.expected == 0 {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "counts here are in the hundreds; f64 is exact far beyond that"
        )]
        Some(self.distance as f64 / self.expected as f64)
    }

    /// `count` as a share of the cards in this condition.
    ///
    /// `f64::from` rather than `as`: every `u32` is exactly representable, so
    /// there is no precision to lose and no lint to silence. The sibling
    /// [`Tally::cer`] does need an `as`, because its accumulators are `u64`.
    fn share(self, count: u32) -> f64 {
        if self.cards == 0 {
            return 0.0;
        }
        f64::from(count) / f64::from(self.cards)
    }
}

fn main() -> ExitCode {
    let mut models = PathBuf::from("dist/models");
    let mut cards_dir = PathBuf::from("dist/cards");
    let mut runtime: Option<PathBuf> = None;
    let mut drop_scores: Vec<f32> = Vec::new();
    let mut limits: Vec<u32> = Vec::new();
    let mut box_threshold: Option<f32> = None;
    let mut det_threshold: Option<f32> = None;
    let mut filter: Option<String> = None;

    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let value = arguments.next();
        match (flag.as_str(), value) {
            ("--models", Some(path)) => models = PathBuf::from(path),
            ("--cards", Some(path)) => cards_dir = PathBuf::from(path),
            ("--runtime", Some(path)) => runtime = Some(PathBuf::from(path)),
            ("--filter", Some(text)) => filter = Some(text),
            // DB post-processing, exposed because PP-OCRv6's own published
            // config uses thresh 0.2 / box_thresh 0.4 where this crate's
            // defaults are 0.3 / 0.6. A third stricter is a plausible cause
            // of a marginal detection being dropped, and a plausible cause is
            // worth a sweep rather than an argument.
            ("--box-thresh", Some(text)) => match text.parse::<f32>() {
                Ok(value) => box_threshold = Some(value),
                Err(error) => {
                    eprintln!("--box-thresh {text}: {error}");
                    return ExitCode::FAILURE;
                }
            },
            ("--det-thresh", Some(text)) => match text.parse::<f32>() {
                Ok(value) => det_threshold = Some(value),
                Err(error) => {
                    eprintln!("--det-thresh {text}: {error}");
                    return ExitCode::FAILURE;
                }
            },
            // The detector's cap on the longer side. Repeatable like
            // `--drop-score`, and here for the same reason that one is: the
            // founder's rig pass on 2026-09-04 found that an area wider than
            // roughly 700 logical pixels stops reading, and `limit_side_len`
            // scaling the frame down before the detector ever sees it is the
            // named suspect. A hypothesis about a knob is worth exactly as much
            // as the sweep that tests it.
            ("--limit-side-len", Some(text)) => match text.parse::<u32>() {
                Ok(limit) if limit >= preprocess::SIDE_MULTIPLE => limits.push(limit),
                Ok(limit) => {
                    eprintln!(
                        "--limit-side-len {limit} is below one {} px multiple, which would \
                         round every frame to nothing",
                        preprocess::SIDE_MULTIPLE
                    );
                    return ExitCode::FAILURE;
                }
                Err(error) => {
                    eprintln!("--limit-side-len {text}: {error}");
                    return ExitCode::FAILURE;
                }
            },
            ("--drop-score", Some(text)) => match text.parse::<f32>() {
                Ok(score) if (0.0..=1.0).contains(&score) => drop_scores.push(score),
                Ok(score) => {
                    eprintln!("--drop-score {score} is outside 0.0..=1.0");
                    return ExitCode::FAILURE;
                }
                Err(error) => {
                    eprintln!("--drop-score {text}: {error}");
                    return ExitCode::FAILURE;
                }
            },
            // A known flag missing its value is reported as such rather than as
            // "unrecognised", which would send the reader to check their
            // spelling instead of their argument list. Same choice as
            // `ocr_smoke.rs`, for the reason its review recorded.
            (
                flag @ ("--models" | "--cards" | "--runtime" | "--drop-score" | "--limit-side-len"
                | "--filter"),
                None,
            ) => {
                eprintln!("{flag} needs a value");
                usage();
                return ExitCode::FAILURE;
            }
            (other, _) => {
                eprintln!("unrecognised argument {other}");
                usage();
                return ExitCode::FAILURE;
            }
        }
    }
    if drop_scores.is_empty() {
        // The shipping value, so a bare run measures what users actually get.
        drop_scores.push(PaddleOptions::default().drop_score);
    }
    if limits.is_empty() {
        limits.push(PaddleOptions::default().limit_side_len);
    }

    let manifest = cards_dir.join("cards.tsv");
    let mut cards = match read_manifest(&manifest) {
        Ok(cards) => cards,
        Err(error) => {
            eprintln!("could not read {}: {error}", manifest.display());
            eprintln!(
                "run: python scripts/render-ocr-cards.py --out {}",
                cards_dir.display()
            );
            return ExitCode::FAILURE;
        }
    };
    if let Some(needle) = &filter {
        cards.retain(|card| card.file.contains(needle.as_str()));
    }
    if cards.is_empty() {
        eprintln!("no cards to measure");
        return ExitCode::FAILURE;
    }
    println!(
        "{} cards, {} drop_score value(s)",
        cards.len(),
        drop_scores.len()
    );
    println!(
        "UPPER BOUND: these are rendered cards, not captured screen text. See this example's header."
    );

    let config = PaddleConfig {
        // Read from the pins rather than retyped. These were string literals
        // until ADR-0036 renamed the detector, which is the change that finds a
        // second copy of a pinned fact.
        detection_model: models.join(ppocr::DETECTION_FILE_NAME),
        recognition_model: models.join(ppocr::RECOGNITION_FILE_NAME),
        dictionary: models.join(ppocr::DICTIONARY_FILE_NAME),
        runtime_library: runtime,
    };

    // The full cross product, so a sweep over one knob at several values of the
    // other is one invocation rather than several a reader has to line up by
    // hand.
    for &limit_side_len in &limits {
        for &drop_score in &drop_scores {
            let mut detector = PaddleOptions::default().detector;
            if let Some(value) = box_threshold {
                detector.box_threshold = value;
            }
            if let Some(value) = det_threshold {
                detector.threshold = value;
            }
            let options = PaddleOptions {
                drop_score,
                limit_side_len,
                detector,
            };
            // Reloaded per combination rather than mutated: both knobs live in
            // the engine's options and there is no setter, and a harness that
            // reached inside to change one would be measuring a configuration
            // the product cannot produce.
            let mut engine = match PaddleEngine::load(&config, options) {
                Ok(engine) => engine,
                Err(error) => {
                    eprintln!(
                        "load failed at drop_score {drop_score}, \
                         limit_side_len {limit_side_len}: {error}"
                    );
                    return ExitCode::FAILURE;
                }
            };
            if let Err(error) = measure(
                &mut engine,
                &cards,
                &cards_dir,
                drop_score,
                limit_side_len,
                detector.threshold,
                detector.box_threshold,
            ) {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn usage() {
    eprintln!(
        "usage: --models <dir> --cards <dir> [--runtime <dll>] \
         [--drop-score <0.0..1.0>]... [--limit-side-len <px>]... \n         [--filter <substring>]"
    );
}

/// Runs every card and prints the breakdown for one `drop_score`.
fn measure(
    engine: &mut PaddleEngine,
    cards: &[Card],
    directory: &Path,
    drop_score: f32,
    limit_side_len: u32,
    det_threshold: f32,
    box_threshold: f32,
) -> Result<(), String> {
    let mut overall = Tally::default();
    // `BTreeMap` so every breakdown prints in a stable order. A run whose rows
    // move between invocations cannot be diffed against the previous one, and
    // comparing runs is the whole point of writing the figures down.
    let mut by_size: BTreeMap<u32, Tally> = BTreeMap::new();
    let mut by_polarity: BTreeMap<String, Tally> = BTreeMap::new();
    let mut by_font: BTreeMap<String, Tally> = BTreeMap::new();
    let mut by_text: BTreeMap<String, Tally> = BTreeMap::new();
    let mut worst: Vec<(f64, String, String)> = Vec::new();

    let started = std::time::Instant::now();
    for card in cards {
        let frame = load_frame(&directory.join(&card.file))
            .map_err(|error| format!("could not read {}: {error}", card.file))?;
        let recognition = engine
            .recognise(&frame)
            .map_err(|error| format!("recognise failed on {}: {error}", card.file))?;
        let expected = normalise(&card.text);
        let got = normalise(&recognition.text());

        overall.add(&expected, &got);
        by_size
            .entry(card.size_px)
            .or_default()
            .add(&expected, &got);
        by_polarity
            .entry(card.polarity.clone())
            .or_default()
            .add(&expected, &got);
        by_font
            .entry(card.font.clone())
            .or_default()
            .add(&expected, &got);
        by_text
            .entry(card.text_key.clone())
            .or_default()
            .add(&expected, &got);

        if expected != got {
            let mut one = Tally::default();
            one.add(&expected, &got);
            worst.push((one.cer().unwrap_or(0.0), card.file.clone(), got));
        }
    }
    let elapsed = started.elapsed();

    println!();
    println!(
        "=== drop_score {drop_score}, limit_side_len {limit_side_len}, \n         det_thresh {det_threshold}, box_thresh {box_threshold} ==="
    );
    println!(
        "{} cards in {:.1} s ({:.0} ms per card)",
        overall.cards,
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / f64::from(overall.cards.max(1))
    );
    print_row("OVERALL", overall);
    println!();
    print_group("by text", &by_text);
    print_group("by font", &by_font);
    print_group("by polarity", &by_polarity);
    print_size_group(&by_size);

    // The ten worst cards by name, because an aggregate says a condition is bad
    // and only a file name says which card to open.
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!();
    println!(
        "worst {} of {} misread cards:",
        worst.len().min(10),
        worst.len()
    );
    for (cer, file, got) in worst.iter().take(10) {
        println!("  CER {cer:5.2}  {file}");
        println!("            read: {got:?}");
    }
    Ok(())
}

fn print_row(label: &str, tally: Tally) {
    let cer = tally
        .cer()
        .map_or("n/a".to_string(), |cer| format!("{cer:5.3}"));
    println!(
        "  {label:<16}  n {:>4}   CER {cer}   exact {:>5.1}%   empty {:>5.1}%",
        tally.cards,
        tally.share(tally.exact) * 100.0,
        tally.share(tally.empty) * 100.0
    );
}

fn print_group(label: &str, group: &BTreeMap<String, Tally>) {
    println!("{label}:");
    for (key, tally) in group {
        print_row(key, *tally);
    }
    println!();
}

/// Sizes print numerically rather than as strings, so 7 sorts before 10.
fn print_size_group(group: &BTreeMap<u32, Tally>) {
    println!("by size:");
    for (size, tally) in group {
        print_row(&format!("{size} px"), *tally);
    }
}

/// Collapses runs of whitespace and trims, on both sides of the comparison.
///
/// The engine already normalises whitespace inside a block and joins lines with
/// newlines, so a card whose text wrapped differently would otherwise score as
/// wrong for a reason that is not a reading error. Case is deliberately NOT
/// folded: reading `Total` as `total` is a real error a user would have to fix
/// by hand, and folding it would flatter the engine.
fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Levenshtein edit distance in characters, not bytes.
///
/// Characters because the recogniser can emit multi-byte output and a
/// byte-distance would charge four edits for one wrong glyph. Two rows rather
/// than a full matrix: the card texts are short, but this runs 192 times per
/// `drop_score` and there is no reason to allocate a matrix for it.
fn levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0_usize; right.len() + 1];
    for (i, l) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, r) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(l != r);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current[j + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// Reads the tab-separated manifest `render-ocr-cards.py` writes.
///
/// Columns are looked up **by header name**, not by position. A positional
/// reader would keep working after a column was inserted, and would silently
/// compare recognised text against a font name.
fn read_manifest(path: &Path) -> Result<Vec<Card>, String> {
    let contents = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut lines = contents.lines();
    let header: Vec<&str> = lines
        .next()
        .ok_or("the manifest is empty")?
        .split('\t')
        .collect();
    let index = |name: &str| -> Result<usize, String> {
        header
            .iter()
            .position(|column| *column == name)
            .ok_or_else(|| format!("the manifest has no {name:?} column"))
    };
    let (file, text_key, font, size_px, polarity, text) = (
        index("file")?,
        index("text_key")?,
        index("font")?,
        index("size_px")?,
        index("polarity")?,
        index("text")?,
    );

    let mut cards = Vec::new();
    for (number, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        // Checked per row rather than once: a short row is how a truncated
        // write reaches the comparison as a card with somebody else's text.
        if fields.len() != header.len() {
            return Err(format!(
                "row {} has {} fields and the header has {}",
                number + 2,
                fields.len(),
                header.len()
            ));
        }
        cards.push(Card {
            file: fields[file].to_owned(),
            text_key: fields[text_key].to_owned(),
            font: fields[font].to_owned(),
            size_px: fields[size_px]
                .parse()
                .map_err(|error| format!("row {}: size_px: {error}", number + 2))?,
            polarity: fields[polarity].to_owned(),
            text: fields[text].to_owned(),
        });
    }
    Ok(cards)
}

/// Reads the flat `.rgba` format described in `ocr_smoke.rs`'s header.
fn load_frame(path: &Path) -> Result<RgbaBitmap, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() < HEADER_BYTES {
        return Err("shorter than its own header".to_owned());
    }
    let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let pixels = bytes[HEADER_BYTES..].to_vec();
    let expected = width as usize * height as usize * BYTES_PER_PIXEL;
    if pixels.len() != expected {
        return Err(format!(
            "header says {width}x{height}, which needs {expected} bytes, and the \
             file carries {}",
            pixels.len()
        ));
    }
    RgbaBitmap::from_pixels(Size::new(width, height), pixels)
        .ok_or_else(|| "the dimensions and the pixel count disagree".to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_are_zero_apart() {
        assert_eq!(levenshtein("Total: 1,284.50", "Total: 1,284.50"), 0);
    }

    /// The three edit kinds, each on its own, so a distance that happens to be
    /// right for substitutions only cannot pass.
    #[test]
    fn one_of_each_edit_costs_exactly_one() {
        assert_eq!(levenshtein("Total", "Tota1"), 1, "substitution");
        assert_eq!(levenshtein("Total", "Totals"), 1, "insertion");
        assert_eq!(levenshtein("Total", "Toal"), 1, "deletion");
    }

    /// An empty reading costs the whole string, which is what makes a card read
    /// as "No text found" score CER 1.0.
    #[test]
    fn an_empty_reading_costs_every_character() {
        assert_eq!(levenshtein("abcd", ""), 4);
        assert_eq!(levenshtein("", "abcd"), 4);
        assert_eq!(levenshtein("", ""), 0);
    }

    /// Characters, not bytes. A byte-distance would charge 3 for this instead
    /// of 1 and quietly inflate the error rate on any non-ASCII reading.
    #[test]
    fn distance_counts_characters_not_bytes() {
        assert_eq!("€".len(), 3, "the premise: three bytes, one character");
        assert_eq!(levenshtein("1€", "1$"), 1);
    }

    /// Whitespace is collapsed on both sides, so a differently wrapped reading
    /// of the right characters is not scored as a misreading.
    #[test]
    fn normalisation_collapses_whitespace_and_trims() {
        assert_eq!(
            normalise("  Total:\n\n1,284.50  \t EUR "),
            "Total: 1,284.50 EUR"
        );
    }

    /// Case is NOT folded. Asserted rather than left implicit, because folding
    /// it is the obvious "improvement" someone would make to raise the score.
    #[test]
    fn case_is_a_real_error_and_stays_one() {
        assert_ne!(normalise("Total"), normalise("total"));
    }

    /// CER is edits over EXPECTED characters, accumulated across cards rather
    /// than averaged per card. The two differ whenever the cards are different
    /// lengths, which they deliberately are.
    #[test]
    fn cer_weights_by_length_rather_than_by_card() {
        let mut tally = Tally::default();
        // Ten expected characters, one wrong.
        tally.add("0123456789", "012345678X");
        // Two expected characters, both wrong.
        tally.add("ab", "XY");
        // Accumulated: 3 edits over 12 characters. A per-card mean would give
        // (0.1 + 1.0) / 2 = 0.55, which is a different and misleading number.
        assert!(
            (tally.cer().unwrap() - 0.25).abs() < 1e-9,
            "{:?}",
            tally.cer()
        );
    }

    #[test]
    fn exact_and_empty_are_counted_separately_from_the_error_rate() {
        let mut tally = Tally::default();
        tally.add("abc", "abc");
        tally.add("abc", "");
        assert_eq!(tally.cards, 2);
        assert_eq!(tally.exact, 1);
        assert_eq!(tally.empty, 1);
        // The empty card contributes a full three edits.
        assert_eq!(tally.distance, 3);
    }

    /// A card read as gibberish and a card read as nothing both score badly and
    /// must remain distinguishable, because `drop_score` trades one for the
    /// other and that trade is the founder's call.
    #[test]
    fn a_gibberish_reading_is_not_counted_as_an_empty_one() {
        let mut tally = Tally::default();
        tally.add("abc", "xyz");
        assert_eq!(tally.empty, 0, "read the wrong thing, not nothing");
        assert_eq!(tally.exact, 0);
    }

    /// The manifest is read by header name. This row has its columns in a
    /// different order from the writer's, and must still be read correctly.
    #[test]
    fn manifest_columns_are_found_by_name_not_by_position() {
        let directory = std::env::temp_dir().join("uptake-ocr-accuracy-manifest-test");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("cards.tsv");
        std::fs::write(
            &path,
            "text\tpolarity\tsize_px\tfont\ttext_key\tfile\n\
             Total: 1\tlight-on-dark\t14\tmono\tinvoice\tcard.rgba\n",
        )
        .unwrap();
        let cards = read_manifest(&path).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].file, "card.rgba");
        assert_eq!(cards[0].text, "Total: 1");
        assert_eq!(cards[0].size_px, 14);
        assert_eq!(cards[0].font, "mono");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A row with the wrong number of fields is refused rather than read. This
    /// is the failure a truncated write produces, and reading it would compare
    /// recognised text against whatever landed in the last column.
    #[test]
    fn a_short_row_is_refused() {
        let directory = std::env::temp_dir().join("uptake-ocr-accuracy-short-row-test");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("cards.tsv");
        std::fs::write(
            &path,
            "file\ttext_key\tfont\tsize_px\tpolarity\ttext\n\
             card.rgba\tinvoice\tmono\n",
        )
        .unwrap();
        let error = read_manifest(&path).unwrap_err();
        assert!(error.contains("fields"), "{error}");
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
