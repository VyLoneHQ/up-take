//! Reading-order sorting and whitespace normalisation.
//!
//! `architecture.md` section 3.2 puts *"reading-order sort, whitespace
//! normalisation"* inside the OCR pipeline, before the result leaves it, and
//! [`crate::engine::Recognition`]'s own documentation says why: a caller that
//! had to sort would need the layout information the engine already has, and two
//! sorts that disagree is a defect nobody sees until the text comes out
//! scrambled on a two-column screenshot.
//!
//! **Pure, and model-free.** The detector's boxes are the only input.

/// How much two boxes must overlap vertically to count as the same line.
///
/// Expressed as a fraction of the shorter box's height. Text on one line rarely
/// aligns perfectly -- a capital letter, a descender, or a different font size
/// in the same run all shift a box's extent -- so an exact-centre test splits
/// one visual line into several. 0.5 means "more than half the shorter box's
/// height is shared", which holds for genuinely-same-line text and fails for
/// consecutive lines at normal leading.
const SAME_LINE_OVERLAP: f32 = 0.5;

/// One item to be placed in reading order.
///
/// Generic over the payload so this module never has to know what a block
/// carries -- it sorts positions, and the caller keeps its own type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed<T> {
    /// Top edge, source-frame pixels.
    pub top: f32,
    /// Bottom edge, source-frame pixels.
    pub bottom: f32,
    /// Left edge, source-frame pixels.
    pub left: f32,
    /// Whatever the caller is sorting.
    pub payload: T,
}

impl<T> Placed<T> {
    /// The box's height, never negative.
    fn height(&self) -> f32 {
        (self.bottom - self.top).max(0.0)
    }

    /// How much vertical extent this shares with `other`.
    fn vertical_overlap(&self, other: &Self) -> f32 {
        (self.bottom.min(other.bottom) - self.top.max(other.top)).max(0.0)
    }

    /// Whether the two sit on the same visual line.
    fn shares_a_line_with(&self, other: &Self) -> bool {
        let shorter = self.height().min(other.height());
        if shorter <= f32::EPSILON {
            // A zero-height box has no overlap to measure; fall back to whether
            // the other box contains its top edge at all.
            return self.top >= other.top && self.top <= other.bottom;
        }
        self.vertical_overlap(other) / shorter > SAME_LINE_OVERLAP
    }
}

