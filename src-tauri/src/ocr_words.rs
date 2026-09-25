//! Where an OCR area's words sit, and which of them a pointer means.
//!
//! Roadmap `1.41`, `ADR-0046`: an OCR area that reads **in place** marks each
//! word over the unchanged screen, and in Placement the user drags across words
//! to select some of them. This module holds the pure part of that: the words
//! of one recognition flattened into reading order with the line each sits on,
//! the hit test a press needs, the nearest-word rule a drag needs, and the text
//! a selection copies. It holds no state and touches no window, so every rule
//! in it is tested here rather than on the rig.
//!
//! **Coordinates are frame-local**: `(0, 0)` is the top-left of the frame the
//! engine read, which is the area's own top-left at the moment it was read
//! (`uptake_ocr::TextBlock` says why the engine never sees screen coordinates).
//! A caller turns a screen point into frame-local by subtracting the area's
//! origin.

use uptake_core::geometry::Point;
use uptake_ocr::Recognition;

/// One recognised word, in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlacedWord {
    /// The word, with no whitespace in it.
    pub(crate) text: String,
    /// Which visual line it sits on, counted from `0` at the top.
    pub(crate) line: u32,
    /// Its four corners, clockwise from the top-left, frame-local.
    ///
    /// The engine's `Word::outline`, which neighbours share an edge of; the
    /// word's `bounds` would overlap its neighbours on a rotated line, so it is
    /// not carried at all.
    pub(crate) outline: [Point; 4],
}

/// The words of `recognition`, in reading order, each with its line.
///
/// A block the engine could not split into words (`TextBlock::words` empty)
/// becomes one word spanning the block's bounds, which is what
/// `TextBlock::words`' own contract tells a caller to do: no text, however it
/// was read, is left out of what the user can select.
pub(crate) fn from_recognition(recognition: &Recognition) -> Vec<PlacedWord> {
    let mut placed = Vec::new();
    for (line, blocks) in recognition.lines().iter().enumerate() {
        let line = u32::try_from(line).unwrap_or(u32::MAX);
        for block in blocks {
            if block.words.is_empty() {
                let b = block.bounds;
                let right = b.origin.x.saturating_add_unsigned(b.size.width);
                let bottom = b.origin.y.saturating_add_unsigned(b.size.height);
                placed.push(PlacedWord {
                    text: block.text.clone(),
                    line,
                    outline: [
                        b.origin,
                        Point::new(right, b.origin.y),
                        Point::new(right, bottom),
                        Point::new(b.origin.x, bottom),
                    ],
                });
                continue;
            }
            for word in &block.words {
                placed.push(PlacedWord {
                    text: word.text.clone(),
                    line,
                    outline: word.outline,
                });
            }
        }
    }
    placed
}

/// Whether `point` is inside `outline`, edges included.
///
/// The outline is a convex quadrilateral (a detector box cut across), so a
/// point is inside when it is on the same side of all four edges. Either
/// winding is accepted: which way round the corners go is the engine's
/// convention and not this test's business.
fn contains(outline: &[Point; 4], point: Point) -> bool {
    let mut positive = false;
    let mut negative = false;
    for index in 0..4 {
        let from = outline[index];
        let to = outline[(index + 1) % 4];
        let cross = i64::from(to.x - from.x) * i64::from(point.y - from.y)
            - i64::from(to.y - from.y) * i64::from(point.x - from.x);
        if cross > 0 {
            positive = true;
        } else if cross < 0 {
            negative = true;
        }
    }
    !(positive && negative)
}

/// The word under `point`, if a word is under it.
///
/// This is the test a **press** makes: a press on a word starts a selection,
/// and a press anywhere else in the area moves the area as it always has.
pub(crate) fn word_at(words: &[PlacedWord], point: Point) -> Option<usize> {
    words.iter().position(|word| contains(&word.outline, point))
}

