//! Manual capture driver for hardware verification: grabs a region of the
//! live desktop and writes it to a PNG.
//!
//! `uptake-capture` is Win32 surface that unit tests cannot exercise
//! (quality-bars.md §2 accepts that for this crate); this example is how a
//! human verifies the real path on the real rig — including regions that
//! straddle monitors, negative coordinates, and mixed DPI.
//!
//! ```text
//! cargo run -p uptake-capture --example grab -- <x> <y> <width> <height> [out.png]
//!
//! cargo run -p uptake-capture --example grab -- 100 100 640 480
//! cargo run -p uptake-capture --example grab -- 2360 100 500 400 straddle.png
//! cargo run -p uptake-capture --example grab -- -1000 0 400 300 portrait.png
//! ```
//!
//! Prints the clamped rectangle and the capture latency — the number the §1
//! budget (selection→clipboard < 300 ms) cares about.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use uptake_core::geometry::Rect;
    use windows_capture::encoder::{ImageEncoder, ImageEncoderPixelFormat, ImageFormat};
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };

    // Without per-monitor-DPI awareness Windows hands this process virtualized
    // coordinates and the capture is subtly misplaced on any scaled monitor.
    // The app gets this from tao; a standalone binary must ask.
    // SAFETY: no memory-safety preconditions; must run before any HWND exists.
    let aware =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if aware == 0 {
        return Err("could not enable per-monitor DPI awareness".into());
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let [x, y, width, height, rest @ ..] = args.as_slice() else {
        return Err("usage: grab <x> <y> <width> <height> [out.png]".into());
    };
    let region = Rect::new(x.parse()?, y.parse()?, width.parse()?, height.parse()?);
    let out_path = rest.first().map_or("grab.png", String::as_str);

    let started = std::time::Instant::now();
    let captured = uptake_capture::capture_region(region)?;
    let elapsed = started.elapsed();

    let png = ImageEncoder::new(ImageFormat::Png, ImageEncoderPixelFormat::Rgba8)?.encode(
        captured.bitmap.pixels(),
        captured.bitmap.width(),
        captured.bitmap.height(),
    )?;
    std::fs::write(out_path, png)?;

    // A second, discarded capture separates process-cold cost (COM/WinRT/DLL
    // initialization, paid once per process) from the per-call cost the §1
    // budget actually governs — the app is a resident tray process, so its
    // captures are all warm ones.
    let warm_started = std::time::Instant::now();
    let _ = uptake_capture::capture_region(region)?;
    let warm_elapsed = warm_started.elapsed();

    println!(
        "captured {:?} (requested {:?}) in {:.1} ms cold / {:.1} ms warm -> {out_path}",
        captured.rect,
        region,
        elapsed.as_secs_f64() * 1000.0,
        warm_elapsed.as_secs_f64() * 1000.0,
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("grab only runs on Windows — uptake-capture is the Windows capture surface");
}
