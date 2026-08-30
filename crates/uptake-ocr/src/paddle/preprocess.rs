//! Turning an [`RgbaBitmap`] into the tensor PP-OCRv4's detector expects.
//!
//! `architecture.md` section 3.2 opens the OCR pipeline with *"preprocess
//! (grayscale, threshold, denoise)"*. That description predates the choice of
//! PP-OCRv4 and does not survive contact with it: **the DB detector is trained
//! on three-channel colour input, normalised with ImageNet statistics**, and
//! feeding it a thresholded binary image would be feeding it something no
//! training sample looked like. Greyscale and thresholding live *inside* DB's
//! own learned filters, which is why the model is 4.7 MB rather than a
//! parameterless function.
//!
//! Recorded here rather than silently diverging from the spec: the pipeline's
//! shape is unchanged, but this stage is a resize and a normalise, not a
//! threshold. Roadmap 1.29's sharpening pass is a separate, deliberate
//! pre-filter and is not this.
//!
//! **Nothing in this file touches ONNX Runtime.** It is arithmetic over a
//! bitmap, so every rule below is tested in CI with no model present.

use uptake_core::bitmap::{BYTES_PER_PIXEL, RgbaBitmap};

/// ImageNet channel means, in RGB order, as PP-OCR's detector was trained.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
/// ImageNet channel standard deviations, in RGB order.
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// The detector's side-length quantum.
///
/// DB's backbone downsamples by 32, so a side that is not a multiple of 32
/// produces a probability map whose dimensions do not divide back cleanly. The
/// reference implementation rounds to this; so do we.
pub const SIDE_MULTIPLE: u32 = 32;

/// Default cap on the longer side, matching PP-OCR's `det_limit_side_len`.
///
/// Cost scales with pixel count, and `quality-bars.md` section 1 is a latency
/// budget, so this is the knob that decides how long a large area takes. It is a
/// default rather than a constant so a caller with a 4K monitor can trade
/// accuracy for time knowingly.
pub const DEFAULT_LIMIT_SIDE_LEN: u32 = 960;

/// A frame resized and normalised for the detector, plus how to get back.
///
/// The scale factors are the reason this is a struct and not a bare `Vec`.
/// Every box the detector produces is in **resized** coordinates, and mapping it
/// home needs the exact ratio that was used -- which is not the requested ratio,
/// because both sides were rounded to a multiple of 32 independently. Deriving
/// it again at the call site would be a second copy of a rounding rule, and the
/// two would disagree the first time one changed.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorInput {
    /// Normalised pixel data, NCHW with N = 1 and C = 3.
    pub tensor: Vec<f32>,
    /// Width the frame was resized to. A multiple of [`SIDE_MULTIPLE`].
    pub width: u32,
    /// Height the frame was resized to. A multiple of [`SIDE_MULTIPLE`].
    pub height: u32,
    /// Multiply a resized x-coordinate by this to get a source x-coordinate.
    pub scale_x: f32,
    /// Multiply a resized y-coordinate by this to get a source y-coordinate.
    pub scale_y: f32,
}

impl DetectorInput {
    /// The tensor's shape, as `ort` wants it: `[batch, channels, height, width]`.
    #[must_use]
    pub fn shape(&self) -> [usize; 4] {
        [1, 3, self.height as usize, self.width as usize]
    }
}

/// Chooses the resized dimensions for a frame.
///
/// Two rules, in this order, and the order is what makes the result predictable:
///
/// 1. If the longer side exceeds `limit_side_len`, scale **both** sides by one
///    ratio so it fits. Aspect ratio is preserved at this step.
/// 2. Round each side **independently** to the nearest multiple of
///    [`SIDE_MULTIPLE`], with a floor of one multiple so a thin strip does not
///    round to zero.
///
/// Step 2 is why aspect ratio is *not* preserved overall, and why
/// [`DetectorInput`] carries two scale factors rather than one. A 100x40 frame
/// becomes 96x32: the x ratio is 100/96, the y ratio is 40/32, and they are not
/// equal. Assuming one ratio here is a coordinate bug that shows up as boxes
/// drifting further from the text the further down the frame they sit --
/// `geometry.rs` calls coordinate maths this project's number one bug source.
#[must_use]
pub fn resized_dimensions(width: u32, height: u32, limit_side_len: u32) -> (u32, u32) {
    let longer = width.max(height);
    let (mut target_width, mut target_height) = (f64::from(width), f64::from(height));
    if longer > limit_side_len && longer > 0 {
        let ratio = f64::from(limit_side_len) / f64::from(longer);
        target_width *= ratio;
        target_height *= ratio;
    }
    (
        round_to_multiple(target_width),
        round_to_multiple(target_height),
    )
}