/// The word a drag at `point` means, whether or not the pointer is on one.
///
/// This is the test a **drag** makes, and it never answers "none" while there
/// are words: a selection follows the pointer across the gaps between lines
/// and past the ends of a line, the way every text selection does. The line
/// is chosen first, by vertical distance to the line's extent, and then the
/// word within it by horizontal distance, so a pointer to the right of a
/// line's last word selects to the end of that line rather than jumping to
/// whichever word on another line happens to be closer as the crow flies.
pub(crate) fn nearest(words: &[PlacedWord], point: Point) -> Option<usize> {
    let span = |word: &PlacedWord, axis: fn(Point) -> i32| {
        let values = word.outline.map(axis);
        let low = values.iter().copied().min().unwrap_or(0);
        let high = values.iter().copied().max().unwrap_or(0);
        (low, high)
    };
    let distance = |(low, high): (i32, i32), at: i32| {
        if at < low {
            i64::from(low) - i64::from(at)
        } else if at > high {
            i64::from(at) - i64::from(high)
        } else {
            0
        }
    };
    // The line whose vertical extent is closest to the pointer.
    let mut best_line: Option<(u32, i64)> = None;
    for word in words {
        let d = distance(span(word, |p| p.y), point.y);
        match best_line {
            Some((_, best)) if best <= d => {}
            _ => best_line = Some((word.line, d)),
        }
    }
    let (line, _) = best_line?;
    words
        .iter()
        .enumerate()
        .filter(|(_, word)| word.line == line)
        .min_by_key(|(_, word)| distance(span(word, |p| p.x), point.x))
        .map(|(index, _)| index)
}

/// The handle of the selection `first..=last` under `point`, as the index of
/// the selection's OTHER end.
///
/// The start handle hangs below the first word's bottom-left corner and the end
/// handle below the last word's bottom-right; the page draws each as a
/// `radius`-sized teardrop reaching down and outward from that corner.
///
/// **The grab target is LARGER than the drawn handle, on purpose**: from
/// `radius` left of the corner to `radius` right of it, and from half a
/// `radius` above it to two below. A handle is a small target for a mouse, and
/// the margin lets a press slightly off the teardrop still take it. The end
/// handle wins where the two overlap (a one-word selection), so a drag from
/// there extends forward, the common case.
pub(crate) fn handle_at(
    words: &[PlacedWord],
    first: usize,
    last: usize,
    point: Point,
    radius: i32,
) -> Option<usize> {
    let bottom_left = |word: &PlacedWord| word.outline[3];
    let bottom_right = |word: &PlacedWord| word.outline[2];
    let end = bottom_right(words.get(last)?);
    if (end.x - radius..=end.x + radius).contains(&point.x)
        && (end.y - radius / 2..=end.y + radius * 2).contains(&point.y)
    {
        return Some(first);
    }
    let start = bottom_left(words.get(first)?);
    if (start.x - radius..=start.x + radius).contains(&point.x)
        && (start.y - radius / 2..=start.y + radius * 2).contains(&point.y)
    {
        return Some(last);
    }
    None
}

/// The text of words `a` to `b` inclusive, in either order.
///
/// Words on one line are joined by a space and lines by a newline, which is
/// how `Recognition::text` lays out the whole of it, so copying everything by
/// selecting everything gives the same text as the automatic copy.
pub(crate) fn text_between(words: &[PlacedWord], a: usize, b: usize) -> String {
    let (first, last) = if a <= b { (a, b) } else { (b, a) };
    let mut text = String::new();
    for index in first..=last.min(words.len().saturating_sub(1)) {
        let Some(word) = words.get(index) else {
            break;
        };
        if index > first {
            let previous_line = words.get(index - 1).map_or(word.line, |w| w.line);
            text.push(if previous_line == word.line {
                ' '
            } else {
                '\n'
            });
        }
        text.push_str(&word.text);
    }
    text
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed unwrap or expect is a failed test"
)]
mod tests {
    use super::*;
    use uptake_core::geometry::Rect;
    use uptake_ocr::{TextBlock, Word};

    /// An upright word from `x` to `x + width` on the line at `y`, 20 px tall.
    fn word(text: &str, line: u32, x: i32, width: i32, y: i32) -> PlacedWord {
        PlacedWord {
            text: text.to_owned(),
            line,
            outline: [
                Point::new(x, y),
                Point::new(x + width, y),
                Point::new(x + width, y + 20),
                Point::new(x, y + 20),
            ],
        }
    }

    /// Two lines: "Die Texte" at y 0 and "für Anfänger" at y 30.
    fn two_lines() -> Vec<PlacedWord> {
        vec![
            word("Die", 0, 0, 30, 0),
            word("Texte", 0, 30, 50, 0),
            word("für", 1, 0, 30, 30),
            word("Anfänger", 1, 30, 80, 30),
        ]
    }

    #[test]
    fn a_press_on_a_word_names_it_and_a_press_between_lines_names_none() {
        let words = two_lines();
        assert_eq!(word_at(&words, Point::new(40, 10)), Some(1));
        assert_eq!(word_at(&words, Point::new(5, 35)), Some(2));
        // The 10 px gap between the lines belongs to no word, so a press there
        // moves the area rather than starting a selection.
        assert_eq!(word_at(&words, Point::new(10, 25)), None);
        assert_eq!(word_at(&words, Point::new(500, 10)), None);
    }

