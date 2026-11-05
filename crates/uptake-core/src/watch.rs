//! Watch areas: noticing that a region of the screen changed (roadmap `1.58`,
//! `ADR-0048`).
//!
//! A watch area reads its own region every few seconds and tells the user when
//! what it shows changes, or when it stops changing. This module is the part
//! of that which needs no screen: what the user asked the area to watch for
//! ([`Watch`]), a small summary of one reading ([`Fingerprint`]), and the rule
//! that turns a run of readings into the two moments worth a notification
//! ([`Detector`]). Taking the readings and raising the notification are the
//! host's (`src-tauri/src/watch.rs`).
//!
//! # `ADR-0048`'s four conditions
//!
//! An area may read its region repeatedly only if the user placed it, it is
//! visibly marked while it reads, reading is switched on per area and off by
//! default, and what it reads stays on the machine. [`Watch::Off`] is the
//! default for that reason, and nothing here keeps a reading: a
//! [`Fingerprint`] is at most 256 brightness averages, which cannot be turned
//! back into the picture of any region larger than 16 by 16 pixels. A region
//! under 16 pixels on a side keeps one value per pixel on that side, which is
//! its greyscale picture at that size; it is held in memory, compared and
//! dropped like any other (review of `#154`, round 9).
//!
//! # Why a grid of averages
//!
//! Two readings of a still screen are not identical bytes: a blinking caret,
//! a clock's colon and the compositor's own rounding all move a few pixels.
//! Comparing the average brightness of each cell of a coarse grid ignores a
//! change that small, because a few pixels move a cell's average by a level or
//! two, and still sees a new line of text, a progress bar moving or a window
//! appearing, which move whole cells. [`CELL_DELTA`] is the line between the
//! two; it is a first value, chosen on arithmetic and not yet tried on a real
//! screen.

use serde::{Deserialize, Serialize};

use crate::bitmap::RgbaBitmap;

/// What a watch area tells the user about (roadmap `1.58`). Chosen per area
/// from its right-click menu.
///
/// Every area carries one, so a conversion to `Watch` has a field to set
/// rather than an optional to create; only a watch area's means anything, and
/// a conversion sets it back to [`Watch::Off`] whatever it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Watch {
    /// Not watching. The default (`ADR-0048`: off until the user switches it
    /// on, for each area).
    #[default]
    Off,
    /// Notify when the region starts to change after being still: a new chat
    /// message, a window appearing, a queue moving.
    Changes,
    /// Notify when the region stops changing after it had been changing: a
    /// download bar or a render reaching its end.
    Settles,
}

impl Watch {
    /// Whether the area is reading its region at all.
    #[must_use]
    pub const fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// How many still readings end a change for this choice: a short calm
    /// re-arms *changes*, a long one is needed to call something finished.
    #[must_use]
    pub const fn settle_readings(self) -> u32 {
        match self {
            Self::Changes => REARM_READINGS,
            _ => SETTLE_READINGS,
        }
    }

    /// Whether `event` is the one this choice asked to be told about.
    #[must_use]
    pub const fn notifies(self, event: Event) -> bool {
        matches!(
            (self, event),
            (Self::Changes, Event::Changed) | (Self::Settles, Event::Settled)
        )
    }
}

/// The grid a reading is summarised into: at most this many cells across and
/// down. A region narrower or shorter than this gets one cell per pixel on
/// that axis.
pub const GRID: u32 = 16;

/// How far one cell's average brightness must move, in levels of 255, before
/// the region counts as changed.
///
/// **Six, measured on a real screen, 2026-11-03** (the maintainer's rig, a
/// 106 by 62 pixel watch area over two thin toolbar icons that appear and
/// disappear on a dark background). Showing or hiding the icons moved the
/// busiest cell by exactly 12 levels, and readings of the still screen
/// differed by 0. The first value, 12, was chosen on arithmetic and compared
/// with `>`, so that real change sat exactly on the line and was never
/// reported. Six is half of it.
///
/// What it still ignores: on a 1,000 by 600 region a cell is about 60 by 40
/// pixels, so a caret 2 by 16 pixels moves its cell's average by about three
/// levels, under six. One line of new text moves a cell by tens of levels.
///
/// ⚠️ **The smaller the region, the bigger a cell's share of each pixel.** On
/// a region a few lines tall a cell is a handful of pixels, and a caret
/// blinking in it does count as a change. A known limit.
pub const CELL_DELTA: u8 = 6;