/// Sorts boxes into reading order: top to bottom, then left to right.
///
/// # Why grouping into lines beats sorting by `(y, x)`
///
/// A plain sort by top edge then left edge puts a line's boxes in the wrong
/// order whenever their tops differ by a pixel -- which is most of the time,
/// because a box around `Type` starts higher than one around `own`. Grouping by
/// vertical overlap first makes the second key apply *within* a line, which is
/// the order a person reads in.
///
/// Two columns of text produce two groups only if their lines do not overlap
/// vertically. **Side-by-side columns at the same height are read across, not
/// down**, and that is a real limit rather than an oversight: separating them
/// needs column detection, which PP-OCR's `quad` output does not provide. A
/// caller that needs true multi-column layout wants a layout-analysis stage this
/// pipeline does not have.
#[must_use]
pub fn sort_into_reading_order<T>(mut items: Vec<Placed<T>>) -> Vec<Placed<T>> {
    if items.len() < 2 {
        return items;
    }
    // Stable pre-sort by top edge, so grouping walks the page downward and the
    // result is deterministic for boxes that tie.
    items.sort_by(|a, b| {
        a.top
            .partial_cmp(&b.top)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.left
                    .partial_cmp(&b.left)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let mut lines: Vec<Vec<Placed<T>>> = Vec::new();
    for item in items {
        // Compare against the line's FIRST member rather than its last: the
        // first is the topmost, so a line that drifts downward one box at a time
        // cannot chain arbitrarily far down the page.
        let target = lines.iter_mut().find(|line| {
            line.first()
                .is_some_and(|first| first.shares_a_line_with(&item))
        });
        match target {
            Some(line) => line.push(item),
            None => lines.push(vec![item]),
        }
    }

    let mut ordered = Vec::new();
    for mut line in lines {
        line.sort_by(|a, b| {
            a.left
                .partial_cmp(&b.left)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ordered.extend(line);
    }
    ordered
}

/// Collapses whitespace runs to single spaces and trims the ends.
///
/// The recogniser emits a character per class, and a dictionary that contains a
/// space entry can produce runs of them at a box's margins. Newlines and tabs
/// are collapsed too: a block is one line of text by construction, so a literal
/// newline inside one is an artifact rather than a line break the user typed.
///
/// **Line breaks BETWEEN blocks are the caller's**, added when the blocks are
/// joined ([`crate::engine::Recognition::text`]), which is why this function can
/// safely flatten every whitespace character it sees.
#[must_use]
pub fn normalise_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_whitespace = false;
    for character in text.chars() {
        if character.is_whitespace() {
            in_whitespace = true;
            continue;
        }
        if in_whitespace && !result.is_empty() {
            result.push(' ');
        }
        in_whitespace = false;
        result.push(character);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(top: f32, bottom: f32, left: f32, label: &str) -> Placed<String> {
        Placed {
            top,
            bottom,
            left,
            payload: label.to_owned(),
        }
    }

    fn labels(items: &[Placed<String>]) -> Vec<&str> {
        items.iter().map(|item| item.payload.as_str()).collect()
    }

    #[test]
    fn an_empty_or_single_item_list_is_returned_unchanged() {
        assert!(sort_into_reading_order(Vec::<Placed<String>>::new()).is_empty());
        let one = vec![placed(5.0, 15.0, 20.0, "only")];
        assert_eq!(labels(&sort_into_reading_order(one)), ["only"]);
    }

    #[test]
    fn boxes_on_one_line_are_ordered_left_to_right() {
        let items = vec![
            placed(10.0, 22.0, 300.0, "third"),
            placed(10.0, 22.0, 100.0, "first"),
            placed(10.0, 22.0, 200.0, "second"),
        ];
        assert_eq!(
            labels(&sort_into_reading_order(items)),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn lines_are_ordered_top_to_bottom() {
        let items = vec![
            placed(100.0, 112.0, 10.0, "lower"),
            placed(10.0, 22.0, 10.0, "upper"),
        ];
        assert_eq!(labels(&sort_into_reading_order(items)), ["upper", "lower"]);
    }

    #[test]
    fn a_ragged_line_still_reads_left_to_right() {
        // This is the case a plain (top, left) sort gets wrong: "Type" starts a
        // pixel higher than "own", so sorting by top alone puts it first even
        // though it sits to the RIGHT.
        let items = vec![
            placed(9.0, 23.0, 200.0, "second"),
            placed(11.0, 22.0, 100.0, "first"),
        ];
        assert_eq!(labels(&sort_into_reading_order(items)), ["first", "second"]);
    }

    #[test]
    fn consecutive_lines_at_normal_leading_do_not_merge() {
        // 14 px tall boxes, 18 px apart: they overlap by nothing.
        let items = vec![
            placed(30.0, 44.0, 500.0, "line2-right"),
            placed(10.0, 24.0, 100.0, "line1-left"),
            placed(30.0, 44.0, 100.0, "line2-left"),
            placed(10.0, 24.0, 500.0, "line1-right"),
        ];
        assert_eq!(
            labels(&sort_into_reading_order(items)),
            ["line1-left", "line1-right", "line2-left", "line2-right"]
        );
    }

    #[test]
    fn a_taller_box_joins_the_line_it_mostly_overlaps() {
        // A large heading glyph next to normal text on the same line.
        let items = vec![
            placed(10.0, 30.0, 200.0, "small"),
            placed(6.0, 34.0, 100.0, "tall"),
        ];
        assert_eq!(labels(&sort_into_reading_order(items)), ["tall", "small"]);
    }

    #[test]
    fn grouping_does_not_chain_down_the_page() {
        // Three boxes each overlapping the next by more than half, but the first
        // and third not overlapping at all. Comparing against a line's FIRST
        // member stops them collapsing into one line, which would put the
        // bottom-most box before boxes above it.
        let items = vec![
            placed(0.0, 20.0, 10.0, "a"),
            placed(12.0, 32.0, 5.0, "b"),
            placed(24.0, 44.0, 0.0, "c"),
        ];
        let ordered = sort_into_reading_order(items);
        assert_eq!(labels(&ordered), ["a", "b", "c"]);
    }

    #[test]
    fn ordering_is_deterministic_for_identical_boxes() {
        let first = vec![placed(10.0, 20.0, 5.0, "x"), placed(10.0, 20.0, 5.0, "y")];
        let second = first.clone();
        assert_eq!(
            labels(&sort_into_reading_order(first)),
            labels(&sort_into_reading_order(second))
        );
    }

    #[test]
    fn whitespace_runs_collapse_to_one_space() {
        assert_eq!(normalise_whitespace("hello    world"), "hello world");
        assert_eq!(normalise_whitespace("a\t\tb"), "a b");
    }

    #[test]
    fn leading_and_trailing_whitespace_is_removed() {
        assert_eq!(normalise_whitespace("   padded   "), "padded");
        assert_eq!(normalise_whitespace("\n\ttext\n"), "text");
    }

    #[test]
    fn newlines_inside_a_block_become_spaces() {
        // A block is one line by construction; a newline in it is an artifact.
        assert_eq!(normalise_whitespace("one\ntwo"), "one two");
    }

    #[test]
    fn an_all_whitespace_string_normalises_to_empty() {
        assert_eq!(normalise_whitespace("   \t\n  "), "");
        assert_eq!(normalise_whitespace(""), "");
    }

    #[test]
    fn text_without_whitespace_is_untouched() {
        assert_eq!(normalise_whitespace("unchanged"), "unchanged");
    }
}
