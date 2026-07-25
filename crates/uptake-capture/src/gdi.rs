//! GDI `BitBlt` fallback for the one case Windows Graphics Capture cannot
//! serve: a monitor whose WGC session refuses to start or stalls without ever
//! delivering a frame.
//!
//! # What this recovers — and what it does not
//!
//! WGC is the primary path and the better one: it captures hardware-overlay
//! (MPO) video crisply (the 2026-07-22 spike, ADR-0014) where GDI returns
//! black, and it excludes the cursor cleanly. This fallback exists only for
//! *"WGC produced no frame at all"* — an unsupported build, a driver/GPU-reset
//! state, an RDP/session quirk, a session that started and then stalled. In
//! those, a `BitBlt` of the screen device context still returns the composited
//! pixels.
//!
//! It deliberately does **not** try to recover protected content. DRM and
//! `WDA_EXCLUDEFROMCAPTURE`/`WDA_MONITOR` windows are composited black by the
//! DWM in *every* capture API — WGC blacks them, and so does GDI. This module
//! is a WGC-unavailable fallback, not a protected-content bypass, and nothing
//! here changes that.
//!
//! # Coordinate space
//!
//! `GetDC(NULL)` returns a device context for the whole virtual desktop whose
//! origin is the primary monitor's top-left; monitors left of or above the
//! primary have negative coordinates, and `BitBlt` accepts them. In a
//! per-monitor-DPI-aware process (the crate's precondition — see the crate
//! docs) those are physical pixels, so a [`Shot`]'s absolute source rectangle
//! maps straight in.
//!
//! # Pixel format
//!
//! A 32-bpp `BI_RGB` DIB section holds pixels as `B, G, R, X` bytes and GDI
//! leaves the `X` (alpha) byte zero. The WGC path produces `R, G, B, 255`
//! (`ColorFormat::Rgba8`, opaque), so [`convert_bgra_to_rgba_opaque`] swaps
//! the channels and forces alpha to `0xFF` — a screen capture is opaque by
//! definition — leaving both capture paths a single, identical format for
//! [`crate::blit`] to composite. The DIB is created top-down (negative
//! height), so its rows already match the top-down, tightly-packed buffer
//! `blit` expects; a 32-bpp DIB row is inherently DWORD-aligned, so there is
//! no stride padding to strip.

use std::ffi::c_void;

use uptake_core::geometry::Size;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HGDIOBJ, ReleaseDC, SRCCOPY,
    SelectObject,
};

use crate::error::CaptureError;
use crate::plan::Shot;

/// Bytes per pixel in the RGBA output — kept local so this module does not
/// reach across into `uptake_core`'s constant for a value this obvious.
const BYTES_PER_PIXEL: usize = 4;

/// Captures one [`Shot`]'s rectangle via GDI, returning tightly-packed,
/// top-down RGBA pixels ready for [`crate::blit::blit`] — the same shape the
/// WGC path yields, so the caller composites either without caring which ran.
///
/// Errors as [`CaptureError::Failed`], carrying the monitor's bounds so a
/// multi-monitor capture still names the culprit.
pub(crate) fn capture_shot(shot: Shot) -> Result<Vec<u8>, CaptureError> {
    // A shot's crop is monitor-local; the screen DC is virtual-desktop space,
    // so add the monitor's origin back to get the absolute source point.
    let src_x = i32::try_from(i64::from(shot.monitor.bounds.origin.x) + i64::from(shot.source_x))
        .map_err(|_| failed(shot, "capture source x is out of range"))?;
    let src_y = i32::try_from(i64::from(shot.monitor.bounds.origin.y) + i64::from(shot.source_y))
        .map_err(|_| failed(shot, "capture source y is out of range"))?;
    let width = i32::try_from(shot.size.width)
        .map_err(|_| failed(shot, "capture width is out of range"))?;
    let height = i32::try_from(shot.size.height)
        .map_err(|_| failed(shot, "capture height is out of range"))?;

    // SAFETY: GetDC(NULL) asks for the screen DC and has no memory-safety
    // preconditions; the matching ReleaseDC runs on every path below.
    let null_hwnd: HWND = std::ptr::null_mut();
    let screen = unsafe { GetDC(null_hwnd) };
    if screen.is_null() {
        return Err(failed(shot, "could not obtain the screen device context"));
    }
    let result = blit_into_dib(screen, src_x, src_y, shot.size, width, height);
    // SAFETY: `screen` is the DC just acquired; releasing it exactly once.
    unsafe { ReleaseDC(null_hwnd, screen) };

    result.map_err(|reason| failed(shot, reason))
}

