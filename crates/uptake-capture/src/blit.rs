//! Pure pixel-buffer operations: cropping rows out of a padded frame and
//! compositing crops into an output bitmap.
//!
//! Like [`crate::plan`], this is the testable half of the capture path — plain
//! byte arithmetic with every bound checked, no Windows calls. A `None`/`false`
//! return means the inputs disagree with each other (a frame smaller than the
//! plan expected, a crop that does not fit); the caller decides what that
//! means, nothing here panics.

use uptake_core::bitmap::{BYTES_PER_PIXEL, RgbaBitmap};
use uptake_core::geometry::Size;

/// Copies the `size` crop at (`x`, `y`) out of a row-padded RGBA frame into a
/// tightly-packed buffer (`size.width × size.height × 4` bytes).
///
/// `row_pitch` is the frame's stride in bytes — D3D-mapped textures pad rows,
/// so it can exceed `frame.width × 4`. Returns `None` when the crop or the
/// pitch does not fit the frame, or the source slice is shorter than the crop
/// needs — each of which means the frame is not what the capture plan thought
/// it was.
pub(crate) fn extract_rect(
    src: &[u8],
    row_pitch: usize,
    frame: Size,
    x: u32,
    y: u32,
    size: Size,
) -> Option<Vec<u8>> {
    if size.is_empty() {
        return None;
    }
    // The crop must lie inside the frame…
    if u64::from(x) + u64::from(size.width) > u64::from(frame.width)
        || u64::from(y) + u64::from(size.height) > u64::from(frame.height)
    {
        return None;
    }
    // …the pitch must fit at least one full frame row…
    let frame_row = (frame.width as usize).checked_mul(BYTES_PER_PIXEL)?;
    if row_pitch < frame_row {
        return None;
    }
    // …and the source must contain the crop's last row in full.
    let crop_row = (size.width as usize).checked_mul(BYTES_PER_PIXEL)?;
    let x_bytes = (x as usize).checked_mul(BYTES_PER_PIXEL)?;
    let last_row_start = (y as usize)
        .checked_add(size.height as usize - 1)?
        .checked_mul(row_pitch)?;
    let needed = last_row_start.checked_add(x_bytes)?.checked_add(crop_row)?;
    if src.len() < needed {
        return None;
    }

    let mut out = Vec::with_capacity((size.height as usize).checked_mul(crop_row)?);
    for row in 0..size.height as usize {
        let start = (y as usize + row) * row_pitch + x_bytes;
        out.extend_from_slice(&src[start..start + crop_row]);
    }
    Some(out)
}

