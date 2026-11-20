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

// A test that reads files and indexes frames: a failure is a panic with a message.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

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
    marks: [u8; MARKS],
    per_frame: Duration,
) -> Vec<(Event, f32)> {
    let mut detector = detector(direction, marks);
    let mut seen = Vec::new();
    let mut at = Duration::ZERO;
    let reading = per_frame / u32::try_from(repeat).expect("a small count");
    for frame in frames {
        let print = Fingerprint::of(&frame.bitmap).expect("a non-empty picture");
        for _ in 0..repeat {
            at += reading;
            if let Some(event) = detector.observe_at(print.clone(), at) {
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

/// A bar first read part full, then emptying, then filling twice: what the
/// area must say. Marks at 20 and 50, the first reading at 30 %.
fn judge_scenarios(frames: &[&Frame], direction: Direction) -> Result<(), String> {
    let at = |value: f32| {
        frames
            .iter()
            .position(|f| f.value >= value)
            .ok_or_else(|| format!("no frame at {value}"))
    };
    let (placed, empty) = (at(30.0)?, at(0.0)?);
    let print = |i: usize| Fingerprint::of(&frames[i].bitmap).expect("a non-empty picture");
    let mut detector = detector(direction, [20, 50, 0, 0]);
    let mut events = Vec::new();
    // Placed over the bar at 30 %: it fills to the end. 20 % was passed.
    detector.observe(print(placed));
    for i in placed..frames.len() {
        events.extend(detector.observe(print(i)));
    }
    if events != [Event::Reached(50), Event::Filled] {
        return Err(format!("placed at 30 %, saw {events:?}"));
    }
    // Then it empties (a new file) and fills again: every moment again.
    let mut again = Vec::new();
    for i in (empty..frames.len()).step_by(4) {
        again.extend(detector.observe(print(i)));
    }
    if again != [Event::Reached(20), Event::Reached(50), Event::Filled] {
        return Err(format!("filling again, saw {again:?}"));
    }
    Ok(())
}

#[test]
#[ignore = "needs the fixtures tools/bar-harness/capture.mjs writes"]
fn the_bar_mode_reads_real_renders() {
    let dir = std::env::var("BAR_FIXTURES").map_or_else(
        |_| {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/bar-fixtures"
            ))
        },
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
    // Readings per frame follow the fixture's own step (0.5 % by default), at about 30 readings
    // a second.
    for path in files {
        let name = path
            .file_stem()
            .expect("a name")
            .to_string_lossy()
            .to_string();
        let style = name.split('_').next().expect("a style").to_string();
        let direction = direction_of(&style);
        let frames = load(&path);
        let refs: Vec<&Frame> = frames.iter().collect();
        let step = f64::from(frames[1].value - frames[0].value).max(0.01);
        let mut report = |label: &str, verdict: Result<String, String>| match verdict {
            Ok(what) => println!("ok    {name:<34} {label:>6}  {what}"),
            Err(why) => {
                failures += 1;
                println!("FAIL  {name:<34} {label:>6}  {why}");
            }
        };
        if style == "marquee" {
            // The sweep of an indeterminate bar, every 1.5 s, 3 s and 6 s: two
            // sweeps and one backwards, then caught at each of its phases, with
            // percentages low enough for the block to reach them. Nothing, ever.
            for (label, seconds) in [("1.5s", 1.5), ("3s", 3.0), ("6s", 6.0)] {
                let per_frame = Duration::from_secs_f64(seconds / (frames.len() - 1) as f64);
                let mut sweeps: Vec<&Frame> = refs.clone();
                sweeps.extend(refs.iter().copied());
                sweeps.extend(refs.iter().rev().copied());
                let events = feed(&sweeps, direction, 1, [20, 50, 80, 0], per_frame);
                // Caught at another phase: a block sitting at the far end reads
                // as a bar filled up to it (the bar's documented limit), so this
                // is counted, not judged.
                let mut elsewhere = 0;
                for start in (1..refs.len()).step_by(23) {
                    let mut phase: Vec<&Frame> = refs[start..].to_vec();
                    phase.extend(refs.iter().copied());
                    elsewhere += feed(&phase, direction, 1, [20, 50, 80, 0], per_frame).len();
                }
                report(
                    label,
                    if events.is_empty() {
                        Ok(format!(
                            "nothing (marks 20 50 80); {elsewhere} events when learned at other phases"
                        ))
                    } else {
                        Err(format!(
                            "a marquee notified {} times, first {:?}",
                            events.len(),
                            events[0]
                        ))
                    },
                );
            }
            continue;
        }
        for (label, seconds) in [("20s", 20.0), ("60s", 60.0), ("120s", 120.0)] {
            let repeat = ((step / 100.0 * seconds * 30.0).round() as usize).max(1);
            let per_frame = Duration::from_secs_f64(step / 100.0 * seconds);
            let events = feed(&refs, direction, repeat, [50, 0, 0, 0], per_frame);
            report(label, judge_fill(&events).map(|()| format!("{events:?}")));
        }
        report(
            "again",
            judge_scenarios(&refs, direction)
                .map(|()| String::from("placed at 30 %, then emptied and filled again")),
        );
    }
    assert_eq!(failures, 0, "{failures} fixture runs failed");
}
