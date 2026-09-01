//! Cropping a detected box, and decoding what the recogniser says about it.
//!
//! Two pure stages sit either side of the recognition model, and both are here
//! because both are testable without it:
//!
//! - [`rectify`] lifts a rotated quad out of the frame into an upright,
//!   fixed-height strip -- the only shape the recogniser accepts.
//! - [`ctc_decode`] turns the model's per-timestep class scores back into
//!   characters.
//!
//! **Nothing here loads a model.** `ctc_decode` takes a slice of logits, so its
//! tests write the logits by hand.

use uptake_core::bitmap::{BYTES_PER_PIXEL, RgbaBitmap};

use super::quad::{PointF, Quad};

/// The height every recogniser crop is scaled to. PP-OCRv4's `rec_image_shape`.
///
/// The model's convolutions collapse the vertical axis entirely, so this is
/// fixed by the architecture rather than chosen: a crop of a different height
/// produces a feature map of the wrong rank and the session refuses it.
pub const REC_HEIGHT: u32 = 48;

/// Widest crop the recogniser is given, in pixels.
///
/// A very long line is scaled down to fit rather than truncated: losing the
/// right-hand half of a sentence silently is worse than recognising all of it
/// slightly smaller.
pub const REC_MAX_WIDTH: u32 = 640;

/// One recognised line and how sure the model was.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedText {
    /// The characters, already collapsed and blank-stripped.
    pub text: String,
    /// Mean probability across the timesteps that contributed a character.
    ///
    /// `0.0` for an empty decode. This is the recogniser's own confidence and is
    /// distinct from the detector's box score: a crisp box around a smudge
    /// scores high on detection and low here.
    pub confidence: f32,
}

/// An upright greyscale-normalised strip, ready for the recogniser.
#[derive(Debug, Clone, PartialEq)]
pub struct RecogniserInput {
    /// Normalised pixels, NCHW with N = 1, C = 3, H = [`REC_HEIGHT`].
    pub tensor: Vec<f32>,
    /// The strip's width in pixels.
    pub width: u32,
}

impl RecogniserInput {
    /// The tensor's shape: `[batch, channels, height, width]`.
    #[must_use]
    pub fn shape(&self) -> [usize; 4] {
        [1, 3, REC_HEIGHT as usize, self.width as usize]
    }
}

/// Lifts a quad out of the frame into an upright strip of [`REC_HEIGHT`] pixels.
///
/// # Why a perspective sample and not a crop-then-rotate
///
/// The detector's quads are rotated rectangles, and a screen-captured line of
/// text can genuinely be rotated -- a tilted photo pasted into a document, a
/// rotated table header. Cropping the axis-aligned bounding box instead would
/// include the neighbouring lines' pixels in the corners, and the recogniser
/// reads those as characters. Sampling along the quad's own edges takes exactly
/// the pixels the box claims and nothing else.
///
/// The output width preserves the box's aspect ratio, capped at
/// [`REC_MAX_WIDTH`] and floored at 1 so a degenerate box still produces a
/// tensor the session will accept rather than a zero-width one it rejects.
///
/// Normalisation is PP-OCR's recogniser convention, `(pixel/255 - 0.5) / 0.5`,
/// which is **not** the ImageNet normalisation the detector uses. Two models,
/// two conventions; using one for both is a silent accuracy loss rather than an
/// error, which is why the constants live next to the stage that needs them.
#[must_use]
pub fn rectify(bitmap: &RgbaBitmap, quad: &Quad) -> Option<RecogniserInput> {
    if bitmap.width() == 0 || bitmap.height() == 0 {
        return None;
    }
    let (long_side, short_side) = quad.side_lengths();
    if !long_side.is_finite() || !short_side.is_finite() || short_side <= 0.0 {
        return None;
    }

    let aspect = long_side / short_side;
    let width = ((REC_HEIGHT as f32 * aspect).round().max(1.0) as u32).min(REC_MAX_WIDTH);

    // The quad's corners, clockwise from top-left. Interpolating along the top
    // and bottom edges and then between them is a bilinear map of the unit
    // square onto the quad -- exact for the affine case, and the standard
    // approximation for the projective one at these sizes.
    let [top_left, top_right, bottom_right, bottom_left] = quad.corners;

    let plane = width as usize * REC_HEIGHT as usize;
    let mut tensor = vec![0.0_f32; plane * 3];
    for y in 0..REC_HEIGHT {
        let v = (y as f32 + 0.5) / REC_HEIGHT as f32;
        for x in 0..width {
            let u = (x as f32 + 0.5) / width as f32;
            let top = PointF::new(
                top_left.x.mul_add(1.0 - u, top_right.x * u),
                top_left.y.mul_add(1.0 - u, top_right.y * u),
            );
            let bottom = PointF::new(
                bottom_left.x.mul_add(1.0 - u, bottom_right.x * u),
                bottom_left.y.mul_add(1.0 - u, bottom_right.y * u),
            );
            let source_x = top.x.mul_add(1.0 - v, bottom.x * v);
            let source_y = top.y.mul_add(1.0 - v, bottom.y * v);

            let destination = y as usize * width as usize + x as usize;
            for channel in 0..3 {
                let raw = sample(bitmap, source_x, source_y, channel) / 255.0;
                tensor[channel * plane + destination] = (raw - 0.5) / 0.5;
            }
        }
    }

    Some(RecogniserInput { tensor, width })
}