/// Rounds a dimension to the nearest multiple of [`SIDE_MULTIPLE`], never zero.
fn round_to_multiple(value: f64) -> u32 {
    let multiple = f64::from(SIDE_MULTIPLE);
    let rounded = (value / multiple).round() * multiple;
    if rounded < multiple {
        SIDE_MULTIPLE
    } else {
        // The cap keeps the cast total: a frame wider than u32::MAX/32 cannot
        // reach here because the caller's width is already a u32.
        rounded.min(f64::from(u32::MAX)) as u32
    }
}

/// Samples one channel of `bitmap` at a subpixel position, bilinearly.
///
/// Clamped at the edges rather than wrapped. Wrapping would let the right-hand
/// column blend into the left-hand one, which on a screenshot of a terminal is a
/// column of text bleeding into the opposite margin.
fn sample_bilinear(bitmap: &RgbaBitmap, x: f32, y: f32, channel: usize) -> f32 {
    let width = bitmap.width();
    let height = bitmap.height();
    if width == 0 || height == 0 {
        return 0.0;
    }
    let max_x = (width - 1) as f32;
    let max_y = (height - 1) as f32;
    let clamped_x = x.clamp(0.0, max_x);
    let clamped_y = y.clamp(0.0, max_y);

    let x0 = clamped_x.floor();
    let y0 = clamped_y.floor();
    let fraction_x = clamped_x - x0;
    let fraction_y = clamped_y - y0;

    // `as` is safe here: both values are clamped into [0, max] before the cast.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (x0, y0) = (x0 as u32, y0 as u32);
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);

    let at = |px: u32, py: u32| -> f32 {
        let index = (py as usize * width as usize + px as usize) * BYTES_PER_PIXEL + channel;
        bitmap
            .pixels()
            .get(index)
            .map_or(0.0, |&value| f32::from(value))
    };

    let top = at(x0, y0).mul_add(1.0 - fraction_x, at(x1, y0) * fraction_x);
    let bottom = at(x0, y1).mul_add(1.0 - fraction_x, at(x1, y1) * fraction_x);
    top.mul_add(1.0 - fraction_y, bottom * fraction_y)
}