/// How far a cell may move between two CONSECUTIVE readings and the region
/// still count as still, once it has started changing.
///
/// Much finer than [`CELL_DELTA`], and that difference is the point. A change
/// is found against the reading from before it started, so a slow one adds up
/// until it crosses [`CELL_DELTA`]; its end is found between neighbouring
/// readings, where a bar growing by a few pixels a reading moves a cell by a
/// few levels and must not read as stopped. Round 1 of the review of `#154`
/// measured the alternative: with one threshold for both, a bar growing 4 px a
/// reading was reported changed and finished sixteen times in one fill.
///
/// ⚠️ **A change slower than this reads as still.** A cell moving by two levels
/// or less per reading, which is under a pixel row of a dark bar on a 40-pixel
/// cell, settles while it moves. A known limit of this first value.
pub const STILL_DELTA: u8 = 2;

/// How many readings in a row must match before a region that was changing
/// counts as settled, for *Notify when it stops changing*. With the host
/// reading every half second, twelve is six seconds of stillness: long enough
/// that a progress bar pausing between chunks is not reported as finished.
/// (Three readings at two seconds until 2026-11-03: the same six seconds.)
pub const SETTLE_READINGS: u32 = 12;

/// How many still readings re-arm *Notify when it changes* after a change:
/// one second at the host's half-second reading. Until 2026-11-03 this mode
/// waited the full [`SETTLE_READINGS`] too, so at the maintainer's rig a
/// second change within six seconds of the first was never reported. This
/// mode reports change, not completion, so a short calm is enough.
pub const REARM_READINGS: u32 = 2;

/// One reading, summarised: the average brightness of each cell of a grid laid
/// over the region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    columns: u32,
    rows: u32,
    cells: Vec<u8>,
}

impl Fingerprint {
    /// Summarises `bitmap`. `None` for an empty bitmap, which has nothing to
    /// summarise.
    #[must_use]
    pub fn of(bitmap: &RgbaBitmap) -> Option<Self> {
        let (width, height) = (bitmap.width(), bitmap.height());
        if width == 0 || height == 0 {
            return None;
        }
        let columns = width.min(GRID);
        let rows = height.min(GRID);
        let mut sums = vec![0_u64; (columns * rows) as usize];
        let mut counts = vec![0_u64; (columns * rows) as usize];
        let pixels = bitmap.pixels();
        for y in 0..height {
            let row = (u64::from(y) * u64::from(rows) / u64::from(height)) as u32;
            for x in 0..width {
                let column = (u64::from(x) * u64::from(columns) / u64::from(width)) as u32;
                let at = ((y * width + x) * 4) as usize;
                let Some(&[r, g, b, _]) = pixels.get(at..at + 4) else {
                    continue;
                };
                let cell = (row * columns + column) as usize;
                if let (Some(sum), Some(count)) = (sums.get_mut(cell), counts.get_mut(cell)) {
                    *sum += u64::from(luma(r, g, b));
                    *count += 1;
                }
            }
        }
        let cells = sums
            .iter()
            .zip(&counts)
            .map(|(&sum, &count)| u8::try_from(sum / count.max(1)).unwrap_or(u8::MAX))
            .collect();
        Some(Self {
            columns,
            rows,
            cells,
        })
    }

    /// How far the cell that moved most between `self` and `other` moved, in
    /// levels of 255. [`u8::MAX`] when the grids differ (the area was resized),
    /// which is as different as two readings get.
    #[must_use]
    pub fn moved(&self, other: &Self) -> u8 {
        if (self.columns, self.rows) != (other.columns, other.rows) {
            return u8::MAX;
        }
        self.cells
            .iter()
            .zip(&other.cells)
            .map(|(&a, &b)| a.abs_diff(b))
            .max()
            .unwrap_or(0)
    }

