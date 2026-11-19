//! The bar mode against real renders (`tools/bar-harness`, roadmap `1.61`).
//!
//! Ignored: it needs the fixtures `tools/bar-harness/capture.mjs` writes
//! (Edge renders of the bar test page), which are not in git. Run it with
//!
//! ```text
//! node tools/bar-harness/capture.mjs --out target/bar-fixtures
//! cargo test -p uptake-core --test bar_fixtures -- --ignored --nocapture
//! ```
//!
//! `BAR_FIXTURES` names another directory, `BAR_FILTER` keeps only the
//! files whose name contains it. It prints one row per fixture and speed and
//! fails at the end if any row is wrong.

use std::path::PathBuf;

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Size;
use uptake_core::watch::{Detector, Direction, Event, Fingerprint, MARKS, Tuning, Watch};

/// One render: the bar at `value` percent (the marquee's sweep, for it).
struct Frame {
    value: f32,
    bitmap: RgbaBitmap,
}

fn load(path: &std::path::Path) -> Vec<Frame> {
    let data = std::fs::read(path).expect("a readable fixture");
    assert_eq!(&data[0..4], b"BARF");
    let word = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().expect("4 bytes"));
    let (width, height, count) = (word(4), word(8), word(12) as usize);
    let size = (width * height) as usize;
    let mut at = 16;
    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        let value = f32::from_le_bytes(data[at..at + 4].try_into().expect("4 bytes"));
        let grey = &data[at + 4..at + 4 + size];
        let pixels: Vec<u8> = grey.iter().flat_map(|&g| [g, g, g, 255]).collect();
        frames.push(Frame {
            value,
            bitmap: RgbaBitmap::from_pixels(Size::new(width, height), pixels)
                .expect("a bitmap of the right size"),
        });
        at += 4 + size;
    }
    frames
}

fn direction_of(style: &str) -> Direction {
    match style {
        "rtl" => Direction::RightToLeft,
        "vertical" => Direction::BottomToTop,
        _ => Direction::LeftToRight,
    }
}

fn detector(direction: Direction, marks: [u8; MARKS]) -> Detector {
    let mut detector = Detector::for_watch(Watch::Fills);
    let tuning = Tuning {
        direction,
        marks,
        ..Tuning::default()
    };
    detector.tune(Watch::Fills, tuning);
    detector
}

/// Events seen, each with the value of the frame that raised it. `repeat`
/// readings per frame: a slow bar shows the same picture many times.
fn feed(
    frames: &[&Frame],
    direction: Direction,
    repeat: usize,
) -> Vec<(Event, f32)> {
    let mut detector = detector(direction, [50, 0, 0, 0]);
    let mut seen = Vec::new();
    for frame in frames {
        let print = Fingerprint::of(&frame.bitmap).expect("a non-empty picture");
        for _ in 0..repeat {
            if let Some(event) = detector.observe(print.clone()) {
                seen.push((event, frame.value));
            }
        }
    }
    seen
}

/// What a fill must report: 50 % once, not before the bar is half full, and
/// the full bar once, not before it is.
fn judge_fill(events: &[(Event, f32)]) -> Result<(), String> {
    match events {
        [(Event::Reached(50), half), (Event::Filled, full)] => {
            if *half < 49.75 {
                Err(format!("50% reported early, at {half}"))
            } else if *full < 98.0 {
                Err(format!("full reported early, at {full}"))
            } else {
                Ok(())
            }
        }
        other => Err(format!("expected [Reached(50), Filled], saw {other:?}")),
    }
}

#[test]
#[ignore = "needs the fixtures tools/bar-harness/capture.mjs writes"]
fn the_bar_mode_reads_real_renders() {
    let dir = std::env::var("BAR_FIXTURES").map_or_else(
        |_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/bar-fixtures")),
        PathBuf::from,
    );
    let filter = std::env::var("BAR_FILTER").unwrap_or_default();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("no fixtures in {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "bar"))
        .filter(|path| path.to_string_lossy().contains(&filter))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures matched");
    let mut failures = 0;
    // Readings per frame at 0.25 % a frame, at about 30 readings a second.
    for path in files {
        let name = path.file_stem().expect("a name").to_string_lossy().to_string();
        let style = name.split('_').next().expect("a style").to_string();
        let direction = direction_of(&style);
        let frames = load(&path);
        let refs: Vec<&Frame> = frames.iter().collect();
        let step = f64::from(frames[1].value - frames[0].value).max(0.01);
        for (label, seconds) in [("20s", 20.0), ("60s", 60.0), ("120s", 120.0)] {
            let repeat = ((step / 100.0 * seconds * 30.0).round() as usize).max(1);
            let verdict = if style == "marquee" {
                // Two sweeps, and one backwards: nothing, ever.
                let mut sweeps: Vec<&Frame> = refs.clone();
                sweeps.extend(refs.iter().copied());
                sweeps.extend(refs.iter().rev().copied());
                let events = feed(&sweeps, direction, repeat);
                if events.is_empty() {
                    Ok(String::from("nothing"))
                } else {
                    Err(format!("a marquee notified: {events:?}"))
                }
            } else {
                let events = feed(&refs, direction, repeat);
                judge_fill(&events).map(|()| format!("{events:?}"))
            };
            match verdict {
                Ok(events) => println!("ok    {name:<34} {label:>4}  {events}"),
                Err(why) => {
                    failures += 1;
                    println!("FAIL  {name:<34} {label:>4}  {why}");
                }
            }
        }
    }
    assert_eq!(failures, 0, "{failures} fixture runs failed");
}
