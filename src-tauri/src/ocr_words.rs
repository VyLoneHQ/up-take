//! Where an OCR area's characters sit, and which of them a pointer means.
//!
//! Roadmap `1.41` and `1.44`, `ADR-0046`: an OCR area that reads **in place**
//! marks each word over the unchanged screen, and in Placement the user drags
//! across the text to select some of it, **down to single characters**
//! (decision 8). This module holds the pure part of that: the characters of one
//! recognition flattened into reading order with the line and word each belongs
//! to, the nearest-character rule a drag needs, the handles, and the text a
//! selection copies. It holds no state and touches no window, so every rule in
//! it is tested here rather than on the rig.
//!
//! **Coordinates are frame-local**: `(0, 0)` is the top-left of the frame the
//! engine read, which is the area's own top-left at the moment it was read
//! (`uptake_ocr::TextBlock` says why the engine never sees screen coordinates).
//! A caller turns a screen point into frame-local by subtracting the area's
//! origin.

use uptake_core::geometry::Point;
use uptake_ocr::Recognition;

/// One recognised character, in reading order: the unit a selection is made
/// of since `1.44`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlacedChar {
    /// The character, as the recogniser spells it: usually one code point,
    /// never whitespace. A word the engine gave no characters, or a block it
    /// gave no words, is one unit holding its whole text (see
    /// [`from_recognition`]).
    pub(crate) text: String,
    /// Which visual line it sits on, counted from `0` at the top.
    pub(crate) line: u32,
    /// Which word it belongs to, counted from `0` across the whole recognition.
    ///
    /// Characters of one word share it, so a copy puts a space only between
    /// words and the page draws one mark per word, as it did before `1.44`.
    pub(crate) word: u32,
    /// Its four corners, clockwise from the top-left, frame-local.
    ///
    /// The engine's `Character::outline`, which neighbours in a word share an
    /// edge of, as words do.
    pub(crate) outline: [Point; 4],
}

/// The characters of `recognition`, in reading order, each with its line and
/// word.
///
/// Two fallbacks keep every piece of text selectable, as `TextBlock::words`'
/// and `Word::characters`' own contracts tell a caller to do: a block the engine
/// could not split into words becomes one unit spanning the block's bounds, and
/// a word with no characters becomes one unit over the word's outline.
pub(crate) fn from_recognition(recognition: &Recognition) -> Vec<PlacedChar> {
    let mut placed = Vec::new();
    let mut word_number: u32 = 0;
    for (line, blocks) in recognition.lines().iter().enumerate() {
        let line = u32::try_from(line).unwrap_or(u32::MAX);
        for block in blocks {
            if block.words.is_empty() {
                let b = block.bounds;
                let right = b.origin.x.saturating_add_unsigned(b.size.width);
                let bottom = b.origin.y.saturating_add_unsigned(b.size.height);
                placed.push(PlacedChar {
                    text: block.text.clone(),
                    line,
                    word: word_number,
                    outline: [
                        b.origin,
                        Point::new(right, b.origin.y),
                        Point::new(right, bottom),
                        Point::new(b.origin.x, bottom),
                    ],
                });
                word_number = word_number.saturating_add(1);
                continue;
            }
            for word in &block.words {
                if word.characters.is_empty() {
                    placed.push(PlacedChar {
                        text: word.text.clone(),
                        line,
                        word: word_number,
                        outline: word.outline,
                    });
                } else {
                    for character in &word.characters {
                        placed.push(PlacedChar {
                            text: character.text.clone(),
                            line,
                            word: word_number,
                            outline: character.outline,
                        });
                    }
                }
                word_number = word_number.saturating_add(1);
            }
        }
    }
    placed
}

