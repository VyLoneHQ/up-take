//! Drives the real PP-OCRv4 pipeline over a real image and prints what it read.
//!
//! # Why this exists
//!
//! Roadmap `1.11` shipped `PaddleEngine` with 85 lib tests covering the five
//! pure stages, and every one of them runs with **no ONNX Runtime and no model
//! file present**. That was the right call for CI and it left one thing
//! untested: `PaddleEngine::load`, `detect` and `recognise_crop` had never
//! executed. `STATUS.md` said so in as many words -- *"have still never been
//! run"* -- because until `1.31` converted the models there was nothing to run
//! them against.
//!
//! This example is the answer to that, and it is deliberately an example rather
//! than a test: it needs a runtime, two models and a dictionary that CI does not
//! have, so as a `#[test]` it would either fail on every machine or be
//! `#[ignore]`d into invisibility. An example that must be invoked by name is
//! honest about being a manual step.
//!
//! # Usage
//!
//! ```text
//! python scripts/convert-ppocr-models.py --out dist/models
//!
//! set ORT_DYLIB_PATH=C:\Windows\System32\onnxruntime.dll
//! cargo run -p uptake-ocr --example ocr_smoke -- ^
//!     --models dist/models --image dist/smoke.rgba
//! ```
//!
//! **`ORT_DYLIB_PATH` pointing at System32 is a DEVELOPER convenience and never
//! a shipping path.** `ADR-0032` **decision 2** requires the runtime to be
//! acquired by *"a documented, checksummed step … pinned SHA-256, verified
//! before load"*, and `resolve_runtime` refuses a search-path DLL for that
//! reason. *(Cited as decision 1 until round 2 of this PR's review; decision 1
//! is the `load-dynamic` feature choice and says nothing about placing or
//! checksumming. Round 1 caught this same class once and the sweep that
//! followed stopped at the file it was found in.)*
//! Windows happens to carry a Microsoft-signed ONNX Runtime that satisfies
//! `ort`'s floor, which makes a smoke test possible today without acquiring
//! anything; it changes no code behaviour and no decision.
//!
//! # The image format, and why it is not a PNG
//!
//! A flat `.rgba` file: little-endian `u32` width, little-endian `u32` height,
//! then `width * height * 4` bytes. Decoding a PNG would mean an image-decoding
//! dependency in a crate whose whole point is that it has almost none, for a
//! manual diagnostic. `scripts/convert-ppocr-models.py`'s sibling in the PR
//! description writes one; any five lines of Pillow will.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Size;
use uptake_ocr::Engine;
use uptake_ocr::paddle::{PaddleConfig, PaddleEngine, PaddleOptions};

/// Bytes per pixel in the flat image format above.
const BYTES_PER_PIXEL: usize = 4;

/// The header is two little-endian `u32`s.
const HEADER_BYTES: usize = 8;

fn main() -> ExitCode {
    let mut models = PathBuf::from("dist/models");
    let mut image = PathBuf::from("dist/smoke.rgba");
    let mut runtime: Option<PathBuf> = None;

    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let value = arguments.next();
        match (flag.as_str(), value) {
            ("--models", Some(path)) => models = PathBuf::from(path),
            ("--image", Some(path)) => image = PathBuf::from(path),
            ("--runtime", Some(path)) => runtime = Some(PathBuf::from(path)),
            // A KNOWN flag with its value missing is reported as such, not as
            // "unrecognised" -- naming a flag that IS recognised sends the
            // reader to check their spelling instead of their argument list.
            // Review of `PR #78`, non-binding.
            (flag @ ("--models" | "--image" | "--runtime"), None) => {
                eprintln!("{flag} needs a value");
                eprintln!("usage: --models <dir> --image <file.rgba> [--runtime <dll>]");
                return ExitCode::FAILURE;
            }
            (other, _) => {
                eprintln!("unrecognised argument {other}");
                eprintln!("usage: --models <dir> --image <file.rgba> [--runtime <dll>]");
                return ExitCode::FAILURE;
            }
        }
    }

    let frame = match load_frame(&image) {
        Ok(frame) => frame,
        Err(message) => {
            eprintln!("could not read {}: {message}", image.display());
            return ExitCode::FAILURE;
        }
    };
    println!("frame {} x {}", frame.size().width, frame.size().height);

    let config = PaddleConfig {
        detection_model: models.join("ch_PP-OCRv4_det.onnx"),
        recognition_model: models.join("ch_PP-OCRv4_rec.onnx"),
        dictionary: models.join("ppocr_keys_v1.txt"),
        runtime_library: runtime,
    };

    let started = std::time::Instant::now();
    let mut engine = match PaddleEngine::load(&config, PaddleOptions::default()) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("load failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("loaded in {} ms", started.elapsed().as_millis());

    let started = std::time::Instant::now();
    let recognition = match engine.recognise(&frame) {
        Ok(recognition) => recognition,
        Err(error) => {
            eprintln!("recognise failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let elapsed = started.elapsed();

    println!("recognised in {} ms", elapsed.as_millis());
    println!("blocks: {}", recognition.blocks().count());
    println!("lines:  {}", recognition.lines().len());
    for block in recognition.blocks() {
        println!(
            "  [{:>4},{:>4} {:>4}x{:>4}]  {}",
            block.bounds.origin.x,
            block.bounds.origin.y,
            block.bounds.size.width,
            block.bounds.size.height,
            block.text
        );
    }
    println!("--- text() ---");
    println!("{}", recognition.text());

    if recognition.is_empty() {
        eprintln!("NO TEXT FOUND -- the pipeline ran and read nothing");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Reads the flat `.rgba` format described in this file's header.
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