    /// Whether `other` shows a different region: some cell moved by more than
    /// [`CELL_DELTA`], or the grid changed.
    #[must_use]
    pub fn differs(&self, other: &Self) -> bool {
        self.moved(other) > CELL_DELTA
    }
}

/// Paints `parts` of `bitmap` black, so they take no part in a comparison.
/// `parts` are in the bitmap's own pixels, top left at `0, 0`; whatever falls
/// outside the bitmap is ignored.
///
/// For UP-TAKE's own drawing inside a watch area. With *Show UP-TAKE in
/// screen recordings* on, the area's border and its *watching* mark are in
/// every reading, and hiding them while the reading is taken would make the
/// mark blink out exactly while the area reads, which `ADR-0048` names as the
/// case to revisit. So they are left in the picture and left out of the
/// comparison instead: the same black in every reading never differs.
pub fn leave_out(bitmap: &mut RgbaBitmap, parts: &[crate::geometry::Rect]) {
    let whole = crate::geometry::Rect::new(0, 0, bitmap.width(), bitmap.height());
    let width = bitmap.width();
    let pixels = bitmap.pixels_mut();
    for part in parts {
        let Some(part) = part.intersection(whole) else {
            continue;
        };
        let (x0, y0) = (part.origin.x.max(0) as u32, part.origin.y.max(0) as u32);
        for y in y0..y0 + part.size.height {
            let start = ((y * width + x0) * 4) as usize;
            let end = start + (part.size.width * 4) as usize;
            if let Some(row) = pixels.get_mut(start..end) {
                for pixel in row.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }
    }
}

/// Brightness of one pixel, Rec. 601 weights in integers.
const fn luma(r: u8, g: u8, b: u8) -> u8 {
    let weighted = 299 * r as u32 + 587 * g as u32 + 114 * b as u32;
    (weighted / 1000) as u8
}

/// One of the two moments a watch area can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The region was still and now differs from its last reading.
    Changed,
    /// The region had been changing and has now held still for the
    /// detector's settle count of readings ([`Watch::settle_readings`]).
    Settled,
}

/// Turns a run of readings into [`Event`]s, one area's worth.
///
/// Two comparisons, one for each moment. **A change starts** when a reading
/// differs by more than [`CELL_DELTA`] from the *baseline*, the reading the
/// region was last still at, so a slow change adds up until it counts. **A
/// change ends** when the settle count of readings in a row
/// ([`Watch::settle_readings`]: [`REARM_READINGS`] for *Notify when it
/// changes*, [`SETTLE_READINGS`] for *Notify when it stops changing*) each
/// differ by no more than [`STILL_DELTA`] from the first reading of that
/// still run; the last becomes the new baseline, wherever the region stopped.
/// Each is reported once, so a video under a watch area raises one
/// notification while it plays, not one a reading.
#[derive(Debug, Clone)]
pub struct Detector {
    baseline: Option<Fingerprint>,
    /// The first reading of the current still run. Stillness is measured
    /// against it, not only against the reading before, so a change too slow
    /// to show between two readings still adds up and keeps the run from
    /// counting as still (which matters more at a half-second reading).
    still_from: Option<Fingerprint>,
    changing: bool,
    still: u32,
    settle: u32,
}

impl Default for Detector {
    fn default() -> Self {
        Self::with_settle(SETTLE_READINGS)
    }
}

impl Detector {
    /// A detector that has seen nothing yet. Its first reading only sets the
    /// baseline: there is nothing to compare it with, and the region the user
    /// just placed the area over has not *changed*.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A detector whose change ends after `settle` still readings (at least
    /// one): [`Watch::settle_readings`] for the area's choice.
    #[must_use]
    pub fn with_settle(settle: u32) -> Self {
        Self {
            baseline: None,
            still_from: None,
            changing: false,
            still: 0,
            settle: settle.max(1),
        }
    }

