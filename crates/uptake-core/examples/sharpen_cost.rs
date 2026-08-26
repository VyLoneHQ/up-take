//! What the roadmap 1.29 sharpening pass costs, per area size, in a release
//! build.
//!
//! # Why this exists
//!
//! [ADR-0031] closes with a warning: *"nothing here is measured. No shader has
//! been written, no capture loop has been timed, and the claim that CAS is
//! cheap is from its published description rather than from this machine."*
//! This is the instrument that answers the third clause. `UT-F-79` is this
//! project's record of what happens without one: a subsystem priced from its
//! name and a conclusion reversed twice.
//!
//! It also answers a question roadmap **1.30** has to ask before it can go
//! live. A pass that is comfortable once per re-take may be impossible at
//! 18 fps, and the difference is arithmetic on the number this prints rather
//! than a judgement.
//!
//! # Run it
//!
//! ```text
//! cargo run --release --example sharpen_cost -p uptake-core
//! ```
//!
//! **`--release` is not optional and the program says so if it is missing.**
//! The filter is per-pixel float arithmetic with bounds checks, which is the
//! shape debug builds punish hardest, so a debug number would be wrong by a
//! factor nobody could name.
//!
//! # What it does NOT measure
//!
//! The capture, which on the same path is a median 236 ms and 96% of the total
//! (ADR-0031). This is the added stage alone, which is the only part 1.29
//! introduced and the only part a shader would move.
//!
//! It is also **synthetic content**: a step-edge fixture, not a screen. The
//! filter is data-dependent only through its branchless per-pixel arithmetic,
//! so the shape of the content moves the result very little, but this is a
//! bound on the pass rather than a reading from a rig. `quality-bars.md` §1
//! footnote 3 wants a run to state its conditions, and these are the
//! conditions.
//!
//! [ADR-0031]: the private planning repo's
//! `DECISIONS/ADR-0031-upscale-is-enhancement-not-magnification.md`

use std::time::Instant;

use uptake_core::bitmap::{BYTES_PER_PIXEL, RgbaBitmap};
use uptake_core::geometry::Size;
use uptake_core::sharpen::{DEFAULT_SHARPNESS, rcas};

/// Runs per size. The median is reported rather than the mean, for the reason
/// ADR-0031's own 51-sample table gives: the distribution on this machine has a
/// long right tail and a mean that sits below its own median.
const RUNS: usize = 9;

/// The fixture's two values. Mid contrast, so the ring has headroom in both
/// directions and the filter has work to do: a black-to-white edge has none and
/// RCAS correctly declines it, which would time the early-out instead.
const DARK: u8 = 102;
const LIGHT: u8 = 153;

fn main() {
    if cfg!(debug_assertions) {
        eprintln!(
            "REFUSED: this is a debug build and the number would be meaningless.\n\
             Run: cargo run --release --example sharpen_cost -p uptake-core"
        );
        std::process::exit(1);
    }
    println!("RCAS cost at {DEFAULT_SHARPNESS} stops, {RUNS} runs per size, release build.");
    println!("Median of {RUNS}; min and max given because a single sample is not a measurement.");
    println!();
    println!(
        "{:>12}  {:>10}  {:>9}  {:>9}  {:>9}",
        "area", "megapixels", "median", "min", "max"
    );
    for (width, height, note) in [
        (400, 300, "a small area"),
        (800, 600, "a typical area"),
        (1280, 720, "a 720p video window"),
        (1920, 1080, "a 1080p player, full screen"),
        (2560, 1440, "the whole primary monitor"),
        (3840, 2160, "4K, the worst case on this rig"),
    ] {
        let mut samples = Vec::with_capacity(RUNS);
        for _ in 0..RUNS {
            let Some(mut bitmap) = step_edge(width, height) else {
                eprintln!("REFUSED: could not build a {width}x{height} fixture.");
                std::process::exit(1);
            };
            let started = Instant::now();
            rcas(&mut bitmap, DEFAULT_SHARPNESS);
            samples.push(started.elapsed().as_micros());
            // Read a filtered byte back so the call above cannot be optimised
            // away as dead, and assert something the filter can actually
            // falsify: the pixel just right of the edge is on the light side
            // and RCAS must have pushed it ABOVE its input. `<= 255` was the
            // first version and the compiler said what it was worth, warning
            // `unused_comparisons` on a `u8` that cannot exceed 255. A guard
            // that cannot go red is `UT-F-75` and it does not stop a dead-code
            // pass either.
            let at = ((height / 2) * width + width / 2) as usize * BYTES_PER_PIXEL;
            assert!(
                bitmap.pixels()[at] > LIGHT,
                "the fixture was not sharpened, so this timed nothing"
            );
        }
        samples.sort_unstable();
        let megapixels = (f64::from(width) * f64::from(height)) / 1_000_000.0;
        println!(
            "{:>12}  {megapixels:>10.2}  {:>7.1}ms  {:>7.1}ms  {:>7.1}ms   {note}",
            format!("{width}x{height}"),
            ms(samples[RUNS / 2]),
            ms(samples[0]),
            ms(samples[RUNS - 1]),
        );
    }
    println!();
    println!(
        "Read the 2560x1440 row against roadmap 1.30: at 18 fps a frame is 55 ms, and this \n\
         pass has to fit inside it ALONGSIDE the 8-11 ms warm readback warm.rs measures."
    );
}

fn ms(micros: u128) -> f64 {
    micros as f64 / 1000.0
}

/// A vertical step edge at mid contrast: the case the filter actually does work
/// on. A flat fixture would measure the early-out rather than the filter.
///
/// `None` where the size does not fit in a `usize`, which the caller reports and
/// exits on. This crate forbids `unwrap` and `expect` outside tests, and an
/// instrument that panics prints a backtrace where it should print a reason.
fn step_edge(width: u32, height: u32) -> Option<RgbaBitmap> {
    let mut pixels = vec![255u8; (width * height) as usize * BYTES_PER_PIXEL];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = (y * width as usize + x) * BYTES_PER_PIXEL;
            let value = if x < width as usize / 2 { DARK } else { LIGHT };
            pixels[at..at + 3].fill(value);
        }
    }
    RgbaBitmap::from_pixels(Size::new(width, height), pixels)
}
