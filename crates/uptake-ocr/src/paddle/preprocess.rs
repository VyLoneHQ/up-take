//! Turning an [`RgbaBitmap`] into the tensor the detector expects.
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

/// A frame resized and normalised for the detector.
///
/// ⚠️ **This deliberately carries NO scale factors, and it used to.** It held a
/// `scale_x`/`scale_y` pair computed as `source / resized`, documented at length
/// as the way boxes were mapped home. **Nothing in production ever read them**:
/// the engine computed its own factors from the *model's output* dimensions,
/// which is the correct source, because the probability map is not required to
/// be the same size as the tensor that produced it. Two rules for one quantity,
/// one of them tested and dead, the other used and untested -- found by the
/// independent review of `PR #76` and removed rather than documented.
///
/// The one rule now lives in [`DetectorInput::scale_to_source`], which the
/// engine calls and these tests cover. It is a method since `I-440`, because it
/// needs the content rectangle this value carries.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorInput {
    /// Normalised pixel data, NCHW with N = 1 and C = 3.
    pub tensor: Vec<f32>,
    /// The tensor's width. A multiple of [`SIDE_MULTIPLE`].
    pub width: u32,
    /// The tensor's height. A multiple of [`SIDE_MULTIPLE`].
    pub height: u32,
    /// How much of the tensor's width, from the left, holds the frame. The
    /// rest is padding ([`fit`]).
    pub content_width: u32,
    /// How much of the tensor's height, from the top, holds the frame.
    pub content_height: u32,
}

impl DetectorInput {
    /// The tensor's shape, as `ort` wants it: `[batch, channels, height, width]`.
    #[must_use]
    pub fn shape(&self) -> [usize; 4] {
        [1, 3, self.height as usize, self.width as usize]
    }

    /// The factors that map a coordinate in the detector's output map back to
    /// the source frame.
    ///
    /// **Two factors, not one.** The frame is resized by one ratio, but each side
    /// is rounded to whole pixels on its own, so the two ratios can differ by a
    /// fraction of a pixel per side; assuming one would let boxes drift from
    /// their text down a tall frame, and `geometry.rs` calls coordinate maths
    /// this project's number one bug source. Until `I-440` they differed by up
    /// to a third, because each side was stretched to a multiple of 32.
    ///
    /// **The map covers the whole tensor and the frame only its content
    /// rectangle** (`I-440`), so a map pixel is `padded / map` tensor pixels and
    /// a tensor pixel is `source / content` frame pixels. A box the detector
    /// finds in the padding maps past the frame's edge, where the caller clamps
    /// it.
    ///
    /// Takes the **map's** dimensions rather than assuming the tensor's,
    /// because the caller reads them off the model's actual output shape. For
    /// PP-OCRv4's detector the two agree, and so do they for PP-OCRv6_small's (a
    /// 320x480 input gives a 320x480 map, measured 2026-09-15), but nothing
    /// enforces that and a model with a different stride would silently place
    /// every box wrong.
    ///
    /// A zero map dimension yields a factor of `0.0` rather than an infinity, so
    /// a degenerate output collapses boxes to a point the size filter drops
    /// instead of poisoning them with `NaN`.
    #[must_use]
    pub fn scale_to_source(
        &self,
        source_width: u32,
        source_height: u32,
        map_width: usize,
        map_height: usize,
    ) -> (f32, f32) {
        let factor = |source: u32, content: u32, padded: u32, map: usize| -> f32 {
            if map == 0 || content == 0 {
                0.0
            } else {
                (source as f32 * padded as f32) / (map as f32 * content as f32)
            }
        };
        (
            factor(source_width, self.content_width, self.width, map_width),
            factor(source_height, self.content_height, self.height, map_height),
        )
    }
}

/// Where a frame goes in the detector's tensor: the rectangle it is resized to,
/// and the tensor around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    /// The frame's width once resized.
    pub content_width: u32,
    /// The frame's height once resized.
    pub content_height: u32,
    /// The tensor's width: the content's, rounded UP to a multiple of
    /// [`SIDE_MULTIPLE`].
    pub width: u32,
    /// The tensor's height, rounded up the same way.
    pub height: u32,
}