    /// Takes one reading and says whether it is a moment worth reporting.
    pub fn observe(&mut self, reading: Fingerprint) -> Option<Event> {
        let Some(baseline) = &self.baseline else {
            self.baseline = Some(reading.clone());
            self.still_from = Some(reading);
            return None;
        };
        if !self.changing {
            if !baseline.differs(&reading) {
                return None;
            }
            self.changing = true;
            self.still = 0;
            self.still_from = Some(reading);
            return Some(Event::Changed);
        }
        let moved = self
            .still_from
            .as_ref()
            .map_or(u8::MAX, |from| from.moved(&reading));
        if moved > STILL_DELTA {
            self.still = 0;
            self.still_from = Some(reading);
            return None;
        }
        self.still += 1;
        if self.still < self.settle {
            return None;
        }
        self.changing = false;
        self.still = 0;
        self.baseline = Some(reading);
        Some(Event::Settled)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CELL_DELTA, Detector, Event, Fingerprint, GRID, SETTLE_READINGS, Watch, leave_out,
    };
    use crate::bitmap::RgbaBitmap;
    use crate::geometry::Rect;
    use crate::geometry::Size;

    /// A `width` by `height` bitmap of one grey level.
    fn grey(width: u32, height: u32, level: u8) -> RgbaBitmap {
        let pixels = [level, level, level, 255].repeat((width * height) as usize);
        RgbaBitmap::from_pixels(Size::new(width, height), pixels).unwrap_or_else(|| {
            RgbaBitmap::transparent(Size::new(1, 1)).unwrap_or_else(|| unreachable!())
        })
    }

    /// `base` with the rectangle `x, y, w, h` painted `level`.
    fn with_patch(
        mut base: RgbaBitmap,
        (x, y, w, h): (u32, u32, u32, u32),
        level: u8,
    ) -> RgbaBitmap {
        let width = base.width();
        let pixels = base.pixels_mut();
        for row in y..y + h {
            for column in x..x + w {
                let at = ((row * width + column) * 4) as usize;
                pixels[at..at + 3].copy_from_slice(&[level, level, level]);
            }
        }
        base
    }

    fn print(bitmap: &RgbaBitmap) -> Fingerprint {
        Fingerprint::of(bitmap).unwrap_or_else(|| unreachable!("a non-empty bitmap"))
    }

    #[test]
    fn off_is_the_default_and_reads_nothing() {
        assert_eq!(Watch::default(), Watch::Off);
        assert!(!Watch::Off.is_on());
        assert!(Watch::Changes.is_on());
        assert!(Watch::Settles.is_on());
    }

    #[test]
    fn each_choice_is_told_about_its_own_moment_only() {
        assert!(Watch::Changes.notifies(Event::Changed));
        assert!(!Watch::Changes.notifies(Event::Settled));
        assert!(Watch::Settles.notifies(Event::Settled));
        assert!(!Watch::Settles.notifies(Event::Changed));
        assert!(!Watch::Off.notifies(Event::Changed));
        assert!(!Watch::Off.notifies(Event::Settled));
    }

    #[test]
    fn an_empty_bitmap_has_no_fingerprint() {
        let empty = RgbaBitmap::from_pixels(Size::new(0, 0), Vec::new());
        assert!(empty.is_none_or(|bitmap| Fingerprint::of(&bitmap).is_none()));
    }

    #[test]
    fn the_same_picture_does_not_differ() {
        let picture = with_patch(grey(320, 200, 240), (10, 10, 100, 20), 20);
        assert!(!print(&picture).differs(&print(&picture.clone())));
    }

    /// A caret: 2 by 16 pixels turning black on white, in a 960 by 640 region
    /// whose cells are 60 by 40. About three levels on one cell, so ignored.
    #[test]
    fn a_blinking_caret_is_not_a_change() {
        let before = grey(960, 640, 250);
        let after = with_patch(before.clone(), (300, 200, 2, 16), 0);
        assert!(!print(&before).differs(&print(&after)));
    }

