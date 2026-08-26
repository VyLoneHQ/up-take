//! Contrast-adaptive sharpening: the pass that makes an `Upscale` area worth
//! having (roadmap 1.29, ADR-0031).
//!
//! # What this is sharpening, and what it is not
//!
//! ADR-0031 asks the question before it answers anything else: **what is
//! UP-TAKE upscaling FROM?** The pixels under an area are already final,
//! native-resolution desktop pixels, so a filter that claims to recover
//! resolution is inventing it. This one does not claim that. It targets the
//! case the founder named, *a lower-resolution source that the application
//! underneath has already stretched*: a 720p video in a 1080p player, a stream
//! scaled by the browser. There the softening a cheap bilinear filter
//! introduced is real, local, and removable. On content that was never
//! upscaled it does very little, which is the correct behaviour rather than a
//! shortfall: there is nothing there to undo.
//!
//! # Why RCAS and not CAS
//!
//! AMD's FidelityFX offers both, and ADR-0031 names them together as
//! *"CAS/RCAS"*. **CAS fuses sharpening with a scaling pass; RCAS is the
//! sharpening on its own.** `Upscale` changes no geometry at all (that is the
//! whole of the founder's `MUST NOT zoom`), so the scaling half of CAS would be
//! an identity resample, and an identity resample is not free: it is a second
//! pass over every pixel that can only lose to doing nothing. RCAS is the half
//! that is actually being asked for.
//!
//! # Licence
//!
//! The algorithm is AMD's, published in FidelityFX FSR 1.0 under the **MIT**
//! licence, which is why ADR-0031 chose it: `MASTER-PLAN.md` `R-3` is the
//! hazard a model-based super-resolver would reintroduce, and a permissive
//! reference retires it. **No AMD code is vendored here.** This is an
//! independent implementation written from the published description of the
//! filter, in Rust, against this project's own pixel type, so the attribution
//! above is owed as credit for the design and not as a licence obligation.
//!
//! # Why the CPU, when the reference is a fragment shader
//!
//! Stated here because it is a choice and it is reversible, not because it is
//! obviously right.
//!
//! v1 is **passive** (ADR-0031's accepted scope): the pass runs on the events
//! that already re-take an area's pixels, not at framerate. Those re-takes are
//! measured at a median 236 ms of which 96% is the capture itself, so the
//! budget here is wide and throughput is not the constraint it would be for
//! roadmap 1.30's live loop.
//!
//! What the CPU buys against that budget is **testability**. The bytes are
//! already decoded on this side of the IPC boundary, so the filter is a pure
//! function of an [`RgbaBitmap`] and every property below is asserted by an
//! ordinary unit test. The GPU alternative is a WebGL canvas inside
//! `+page.svelte`, an 800-line `$derived` component with **no component test
//! harness at all** (UP-TAKE `I-23`), which is the surface this repository's
//! two worst recent defects both lived on.
//!
//! ## 🔴 MEASURED, and it decides roadmap 1.30 rather than merely informing it
//!
//! `examples/sharpen_cost.rs`, release build, this workstation, 2026-08-26.
//! Nine runs per size; both columns are medians, from **two separate runs of
//! the instrument** rather than one, because a single run of anything on this
//! machine is what `UT-F-39` and `ADR-0025` both warn about:
//!
//! ```text
//!                            run 1     run 2
//!      400x300   0.12 Mpx    4.6 ms    4.6 ms
//!      800x600   0.48 Mpx   18.5 ms   19.2 ms
//!     1280x720   0.92 Mpx   35.7 ms   37.3 ms
//!    1920x1080   2.07 Mpx   80.1 ms   84.3 ms
//!    2560x1440   3.69 Mpx  142.3 ms  143.9 ms
//!    3840x2160   8.29 Mpx  319.2 ms  320.6 ms
//! ```
//!
//! **Linear in pixels at 38.6 to 40.7 ms per megapixel**, across a 70-fold
//! range. The rate is what to carry forward, not any single cell: the two runs
//! differ by up to 5% at a size, so a figure quoted to three digits would be
//! quoting this machine's noise.
//!
//! **For v1 this is affordable and the decision above stands.** A typical
//! 800x600 area adds 18.5 ms to a 236 ms re-take: 8%, on a path that runs on a
//! move, a resize or a conversion rather than continuously.
//!
//! **For roadmap 1.30 it is not, and this is the finding.** That row's budget
//! is 55 ms a frame at 18 fps, and the warm readback already takes 8-11 ms of
//! it, leaving roughly 45 ms. At 38.6 ms per megapixel, the kinder end of the
//! two runs, that caps a live
//! sharpened area at about **1.15 Mpx, near 1280x900**, and the founder's own
//! use case is an area over a *1080p player*, which is 2.07 Mpx and needs
//! 80 ms for the filter alone. ⚠️ **So a live `Upscale` area at the size it
//! exists for cannot be done on the CPU**, and ADR-0031's expectation that
//! *"sharpening still ships cheaply"* holds for v1 and fails for the live leg.
//!
//! **This does not change v1 and it does change what 1.30 has to build.** That
//! row inherits a decision rather than a suggestion: **the shader is required,
//! not optional.** This module stays the specification of what it must
//! reproduce, which is what the tests below are for. UP-TAKE `I-306`.
//!
//! ⚠️ **One machine, synthetic content, and single-threaded.** The instrument
//! says so itself. A `rayon` row split would likely buy most of a core count
//! and is not attempted here, because v1 does not need it and 1.30 should not
//! be built on the CPU at all if the shader is the answer.
//!
//! # Colour space
//!
//! The filter runs directly on the stored sRGB-encoded values, normalised to
//! `0.0..=1.0`, which is what the reference does. It is deliberate rather than
//! an omission: RCAS's limiter is defined against the **displayed** headroom
//! above and below each pixel, and linearising first would move the limits away
//! from the values the user is actually looking at.