/// Chooses where a frame goes in the detector's tensor.
///
/// 1. If the longer side exceeds `limit_side_len`, scale **both** sides by one
///    ratio so it fits. Otherwise the frame keeps its size.
/// 2. Pad each side **up** to the next multiple of [`SIDE_MULTIPLE`]. The frame
///    sits at the top left and is never stretched.
///
/// # Why padding, and what it replaced (`I-440`)
///
/// Step 2 used to round each side to the NEAREST multiple and stretch the frame
/// to fill it, as PaddleOCR's own preprocessing does. That changes the aspect
/// ratio by up to a third on a short side, and on a wide, short frame it is a
/// cliff: a 110 px tall strip scaled to a 960 px long side is 44 px tall, which
/// rounded to 32, squeezing 13 px text to about 4 px, and the detector read
/// nothing. Measured on the founder's 2385 x 110 editor strip: every width up to
/// 2200 px read about 130 words and every width from 2201 read none, exactly
/// where `110 x 960 / width` falls below 48. Padding keeps the text at the size
/// the ratio gives it, whatever the frame's shape.
///
/// ⚠️ **What padding does not fix is the ratio itself.** Step 1 still shrinks a
/// frame's longer side to `limit_side_len`, so on a wide frame the text shrinks
/// with it: his 2385 px strip puts 13 px text at about 5 px, and measured after
/// this change it reads 6 words, where its left 2200 px read about 130 before
/// it. Capping the pixel count instead of the longer side read 138 words there,
/// and costs more time on every large frame; that trade is recorded in
/// `I-440`'s follow-up and not made here.
#[must_use]
pub fn fit(width: u32, height: u32, limit_side_len: u32) -> Fit {
    let longer = width.max(height);
    let (content_width, content_height) = if longer > limit_side_len {
        let ratio = f64::from(limit_side_len) / f64::from(longer);
        (scaled(width, ratio), scaled(height, ratio))
    } else {
        (width.max(1), height.max(1))
    };
    Fit {
        content_width,
        content_height,
        width: round_up_to_multiple(content_width),
        height: round_up_to_multiple(content_height),
    }
}

/// `side` scaled by `ratio`, to the nearest whole pixel and never zero.
fn scaled(side: u32, ratio: f64) -> u32 {
    let value = (f64::from(side) * ratio).round().max(1.0);
    // `ratio` is below 1 whenever this is called, so the value fits in a u32.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to at least 1 and scaled down from a u32, so it is a positive u32"
    )]
    let value = value as u32;
    value
}