    /// A new line of text: a dark band across most of the region's width.
    #[test]
    fn a_new_line_of_text_is_a_change() {
        let before = grey(320, 200, 250);
        let after = with_patch(before.clone(), (10, 150, 300, 8), 0);
        assert!(print(&before).differs(&print(&after)));
    }

    /// The real screen it was tuned on (2026-11-03): two toolbar icons
    /// appearing moved the busiest cell by exactly 12 levels, which the first
    /// threshold did not report. That change must count.
    #[test]
    fn the_measured_icons_are_a_change() {
        let before = grey(GRID, GRID, 100);
        let after = grey(GRID, GRID, 112);
        assert_eq!(print(&before).moved(&print(&after)), 12);
        assert!(print(&before).differs(&print(&after)));
    }

    /// Exactly the line: a whole cell moving by `CELL_DELTA` is not a change,
    /// and one level more is.
    #[test]
    fn the_line_is_cell_delta_exclusive() {
        let base = grey(GRID, GRID, 100);
        assert!(!print(&base).differs(&print(&grey(GRID, GRID, 100 + CELL_DELTA))));
        assert!(print(&base).differs(&print(&grey(GRID, GRID, 101 + CELL_DELTA))));
    }

    #[test]
    fn a_resized_region_differs() {
        assert!(print(&grey(320, 200, 100)).differs(&print(&grey(8, 8, 100))));
    }

    /// A region smaller than the grid gets one cell per pixel and still works.
    #[test]
    fn a_tiny_region_is_summarised_per_pixel() {
        let before = grey(3, 2, 0);
        let after = with_patch(before.clone(), (2, 1, 1, 1), 255);
        assert!(print(&before).differs(&print(&after)));
    }

    #[test]
    fn the_first_reading_is_a_baseline_not_a_change() {
        let mut detector = Detector::new();
        assert_eq!(detector.observe(print(&grey(64, 64, 10))), None);
    }

    /// The whole story of a download bar: still, then moving for several
    /// readings, then still for good. One `Changed` at the start, one
    /// `Settled` after `SETTLE_READINGS` still readings, nothing in between.
    #[test]
    fn a_change_is_reported_once_and_its_end_once() {
        let mut detector = Detector::new();
        let mut events = Vec::new();
        let mut level = 10_u8;
        events.push(detector.observe(print(&grey(64, 64, level))));
        events.push(detector.observe(print(&grey(64, 64, level))));
        for _ in 0..4 {
            level += 40;
            events.push(detector.observe(print(&grey(64, 64, level))));
        }
        for _ in 0..SETTLE_READINGS + 2 {
            events.push(detector.observe(print(&grey(64, 64, level))));
        }
        let reported: Vec<_> = events.into_iter().flatten().collect();
        assert_eq!(reported, vec![Event::Changed, Event::Settled]);
    }

    /// A pause shorter than `SETTLE_READINGS` is not the end.
    #[test]
    fn a_short_pause_is_not_settling() {
        let mut detector = Detector::new();
        detector.observe(print(&grey(64, 64, 10)));
        assert_eq!(
            detector.observe(print(&grey(64, 64, 60))),
            Some(Event::Changed)
        );
        for _ in 1..SETTLE_READINGS {
            assert_eq!(detector.observe(print(&grey(64, 64, 60))), None);
        }
        assert_eq!(
            detector.observe(print(&grey(64, 64, 110))),
            None,
            "moving again"
        );
        for _ in 1..SETTLE_READINGS {
            assert_eq!(detector.observe(print(&grey(64, 64, 110))), None);
        }
        assert_eq!(
            detector.observe(print(&grey(64, 64, 110))),
            Some(Event::Settled)
        );
    }