/// Nearest-neighbour sample of one channel, clamped at the edges.
///
/// Nearest rather than bilinear here, deliberately: the crop is being *enlarged*
/// to 48 px from text that is often 10-14 px tall, and bilinear smoothing at
/// that ratio blurs the thin strokes the recogniser keys on. PP-OCR's own
/// preprocessing resizes with a plain interpolation for the same reason.
fn sample(bitmap: &RgbaBitmap, x: f32, y: f32, channel: usize) -> f32 {
    let width = bitmap.width();
    let height = bitmap.height();
    if width == 0 || height == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let px = x.round().clamp(0.0, (width - 1) as f32) as u32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let py = y.round().clamp(0.0, (height - 1) as f32) as u32;
    let index = (py as usize * width as usize + px as usize) * BYTES_PER_PIXEL + channel;
    bitmap
        .pixels()
        .get(index)
        .map_or(0.0, |&value| f32::from(value))
}

/// The character set the recogniser's class indices point into.
///
/// # Index 0 is the CTC blank and is not in this list
///
/// PP-OCR's dictionary files hold only the real characters; the runtime prepends
/// the blank. So class `0` is blank, class `n` is `characters[n - 1]`, and an
/// off-by-one here shifts **every** character by one position in the
/// dictionary -- which produces fluent-looking, entirely wrong text rather than
/// an obvious failure. That is why the offset is encoded in one method
/// ([`CharacterDictionary::character_for_class`]) rather than at each call site.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CharacterDictionary {
    characters: Vec<String>,
}

impl CharacterDictionary {
    /// Builds a dictionary from the lines of a PP-OCR `*_dict.txt`, **without**
    /// PaddleOCR's appended space.
    ///
    /// Use [`CharacterDictionary::from_ppocr_dictionary`] for a real PP-OCR
    /// model. This constructor is the literal reading -- one character per line,
    /// nothing added -- and it exists for dictionaries that genuinely carry every
    /// character they describe, and for tests that state a class list outright.
    ///
    /// Trailing carriage returns are stripped so a CRLF file behaves the same as
    /// an LF one, and a single trailing newline is treated as a file-format
    /// artifact rather than as an empty character.
    ///
    /// *(This was the ONLY constructor until 2026-09-01, and its doc claimed
    /// blank lines are kept "because the space character is represented by a
    /// line containing a single space in some of PaddleOCR's dictionaries".
    /// **That is false of `ppocr_keys_v1.txt`**, which is the dictionary
    /// PP-OCRv4's Chinese recogniser actually uses -- it is 6623 lines with no
    /// space line and no trailing newline. UP-TAKE `I-333`, confirmed against
    /// the real file while executing roadmap `1.31`.)*
    #[must_use]
    pub fn from_lines(contents: &str) -> Self {
        let characters = contents
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
            .collect::<Vec<_>>();
        // A trailing newline produces one empty entry that is an artifact of the
        // file format rather than a character.
        let mut characters = characters;
        if characters.last().is_some_and(String::is_empty) {
            characters.pop();
        }
        Self { characters }
    }