use crate::bitmap::{BYTES_PER_PIXEL, RgbaBitmap};

/// How far RCAS will let one pass push a pixel past its neighbours, as a
/// fraction of the local range.
///
/// `0.25 - 1/16`, the reference's own `FSR_RCAS_LIMIT`. This is the constant
/// that makes the filter *robust*: the lobe is clamped against the headroom the
/// ring actually has, so a sharpened edge cannot overshoot into the halo an
/// unsharp mask produces on exactly this content.
const LIMIT: f32 = 0.25 - 1.0 / 16.0;

/// Sharpening strength in stops, where `0.0` is the strongest the filter allows
/// and larger numbers are weaker.
///
/// **This number is the rig's to settle and nothing in a unit test can have an
/// opinion about it.** ADR-0031 says so in as many words: *"looks sharper is
/// not a bar. The honest v1 gate is the founder's eye at the rig."* It is a
/// constant so that changing it is one edit and cannot disagree with itself:
/// the same reason `Zoom::UPSCALE` was one, and the same expectation that a
/// hardware sitting moves it.
///
/// `0.2` is a mild default chosen so the first thing seen on hardware errs
/// toward too little rather than toward the crunchy over-sharpening that would
/// make the type look broken on its first showing.
pub const DEFAULT_SHARPNESS: f32 = 0.2;