/// Blits the source rectangle out of `screen` into a fresh top-down DIB and
/// returns its pixels as opaque RGBA. Every GDI object it creates is destroyed
/// before returning, on both the success and failure paths.
///
/// Returns a `&'static str` reason on failure so the caller can attach the
/// monitor context; keeping the raw GDI plumbing here means [`capture_shot`]
/// reads as intent.
fn blit_into_dib(
    screen: windows_sys::Win32::Graphics::Gdi::HDC,
    src_x: i32,
    src_y: i32,
    size: Size,
    width: i32,
    height: i32,
) -> Result<Vec<u8>, &'static str> {
    // SAFETY: a self-contained GDI sequence — create a memory DC and a DIB
    // section sized to the crop, BitBlt into it, read the pixels, and tear
    // every object back down. `bits` points into the DIB section and is valid
    // until `DeleteObject(bitmap)`; it is read before that. Each early return
    // first frees the objects already created.
    unsafe {
        let mem_dc = CreateCompatibleDC(screen);
        if mem_dc.is_null() {
            return Err("could not create a compatible device context");
        }

        // Negative height requests a top-down DIB, so row 0 is the top row and
        // the buffer matches blit's top-down expectation without a flip.
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };

        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            mem_dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if bitmap.is_null() || bits.is_null() {
            DeleteDC(mem_dc);
            return Err("could not allocate the capture bitmap");
        }

        let previous = SelectObject(mem_dc, bitmap as HGDIOBJ);
        let blitted = BitBlt(
            mem_dc,
            0,
            0,
            width,
            height,
            screen,
            src_x,
            src_y,
            // CAPTUREBLT so layered/transparent windows are included, matching
            // "what is actually on screen". BitBlt never draws the cursor, so
            // — like the WGC path — the result excludes it.
            SRCCOPY | CAPTUREBLT,
        );
        // DIB sections are written asynchronously; flush before reading `bits`.
        GdiFlush();

        let pixels = if blitted == 0 {
            Err("the screen-to-memory BitBlt failed")
        } else {
            let byte_len = (size.width as usize)
                .checked_mul(size.height as usize)
                .and_then(|px| px.checked_mul(BYTES_PER_PIXEL));
            match byte_len {
                // The bitmap was allocated at this size, so this cannot
                // overflow in practice; refuse rather than risk a bad slice.
                None => Err("capture dimensions overflow a pixel buffer"),
                Some(len) => {
                    let raw = std::slice::from_raw_parts(bits as *const u8, len);
                    Ok(convert_bgra_to_rgba_opaque(raw))
                }
            }
        };

        SelectObject(mem_dc, previous);
        DeleteObject(bitmap as HGDIOBJ);
        DeleteDC(mem_dc);
        pixels
    }
}

/// Rewrites a top-down `B, G, R, X` DIB buffer as `R, G, B, 0xFF`: swap the
/// blue and red channels GDI stores reversed, and force full alpha because a
/// screen capture is opaque and GDI leaves the alpha byte zero.
///
/// `src.len()` is a whole number of pixels by construction (the DIB is
/// `width × height × 4`); a trailing partial pixel, which cannot occur, is
/// dropped rather than read out of bounds.
fn convert_bgra_to_rgba_opaque(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    for px in src.chunks_exact(BYTES_PER_PIXEL) {
        out.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
    }
    out
}

/// A [`CaptureError::Failed`] naming the shot's monitor, so a GDI failure reads
/// the same as a WGC one to anything upstream.
fn failed(shot: Shot, reason: &str) -> CaptureError {
    CaptureError::Failed {
        monitor: shot.monitor.bounds,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_channels_and_forces_opaque_alpha() {
        // Two pixels: GDI stores them B, G, R, X with X (alpha) left at 0.
        let bgra = [
            10, 20, 30, 0, // -> R=30, G=20, B=10, A=255
            0, 0, 0, 0, // pure black, still opaque out
        ];
        let rgba = convert_bgra_to_rgba_opaque(&bgra);
        assert_eq!(rgba, [30, 20, 10, 0xFF, 0, 0, 0, 0xFF]);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(convert_bgra_to_rgba_opaque(&[]).is_empty());
    }

    #[test]
    fn a_trailing_partial_pixel_is_dropped_not_read_past() {
        // Six bytes = one whole pixel plus two stray bytes; only the whole
        // pixel is converted, and nothing panics.
        let out = convert_bgra_to_rgba_opaque(&[1, 2, 3, 4, 9, 9]);
        assert_eq!(out, [3, 2, 1, 0xFF]);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// For any whole number of pixels: output is the same length, every
        /// alpha byte is forced opaque, red and blue are swapped, and green is
        /// carried through untouched.
        #[test]
        fn conversion_is_a_channel_swap_with_opaque_alpha(
            pixels in proptest::collection::vec(any::<u8>(), 0..4096)
                .prop_map(|mut v| { v.truncate(v.len() / 4 * 4); v })
        ) {
            let out = convert_bgra_to_rgba_opaque(&pixels);
            prop_assert_eq!(out.len(), pixels.len());
            for (bgra, rgba) in pixels.chunks_exact(4).zip(out.chunks_exact(4)) {
                prop_assert_eq!(rgba[0], bgra[2]); // R <- B position
                prop_assert_eq!(rgba[1], bgra[1]); // G unchanged
                prop_assert_eq!(rgba[2], bgra[0]); // B <- R position
                prop_assert_eq!(rgba[3], 0xFF);    // alpha forced opaque
            }
        }
    }
}