    /// A bar growing slowly across a 960 by 640 region, stopping at reading 80:
    /// one `Changed` near the start and one `Settled` after it stops, at every
    /// rate from 4 to 20 pixels a reading. Round 1 of the review of `#154`
    /// found 16 of each at 4 px with one threshold for both moments.
    #[test]
    fn a_slow_bar_is_one_change_and_one_end() {
        for step in [4_u32, 10, 20] {
            let mut detector = Detector::new();
            let mut reported = Vec::new();
            for reading in 0..80 + SETTLE_READINGS + 5 {
                let filled = (reading.min(80) * step).min(940);
                let picture = with_patch(grey(960, 640, 240), (10, 300, filled.max(1), 8), 20);
                if let Some(event) = detector.observe(print(&picture)) {
                    reported.push(event);
                }
            }
            assert_eq!(
                reported,
                vec![Event::Changed, Event::Settled],
                "{step} px a reading"
            );
        }
    }

    /// What is left out never differs, whatever was drawn there.
    #[test]
    fn what_is_left_out_takes_no_part() {
        let mut before = grey(320, 200, 250);
        let mut after = with_patch(before.clone(), (0, 0, 100, 20), 0);
        let mark = [Rect::new(0, 0, 100, 20), Rect::new(300, 190, 50, 50)];
        leave_out(&mut before, &mark);
        leave_out(&mut after, &mark);
        assert!(!print(&before).differs(&print(&after)));
        let mut elsewhere = with_patch(grey(320, 200, 250), (100, 100, 200, 60), 0);
        leave_out(&mut elsewhere, &mark);
        assert!(
            print(&before).differs(&print(&elsewhere)),
            "the rest still counts"
        );
    }

    /// The maintainer's rig, 2026-11-03: icons switched every few seconds. In
    /// *changes* mode each switch is reported once the region has been calm
    /// for [`REARM_READINGS`], not only the first in every six seconds.
    #[test]
    fn changes_mode_reports_each_change_after_a_short_calm() {
        let mut detector = Detector::with_settle(Watch::Changes.settle_readings());
        let (a, b) = (print(&grey(64, 64, 10)), print(&grey(64, 64, 60)));
        let mut reported = Vec::new();
        detector.observe(a.clone());
        // Switch every three readings: one changed reading, then a calm of
        // two, which re-arms.
        for round in 0..6 {
            let next = if round % 2 == 0 { &b } else { &a };
            for _ in 0..3 {
                reported.extend(detector.observe(next.clone()));
            }
        }
        let changes = reported
            .iter()
            .filter(|event| **event == Event::Changed)
            .count();
        assert_eq!(changes, 6, "{reported:?}");
        assert_eq!(Watch::Settles.settle_readings(), SETTLE_READINGS);
    }

    /// Slower than [`STILL_DELTA`] between two readings, which a half-second
    /// reading makes common: measured from the start of the still run, the
    /// drift adds up and a moving bar does not settle until it stops.
    #[test]
    fn a_bar_slower_than_the_still_delta_does_not_settle_while_it_moves() {
        for step in [1_u32, 2] {
            let mut detector = Detector::new();
            let mut reported = Vec::new();
            for reading in 0..120_u32 {
                let filled = (reading.min(90) * step).min(940);
                let picture = with_patch(grey(960, 640, 240), (10, 300, filled.max(1), 8), 20);
                if let Some(event) = detector.observe(print(&picture)) {
                    reported.push((reading, event));
                }
            }
            let settled: Vec<_> = reported
                .iter()
                .filter(|(_, event)| *event == Event::Settled)
                .collect();
            assert_eq!(settled.len(), 1, "{step} px a reading: {reported:?}");
            assert!(settled[0].0 >= 90, "{step} px: settled at {}", settled[0].0);
        }
    }

    /// After settling, the next change is reported again.
    #[test]
    fn a_settled_region_reports_its_next_change() {
        let mut detector = Detector::new();
        detector.observe(print(&grey(64, 64, 10)));
        detector.observe(print(&grey(64, 64, 60)));
        for _ in 0..SETTLE_READINGS {
            detector.observe(print(&grey(64, 64, 60)));
        }
        assert_eq!(
            detector.observe(print(&grey(64, 64, 200))),
            Some(Event::Changed)
        );
    }
}