/// Resizes and normalises a frame into the detector's input tensor.
///
/// Returns `None` for an empty bitmap -- there is no tensor for a zero-pixel
/// frame, and a caller that got one has a bug upstream rather than an empty
/// recognition.
///
/// # Alpha is ignored, deliberately
///
/// The detector wants RGB. A captured frame is opaque, and where it is not, the
/// honest choice is to read the colour channels as they stand rather than
/// composite against a background nobody chose: compositing onto white would
/// invent contrast the model then reports as text.
#[must_use]
pub fn detector_input(bitmap: &RgbaBitmap, limit_side_len: u32) -> Option<DetectorInput> {
    let (source_width, source_height) = (bitmap.width(), bitmap.height());
    if source_width == 0 || source_height == 0 {
        return None;
    }
    let (width, height) = resized_dimensions(source_width, source_height, limit_side_len);

    let scale_x = f64::from(source_width) as f32 / width as f32;
    let scale_y = f64::from(source_height) as f32 / height as f32;

    let plane = width as usize * height as usize;
    let mut tensor = vec![0.0_f32; plane * 3];
    for y in 0..height {
        for x in 0..width {
            // Sample at the centre of the destination pixel, mapped back into
            // source space. The half-pixel offsets matter: without them the
            // resize is biased half a pixel up and left, which on 8px text is a
            // sixteenth of a glyph.
            let source_x = (x as f32 + 0.5) * scale_x - 0.5;
            let source_y = (y as f32 + 0.5) * scale_y - 0.5;
            let destination = y as usize * width as usize + x as usize;
            for channel in 0..3 {
                let raw = sample_bilinear(bitmap, source_x, source_y, channel) / 255.0;
                tensor[channel * plane + destination] = (raw - MEAN[channel]) / STD[channel];
            }
        }
    }

    Some(DetectorInput {
        tensor,
        width,
        height,
        scale_x,
        scale_y,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use uptake_core::geometry::Size;

    /// A bitmap filled with one RGBA colour.
    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> RgbaBitmap {
        let pixels = rgba
            .iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * BYTES_PER_PIXEL)
            .collect();
        RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap()
    }

    #[test]
    fn a_small_frame_is_rounded_up_to_one_multiple_rather_than_to_zero() {
        assert_eq!(resized_dimensions(10, 4, 960), (32, 32));
    }

    #[test]
    fn sides_within_the_limit_are_only_snapped_to_the_multiple() {
        // 100 -> 96 (nearest multiple of 32), 40 -> 32.
        assert_eq!(resized_dimensions(100, 40, 960), (96, 32));
    }

    #[test]
    fn a_frame_over_the_limit_is_scaled_down_first() {
        // 1920x1080, limit 960: ratio 0.5 gives 960x540, then snapped to 960x544.
        assert_eq!(resized_dimensions(1920, 1080, 960), (960, 544));
    }

    #[test]
    fn both_sides_are_always_multiples_of_the_quantum() {
        for (width, height) in [(1, 1), (33, 65), (1920, 1080), (3840, 2160), (7, 4000)] {
            let (resized_width, resized_height) = resized_dimensions(width, height, 960);
            assert_eq!(resized_width % SIDE_MULTIPLE, 0, "width {resized_width}");
            assert_eq!(resized_height % SIDE_MULTIPLE, 0, "height {resized_height}");
            assert!(resized_width >= SIDE_MULTIPLE && resized_height >= SIDE_MULTIPLE);
        }
    }

    #[test]
    fn the_two_scale_factors_differ_when_the_rounding_is_uneven() {
        // This is the property that makes DetectorInput carry two factors. A
        // 100x40 frame resizes to 96x32, so x and y ratios are NOT equal, and a
        // caller assuming one ratio would place every box wrong.
        let input = detector_input(&solid(100, 40, [0, 0, 0, 255]), 960).unwrap();
        assert_eq!((input.width, input.height), (96, 32));
        assert!(
            (input.scale_x - input.scale_y).abs() > 0.1,
            "expected genuinely different factors, got {} and {}",
            input.scale_x,
            input.scale_y
        );
        assert!((input.scale_x - 100.0 / 96.0).abs() < 1e-5);
        assert!((input.scale_y - 40.0 / 32.0).abs() < 1e-5);
    }

    #[test]
    fn the_tensor_is_nchw_and_the_right_length() {
        let input = detector_input(&solid(64, 64, [0, 0, 0, 255]), 960).unwrap();
        assert_eq!(input.shape(), [1, 3, 64, 64]);
        assert_eq!(input.tensor.len(), 3 * 64 * 64);
    }

    #[test]
    fn normalisation_maps_black_and_white_to_the_imagenet_extremes() {
        // Black: (0 - mean) / std. White: (1 - mean) / std. Checked per channel,
        // because a channel-order slip (RGB vs BGR) is invisible on grey input
        // and this is the cheapest place to catch it.
        let black = detector_input(&solid(32, 32, [0, 0, 0, 255]), 960).unwrap();
        let white = detector_input(&solid(32, 32, [255, 255, 255, 255]), 960).unwrap();
        let plane = 32 * 32;
        for channel in 0..3 {
            let expected_black = (0.0 - MEAN[channel]) / STD[channel];
            let expected_white = (1.0 - MEAN[channel]) / STD[channel];
            assert!(
                (black.tensor[channel * plane] - expected_black).abs() < 1e-4,
                "channel {channel} black"
            );
            assert!(
                (white.tensor[channel * plane] - expected_white).abs() < 1e-4,
                "channel {channel} white"
            );
        }
    }

    #[test]
    fn the_channels_are_rgb_and_not_bgr() {
        // A pure-red frame: after normalisation channel 0 must be the bright one.
        let input = detector_input(&solid(32, 32, [255, 0, 0, 255]), 960).unwrap();
        let plane = 32 * 32;
        let red = input.tensor[0];
        let green = input.tensor[plane];
        let blue = input.tensor[2 * plane];
        assert!(red > green, "red {red} should exceed green {green}");
        assert!(red > blue, "red {red} should exceed blue {blue}");
    }

    #[test]
    fn an_empty_frame_yields_no_tensor_rather_than_an_empty_one() {
        let empty = RgbaBitmap::from_pixels(Size::new(0, 0), Vec::new());
        if let Some(bitmap) = empty {
            assert!(detector_input(&bitmap, 960).is_none());
        }
    }
}
