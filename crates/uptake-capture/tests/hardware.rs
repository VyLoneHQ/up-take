//! Hardware-bound integration tests: they drive real WGC sessions and need a
//! real, interactive desktop, which CI runners do not have. All `#[ignore]`d;
//! run on the rig with:
//!
//! ```text
//! cargo test -p uptake-capture -- --ignored
//! ```
//!
//! quality-bars.md §2 scopes this crate to "thin integration tests only" for
//! exactly this reason — the pure planning/compositing logic is unit-tested,
//! the WGC path is verified here and via `examples/grab.rs` on the rig.

#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use uptake_core::geometry::{Rect, Size};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Physical coordinates require per-monitor-DPI awareness (see the crate
/// docs). Idempotent: the second call in the same process fails harmlessly.
fn ensure_dpi_aware() {
    // SAFETY: no memory-safety preconditions.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[test]
#[ignore = "needs a real desktop: drives a live WGC session"]
fn captures_a_small_region_at_the_primary_origin() {
    ensure_dpi_aware();
    // The primary monitor's top-left is (0, 0) by definition, so this region
    // exists on every machine.
    let captured = uptake_capture::capture_region(Rect::new(0, 0, 64, 48)).unwrap();
    assert_eq!(captured.rect, Rect::new(0, 0, 64, 48));
    assert_eq!(captured.bitmap.size(), Size::new(64, 48));
    // A real desktop frame is opaque — WGC reports full alpha. All-zero
    // pixels would mean we composited nothing and called it success.
    assert!(
        captured
            .bitmap
            .pixels()
            .chunks_exact(4)
            .all(|px| px[3] == 0xFF)
    );
}

#[test]
#[ignore = "needs a real desktop: drives a live WGC session"]
fn off_screen_and_empty_regions_error_without_capturing() {
    ensure_dpi_aware();
    assert!(matches!(
        uptake_capture::capture_region(Rect::new(1_000_000, 1_000_000, 10, 10)),
        Err(uptake_capture::CaptureError::Offscreen)
    ));
    assert!(matches!(
        uptake_capture::capture_region(Rect::new(0, 0, 0, 10)),
        Err(uptake_capture::CaptureError::EmptyRegion)
    ));
}

/// The GDI fallback (task 1.8), driven directly since it does not fire on its
/// own on a healthy desktop. Proves the DIB is captured and the BGRA→RGBA-opaque
/// conversion is right end to end: a real desktop frame comes back fully opaque,
/// and an all-zero result would mean nothing was blitted.
///
/// Selects the path by argument, not by an environment variable: libtest runs
/// these tests concurrently, so a process-global switch set here would be read
/// by `captures_a_small_region_at_the_primary_origin` running beside it, which
/// would then silently capture via GDI and still pass — leaving the crate's
/// only live-WGC assertion testing the wrong path.
#[test]
#[ignore = "needs a real desktop: drives a real GDI screen BitBlt"]
fn the_forced_gdi_fallback_captures_an_opaque_frame() {
    ensure_dpi_aware();
    let captured = uptake_capture::capture_region_via_gdi(Rect::new(0, 0, 64, 48)).unwrap();

    assert_eq!(captured.rect, Rect::new(0, 0, 64, 48));
    assert_eq!(captured.bitmap.size(), Size::new(64, 48));
    assert!(
        captured
            .bitmap
            .pixels()
            .chunks_exact(4)
            .all(|px| px[3] == 0xFF)
    );
}