/// The character a drag at `point` means, whether or not the pointer is on one.
///
/// This is the test both a **press** and a **drag** make (`1.44`: a drag
/// starting anywhere inside an in-place OCR area selects from the nearest
/// character), and it never answers "none" while there are characters: a
/// selection follows the pointer across the gaps between lines and past the
/// ends of a line, the way every text selection does. The line is chosen first,
/// by vertical distance to the line's extent, so a pointer to the right of a
/// line's last character selects to the end of that line rather than jumping to
/// whichever character on another line happens to be closer as the crow flies.
///
/// **Within the line, the character is the one whose OUTLINE is nearest**, by
/// true distance, zero inside it. Horizontal distance alone, which words used
/// before `1.44`, ties every character of a vertical or steeply rotated line
/// that shares the pointer's x, and the first of them won (review of `#122`,
/// round 2).
pub(crate) fn nearest(chars: &[PlacedChar], point: Point) -> Option<usize> {
    let span = |unit: &PlacedChar, axis: fn(Point) -> i32| {
        let values = unit.outline.map(axis);
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
    // A pointer INSIDE a character means that character, whatever its line:
    // rotated lines can overlap in their vertical extents, so the line rule
    // below can pick the wrong one (review of `#122`, round 3). The line rule
    // is for a pointer between characters or past a line's end.
    if let Some(inside) = chars
        .iter()
        .position(|unit| outline_distance(&unit.outline, point) == 0.0)
    {
        return Some(inside);
    }
    // The line whose vertical extent is closest to the pointer.
    let mut best_line: Option<(u32, i64)> = None;
    for unit in chars {
        let d = distance(span(unit, |p| p.y), point.y);
        match best_line {
            Some((_, best)) if best <= d => {}
            _ => best_line = Some((unit.line, d)),
        }
    }
    let (line, _) = best_line?;
    chars
        .iter()
        .enumerate()
        .filter(|(_, unit)| unit.line == line)
        .min_by(|(_, a), (_, b)| {
            outline_distance(&a.outline, point).total_cmp(&outline_distance(&b.outline, point))
        })
        .map(|(index, _)| index)
}

/// How far `point` is from `outline`: `0.0` inside it or on its edge,
/// otherwise the distance to the nearest of its four edges.
///
/// The outline is a convex quadrilateral (a detector box cut across), so a
/// point is inside when it is on the same side of all four edges, in either
/// winding.
fn outline_distance(outline: &[Point; 4], point: Point) -> f64 {
    let mut positive = false;
    let mut negative = false;
    let mut nearest = f64::INFINITY;
    let (px, py) = (f64::from(point.x), f64::from(point.y));
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
        let (ax, ay) = (f64::from(from.x), f64::from(from.y));
        let (dx, dy) = (f64::from(to.x) - ax, f64::from(to.y) - ay);
        let length = dx.mul_add(dx, dy * dy);
        let t = if length > 0.0 {
            ((px - ax).mul_add(dx, (py - ay) * dy) / length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (ex, ey) = (t.mul_add(dx, ax) - px, t.mul_add(dy, ay) - py);
        nearest = nearest.min(ex.mul_add(ex, ey * ey).sqrt());
    }
    if positive && negative { nearest } else { 0.0 }
}

/// The handle of the selection `first..=last` under `point`, as the index of
/// the selection's OTHER end.
///
/// The start handle hangs below the first character's bottom-left corner and
/// the end handle below the last one's bottom-right; the page draws each as a
/// `radius`-sized teardrop reaching down and outward from that corner.
///
/// **The grab target is LARGER than the drawn handle, on purpose**: from
/// `radius` left of the corner to `radius` right of it, and from half a
/// `radius` above it to two below. A handle is a small target for a mouse, and
/// the margin lets a press slightly off the teardrop still take it. The end
/// handle wins where the two overlap (a one-character selection), so a drag
/// from there extends forward, the common case.
pub(crate) fn handle_at(
    chars: &[PlacedChar],
    first: usize,
    last: usize,
    point: Point,
    radius: i32,
) -> Option<usize> {
    let bottom_left = |unit: &PlacedChar| unit.outline[3];
    let bottom_right = |unit: &PlacedChar| unit.outline[2];
    let end = bottom_right(chars.get(last)?);
    if (end.x - radius..=end.x + radius).contains(&point.x)
        && (end.y - radius / 2..=end.y + radius * 2).contains(&point.y)
    {
        return Some(first);
    }
    let start = bottom_left(chars.get(first)?);
    if (start.x - radius..=start.x + radius).contains(&point.x)
        && (start.y - radius / 2..=start.y + radius * 2).contains(&point.y)
    {
        return Some(last);
    }
    None
}

/// The text of characters `a` to `b` inclusive, in either order.
///
/// Characters of one word are joined with nothing, words on one line by a
/// space and lines by a newline, which is how `Recognition::text` lays out the
/// whole of it, so copying everything by selecting everything gives the same
/// text as the automatic copy.
pub(crate) fn text_between(chars: &[PlacedChar], a: usize, b: usize) -> String {
    let (first, last) = if a <= b { (a, b) } else { (b, a) };
    let mut text = String::new();
    for index in first..=last.min(chars.len().saturating_sub(1)) {
        let Some(unit) = chars.get(index) else {
            break;
        };
        if index > first
            && let Some(previous) = chars.get(index - 1)
        {
            if previous.line != unit.line {
                text.push('\n');
            } else if previous.word != unit.word {
                text.push(' ');
            }
        }
        text.push_str(&unit.text);
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
    use uptake_ocr::{Character, TextBlock, Word};

    /// Upright characters of `text` from `x`, each `pitch` wide, on the line at
    /// `y`, 20 px tall, all in word `word`.
    fn chars(text: &str, line: u32, word: u32, x: i32, pitch: i32, y: i32) -> Vec<PlacedChar> {
        text.chars()
            .zip(0..)
            .map(|(c, n)| {
                let left = x + pitch * n;
                PlacedChar {
                    text: c.to_string(),
                    line,
                    word,
                    outline: [
                        Point::new(left, y),
                        Point::new(left + pitch, y),
                        Point::new(left + pitch, y + 20),
                        Point::new(left, y + 20),
                    ],
                }
            })
            .collect()
    }

    /// Two lines: "Die Texte" at y 0 and "für Anfänger" at y 30, 10 px a
    /// character, a 10 px space between words.
    fn two_lines() -> Vec<PlacedChar> {
        let mut all = chars("Die", 0, 0, 0, 10, 0);
        all.extend(chars("Texte", 0, 1, 40, 10, 0));
        all.extend(chars("für", 1, 2, 0, 10, 30));
        all.extend(chars("Anfänger", 1, 3, 40, 10, 30));
        all
    }

    fn centre(unit: &PlacedChar) -> Point {
        let (x, y) = unit
            .outline
            .iter()
            .fold((0, 0), |(x, y), corner| (x + corner.x, y + corner.y));
        Point::new(x / 4, y / 4)
    }

    #[test]
    fn a_characters_centre_is_nearest_to_that_character() {
        // The grab offset of a selection handle (review of `#115`, third GPT-6
        // Astra round) moves the pointer to the grabbed character's centre, so
        // a click on the handle must resolve back to that character.
        let all = two_lines();
        for (index, unit) in all.iter().enumerate() {
            assert_eq!(nearest(&all, centre(unit)), Some(index), "{}", unit.text);
        }
    }

    #[test]
    fn a_press_anywhere_names_the_nearest_character() {
        let all = two_lines();
        // Inside the "x" of "Texte" (x 60 to 70 on line 0).
        assert_eq!(all[nearest(&all, Point::new(64, 10)).unwrap()].text, "x");
        // In the space between "Die" and "Texte": the nearer of "e" and "T".
        assert_eq!(all[nearest(&all, Point::new(31, 10)).unwrap()].text, "e");
        assert_eq!(all[nearest(&all, Point::new(39, 10)).unwrap()].text, "T");
        // In the gap between the lines, nearer to line 1.
        assert_eq!(nearest(&all, Point::new(5, 28)), Some(8));
    }

    #[test]
    fn a_drag_past_the_end_of_a_line_stays_on_that_line() {
        let all = two_lines();
        // Far right of line 0: its last character, never one of line 1.
        assert_eq!(nearest(&all, Point::new(400, 10)), Some(7));
        // Above everything: the top line's first character.
        assert_eq!(nearest(&all, Point::new(-5, -40)), Some(0));
        assert_eq!(nearest(&[], Point::new(0, 0)), None);
    }

    #[test]
    fn a_selection_copies_characters_words_and_line_breaks_in_either_direction() {
        let all = two_lines();
        // "xte" of "Texte" (indices 5 to 7): no space inside a word.
        assert_eq!(text_between(&all, 5, 7), "xte");
        // From the "e" of "Die" to the "T" of "Texte": a space between words.
        assert_eq!(text_between(&all, 2, 3), "e T");
        // Across the line break, both directions.
        assert_eq!(text_between(&all, 7, 8), "e\nf");
        assert_eq!(text_between(&all, 8, 7), "e\nf");
        assert_eq!(
            text_between(&all, 0, all.len() - 1),
            "Die Texte\nfür Anfänger"
        );
        assert_eq!(text_between(&all, 9, 99), "ür Anfänger");
        assert_eq!(text_between(&[], 0, 0), "");
    }

    #[test]
    fn a_handle_grabs_the_end_it_hangs_off_and_anchors_the_other() {
        let all = two_lines();
        // Selection "T" (3) to "f" (8). The end handle hangs below "f"'s
        // bottom-right, (10, 50); the start handle below "T"'s bottom-left,
        // (40, 20). A grab on the end handle anchors the start, and the other
        // way round.
        assert_eq!(handle_at(&all, 3, 8, Point::new(12, 58), 6), Some(3));
        assert_eq!(handle_at(&all, 3, 8, Point::new(38, 26), 6), Some(8));
        // Far from both: no handle.
        assert_eq!(handle_at(&all, 3, 8, Point::new(90, 58), 6), None);
        // A stale selection past the characters has no handles.
        assert_eq!(handle_at(&all, 3, 99, Point::new(12, 58), 6), None);
    }

    #[test]
    fn on_a_vertical_or_rotated_line_the_character_under_the_pointer_wins() {
        // Review of `#122`, round 2: by horizontal distance alone, every
        // character sharing the pointer's x tied and the first one won.
        let unit = |n: i32, outline: [Point; 4]| PlacedChar {
            text: n.to_string(),
            line: 0,
            word: 0,
            outline,
        };
        // Vertical text: three 10 x 10 characters stacked on one line, all
        // spanning x 0 to 10.
        let stacked: Vec<PlacedChar> = (0..3)
            .map(|n| {
                let top = n * 10;
                unit(
                    n,
                    [
                        Point::new(0, top),
                        Point::new(10, top),
                        Point::new(10, top + 10),
                        Point::new(0, top + 10),
                    ],
                )
            })
            .collect();
        assert_eq!(nearest(&stacked, Point::new(5, 25)), Some(2));
        assert_eq!(nearest(&stacked, Point::new(5, 15)), Some(1));
        assert_eq!(nearest(&stacked, Point::new(5, 5)), Some(0));
        // A line at 45 degrees: neighbours share an edge, their x-spans
        // overlap, and each character's centre must still name it.
        let rotated: Vec<PlacedChar> = (0..4)
            .map(|n| {
                let o = n * 10;
                unit(
                    n,
                    [
                        Point::new(o, o),
                        Point::new(o + 10, o + 10),
                        Point::new(o + 5, o + 15),
                        Point::new(o - 5, o + 5),
                    ],
                )
            })
            .collect();
        for (index, character) in rotated.iter().enumerate() {
            assert_eq!(nearest(&rotated, centre(character)), Some(index));
        }
    }

    #[test]
    fn a_pointer_inside_a_character_of_an_overlapping_line_selects_that_line() {
        // Review of `#122`, round 3: two lines slanted so their vertical
        // extents overlap. By the line rule alone, line 0 (y 0 to 40) and line
        // 1 (y 20 to 60) are both at distance 0 from y 40, the first won, and
        // the character under the pointer on line 1 could never be chosen.
        let slanted = |line: u32, word: u32, x: i32, y: i32| PlacedChar {
            text: format!("{line}"),
            line,
            word,
            outline: [
                Point::new(x, y),
                Point::new(x + 20, y + 20),
                Point::new(x + 20, y + 40),
                Point::new(x, y + 20),
            ],
        };
        let lines = vec![slanted(0, 0, 0, 0), slanted(1, 1, 60, 20)];
        // Inside line 1's character, at y 40, where line 0's extent also is.
        assert_eq!(nearest(&lines, Point::new(70, 40)), Some(1));
        // Inside line 0's character.
        assert_eq!(nearest(&lines, Point::new(10, 20)), Some(0));
    }

    fn quad(x: i32, y: i32, width: i32) -> [Point; 4] {
        [
            Point::new(x, y),
            Point::new(x + width, y),
            Point::new(x + width, y + 20),
            Point::new(x, y + 20),
        ]
    }

    #[test]
    fn characters_carry_their_word_and_the_fallbacks_keep_all_text_selectable() {
        let recognition = Recognition::from_lines(vec![
            vec![TextBlock {
                text: "whole line".to_owned(),
                bounds: Rect::new(10, 20, 100, 30),
                words: Vec::new(),
            }],
            vec![TextBlock {
                text: "ab c".to_owned(),
                bounds: Rect::new(10, 60, 60, 20),
                words: vec![
                    Word {
                        text: "ab".to_owned(),
                        outline: quad(10, 60, 20),
                        bounds: Rect::new(10, 60, 20, 20),
                        characters: vec![
                            Character {
                                text: "a".to_owned(),
                                outline: quad(10, 60, 10),
                            },
                            Character {
                                text: "b".to_owned(),
                                outline: quad(20, 60, 10),
                            },
                        ],
                    },
                    Word {
                        text: "c".to_owned(),
                        outline: quad(40, 60, 30),
                        bounds: Rect::new(40, 60, 30, 20),
                        characters: Vec::new(),
                    },
                ],
            }],
        ]);
        let all = from_recognition(&recognition);
        assert_eq!(
            all.iter()
                .map(|u| (u.text.as_str(), u.line, u.word))
                .collect::<Vec<_>>(),
            vec![("whole line", 0, 0), ("a", 1, 1), ("b", 1, 1), ("c", 1, 2)]
        );
        assert_eq!(all[0].outline[2], Point::new(110, 50));
        assert_eq!(
            all[2].outline,
            quad(20, 60, 10),
            "a character's own outline"
        );
        assert_eq!(
            all[3].outline,
            quad(40, 60, 30),
            "a word without characters"
        );
        // And selecting all of it copies exactly what the automatic copy does.
        assert_eq!(text_between(&all, 0, 3), recognition.text());
    }
}