/// Sharpens `bitmap` in place with RCAS at `sharpness` stops (see
/// [`DEFAULT_SHARPNESS`]).
///
/// **Alpha is copied through untouched.** Sharpening an alpha channel would
/// make an area's edges ring against whatever is behind them, and captures
/// arrive opaque in any case; a filter that quietly modified it would be
/// inventing translucency nothing asked for.
///
/// # Edges
///
/// The four-tap ring is **clamped** at the bitmap's border, so an edge pixel
/// sees itself where a neighbour would be. The alternative, leaving a
/// one-pixel frame unfiltered, puts a visible unsharpened line around every
/// area, which is the more noticeable of the two artefacts and the one a user
/// would report as a defect.
///
/// # What it declines to do, and why that is not a refusal
///
/// A bitmap under 3 pixels on either side returns untouched: below that every
/// tap clamps onto the centre, the ring collapses to `e`, and the arithmetic
/// reduces to the identity. A non-finite `sharpness` returns untouched too, and
/// that one *is* a refusal: a NaN scale would propagate into every pixel and
/// produce a transparent black rectangle rather than an obviously wrong image.
pub fn rcas(bitmap: &mut RgbaBitmap, sharpness: f32) {
    let (width, height) = (bitmap.width() as usize, bitmap.height() as usize);
    if width < 3 || height < 3 || !sharpness.is_finite() {
        return;
    }
    // `exp2(-sharpness)`: the reference's stops-to-scale conversion.
    let scale = (-sharpness).exp2();
    // Read from a copy. RCAS is a neighbourhood filter, so writing into the
    // buffer it is reading would feed already-sharpened pixels back in as
    // neighbours, and the result would depend on the traversal order and would not
    // be RCAS at all.
    let source = bitmap.pixels().to_vec();
    let output = bitmap.pixels_mut();
    for y in 0..height {
        for x in 0..width {
            let centre = (y * width + x) * BYTES_PER_PIXEL;
            let ring = [
                tap(&source, width, x, y.saturating_sub(1)),
                tap(&source, width, x, (y + 1).min(height - 1)),
                tap(&source, width, x.saturating_sub(1), y),
                tap(&source, width, (x + 1).min(width - 1), y),
            ];
            let lobe = lobe(&ring) * scale;
            for channel in 0..3 {
                let e = f32::from(source[centre + channel]) / 255.0;
                let sum: f32 = ring.iter().map(|tap| tap[channel]).sum();
                // `4 * lobe + 1` is at worst `1 - 4 * LIMIT` = 0.25, because
                // `lobe` is clamped into `-LIMIT..=0` and `scale` is positive,
                // so this cannot divide by zero and cannot flip the sign.
                let resolved = sum.mul_add(lobe, e) / lobe.mul_add(4.0, 1.0);
                // The limiter is what should keep this in range, and the clamp
                // is insurance against float error at the ends rather than a
                // second opinion about the filter: without it a value a
                // ten-thousandth over 1.0 saturates the cast, and one a
                // ten-thousandth under 0.0 shows as a black speck on a white
                // edge.
                output[centre + channel] = (resolved.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
}

/// One ring tap as normalised RGB, dropping alpha.
fn tap(source: &[u8], width: usize, x: usize, y: usize) -> [f32; 3] {
    let at = (y * width + x) * BYTES_PER_PIXEL;
    [
        f32::from(source[at]) / 255.0,
        f32::from(source[at + 1]) / 255.0,
        f32::from(source[at + 2]) / 255.0,
    ]
}

/// The RCAS lobe for one pixel: the weight its ring gets, always at or below
/// zero and never past [`LIMIT`].
///
/// # Why the *largest* of the three channels is the most restrictive one
///
/// Every per-channel lobe is at or below zero: `-hit_min` because `hit_min` is
/// a ratio of non-negative values, and `hit_max` because its numerator is
/// non-negative while its denominator cannot be positive. So a lobe nearer zero
/// is *less* sharpening, and taking the maximum picks the channel with the
/// least headroom left. That is what stops a sharpened edge shifting hue: a
/// channel about to clip holds the other two back rather than being clipped on
/// its own.
///
/// ⚠️ **The sign here is the whole filter and it is easy to get backwards.**
/// This function seeded its running maximum at `0.0` and then negated the
/// result in its first draft, which (since every per-channel lobe is at or
/// below zero) pinned the maximum at exactly `0.0` for every pixel of every
/// image, and a lobe of zero is the identity. It would have shipped a
/// sharpening pass that provably sharpens nothing, with a flat-image test
/// passing, because a flat image is the identity at *every* lobe.
/// `rcas_sharpens_a_mid_contrast_edge` is the test that fails on it.
fn lobe(ring: &[[f32; 3]; 4]) -> f32 {
    // Seeded below every reachable value rather than at zero, for the reason
    // above.
    let mut least_headroom = f32::NEG_INFINITY;
    for channel in 0..3 {
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for tap in ring {
            min = min.min(tap[channel]);
            max = max.max(tap[channel]);
        }
        // **Both divisions are 0/0 exactly where they are undefined, and zero
        // is the limit rather than an arbitrary pick.** `4 * max == 0` forces
        // `max == 0`, and `min <= max` with both non-negative forces `min == 0`
        // with it, so the numerator is zero. `4 * min - 4 == 0` forces
        // `min == 1`, and `max >= min` with both at most 1 forces `max == 1`,
        // so `1 - max` is zero with it. Both cases are a ring with no headroom
        // in that direction, and a lobe of zero is what "do not sharpen here"
        // means.
        let hit_min = ratio(min, 4.0 * max);
        let hit_max = ratio(1.0 - max, min.mul_add(4.0, -4.0));
        least_headroom = least_headroom.max((-hit_min).max(hit_max));
    }
    // Clamped to at most zero: RCAS only ever gives the ring negative weight,
    // and a positive lobe would blur rather than sharpen. [`LIMIT`] bounds the
    // other end, which is the anti-ringing guarantee.
    least_headroom.clamp(-LIMIT, 0.0)
}

/// `numerator / denominator`, or zero where the denominator is.
fn ratio(numerator: f32, denominator: f32) -> f32 {
    if denominator == 0.0 {
        0.0
    } else {
        numerator / denominator
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]
mod tests {
    use super::*;
    use crate::geometry::Size;

    /// A bitmap of `width` x `height` where every pixel is `rgb` at full alpha.
    fn flat(width: u32, height: u32, rgb: [u8; 3]) -> RgbaBitmap {
        let pixel = [rgb[0], rgb[1], rgb[2], 255];
        let pixels = pixel
            .iter()
            .copied()
            .cycle()
            .take((width * height) as usize * BYTES_PER_PIXEL)
            .collect();
        RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap()
    }

    /// The red channel of the pixel at `(x, y)`. The three colour channels
    /// carry the same value in every fixture here, so one of them is the whole
    /// story.
    fn red(bitmap: &RgbaBitmap, x: u32, y: u32) -> u8 {
        let at = ((y * bitmap.width() + x) as usize) * BYTES_PER_PIXEL;
        bitmap.pixels()[at]
    }

    fn alpha(bitmap: &RgbaBitmap, x: u32, y: u32) -> u8 {
        let at = ((y * bitmap.width() + x) as usize) * BYTES_PER_PIXEL;
        bitmap.pixels()[at + 3]
    }

    /// A vertical step edge down the middle: `dark` on the left half, `light`
    /// on the right.
    fn step_edge(width: u32, height: u32, dark: u8, light: u8) -> RgbaBitmap {
        let mut bitmap = flat(width, height, [dark; 3]);
        let stride = bitmap.width() as usize * BYTES_PER_PIXEL;
        let pixels = bitmap.pixels_mut();
        for y in 0..height as usize {
            for x in (width / 2) as usize..width as usize {
                let at = y * stride + x * BYTES_PER_PIXEL;
                pixels[at..at + 3].fill(light);
            }
        }
        bitmap
    }

    /// Left-to-right mirror, used to change the traversal order relative to the
    /// content without changing the content.
    fn mirror(bitmap: &RgbaBitmap) -> RgbaBitmap {
        let (width, height) = (bitmap.width() as usize, bitmap.height() as usize);
        let mut pixels = vec![0u8; width * height * BYTES_PER_PIXEL];
        for y in 0..height {
            for x in 0..width {
                let from = (y * width + x) * BYTES_PER_PIXEL;
                let to = (y * width + (width - 1 - x)) * BYTES_PER_PIXEL;
                pixels[to..to + BYTES_PER_PIXEL]
                    .copy_from_slice(&bitmap.pixels()[from..from + BYTES_PER_PIXEL]);
            }
        }
        RgbaBitmap::from_pixels(bitmap.size(), pixels).unwrap()
    }

    /// Nothing to sharpen, so nothing changes. Holds at **every** lobe, which
    /// is why this test alone cannot prove the filter works. See
    /// [`rcas_sharpens_a_mid_contrast_edge`].
    #[test]
    fn rcas_leaves_a_flat_image_alone() {
        let mut bitmap = flat(8, 8, [128, 64, 200]);
        let before = bitmap.clone();
        rcas(&mut bitmap, DEFAULT_SHARPNESS);
        assert_eq!(bitmap, before);
    }

    /// **The test the sign error would have failed.** A mid-contrast edge has
    /// headroom on both sides, so RCAS must push the light side lighter and the
    /// dark side darker. A lobe pinned at zero, which is the first draft's
    /// defect, returns the image unchanged and fails here.
    #[test]
    fn rcas_sharpens_a_mid_contrast_edge() {
        let mut bitmap = step_edge(8, 8, 102, 153);
        rcas(&mut bitmap, DEFAULT_SHARPNESS);
        let (dark_side, light_side) = (red(&bitmap, 3, 4), red(&bitmap, 4, 4));
        assert!(
            dark_side < 102,
            "the dark side of the edge must get darker, got {dark_side}"
        );
        assert!(
            light_side > 153,
            "the light side of the edge must get lighter, got {light_side}"
        );
    }

    /// Away from the edge the image is locally flat, so it is untouched even
    /// though the same call sharpened the edge. This is what *contrast
    /// adaptive* means, and it is the property separating RCAS from a global
    /// unsharp mask.
    #[test]
    fn rcas_leaves_the_flat_regions_of_an_edged_image_alone() {
        let mut bitmap = step_edge(12, 12, 102, 153);
        rcas(&mut bitmap, DEFAULT_SHARPNESS);
        assert_eq!(red(&bitmap, 1, 6), 102, "deep inside the dark half");
        assert_eq!(red(&bitmap, 10, 6), 153, "deep inside the light half");
    }

    /// A black-to-white edge has **no headroom in either direction**, so the
    /// limiter takes the lobe to zero and the filter declines. That is the
    /// anti-ringing guarantee, and it is the reason the sharpening test above
    /// uses a mid-contrast edge rather than the obvious one.
    #[test]
    fn rcas_declines_an_edge_with_no_headroom_left() {
        let mut bitmap = step_edge(8, 8, 0, 255);
        let before = bitmap.clone();
        rcas(&mut bitmap, DEFAULT_SHARPNESS);
        assert_eq!(bitmap, before);
    }

    /// Alpha is carried through untouched, even where the colour channels move.
    #[test]
    fn rcas_does_not_touch_alpha() {
        let mut bitmap = step_edge(8, 8, 102, 153);
        let stride = bitmap.width() as usize * BYTES_PER_PIXEL;
        // A non-uniform alpha, so a filter that ran over it would be visible
        // rather than idempotent by luck.
        for y in 0..8usize {
            for x in 0..8usize {
                bitmap.pixels_mut()[y * stride + x * BYTES_PER_PIXEL + 3] =
                    u8::try_from(x * 8 + y).unwrap();
            }
        }
        let before = bitmap.clone();
        rcas(&mut bitmap, DEFAULT_SHARPNESS);
        assert_ne!(bitmap, before, "the fixture must actually be sharpened");
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(
                    alpha(&bitmap, x, y),
                    alpha(&before, x, y),
                    "alpha moved at ({x}, {y})"
                );
            }
        }
    }

    /// Larger `sharpness` is weaker, because it is measured in stops. Asserted
    /// because the sign of that relationship is the one thing a caller reading
    /// the name would guess wrong.
    #[test]
    fn a_larger_sharpness_in_stops_sharpens_less() {
        let mut strong = step_edge(8, 8, 102, 153);
        let mut weak = strong.clone();
        rcas(&mut strong, 0.0);
        rcas(&mut weak, 2.0);
        assert!(
            red(&strong, 4, 4) > red(&weak, 4, 4),
            "0 stops must sharpen harder than 2: got {} against {}",
            red(&strong, 4, 4),
            red(&weak, 4, 4)
        );
        assert!(
            red(&weak, 4, 4) > 153,
            "2 stops must still sharpen, got {}",
            red(&weak, 4, 4)
        );
    }

    /// The filter reads a snapshot, so a pixel's neighbours are its ORIGINAL
    /// neighbours. Filtering in place would make the result depend on traversal
    /// order; this asserts it does not. Mirroring the image, filtering, and
    /// mirroring back must give the same answer as filtering directly.
    #[test]
    fn rcas_does_not_feed_its_own_output_back_in() {
        let mut forward = step_edge(9, 9, 90, 160);
        let mut mirrored = mirror(&forward);
        rcas(&mut forward, DEFAULT_SHARPNESS);
        rcas(&mut mirrored, DEFAULT_SHARPNESS);
        assert_eq!(mirror(&mirrored), forward);
    }

    /// Under 3 pixels a side the ring collapses onto the centre and the
    /// arithmetic is the identity, so the early return costs nothing. Asserted
    /// so that removing the guard is a behaviour change somebody has to justify
    /// rather than a silent index panic.
    #[test]
    fn rcas_returns_a_bitmap_too_small_to_filter_untouched() {
        for (width, height) in [(1, 1), (2, 2), (2, 8), (8, 2)] {
            let mut bitmap = step_edge(width, height, 102, 153);
            let before = bitmap.clone();
            rcas(&mut bitmap, DEFAULT_SHARPNESS);
            assert_eq!(bitmap, before, "{width}x{height} must be untouched");
        }
    }

    /// A NaN or infinite `sharpness` returns the image untouched rather than
    /// writing NaN into every pixel, which the cast would turn into a
    /// transparent black rectangle: a plausible-looking failure rather than an
    /// obvious one.
    #[test]
    fn rcas_refuses_a_non_finite_sharpness() {
        for sharpness in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut bitmap = step_edge(8, 8, 102, 153);
            let before = bitmap.clone();
            rcas(&mut bitmap, sharpness);
            assert_eq!(bitmap, before, "{sharpness} must be refused");
        }
    }

    /// Every edge pixel is filtered, because the ring clamps at the border. The
    /// alternative leaves an unsharpened one-pixel frame around every area,
    /// which is what a user would report as the defect.
    #[test]
    fn rcas_filters_the_border_row_and_column() {
        let mut bitmap = step_edge(8, 8, 102, 153);
        rcas(&mut bitmap, DEFAULT_SHARPNESS);
        for (x, y) in [(3, 0), (4, 0), (3, 7), (4, 7)] {
            let unfiltered = if x < 4 { 102 } else { 153 };
            assert_ne!(
                red(&bitmap, x, y),
                unfiltered,
                "border pixel ({x}, {y}) was skipped"
            );
        }
    }

    /// The limiter is what keeps the result inside the byte range, and the
    /// clamp in [`rcas`] is insurance rather than the mechanism. Driven over a
    /// wide spread of edge contrasts, so a contrast that overshoots is found
    /// here rather than as a black speck on a rig.
    #[test]
    fn rcas_never_overshoots_the_representable_range() {
        for dark in (0..=240u32).step_by(15) {
            for light in (dark..=255u32).step_by(15) {
                let mut bitmap = step_edge(
                    8,
                    8,
                    u8::try_from(dark).unwrap(),
                    u8::try_from(light).unwrap(),
                );
                rcas(&mut bitmap, 0.0);
                // The bound RCAS promises: one pass moves a pixel by at most
                // the local range, so nothing can land outside the fixture's
                // own two values widened by that range.
                let range = light - dark;
                let floor = dark.saturating_sub(range);
                let ceiling = (light + range).min(255);
                for y in 0..8 {
                    for x in 0..8 {
                        let value = u32::from(red(&bitmap, x, y));
                        assert!(
                            (floor..=ceiling).contains(&value),
                            "{dark}->{light} produced {value} at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }
}
