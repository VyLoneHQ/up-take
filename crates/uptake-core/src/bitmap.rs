//! CPU-side RGBA bitmaps — the currency between capture and everything after.
//!
//! [`RgbaBitmap`] is what `uptake-capture` produces and what OCR, encoding and
//! rendering consume. It lives in `uptake-core` because of the dependency rule
//! (crates may share types only through `core`): `uptake-ocr` must be able to
//! accept a frame without depending on the Windows-only capture crate.
//!
//! The pixel format is fixed: **RGBA, 8 bits per channel, row-major, top-down,
//! no row padding**. A bitmap is dumb bytes with an enforced size invariant —
//! `pixels.len() == width × height × 4` — and no notion of *where* on screen it
//! came from. Position stays with the caller (in physical virtual-desktop
//! coordinates, per the crate rule); a bitmap is already resolved pixels, and
//! carrying a coordinate here would invite arithmetic in the wrong space.

use crate::geometry::Size;

/// Bytes per RGBA pixel.
pub const BYTES_PER_PIXEL: usize = 4;

/// An owned RGBA8 image: row-major, top-down, no row padding.
///
/// The size invariant (`pixels.len() == width × height × 4`) holds for every
/// value of this type — both constructors enforce it and no method can break
/// it ([`pixels_mut`](Self::pixels_mut) exposes the bytes but cannot change
/// their length).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaBitmap {
    size: Size,
    pixels: Vec<u8>,
}

impl RgbaBitmap {
    /// A bitmap of `size` filled with transparent black (`0, 0, 0, 0`).
    ///
    /// Returns `None` when `width × height × 4` does not fit in `usize` — an
    /// allocation that could not succeed and, on any real input, indicates the
    /// caller's geometry is corrupt rather than merely large. Callers pass
    /// screen-sized rectangles; refusing loudly beats aborting the process on
    /// an impossible `Vec` allocation.
    #[must_use]
    pub fn transparent(size: Size) -> Option<Self> {
        let len = byte_len(size)?;
        Some(Self {
            size,
            pixels: vec![0; len],
        })
    }

    /// Wraps existing pixel bytes as a bitmap of `size`.
    ///
    /// Returns `None` unless `pixels.len()` is exactly `width × height × 4` —
    /// a mismatch means the bytes and the claimed dimensions disagree, and
    /// there is no non-arbitrary way to pick which of the two is lying.
    #[must_use]
    pub fn from_pixels(size: Size, pixels: Vec<u8>) -> Option<Self> {
        if byte_len(size)? != pixels.len() {
            return None;
        }
        Some(Self { size, pixels })
    }

    /// The bitmap's dimensions in pixels.
    #[must_use]
    pub const fn size(&self) -> Size {
        self.size
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.size.width
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.size.height
    }

    /// The pixel bytes: RGBA8, row-major, top-down, no padding.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Mutable access to the pixel bytes, for compositing into the bitmap.
    ///
    /// A slice cannot grow or shrink, so the size invariant survives any use
    /// of this method.
    #[must_use]
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Consumes the bitmap, returning its pixel bytes.
    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

/// `width × height × 4` as a `usize`, or `None` on overflow.
fn byte_len(size: Size) -> Option<usize> {
    let area = u64::from(size.width).checked_mul(u64::from(size.height))?;
    usize::try_from(area.checked_mul(BYTES_PER_PIXEL as u64)?).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn transparent_allocates_zeroed_pixels_of_the_right_length() {
        let bitmap = RgbaBitmap::transparent(Size::new(3, 2)).unwrap();
        assert_eq!(bitmap.width(), 3);
        assert_eq!(bitmap.height(), 2);
        assert_eq!(bitmap.pixels().len(), 3 * 2 * BYTES_PER_PIXEL);
        assert!(bitmap.pixels().iter().all(|&b| b == 0));
    }

    #[test]
    fn zero_sized_bitmaps_are_representable_and_empty() {
        // A zero-area bitmap is a valid value (an empty capture), not an error
        // — rejecting empties is the *caller's* policy decision.
        let bitmap = RgbaBitmap::transparent(Size::new(0, 5)).unwrap();
        assert!(bitmap.pixels().is_empty());
    }

    #[test]
    fn from_pixels_accepts_exactly_matching_lengths_only() {
        let size = Size::new(2, 2);
        assert!(RgbaBitmap::from_pixels(size, vec![7; 16]).is_some());
        assert!(RgbaBitmap::from_pixels(size, vec![7; 15]).is_none());
        assert!(RgbaBitmap::from_pixels(size, vec![7; 17]).is_none());
        assert!(RgbaBitmap::from_pixels(size, Vec::new()).is_none());
    }

    #[test]
    fn overflowing_dimensions_refuse_rather_than_panic() {
        assert!(RgbaBitmap::transparent(Size::new(u32::MAX, u32::MAX)).is_none());
        assert!(RgbaBitmap::from_pixels(Size::new(u32::MAX, u32::MAX), Vec::new()).is_none());
    }

    #[test]
    fn pixels_round_trip_through_accessors() {
        let bytes: Vec<u8> = (0..16).collect();
        let mut bitmap = RgbaBitmap::from_pixels(Size::new(2, 2), bytes.clone()).unwrap();
        assert_eq!(bitmap.pixels(), &bytes[..]);
        bitmap.pixels_mut()[0] = 255;
        assert_eq!(bitmap.pixels()[0], 255);
        let recovered = bitmap.into_pixels();
        assert_eq!(recovered[0], 255);
        assert_eq!(recovered.len(), 16);
    }
}