/// Rounds a side up to the next multiple of [`SIDE_MULTIPLE`], never zero.
const fn round_up_to_multiple(side: u32) -> u32 {
    let multiples = side.div_ceil(SIDE_MULTIPLE);
    let multiples = if multiples == 0 { 1 } else { multiples };
    multiples.saturating_mul(SIDE_MULTIPLE)
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
    let Fit {
        content_width,
        content_height,
        width,
        height,
    } = fit(source_width, source_height, limit_side_len);

    // Frame pixels per content pixel. One ratio scaled both sides, so these
    // differ only by each side's rounding to whole pixels.
    let scale_x = source_width as f32 / content_width as f32;
    let scale_y = source_height as f32 / content_height as f32;

    let plane = width as usize * height as usize;
    // The padding is left at 0.0, which after normalisation is the ImageNet
    // mean colour: the value that carries the least signal into a network
    // trained on mean-subtracted input (`I-440`).
    let mut tensor = vec![0.0_f32; plane * 3];
    for y in 0..content_height {
        for x in 0..content_width {
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
        content_width,
        content_height,
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

    /// A fit of `content` inside a tensor of `padded`, both `(width, height)`.
    fn fitted(content: (u32, u32), padded: (u32, u32)) -> Fit {
        Fit {
            content_width: content.0,
            content_height: content.1,
            width: padded.0,
            height: padded.1,
        }
    }

    #[test]
    fn a_small_frame_keeps_its_size_and_is_padded_to_one_multiple() {
        assert_eq!(fit(10, 4, 960), fitted((10, 4), (32, 32)));
    }

    #[test]
    fn sides_within_the_limit_keep_their_size_and_are_padded_up() {
        // 100 x 40 is not stretched to 96 x 32 any more (I-440): it stays
        // 100 x 40 inside a 128 x 64 tensor.
        assert_eq!(fit(100, 40, 960), fitted((100, 40), (128, 64)));
        // A side already on the multiple gains no padding.
        assert_eq!(fit(64, 96, 960), fitted((64, 96), (64, 96)));
    }

    #[test]
    fn a_frame_over_the_limit_is_scaled_down_by_one_ratio() {
        // 1920 x 1080, limit 960: ratio 0.5 gives 960 x 540, padded to 544.
        assert_eq!(fit(1920, 1080, 960), fitted((960, 540), (960, 544)));
    }

    #[test]
    fn a_wide_short_strip_keeps_its_text_height() {
        // I-440's own frame, the founder's 2385 x 110 editor strip. Ratio
        // 960 / 2385 makes it 44.28 px tall; the old rule rounded that to 32
        // and squeezed the text by a quarter, so the detector read nothing.
        assert_eq!(fit(2385, 110, 960), fitted((960, 44), (960, 64)));
        // Either side of the measured cliff now fits the same way.
        assert_eq!(fit(2200, 110, 960).content_height, 48);
        assert_eq!(fit(2201, 110, 960).content_height, 48);
    }

    #[test]
    fn the_content_keeps_the_frames_aspect_ratio_and_the_tensor_holds_it() {
        for (width, height) in [
            (1, 1),
            (33, 65),
            (1920, 1080),
            (3840, 2160),
            (7, 4000),
            (2385, 110),
            (4000, 7),
        ] {
            let fit = fit(width, height, 960);
            assert_eq!(fit.width % SIDE_MULTIPLE, 0, "{width}x{height}");
            assert_eq!(fit.height % SIDE_MULTIPLE, 0, "{width}x{height}");
            assert!(fit.width >= fit.content_width && fit.height >= fit.content_height);
            // Less than one multiple of padding on each side.
            assert!(fit.width - fit.content_width < SIDE_MULTIPLE);
            assert!(fit.height - fit.content_height < SIDE_MULTIPLE);
            // One ratio for both sides: each content side is the frame's side
            // times that ratio, to within the half pixel of its own rounding
            // (or the one-pixel floor).
            let ratio =
                f64::from(fit.content_width.max(fit.content_height)) / f64::from(width.max(height));
            for (side, content) in [(width, fit.content_width), (height, fit.content_height)] {
                let exact = f64::from(side) * ratio;
                assert!(
                    (f64::from(content) - exact).abs() <= 0.5 || content == 1,
                    "{width}x{height}: {content} against {exact}"
                );
            }
        }
    }

    #[test]
    fn the_frame_is_sampled_unstretched_and_the_padding_is_the_mean() {
        // A frame within the limit lands pixel for pixel: column x of the
        // tensor is column x of the frame. Before I-440 a 100 px wide frame was
        // stretched onto 96 columns, so this would read a different column.
        let (width, height) = (100_u32, 40_u32);
        let pixels = (0..height)
            .flat_map(|_| (0..width).flat_map(|x| [x as u8 * 2, 0, 0, 255]))
            .collect();
        let frame = RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap();
        let input = detector_input(&frame, 960).unwrap();
        assert_eq!((input.width, input.height), (128, 64));
        assert_eq!((input.content_width, input.content_height), (100, 40));
        let red = |x: usize, y: usize| input.tensor[y * input.width as usize + x];
        for x in [0_usize, 1, 50, 99] {
            let expected = (f32::from(x as u8 * 2) / 255.0 - MEAN[0]) / STD[0];
            assert!((red(x, 0) - expected).abs() < 1e-4, "column {x}");
            assert!((red(x, 39) - expected).abs() < 1e-4, "column {x}, last row");
        }
        // Right of the content and below it: the mean, in every channel.
        let plane = input.width as usize * input.height as usize;
        for (x, y) in [(100_usize, 0_usize), (127, 39), (0, 40), (127, 63)] {
            for channel in 0..3 {
                let value = input.tensor[channel * plane + y * input.width as usize + x];
                assert!(value.abs() < f32::EPSILON, "({x}, {y}) channel {channel}");
            }
        }
    }

    #[test]
    fn scale_to_source_maps_the_content_edge_to_the_frame_edge() {
        // The strip from I-440, through a map the size of the tensor: the
        // content's far corner is the frame's far corner, and the padding maps
        // past it, where the caller clamps.
        let input = detector_input(&solid(2385, 110, [0, 0, 0, 255]), 960).unwrap();
        assert_eq!((input.width, input.height), (960, 64));
        let (scale_x, scale_y) = input.scale_to_source(2385, 110, 960, 64);
        assert!((960.0 * scale_x - 2385.0).abs() < 1e-3, "x {scale_x}");
        assert!((44.0 * scale_y - 110.0).abs() < 1e-3, "y {scale_y}");
        assert!(64.0 * scale_y > 110.0, "the padding maps past the frame");
    }

    #[test]
    fn scale_to_source_maps_a_map_coordinate_home() {
        // An 80 x 60 frame is 80 x 60 content in a 96 x 64 tensor; a 48 x 32
        // map is half the tensor, so a map pixel is two tensor pixels.
        let input = detector_input(&solid(80, 60, [0, 0, 0, 255]), 960).unwrap();
        let (scale_x, scale_y) = input.scale_to_source(80, 60, 48, 32);
        assert!((scale_x - 2.0).abs() < 1e-6, "scale_x was {scale_x}");
        assert!((scale_y - 2.0).abs() < 1e-6, "scale_y was {scale_y}");
    }

    #[test]
    fn scale_to_source_survives_a_degenerate_map_without_infinities() {
        // A model that returned a zero dimension must not poison every box with
        // an infinity or a NaN -- the size filter's comparisons would then be
        // silently false rather than rejecting.
        let input = detector_input(&solid(80, 60, [0, 0, 0, 255]), 960).unwrap();
        let (scale_x, scale_y) = input.scale_to_source(80, 60, 0, 0);
        assert!(scale_x.is_finite() && scale_y.is_finite());
        assert!((scale_x - 0.0).abs() < f32::EPSILON);
        assert!((scale_y - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scale_to_source_reads_the_map_and_not_the_resize() {
        // The regression this function exists to prevent: if the model emits a
        // map at HALF the input resolution, the factors must double. Computing
        // from the tensor instead would return 1.0 and place every box at half
        // its true distance from the origin.
        let input = detector_input(&solid(960, 960, [0, 0, 0, 255]), 960).unwrap();
        let (from_half_map, _) = input.scale_to_source(960, 960, 480, 480);
        let (from_full_map, _) = input.scale_to_source(960, 960, 960, 960);
        assert!((from_half_map - 2.0).abs() < 1e-6, "was {from_half_map}");
        assert!((from_full_map - 1.0).abs() < 1e-6, "was {from_full_map}");
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