/// Composites a tightly-packed RGBA crop into `dst` with its top-left at
/// (`dest_x`, `dest_y`).
///
/// Returns `false` — leaving `dst` partially written in no case — when the
/// crop does not fit inside the bitmap or `src`'s length disagrees with
/// `src_size`.
pub(crate) fn blit(
    dst: &mut RgbaBitmap,
    dest_x: u32,
    dest_y: u32,
    src: &[u8],
    src_size: Size,
) -> bool {
    let Some(src_row) = (src_size.width as usize).checked_mul(BYTES_PER_PIXEL) else {
        return false;
    };
    if src_row.checked_mul(src_size.height as usize) != Some(src.len()) {
        return false;
    }
    if u64::from(dest_x) + u64::from(src_size.width) > u64::from(dst.width())
        || u64::from(dest_y) + u64::from(src_size.height) > u64::from(dst.height())
    {
        return false;
    }
    let Some(dst_row) = (dst.width() as usize).checked_mul(BYTES_PER_PIXEL) else {
        return false;
    };

    let x_bytes = dest_x as usize * BYTES_PER_PIXEL;
    let pixels = dst.pixels_mut();
    for row in 0..src_size.height as usize {
        let dst_start = (dest_y as usize + row) * dst_row + x_bytes;
        let src_start = row * src_row;
        pixels[dst_start..dst_start + src_row]
            .copy_from_slice(&src[src_start..src_start + src_row]);
    }
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// A frame whose pixel at (x, y) is `[x, y, 0xAA, 0xFF]`, with `pad` bytes
    /// of `0xEE` row padding — distinguishable from any real pixel byte.
    fn synthetic_frame(width: u8, height: u8, pad: usize) -> (Vec<u8>, usize) {
        let row_pitch = usize::from(width) * BYTES_PER_PIXEL + pad;
        let mut frame = vec![0xEE; row_pitch * usize::from(height)];
        for y in 0..height {
            for x in 0..width {
                let at = usize::from(y) * row_pitch + usize::from(x) * BYTES_PER_PIXEL;
                frame[at..at + 4].copy_from_slice(&[x, y, 0xAA, 0xFF]);
            }
        }
        (frame, row_pitch)
    }

    #[test]
    fn extracts_an_interior_crop_from_a_padded_frame() {
        let (frame, pitch) = synthetic_frame(8, 6, 12);
        let out = extract_rect(&frame, pitch, Size::new(8, 6), 2, 1, Size::new(3, 2)).unwrap();
        assert_eq!(out.len(), 3 * 2 * BYTES_PER_PIXEL);
        // First pixel of the crop is frame pixel (2, 1); last is (4, 2).
        assert_eq!(&out[0..4], &[2, 1, 0xAA, 0xFF]);
        assert_eq!(&out[out.len() - 4..], &[4, 2, 0xAA, 0xFF]);
        // No padding byte leaked into the crop.
        assert!(!out.contains(&0xEE));
    }

    #[test]
    fn extracts_the_full_frame_when_unpadded() {
        let (frame, pitch) = synthetic_frame(4, 3, 0);
        let out = extract_rect(&frame, pitch, Size::new(4, 3), 0, 0, Size::new(4, 3)).unwrap();
        assert_eq!(out, frame);
    }

    #[test]
    fn rejects_crops_that_leave_the_frame_or_lie_about_the_pitch() {
        let (frame, pitch) = synthetic_frame(8, 6, 4);
        let size = Size::new(8, 6);
        // Off the right edge, off the bottom, empty crop.
        assert!(extract_rect(&frame, pitch, size, 6, 0, Size::new(3, 2)).is_none());
        assert!(extract_rect(&frame, pitch, size, 0, 5, Size::new(1, 2)).is_none());
        assert!(extract_rect(&frame, pitch, size, 0, 0, Size::new(0, 2)).is_none());
        // A pitch smaller than one frame row is malformed.
        assert!(extract_rect(&frame, 8 * BYTES_PER_PIXEL - 1, size, 0, 0, size).is_none());
        // A source slice shorter than the crop needs.
        assert!(
            extract_rect(
                &frame[..frame.len() - 8],
                pitch,
                size,
                4,
                5,
                Size::new(4, 1)
            )
            .is_none()
        );
    }

    #[test]
    fn blits_into_the_right_rows_and_columns() {
        let mut dst = RgbaBitmap::transparent(Size::new(4, 4)).unwrap();
        let src = vec![7u8; 2 * 2 * BYTES_PER_PIXEL];
        assert!(blit(&mut dst, 1, 2, &src, Size::new(2, 2)));
        let px = |x: usize, y: usize| {
            let at = (y * 4 + x) * BYTES_PER_PIXEL;
            dst.pixels()[at]
        };
        // The 2×2 block at (1, 2) is written; its neighbours are untouched.
        assert_eq!(px(1, 2), 7);
        assert_eq!(px(2, 3), 7);
        assert_eq!(px(0, 2), 0);
        assert_eq!(px(3, 3), 0);
        assert_eq!(px(1, 1), 0);
    }

    #[test]
    fn blit_rejects_out_of_bounds_and_mismatched_sources() {
        let mut dst = RgbaBitmap::transparent(Size::new(4, 4)).unwrap();
        let src = vec![7u8; 2 * 2 * BYTES_PER_PIXEL];
        // Would overhang the right edge / bottom edge.
        assert!(!blit(&mut dst, 3, 0, &src, Size::new(2, 2)));
        assert!(!blit(&mut dst, 0, 3, &src, Size::new(2, 2)));
        // Source length disagrees with the claimed size.
        assert!(!blit(&mut dst, 0, 0, &src[..8], Size::new(2, 2)));
        // Nothing was written by the rejected calls.
        assert!(dst.pixels().iter().all(|&b| b == 0));
    }

    #[test]
    fn extract_then_blit_round_trips_a_monitor_crop() {
        // The end-to-end path a real shot takes: crop from a padded frame,
        // composite into a larger output at an offset.
        let (frame, pitch) = synthetic_frame(8, 6, 20);
        let crop = extract_rect(&frame, pitch, Size::new(8, 6), 1, 1, Size::new(4, 3)).unwrap();
        let mut out = RgbaBitmap::transparent(Size::new(10, 5)).unwrap();
        assert!(blit(&mut out, 6, 2, &crop, Size::new(4, 3)));
        // Output pixel (6, 2) is frame pixel (1, 1); (9, 4) is (4, 3).
        let px = |x: usize, y: usize| {
            let at = (y * 10 + x) * BYTES_PER_PIXEL;
            &out.pixels()[at..at + 4]
        };
        assert_eq!(px(6, 2), &[1, 1, 0xAA, 0xFF]);
        assert_eq!(px(9, 4), &[4, 3, 0xAA, 0xFF]);
        // A pixel outside the blit stays transparent.
        assert_eq!(px(0, 0), &[0, 0, 0, 0]);
    }
}