    #[test]
    fn a_rotated_word_is_hit_inside_its_slant_and_missed_outside_it() {
        // A word slanted 45 degrees: its bounding box's corner is not the word.
        let slanted = PlacedWord {
            text: "slant".to_owned(),
            line: 0,
            outline: [
                Point::new(0, 0),
                Point::new(50, 50),
                Point::new(40, 60),
                Point::new(-10, 10),
            ],
        };
        let words = vec![slanted];
        assert_eq!(word_at(&words, Point::new(20, 25)), Some(0));
        assert_eq!(word_at(&words, Point::new(45, 5)), None, "the box corner");
    }

    #[test]
    fn a_drag_past_the_end_of_a_line_stays_on_that_line() {
        let words = two_lines();
        // Far to the right of line 0, and nearer in a straight line to nothing
        // on line 1: the selection runs to the end of line 0.
        assert_eq!(nearest(&words, Point::new(400, 10)), Some(1));
        // In the gap between the lines, nearer to line 1.
        assert_eq!(nearest(&words, Point::new(5, 28)), Some(2));
        // Above everything: the top line.
        assert_eq!(nearest(&words, Point::new(5, -40)), Some(0));
        assert_eq!(nearest(&[], Point::new(0, 0)), None);
    }

    #[test]
    fn a_selection_copies_with_the_screens_line_breaks_in_either_direction() {
        let words = two_lines();
        assert_eq!(text_between(&words, 1, 2), "Texte\nfür");
        assert_eq!(text_between(&words, 2, 1), "Texte\nfür");
        assert_eq!(text_between(&words, 0, 3), "Die Texte\nfür Anfänger");
        assert_eq!(text_between(&words, 3, 3), "Anfänger");
        assert_eq!(text_between(&words, 2, 99), "für Anfänger");
        assert_eq!(text_between(&[], 0, 0), "");
    }

    #[test]
    fn a_handle_grabs_the_end_it_hangs_off_and_anchors_the_other() {
        let words = two_lines();
        // Selection Texte (1) to für (2). The end handle hangs below für's
        // bottom-right, (30, 50); the start handle below Texte's bottom-left,
        // (30, 20). A grab on the end handle anchors the start, and the other
        // way round.
        assert_eq!(handle_at(&words, 1, 2, Point::new(32, 58), 10), Some(1));
        assert_eq!(handle_at(&words, 1, 2, Point::new(28, 26), 10), Some(2));
        // Far from both: no handle, so the press is a word or a move.
        assert_eq!(handle_at(&words, 1, 2, Point::new(90, 58), 10), None);
        // A stale selection past the words has no handles.
        assert_eq!(handle_at(&words, 1, 9, Point::new(32, 58), 10), None);
    }

    #[test]
    fn a_block_without_words_becomes_one_word_over_its_bounds() {
        let recognition = Recognition::from_lines(vec![
            vec![TextBlock {
                text: "whole line".to_owned(),
                bounds: Rect::new(10, 20, 100, 30),
                words: Vec::new(),
            }],
            vec![TextBlock {
                text: "a b".to_owned(),
                bounds: Rect::new(10, 60, 40, 20),
                words: vec![
                    Word {
                        text: "a".to_owned(),
                        outline: [
                            Point::new(10, 60),
                            Point::new(30, 60),
                            Point::new(30, 80),
                            Point::new(10, 80),
                        ],
                        bounds: Rect::new(10, 60, 20, 20),
                    },
                    Word {
                        text: "b".to_owned(),
                        outline: [
                            Point::new(30, 60),
                            Point::new(50, 60),
                            Point::new(50, 80),
                            Point::new(30, 80),
                        ],
                        bounds: Rect::new(30, 60, 20, 20),
                    },
                ],
            }],
        ]);
        let words = from_recognition(&recognition);
        assert_eq!(
            words
                .iter()
                .map(|w| (w.text.as_str(), w.line))
                .collect::<Vec<_>>(),
            vec![("whole line", 0), ("a", 1), ("b", 1)]
        );
        assert_eq!(words[0].outline[2], Point::new(110, 50));
        // And selecting all of it copies exactly what the automatic copy does.
        assert_eq!(text_between(&words, 0, 2), recognition.text());
    }
}
