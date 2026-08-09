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
//! [`capture_region_via_gdi`] takes the fallback for every monitor, skipping
//! WGC. The fallback does not otherwise fire on a healthy modern desktop, so
//! this is how it is exercised and pixel-compared against WGC on the rig
//! (`examples/grab.rs --gdi`), and it is the diagnostic to reach for if WGC
//! ever misbehaves in the field.
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
pub mod warm;
#[cfg(windows)]
mod wgc;

pub use error::CaptureError;
pub use plan::MonitorInfo;

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

/// The monitors of the virtual desktop **as this crate sees them**, with their
/// `HMONITOR` handles and their bounds in physical virtual-desktop pixels.
///
/// # Why this is public, when `monitors`'s own docs say the two enumerations are
/// deliberately separate
///
/// They are, and this does not merge them. That module gives the reason: the
/// Tauri app's enumeration reports through tao and carries scale factors for DPI
/// decisions, while capture needs the raw handle and nothing else. Both describe
/// the same hardware through the same OS tables.
///
/// What this exposes is that **a caller choosing a region to capture should
/// choose it from the list the capture will be clamped against**, and until now
/// it could not. `up-take`'s instant monitor grab picked its monitor from tao's
/// list and handed the result to [`capture_region`], which clamps against this
/// one: two enumerations, one decision, and any disagreement between them is a
/// screenshot of a rectangle nobody asked for. That is `I-31`.
///
/// **It is also the cheaper call, which is the half that is easy to miss.** Tao's
/// `available_monitors()` is a `window_getter!` into `send_user_message`, so from
/// any thread that is not the event-loop thread it posts to the event-loop proxy
/// and blocks on `rx.recv()` with no timeout. This is a direct
/// `EnumDisplayMonitors` on the calling thread.
///
/// **Coordinates are physical pixels only in a per-monitor-DPI-aware process.**
/// The UP-TAKE app is one because tao opts in; a standalone binary using this
/// crate must opt in itself (see `examples/grab.rs`) or Windows serves it
/// DPI-virtualized coordinates and every rectangle here is subtly wrong.
///
/// An empty desktop comes back as an empty `Vec` rather than an error, so this
/// fails only when `EnumDisplayMonitors` itself does.
#[cfg(windows)]
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    monitors::enumerate()
}

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
    capture_region_inner(region, false)
}

/// Captures `region` through the GDI fallback alone, as if WGC were
/// unavailable on every monitor.
///
/// The fallback never fires on its own on a healthy desktop, so this is how it
/// is verified (`tests/hardware.rs`, `examples/grab.rs --gdi`) and the
/// diagnostic to reach for if WGC misbehaves in the field. Prefer
/// [`capture_region`] everywhere else: GDI captures hardware-overlay video as
/// black where WGC is crisp, and it cannot be told to exclude a window.
///
/// This is an explicit parameter rather than an environment switch on purpose —
/// a process-global that changes which path a capture takes can be flipped by
/// one test and silently observed by another running beside it.
#[cfg(windows)]
pub fn capture_region_via_gdi(region: Rect) -> Result<CapturedRegion, CaptureError> {
    capture_region_inner(region, true)
}

#[cfg(windows)]
fn capture_region_inner(region: Rect, force_gdi: bool) -> Result<CapturedRegion, CaptureError> {
    let monitors = monitors::enumerate()?;
    let capture_plan = plan::plan(region, &monitors)?;
    let mut bitmap =
        RgbaBitmap::transparent(capture_plan.output.size).ok_or(CaptureError::TooLarge)?;

    if force_gdi {
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