    /// Builds a dictionary the way PaddleOCR itself does, for a model exported
    /// with `use_space_char: true` -- which is every PP-OCRv4 recogniser we
    /// convert.
    ///
    /// # The space is APPENDED, not read
    ///
    /// PaddleOCR reads the file's lines and then adds a space as one more
    /// character; the blank is prepended after that. From
    /// `ppocr/postprocess/rec_postprocess.py` at tag `v2.7.0`, in
    /// `BaseRecLabelDecode.__init__`:
    ///
    /// ```text
    /// for line in lines:
    ///     line = line.decode('utf-8').strip(...)
    ///     self.character_str.append(line)
    /// if use_space_char:
    ///     self.character_str.append(" ")
    /// dict_character = self.add_special_char(dict_character)   # CTC: ['blank'] + ...
    /// ```
    ///
    /// So for `ppocr_keys_v1.txt` the arithmetic is **6623 lines + 1 space + 1
    /// blank = 6625**, and 6625 is exactly what the converted
    /// `ch_PP-OCRv4_rec_infer` emits on its last axis. Reading the file
    /// literally gives 6624, and [`super::PaddleEngine`]'s class-count guard
    /// then refuses the real model outright -- which is how this was found,
    /// rather than by the text coming out shifted by one.
    ///
    /// **Why a separate constructor rather than a flag.** The two readings
    /// differ by one class, and one class is the difference between correct text
    /// and *every* character displaced by one position. A caller that has to
    /// name the convention it wants cannot get the PP-OCR case by default and be
    /// silently wrong; the name is the documentation.
    ///
    /// The space is appended even if the file already ends with a line holding
    /// one. PaddleOCR does not check, so neither does this -- matching the
    /// export is the whole job, and de-duplicating here would put us one class
    /// BELOW a model built that way.
    #[must_use]
    pub fn from_ppocr_dictionary(contents: &str) -> Self {
        let mut dictionary = Self::from_lines(contents);
        dictionary.characters.push(" ".to_owned());
        dictionary
    }

    /// How many classes the model must emit for this dictionary: characters + blank.
    #[must_use]
    pub fn class_count(&self) -> usize {
        self.characters.len() + 1
    }

    /// The character a class index names, or `None` for the blank and for
    /// anything past the end of the dictionary.
    #[must_use]
    pub fn character_for_class(&self, class: usize) -> Option<&str> {
        if class == 0 {
            return None;
        }
        self.characters.get(class - 1).map(String::as_str)
    }

    /// Whether the dictionary has no characters.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.characters.is_empty()
    }
}

