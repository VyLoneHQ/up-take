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

    /// Copies out the sub-rectangle `rect`, whose origin is **relative to this
    /// bitmap's top-left**, not to the screen.
    ///
    /// Returns `None` unless `rect` lies wholly inside the bitmap. That is the
    /// whole containment policy: a partial overlap is not clamped to what
    /// happens to be available, because every caller wants the rectangle it
    /// asked for or nothing. Cropping a held full-monitor frame to an area that
    /// runs off that monitor must **fall back to a real capture** rather than
    /// return a short image — see task 1.9c's straddle case, where clamping
    /// would silently produce a screenshot missing its right-hand third.
    ///
    /// # Why the origin is bitmap-local
    ///
    /// A bitmap has no notion of where on screen it came from (see the module
    /// docs), so a screen-space rectangle cannot be interpreted here. The
    /// caller holds the frame's position and does that subtraction — which also
    /// keeps this function pure arithmetic that tests without a desktop.
    #[must_use]
    pub fn crop(&self, rect: crate::geometry::Rect) -> Option<Self> {
        // Negative origins are out of bounds rather than saturating: a caller
        // that subtracted in the wrong direction should get `None` and take its
        // fallback, not a plausible image of the wrong part of the screen.
        let left = usize::try_from(rect.origin.x).ok()?;
        let top = usize::try_from(rect.origin.y).ok()?;
        let width = rect.size.width as usize;
        let height = rect.size.height as usize;
        let (source_width, source_height) = (self.size.width as usize, self.size.height as usize);
        if left.checked_add(width)? > source_width || top.checked_add(height)? > source_height {
            return None;
        }

        let row_bytes = width.checked_mul(BYTES_PER_PIXEL)?;
        let source_stride = source_width.checked_mul(BYTES_PER_PIXEL)?;
        let mut pixels = Vec::with_capacity(row_bytes.checked_mul(height)?);
        for row in 0..height {
            let start = (top + row)
                .checked_mul(source_stride)?
                .checked_add(left.checked_mul(BYTES_PER_PIXEL)?)?;
            pixels.extend_from_slice(self.pixels.get(start..start.checked_add(row_bytes)?)?);
        }
        Self::from_pixels(rect.size, pixels)
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
    use crate::geometry::Rect;

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

    /// A bitmap in which every pixel encodes its own coordinates, so a crop can
    /// be checked against *where the pixel came from* rather than against the
    /// same arithmetic the implementation used.
    fn coordinate_pattern(width: u32, height: u32) -> RgbaBitmap {
        let mut pixels = Vec::with_capacity((width * height) as usize * BYTES_PER_PIXEL);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    u8::try_from(x % 251).unwrap(),
                    u8::try_from(y % 251).unwrap(),
                    u8::try_from((x / 251) % 251).unwrap(),
                    u8::try_from((y / 251) % 251).unwrap(),
                ]);
            }
        }
        RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap()
    }

    fn pixel_at(bitmap: &RgbaBitmap, x: u32, y: u32) -> [u8; 4] {
        let start = (y as usize * bitmap.width() as usize + x as usize) * BYTES_PER_PIXEL;
        bitmap.pixels()[start..start + 4].try_into().unwrap()
    }

    #[test]
    fn a_crop_carries_the_pixels_that_were_at_that_offset() {
        // The property task 1.9c's fast path rests on: cropping a held
        // full-monitor frame must produce exactly the pixels a capture of that
        // sub-rectangle would have. Checked pixel-for-pixel against the *source*
        // coordinates, not against a second run of the crop arithmetic — a
        // round trip between two functions written together proves only that
        // they agree with each other (F-38).
        let source = coordinate_pattern(37, 23);
        let rect = Rect::new(11, 7, 13, 9);
        let cropped = source.crop(rect).unwrap();

        assert_eq!(cropped.width(), 13);
        assert_eq!(cropped.height(), 9);
        for y in 0..cropped.height() {
            for x in 0..cropped.width() {
                assert_eq!(
                    pixel_at(&cropped, x, y),
                    pixel_at(&source, x + 11, y + 7),
                    "pixel ({x}, {y}) of the crop"
                );
            }
        }
    }

    #[test]
    fn a_full_size_crop_is_the_original() {
        let source = coordinate_pattern(9, 5);
        let whole = source.crop(Rect::new(0, 0, 9, 5)).unwrap();
        assert_eq!(whole, source);
    }

    #[test]
    fn a_crop_that_does_not_fit_refuses_rather_than_clamping() {
        // Each of these is the straddle case in miniature. Clamping would hand
        // back a plausible image of the wrong size, which is exactly the quiet
        // failure `export_source` was written to stop.
        let source = coordinate_pattern(10, 10);
        for bad in [
            Rect::new(5, 0, 6, 1),   // runs off the right edge by one
            Rect::new(0, 5, 1, 6),   // off the bottom by one
            Rect::new(-1, 0, 2, 2),  // negative origin
            Rect::new(0, -1, 2, 2),  // negative origin, other axis
            Rect::new(10, 10, 1, 1), // wholly outside
            Rect::new(0, 0, 11, 10), // wider than the source
        ] {
            assert!(source.crop(bad).is_none(), "{bad:?} should not crop");
        }
        // The exact-fit boundary is the one that must still succeed, or every
        // area placed flush against a monitor's right or bottom edge would take
        // the slow path forever.
        assert!(source.crop(Rect::new(9, 9, 1, 1)).is_some());
        assert!(source.crop(Rect::new(0, 0, 10, 10)).is_some());
    }

    #[test]
    fn a_zero_sized_crop_inside_the_bitmap_is_empty_not_an_error() {
        let source = coordinate_pattern(4, 4);
        let empty = source.crop(Rect::new(2, 2, 0, 0)).unwrap();
        assert!(empty.pixels().is_empty());
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
