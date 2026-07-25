//! One-shot Windows Graphics Capture of virtual-desktop regions.
//!
//! The capture layer ADR-0014 pulled into Phase 1: [`capture_region`] takes a
//! rectangle in **physical pixels, virtual-desktop space** (the project-wide
//! geometry rule) and returns the screen's pixels there as an
//! [`RgbaBitmap`](uptake_core::bitmap::RgbaBitmap). This one entry point is
//! what powers the capture-type areas, freeze-on-demand, the instant monitor
//! grab, and Screenshot-pins-its-capture — each of those is "capture some
//! rectangle", and which rectangle is the caller's business.
//!
//! # How a capture runs
//!
//! WGC captures *monitors* (or windows), not arbitrary rectangles, so a
//! region is captured as: plan which monitors it touches ([`plan`]-module
//! arithmetic, pure and tested), take one WGC frame per touched monitor in
//! parallel ([`wgc`], one pump thread each), crop each frame, and composite
//! the crops into one output bitmap ([`blit`], pure and tested). Pixels of
//! the output covered by no monitor — the dead zones an uneven arrangement
//! leaves in the desktop's bounding box — come back transparent black, and a
//! region partly outside the virtual desktop is clamped to it: the returned
//! [`CapturedRegion::rect`] says what the bitmap actually shows.
//!
//! When a touched monitor's WGC session cannot deliver a frame, the capture
//! falls back to a GDI `BitBlt` of that monitor's rectangle rather than failing
//! outright ([`gdi`]; architecture.md §5, "degrade gracefully"). That fallback
//! recovers the *no-WGC-frame* case — a session that refused to start or
//! stalled — and nothing more: DRM and hardware-overlay content are composited
//! black by the DWM under every capture API, GDI included, so this is a
//! WGC-unavailable fallback, not a protected-content bypass. A capture fails
//! only when **both** paths fail for a monitor, or the display topology changes
//! mid-capture (which aborts the whole capture, since the plan is now stale).
//!
//! Setting the `UPTAKE_FORCE_GDI` environment variable skips WGC entirely and
//! captures every monitor through GDI. The fallback path does not otherwise
//! fire on a healthy modern desktop, so this is how it is exercised and
//! pixel-compared against WGC on the rig (`examples/grab.rs --gdi`).
//!
//! # What callers must know
//!
//! - **DPI awareness is a precondition.** Monitor coordinates are physical
//!   pixels only in a per-monitor-DPI-aware process; the UP-TAKE app opts in
//!   via tao, standalone binaries must do it themselves (`examples/grab.rs`).
//! - **Any thread may call this.** Capture threads are spawned internally;
//!   the calling thread only blocks waiting, bounded by
//!   [`wgc::FIRST_FRAME_TIMEOUT`] (2 s — a failure bound, not the expected
//!   latency: the spike measured ~90 ms to first frame against a 300 ms
//!   selection→clipboard budget).
//! - **The capture excludes the cursor and draws no border** where Windows
//!   allows (Win 10 2004+ / Server 2022+ respectively); on older builds one
//!   silent retry accepts the system defaults instead, so the result may
//!   include the cursor there — degraded beats absent.

#![deny(missing_docs)]

mod blit;
mod error;
mod plan;

#[cfg(windows)]
mod gdi;
#[cfg(windows)]
mod monitors;
#[cfg(windows)]
mod wgc;

pub use error::CaptureError;

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

/// The pixels of a screen region, together with the rectangle they show.
///
/// `rect` equals the requested region clamped to the virtual desktop — the
/// two differ exactly when the request reached off-screen, and callers that
/// care (a pin re-rendering at its capture position) must place the bitmap at
/// `rect`, not at the region they asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRegion {
    /// What the bitmap shows, in physical virtual-desktop pixels.
    pub rect: Rect,
    /// The pixels: RGBA8, row-major, top-down, `rect.size` in dimensions.
    /// Pixels on no monitor (dead zones) are transparent black.
    pub bitmap: RgbaBitmap,
}

/// Captures `region` (physical pixels, virtual-desktop space) from the live
/// desktop.
///
/// Blocks the calling thread for the duration of the capture — ~100 ms on the
/// reference machine, bounded by [`wgc::FIRST_FRAME_TIMEOUT`] per attempt on
/// failure. See the crate docs for the full behaviour contract; errors are
/// [`CaptureError`] and each variant's message says what to do about it.
#[cfg(windows)]
pub fn capture_region(region: Rect) -> Result<CapturedRegion, CaptureError> {
    let monitors = monitors::enumerate()?;
    let capture_plan = plan::plan(region, &monitors)?;
    let mut bitmap =
        RgbaBitmap::transparent(capture_plan.output.size).ok_or(CaptureError::TooLarge)?;

    // The forcing seam (see the crate docs): capture every monitor through GDI,
    // as if WGC were unavailable, so the fallback path can be exercised on a
    // desktop where it would never fire on its own.
    if std::env::var_os("UPTAKE_FORCE_GDI").is_some() {
        for &shot in &capture_plan.shots {
            let pixels = gdi::capture_shot(shot)?;
            if !blit::blit(&mut bitmap, shot.dest_x, shot.dest_y, &pixels, shot.size) {
                return Err(CaptureError::DisplayChanged);
            }
        }
        return Ok(CapturedRegion {
            rect: capture_plan.output,
            bitmap,
        });
    }

    // Spawn every shot before waiting on any: monitors capture in parallel,
    // so a multi-monitor region costs one first-frame latency, not one per
    // monitor. An early failure drops the later PendingShots, whose Drop
    // unwinds their pump threads.
    let deadline = std::time::Instant::now() + wgc::FIRST_FRAME_TIMEOUT;
    let mut pending = Vec::with_capacity(capture_plan.shots.len());
    for &shot in &capture_plan.shots {
        pending.push(wgc::spawn(shot)?);
    }

    for shot_in_flight in pending {
        let shot = shot_in_flight.shot();
        let pixels = match shot_in_flight.wait(deadline) {
            Ok(pixels) => pixels,
            // A stale plan means the desktop changed under us; the whole
            // capture is now built against the wrong topology, so abort rather
            // than fall back monitor-by-monitor onto coordinates that moved.
            Err(err @ CaptureError::DisplayChanged) => return Err(err),
            // WGC could not deliver this monitor — fall back to GDI for it.
            // Keep the original WGC error if GDI also fails: it names the
            // monitor and the underlying reason, and a double failure means the
            // system is genuinely unable to capture, not that GDI is the story.
            Err(wgc_err) => gdi::capture_shot(shot).map_err(|_gdi_err| wgc_err)?,
        };
        if !blit::blit(&mut bitmap, shot.dest_x, shot.dest_y, &pixels, shot.size) {
            // The extracted crop always matches the plan by construction; a
            // mismatch means the world changed under us mid-capture.
            return Err(CaptureError::DisplayChanged);
        }
    }

    Ok(CapturedRegion {
        rect: capture_plan.output,
        bitmap,
    })
}
