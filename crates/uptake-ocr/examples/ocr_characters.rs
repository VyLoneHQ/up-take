//! Prints where the recogniser placed every character, for measuring a rule
//! that turns its timestep runs into character positions (roadmap `1.44`).
//!
//! # Why this exists
//!
//! `1.44` selects inside an OCR area down to single characters, so a selection
//! end has to land between two characters. The engine knows only which
//! timesteps chose each character, and a timestep is several source pixels
//! wide. Whether that is fine enough, and which point between two runs is the
//! boundary, is a measurement. `1.40` measured the same question for words and
//! its script was never kept; this one is.
//!
//! It prints raw runs and quads rather than a verdict, so the rule can be
//! changed and re-scored without re-running the models:
//! `scripts/measure-ocr-characters.py` renders the lines, runs this, and scores.
//!
//! # Usage
//!
//! ```text
//! cargo run -p uptake-ocr --example ocr_characters -- ^
//!     --models src-tauri/assets/models --runtime dist/runtime/onnxruntime.dll ^
//!     --lines dist/char-lines
//! ```
//!
//! `--lines` is a folder of `.rgba` files in `ocr_smoke`'s flat format. Output,
//! one record per line, tab-separated:
//!
//! ```text
//! line  <file>  <index>  <timesteps>  <x0 y0 x1 y1 x2 y2 x3 y3>
//! char  <file>  <index>  <code points, hex, '+'-joined>  <first>  <last>
//! glyph <file>  <block>  <code points, hex, '+'-joined>  <left x>  <right x>  <x0 y0 x1 y1 x2 y2 x3 y3>
//! word  <file>  <block>  <x0 y0 x1 y1 x2 y2 x3 y3>
//! ```
//!
//! Quad corners run clockwise from the top-left. A character's `first` and
//! `last` are inclusive timestep indices. `line` and `char` are the raw decode,
//! for trying other rules; `glyph` is what `Engine::recognise` itself returns,
//! a character outline's top edge in whole pixels, so the rule the engine ships
//! is scored on the engine's own output. Its last field and the `word` records
//! are the whole outlines, for `scripts/draw-ocr-characters.py`, which draws
//! them onto a real screenshot: a screen has no ground truth to score against,
//! so the rig half of `1.44`'s measurement is a picture judged by eye.

#![allow(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a console program's output is its interface; this is never in the release binary"
)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use uptake_assets::ppocr;
use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::{Point, Size};
use uptake_ocr::Engine;
use uptake_ocr::paddle::{PaddleConfig, PaddleEngine, PaddleOptions};

/// Bytes per pixel in the flat image format.
const BYTES_PER_PIXEL: usize = 4;

/// The header is two little-endian `u32`s.
const HEADER_BYTES: usize = 8;

const USAGE: &str = "usage: --models <dir> --lines <dir> [--runtime <dll>]";

fn main() -> ExitCode {
    let mut models = PathBuf::from("src-tauri/assets/models");
    let mut lines = PathBuf::from("dist/char-lines");
    let mut runtime: Option<PathBuf> = None;

    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        match (flag.as_str(), arguments.next()) {
            ("--models", Some(path)) => models = PathBuf::from(path),
            ("--lines", Some(path)) => lines = PathBuf::from(path),
            ("--runtime", Some(path)) => runtime = Some(PathBuf::from(path)),
            (other, _) => {
                eprintln!("unrecognised or incomplete argument {other}");
                eprintln!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    let config = PaddleConfig {
        detection_model: models.join(ppocr::DETECTION_FILE_NAME),
        recognition_model: models.join(ppocr::RECOGNITION_FILE_NAME),
        dictionary: models.join(ppocr::DICTIONARY_FILE_NAME),
        runtime_library: runtime,
    };
    let mut engine = match PaddleEngine::load(&config, PaddleOptions::default()) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("load failed: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut files: Vec<PathBuf> = match std::fs::read_dir(&lines) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "rgba")
            })
            .collect(),
        Err(error) => {
            eprintln!("could not list {}: {error}", lines.display());
            return ExitCode::FAILURE;
        }
    };
    files.sort();
    if files.is_empty() {
        eprintln!("no .rgba files in {}", lines.display());
        return ExitCode::FAILURE;
    }

    for file in &files {
        let name = file
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let frame = match load_frame(file) {
            Ok(frame) => frame,
            Err(message) => {
                eprintln!("could not read {name}: {message}");
                return ExitCode::FAILURE;
            }
        };
        let decoded = match engine.decode_lines(&frame) {
            Ok(decoded) => decoded,
            Err(error) => {
                eprintln!("{name}: {error}");
                return ExitCode::FAILURE;
            }
        };
        for (index, (quad, text)) in decoded.iter().enumerate() {
            let corners: Vec<String> = quad
                .corners
                .iter()
                .flat_map(|corner| [corner.x, corner.y])
                .map(|value| format!("{value:.3}"))
                .collect();
            println!(
                "line\t{name}\t{index}\t{}\t{}",
                text.timesteps,
                corners.join(" ")
            );
            for character in &text.characters {
                let code_points: Vec<String> = character
                    .text
                    .chars()
                    .map(|c| format!("{:x}", u32::from(c)))
                    .collect();
                println!(
                    "char\t{name}\t{index}\t{}\t{}\t{}",
                    code_points.join("+"),
                    character.first,
                    character.last
                );
            }
        }
        // The shipping path's own answer: what `recognise` places, after its
        // filtering and whole-pixel rounding.
        let recognition = match engine.recognise(&frame) {
            Ok(recognition) => recognition,
            Err(error) => {
                eprintln!("{name}: {error}");
                return ExitCode::FAILURE;
            }
        };
        for (index, block) in recognition.blocks().enumerate() {
            for word in &block.words {
                println!("word\t{name}\t{index}\t{}", outline_fields(&word.outline));
                for character in &word.characters {
                    let code_points: Vec<String> = character
                        .text
                        .chars()
                        .map(|c| format!("{:x}", u32::from(c)))
                        .collect();
                    println!(
                        "glyph\t{name}\t{index}\t{}\t{}\t{}\t{}",
                        code_points.join("+"),
                        character.outline[0].x,
                        character.outline[1].x,
                        outline_fields(&character.outline)
                    );
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// An outline's four corners as `x0 y0 x1 y1 x2 y2 x3 y3`.
fn outline_fields(outline: &[Point; 4]) -> String {
    outline
        .iter()
        .flat_map(|corner| [corner.x, corner.y])
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

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