/// Greedy CTC decode of one line's per-timestep class scores.
///
/// `logits` is `timesteps * class_count`, row-major: timestep `t`'s scores are
/// `logits[t * class_count .. (t + 1) * class_count]`. The values may be raw
/// scores or probabilities -- only their order within a timestep decides the
/// character, and the confidence is the mean of the winning values, so a caller
/// handing raw logits gets a confidence on that scale.
///
/// # The two CTC rules, and why both matter
///
/// 1. **Collapse repeats.** Consecutive timesteps choosing the same class are
///    one character. The recogniser fires for several timesteps across one
///    glyph, so without this every letter appears three or four times.
/// 2. **Then drop blanks.** The blank exists so a genuine double letter
///    survives: `l`, blank, `l` is "ll", while `l`, `l` is "l". **The order is
///    load-bearing** -- dropping blanks first would merge those two cases and
///    silently turn every "ll" into "l".
#[must_use]
pub fn ctc_decode(
    logits: &[f32],
    class_count: usize,
    dictionary: &CharacterDictionary,
) -> DecodedText {
    if class_count == 0 || logits.len() < class_count {
        return DecodedText {
            text: String::new(),
            confidence: 0.0,
        };
    }

    let mut text = String::new();
    let mut total_confidence = 0.0_f32;
    let mut counted = 0_u32;
    let mut previous_class: Option<usize> = None;

    for timestep in logits.chunks_exact(class_count) {
        let mut best_class = 0_usize;
        let mut best_value = f32::NEG_INFINITY;
        for (class, &value) in timestep.iter().enumerate() {
            if value > best_value {
                best_value = value;
                best_class = class;
            }
        }

        // Rule 1: a repeat of the previous timestep's class contributes nothing.
        if previous_class == Some(best_class) {
            continue;
        }
        previous_class = Some(best_class);

        // Rule 2, applied after the repeat check, never before.
        if let Some(character) = dictionary.character_for_class(best_class) {
            text.push_str(character);
            total_confidence += best_value;
            counted += 1;
        }
    }

    let confidence = if counted == 0 {
        0.0
    } else {
        total_confidence / counted as f32
    };
    DecodedText { text, confidence }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use uptake_core::geometry::Size;

    fn dictionary() -> CharacterDictionary {
        CharacterDictionary::from_lines("a\nb\nl\no\n \n")
    }

    /// Builds `timesteps` of one-hot scores from a list of class indices.
    fn logits(classes: &[usize], class_count: usize) -> Vec<f32> {
        let mut data = vec![0.0_f32; classes.len() * class_count];
        for (timestep, &class) in classes.iter().enumerate() {
            data[timestep * class_count + class] = 1.0;
        }
        data
    }

    #[test]
    fn class_zero_is_the_blank_and_names_no_character() {
        let dict = dictionary();
        assert_eq!(dict.character_for_class(0), None);
        assert_eq!(dict.character_for_class(1), Some("a"));
        assert_eq!(dict.character_for_class(2), Some("b"));
        assert_eq!(dict.class_count(), 6);
    }

    #[test]
    fn a_dictionary_keeps_its_space_entry() {
        // The 5th line is a single space. Dropping it would shift every later
        // index by one and silently corrupt every decode.
        assert_eq!(dictionary().character_for_class(5), Some(" "));
    }

    #[test]
    fn a_crlf_dictionary_reads_the_same_as_an_lf_one() {
        let lf = CharacterDictionary::from_lines("a\nb\nc\n");
        let crlf = CharacterDictionary::from_lines("a\r\nb\r\nc\r\n");
        assert_eq!(lf, crlf);
    }

    #[test]
    fn repeated_timesteps_collapse_to_one_character() {
        let dict = dictionary();
        // a a a b b -> "ab"
        let decoded = ctc_decode(&logits(&[1, 1, 1, 2, 2], 6), 6, &dict);
        assert_eq!(decoded.text, "ab");
    }

    #[test]
    fn a_blank_between_two_identical_classes_preserves_the_double_letter() {
        let dict = dictionary();
        // l blank l -> "ll". This is the case that fails if blanks are stripped
        // before repeats are collapsed, and it is the whole reason CTC has a
        // blank symbol at all.
        let decoded = ctc_decode(&logits(&[3, 0, 3], 6), 6, &dict);
        assert_eq!(decoded.text, "ll");
    }

    #[test]
    fn without_the_blank_the_same_two_classes_are_one_letter() {
        let dict = dictionary();
        let decoded = ctc_decode(&logits(&[3, 3], 6), 6, &dict);
        assert_eq!(decoded.text, "l", "the repeat collapse did not fire");
    }

    #[test]
    fn leading_and_trailing_blanks_contribute_nothing() {
        let dict = dictionary();
        let decoded = ctc_decode(&logits(&[0, 0, 1, 0, 0], 6), 6, &dict);
        assert_eq!(decoded.text, "a");
    }

    #[test]
    fn an_all_blank_line_decodes_to_nothing_with_zero_confidence() {
        let dict = dictionary();
        let decoded = ctc_decode(&logits(&[0, 0, 0], 6), 6, &dict);
        assert_eq!(decoded.text, "");
        assert!((decoded.confidence - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn confidence_averages_only_the_timesteps_that_produced_characters() {
        let dict = dictionary();
        let class_count = 6;
        let mut data = vec![0.0_f32; 3 * class_count];
        // t0: class 1 at 0.8. t1: blank at 0.9. t2: class 2 at 0.6.
        data[1] = 0.8;
        data[class_count] = 0.9;
        data[2 * class_count + 2] = 0.6;
        let decoded = ctc_decode(&data, class_count, &dict);
        assert_eq!(decoded.text, "ab");
        // The blank's 0.9 must NOT be in the mean: (0.8 + 0.6) / 2 = 0.7.
        assert!(
            (decoded.confidence - 0.7).abs() < 1e-5,
            "confidence was {}",
            decoded.confidence
        );
    }

    #[test]
    fn a_class_past_the_dictionary_is_skipped_rather_than_panicking() {
        let dict = dictionary();
        // Class 9 does not exist in a 5-character dictionary.
        let decoded = ctc_decode(&logits(&[9, 1], 12), 12, &dict);
        assert_eq!(decoded.text, "a");
    }

    #[test]
    fn empty_or_ragged_logits_decode_to_nothing_rather_than_panicking() {
        let dict = dictionary();
        assert_eq!(ctc_decode(&[], 6, &dict).text, "");
        assert_eq!(ctc_decode(&[0.1, 0.2], 6, &dict).text, "");
        assert_eq!(ctc_decode(&[0.1, 0.2], 0, &dict).text, "");
    }

    // --- rectify -------------------------------------------------------------

    fn frame(width: u32, height: u32, fill: [u8; 4]) -> RgbaBitmap {
        let pixels = fill
            .iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * BYTES_PER_PIXEL)
            .collect();
        RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap()
    }

    fn axis_aligned(x: f32, y: f32, width: f32, height: f32) -> Quad {
        Quad::new([
            PointF::new(x, y),
            PointF::new(x + width, y),
            PointF::new(x + width, y + height),
            PointF::new(x, y + height),
        ])
    }

    #[test]
    fn a_crop_is_always_the_recognisers_fixed_height() {
        let bitmap = frame(200, 100, [128, 128, 128, 255]);
        for (width, height) in [(60.0, 12.0), (20.0, 20.0), (150.0, 8.0)] {
            let input = rectify(&bitmap, &axis_aligned(5.0, 5.0, width, height)).unwrap();
            assert_eq!(input.shape()[2], REC_HEIGHT as usize);
            assert_eq!(
                input.tensor.len(),
                3 * REC_HEIGHT as usize * input.width as usize
            );
        }
    }

    #[test]
    fn crop_width_follows_the_boxs_aspect_ratio() {
        let bitmap = frame(400, 100, [128, 128, 128, 255]);
        // A 4:1 box at 48 px tall should be about 192 px wide.
        let input = rectify(&bitmap, &axis_aligned(5.0, 5.0, 80.0, 20.0)).unwrap();
        assert_eq!(input.width, REC_HEIGHT * 4);
    }

    #[test]
    fn a_very_long_line_is_capped_rather_than_truncated() {
        let bitmap = frame(4000, 100, [128, 128, 128, 255]);
        let input = rectify(&bitmap, &axis_aligned(0.0, 0.0, 3900.0, 10.0)).unwrap();
        assert_eq!(input.width, REC_MAX_WIDTH);
    }

    #[test]
    fn normalisation_maps_mid_grey_near_zero_and_the_extremes_to_plus_minus_one() {
        let black = rectify(
            &frame(60, 30, [0, 0, 0, 255]),
            &axis_aligned(0.0, 0.0, 40.0, 10.0),
        )
        .unwrap();
        let white = rectify(
            &frame(60, 30, [255, 255, 255, 255]),
            &axis_aligned(0.0, 0.0, 40.0, 10.0),
        )
        .unwrap();
        assert!(
            (black.tensor[0] + 1.0).abs() < 1e-4,
            "black was {}",
            black.tensor[0]
        );
        assert!(
            (white.tensor[0] - 1.0).abs() < 1e-4,
            "white was {}",
            white.tensor[0]
        );
    }

    #[test]
    fn rectify_reads_the_pixels_the_box_names_and_not_its_neighbours() {
        // A frame that is black on the left half and white on the right. A box
        // over the white half must come back white: if rectify sampled the
        // bounding box of the whole frame, or got its u/v axes crossed, black
        // pixels would appear.
        let width = 80_u32;
        let height = 40_u32;
        let mut pixels = vec![0_u8; width as usize * height as usize * BYTES_PER_PIXEL];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = if x >= 40 { 255 } else { 0 };
                let index = (y * width as usize + x) * BYTES_PER_PIXEL;
                pixels[index] = value;
                pixels[index + 1] = value;
                pixels[index + 2] = value;
                pixels[index + 3] = 255;
            }
        }
        let bitmap = RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap();
        let input = rectify(&bitmap, &axis_aligned(45.0, 10.0, 30.0, 10.0)).unwrap();
        assert!(
            input.tensor.iter().all(|&value| value > 0.9),
            "the crop picked up pixels from outside its box"
        );
    }

    #[test]
    fn a_degenerate_box_yields_no_crop_rather_than_a_zero_width_tensor() {
        let bitmap = frame(60, 30, [128, 128, 128, 255]);
        let point = PointF::new(5.0, 5.0);
        assert!(rectify(&bitmap, &Quad::new([point, point, point, point])).is_none());
    }

    /// The exact shape of `ppocr_keys_v1.txt`, which is what PP-OCRv4's Chinese
    /// recogniser is exported against: no space line, and **no trailing
    /// newline**.
    ///
    /// Both properties are load-bearing and both were measured from the real
    /// file rather than assumed -- 26249 bytes, `sha256`
    /// `28b2362a...3b1dc7f7`, fetched from PaddleOCR at tag `v2.7.0`.
    fn ppocr_keys_shaped(lines: usize) -> String {
        (0..lines)
            .map(|index| format!("c{index}"))
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    }

    #[test]
    fn the_ppocr_reading_appends_a_space_that_the_file_does_not_contain() {
        // I-333. The literal reading is one class short of the model, and the
        // difference is precisely the space PaddleOCR appends.
        let contents = ppocr_keys_shaped(6623);
        let literal = CharacterDictionary::from_lines(&contents);
        let ppocr = CharacterDictionary::from_ppocr_dictionary(&contents);

        assert_eq!(literal.class_count(), 6624, "the literal reading");
        assert_eq!(
            ppocr.class_count(),
            6625,
            "must equal the last axis of the converted ch_PP-OCRv4_rec_infer model"
        );
        assert_eq!(
            ppocr.character_for_class(6624),
            Some(" "),
            "the appended space is the LAST class, after every dictionary line"
        );
        assert_eq!(
            ppocr.character_for_class(1),
            Some("c0"),
            "appending must not disturb the existing indices"
        );
    }

    #[test]
    fn a_file_with_no_trailing_newline_loses_no_character() {
        // ppocr_keys_v1.txt does not end with a newline. An implementation that
        // unconditionally dropped a last entry would silently lose the final
        // character and shift nothing else, which reads as a rare OCR miss.
        let dictionary = CharacterDictionary::from_lines(
            "a
b
c",
        );
        assert_eq!(dictionary.class_count(), 4);
        assert_eq!(dictionary.character_for_class(3), Some("c"));
    }

    #[test]
    fn the_appended_space_is_not_deduplicated_against_a_space_line() {
        // PaddleOCR appends unconditionally. Matching that is the job: being
        // "helpful" here would put us one class BELOW a model exported from a
        // dictionary that does carry a space line.
        let dictionary = CharacterDictionary::from_ppocr_dictionary(
            "a
 
b",
        );
        assert_eq!(dictionary.class_count(), 5);
        assert_eq!(dictionary.character_for_class(2), Some(" "));
        assert_eq!(dictionary.character_for_class(4), Some(" "));
    }
}
