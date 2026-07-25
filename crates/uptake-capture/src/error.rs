//! Capture failures, classified per architecture.md §5: every variant's message
//! tells the user (or the caller acting for them) what actually went wrong and
//! what would fix it — never a raw HRESULT with no verdict.

use thiserror::Error;
use uptake_core::geometry::Rect;

/// Why a capture produced no bitmap.
#[derive(Debug, Error)]
pub enum CaptureError {
    /// The requested region has zero width or height.
    #[error("the capture region is empty (zero width or height)")]
    EmptyRegion,

    /// Windows reported no monitors at all — mid display-topology change, a
    /// remote session tearing down, or enumeration running before the desktop
    /// exists.
    #[error(
        "no monitors are reported by the system; if displays were just \
             (un)plugged, try again in a moment"
    )]
    NoMonitors,

    /// The region does not overlap any monitor: entirely outside the virtual
    /// desktop, or inside one of the dead zones an uneven monitor arrangement
    /// leaves in its bounding rectangle.
    #[error("the capture region lies entirely off-screen")]
    Offscreen,

    /// `EnumDisplayMonitors` itself failed, which on a healthy desktop it does
    /// not do.
    #[error("monitor enumeration failed; if this persists, restart UP-TAKE")]
    Enumeration,

    /// The Windows Graphics Capture session for one monitor failed. Carries
    /// that monitor's bounds so a multi-monitor capture names the culprit.
    #[error("capture failed on the monitor at {monitor:?}: {reason}")]
    Failed {
        /// Bounds of the monitor whose capture failed, in physical
        /// virtual-desktop pixels.
        monitor: Rect,
        /// What the capture session reported, in prose.
        reason: String,
    },

    /// A capture session started but no frame arrived in time. Seen when the
    /// compositor is stalled or the session silently died; the capture thread
    /// is told to shut down before this is returned.
    #[error(
        "the monitor at {monitor:?} produced no frame within {timeout_ms} ms; \
             the system may be under heavy load — try again"
    )]
    Timeout {
        /// Bounds of the monitor that produced no frame.
        monitor: Rect,
        /// How long the capture waited before giving up.
        timeout_ms: u64,
    },

    /// A frame arrived whose dimensions no longer match the monitor the plan
    /// was built against — the display configuration changed mid-capture.
    #[error("the display configuration changed during capture; try again")]
    DisplayChanged,

    /// The composed output bitmap could not be allocated. With the output
    /// clamped to the virtual desktop this indicates corrupt geometry rather
    /// than a genuinely huge request.
    #[error("the capture is too large to hold in memory")]
    TooLarge,
}
