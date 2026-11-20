//! Watch areas: noticing that a region of the screen changed (roadmap `1.58`,
//! `ADR-0048`).
//!
//! A watch area reads its own region every few seconds and tells the user when
//! what it shows changes, when it stops changing, or, over a progress bar,
//! when the bar is full (roadmap `1.61`). This module is the part of that
//! which needs no screen: what the user asked the area to watch for
//! ([`Watch`]), a small summary of one reading ([`Fingerprint`]), and the rule
//! that turns a run of readings into the moments worth a notification
//! ([`Detector`], and its `Bar` for a progress bar). Taking the readings and
//! raising the notification are the host's (`src-tauri/src/watch.rs`).
//!
//! # `ADR-0048`'s four conditions
//!
//! An area may read its region repeatedly only if the user placed it, it is
//! visibly marked while it reads, reading is switched on per area, and what it
//! reads stays on the machine. Since `ADR-0053` making a watch area is what
//! switches it on, and pausing it (the area's `held` flag) stops it for a
//! while; this module only compares. Nothing here keeps a reading beyond the
//! comparison: a [`Fingerprint`] is the average brightness of each [`CELL`]
//! by [`CELL`] pixel square of the region, which is the region's greyscale
//! picture at a quarter of its size. That is a picture, not a summary, and
//! it is held in memory, compared and dropped like any other reading.
//! (Until roadmap `1.61` it was at most 16 by 16 averages whatever the
//! region's size; [`CELL`] says why that changed.)
//!
//! # Why squares of averages
//!
//! Two readings of a still screen are not identical bytes: the compositor's
//! own rounding moves a few levels here and there. Comparing the average
//! brightness of small squares ignores that, and still sees an icon, a word,
//! a progress bar moving or a window appearing, each of which moves at least
//! one square by tens of levels. [`CELL_DELTA`] is the line between the two,
//! measured on a real screen. How MUCH of the region must change is the
//! area's own choice, its tolerance ([`Tuning::share_permille`]).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::bitmap::RgbaBitmap;

/// What a watch area tells the user about (roadmap `1.58`). Chosen per area
/// from its right-click menu.
///
/// Every area carries one, so a conversion to `Watch` has a field to set
/// rather than an optional to create; only a watch area's means anything, and
/// a conversion sets it back to the default whatever it was.
///
/// **There is no "off" here, since `ADR-0053`.** A watch area starts watching
/// the moment it is made, in [`Watch::Changes`], and the user stops it for a
/// while by pausing it, which is the area's `held` flag
/// (`crate::area::Area::held`) and keeps this choice for when it resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Watch {
    /// Notify when the region starts to change after being still: a new chat
    /// message, a window appearing, a queue moving. The default (`ADR-0053`
    /// decision 1, the founder's choice of 2026-11-03).
    #[default]
    Changes,
    /// Notify when the region stops changing after it had been changing: a
    /// download bar or a render reaching its end.
    Settles,
    /// Notify when a progress bar under the area is full (roadmap `1.61`,
    /// the maintainer's choice of 2026-11-10): the area is placed tightly
    /// over the bar, the first reading shows what its empty part looks like,
    /// and the area notifies once the fill reaches the far end, plus at each
    /// of the area's own percentages ([`Tuning::marks`]). Which end the bar
    /// fills from is [`Tuning::direction`]. See [`Bar`] for the rule.
    Fills,
}

impl Watch {
    /// How many still readings end a change for this choice: a short calm
    /// re-arms *changes*, a long one is needed to call something finished.
    #[must_use]
    pub const fn settle_readings(self) -> u32 {
        match self {
            Self::Changes => REARM_READINGS,
            Self::Settles | Self::Fills => SETTLE_READINGS,
        }
    }

    /// How long a calm ends a change for this choice before the area's own
    /// tuning is applied: [`REARM_TIME`] re-arms *changes*, and
    /// [`SETTLE_READINGS`] readings at [`READING_STEP`] (six seconds) call
    /// something finished. The host replaces the second with the area's
    /// [`Tuning::settle_ms`] before every reading ([`Detector::tune`]), so a
    /// running area waits as long as its panel says.
    #[must_use]
    pub fn settle_time(self) -> Duration {
        match self {
            Self::Changes => REARM_TIME,
            _ => READING_STEP * self.settle_readings(),
        }
    }

    /// Whether `event` is the one this choice asked to be told about.
    #[must_use]
    pub const fn notifies(self, event: Event) -> bool {
        matches!(
            (self, event),
            (Self::Changes, Event::Changed)
                | (Self::Settles, Event::Settled)
                | (Self::Fills, Event::Filled | Event::Reached(_))
        )
    }
}

/// The time [`Detector::observe`] takes to pass between two readings, and so
/// the unit the settle counts are given in. The host reads when a frame
/// touches the area (the event-driven watch area, 2026-11-05), at no fixed
/// rate, and gives each reading its real time ([`Detector::observe_at`]), so
/// a calm is measured in time, whatever the rate.
pub const READING_STEP: Duration = Duration::from_millis(500);

/// The side of one cell, in screen pixels: a reading is the average
/// brightness of each square this size (roadmap `1.61`, the maintainer's
/// choice of 2026-11-07, *fixed-size cells*). A larger region has more cells,
/// not larger ones, so a small change counts the same on any area.
///
/// **Why it changed.** Until `1.61` every region was a grid of at most 16 by
/// 16 cells. On a monitor-sized area that made each cell about 14,400
/// pixels, so one changed pixel moved its cell by about 0.02 levels, and at
/// the maintainer's rig (2026-11-07) a small change on a large area never
/// fired. At 4 pixels a cell is 16 pixels: an icon or a word moves at least
/// one cell by tens of levels on any area size.
///
/// **Why four, not eight** (review of `#168`, round 4). The tolerance slider
/// starts at *Any change*, his *"any pixel"*. At 8 pixels one pixel moved its
/// 64-pixel cell by at most 3 levels, under [`CELL_DELTA`], so no single
/// pixel ever counted. At 4 pixels one pixel moves its cell by its own change
/// divided by 16: **a single pixel counts when it changes by more than about
/// 100 levels of 255** (`CELL_DELTA * 16`), which a glyph, an icon edge or a
/// status light does. A fainter single pixel still does not, and that is the
/// floor *Any change* means. Four times as many cells as at 8 pixels: about
/// 130,000 for a 1080p area, compared on every reading.
///
/// A power of two, so the GPU's mip chain averages exactly one cell per
/// texel at level [`CELL_LEVEL`].
pub const CELL: u32 = 4;

/// The mip level whose texels are one [`CELL`] each: the level the GPU route
/// reads back.
pub const CELL_LEVEL: u32 = CELL.ilog2();

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
/// ⚠️ **A blinking caret counts, on any area size, since `1.61`.** A cell is
/// 4 by 4 pixels, and a caret 2 by 16 pixels fills half of four cells,
/// moving each by tens of levels. Until then a caret was ignored on a large
/// area (its cell was about 60 by 40 pixels) and counted on a small one. The
/// answer is the area's tolerance ([`Tuning::share_permille`]): a share
/// larger than the caret's leaves it out.
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
/// counts as settled, for *Notify when it stops changing*, at
/// [`READING_STEP`]: six seconds of stillness ([`Watch::settle_time`]), long
/// enough that a progress bar pausing between chunks is not reported as
/// finished. The event-driven host measures it in time, at no fixed rate.
/// (Three readings at two seconds until 2026-11-03: the same six seconds.)
pub const SETTLE_READINGS: u32 = 12;

/// How many still readings re-arm *Notify when it changes* after a change, at
/// [`READING_STEP`] (one second): the count [`Detector::observe`] uses. The
/// event-driven host re-arms after [`REARM_TIME`] instead. Until 2026-11-03 this mode
/// waited the full [`SETTLE_READINGS`] too, so at the maintainer's rig a
/// second change within six seconds of the first was never reported. This
/// mode reports change, not completion, so a short calm is enough.
pub const REARM_READINGS: u32 = 2;

/// How long a calm re-arms *Notify when it changes* for the event-driven
/// host. The maintainer's rig, 2026-11-05: a region switched back and forth
/// faster than a second notified only once, and he asked for every change
/// (*"as responsive and realtime as possible"*). A sixth of a second is long
/// enough for a change's own redraw to finish, so one change is one
/// notification, and short enough for a hand toggling something to be seen
/// every time. The count above is the same rule at half-second readings.
pub const REARM_TIME: Duration = Duration::from_millis(150);

/// One reading: the average brightness of each [`CELL`]-pixel square of the
/// region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    columns: u32,
    rows: u32,
    cells: Vec<u8>,
    /// Cells that were under one of UP-TAKE's own windows when this was read
    /// (a notification card, the settings panel): what the screen shows there
    /// is unknown, so every comparison skips them (`I-480`, review of `#160`,
    /// round 1). Painting them out instead, as [`leave_out`] does for the
    /// area's own border, would itself read as a change when such a window
    /// comes or goes, since the black is not in every reading.
    hidden: Vec<bool>,
    /// How many of the region's pixels the last column and the last row of
    /// cells hold, across and down: [`CELL`] where the region's side is a
    /// multiple of it, less where it is not. A cell's weight in the
    /// tolerance is the pixels it holds (review of `#168`, round 2).
    edge: (u32, u32),
}

impl Fingerprint {
    /// Summarises `bitmap`, a region read at full size. `None` for an empty
    /// bitmap, which has nothing to summarise.
    #[must_use]
    pub fn of(bitmap: &RgbaBitmap) -> Option<Self> {
        Self::of_hiding(bitmap, &[])
    }

    /// [`Self::of`], with every cell that any of `hide` touches marked
    /// unknown. `hide` is in the bitmap's own pixels, top left at `0, 0`;
    /// whatever falls outside the bitmap is ignored.
    #[must_use]
    pub fn of_hiding(bitmap: &RgbaBitmap, hide: &[crate::geometry::Rect]) -> Option<Self> {
        let region = crate::geometry::Size::new(bitmap.width(), bitmap.height());
        Self::of_region(bitmap, region, hide)
    }

    /// Summarises `bitmap`, a picture of a region `region` pixels in size,
    /// read at that size or smaller (the GPU route reads it averaged down to
    /// one texel per [`CELL`]). The cells are the region's: one per [`CELL`]
    /// pixels on each side, rounded up, and never more than the bitmap has
    /// pixels. `hide` is in the bitmap's own pixels, as for
    /// [`Self::of_hiding`].
    #[must_use]
    pub fn of_region(
        bitmap: &RgbaBitmap,
        region: crate::geometry::Size,
        hide: &[crate::geometry::Rect],
    ) -> Option<Self> {
        let mut fingerprint = Self::summarise(bitmap, region)?;
        let (width, height) = (bitmap.width(), bitmap.height());
        let whole = crate::geometry::Rect::new(0, 0, width, height);
        let (columns, rows) = (fingerprint.columns, fingerprint.rows);
        // The pixel-to-cell mapping in `summarise` ([`cell_on`]) is
        // monotonic, so a rectangle's cells are those of its first and last
        // pixel and all between.
        let column = |x: u32| cell_on(x, width, region.width, columns);
        let row = |y: u32| cell_on(y, height, region.height, rows);
        for part in hide {
            let Some(part) = part.intersection(whole) else {
                continue;
            };
            let (x0, y0) = (part.origin.x.max(0) as u32, part.origin.y.max(0) as u32);
            let (x1, y1) = (x0 + part.size.width - 1, y0 + part.size.height - 1);
            for r in row(y0)..=row(y1) {
                for c in column(x0)..=column(x1) {
                    if let Some(cell) = fingerprint.hidden.get_mut((r * columns + c) as usize) {
                        *cell = true;
                    }
                }
            }
        }
        Some(fingerprint)
    }

    /// Takes the value of every cell `self` did not know and `seen` shows,
    /// as if it had been read that way: what lay under a window that has gone
    /// is learned at its first sight, not reported as a change, and is watched
    /// from then on. Nothing for a grid of another size.
    fn learn(&mut self, seen: &Self) {
        if (self.columns, self.rows) != (seen.columns, seen.rows) {
            return;
        }
        for ((hidden, cell), (&seen_hidden, &seen_cell)) in self
            .hidden
            .iter_mut()
            .zip(self.cells.iter_mut())
            .zip(seen.hidden.iter().zip(&seen.cells))
        {
            if *hidden && !seen_hidden {
                *hidden = false;
                *cell = seen_cell;
            }
        }
    }

    fn summarise(bitmap: &RgbaBitmap, region: crate::geometry::Size) -> Option<Self> {
        let (width, height) = (bitmap.width(), bitmap.height());
        if width == 0 || height == 0 {
            return None;
        }
        let columns = region.width.div_ceil(CELL).clamp(1, width);
        let rows = region.height.div_ceil(CELL).clamp(1, height);
        let mut sums = vec![0_u64; (columns * rows) as usize];
        let mut counts = vec![0_u64; (columns * rows) as usize];
        let pixels = bitmap.pixels();
        for y in 0..height {
            let row = cell_on(y, height, region.height, rows);
            for x in 0..width {
                let column = cell_on(x, width, region.width, columns);
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
        // The GPU route's last texel on a side needs no correction: its
        // padding repeats the region's own last column and row
        // (`average_box` in `uptake-capture`), so a one-pixel edge is the
        // whole texel, not a sixteenth of it. (Until round 10 of the review
        // of `#168` the padding was black and this scaled the texel back,
        // which cannot undo what the 8-bit texel already lost.)
        let cells = sums
            .iter()
            .zip(&counts)
            .map(|(&sum, &count)| u8::try_from(sum / count.max(1)).unwrap_or(u8::MAX))
            .collect();
        let last = |region: u32, cells: u32| {
            region
                .saturating_sub((cells - 1).saturating_mul(CELL))
                .clamp(1, CELL)
        };
        Some(Self {
            columns,
            rows,
            cells,
            hidden: vec![false; (columns * rows) as usize],
            edge: (last(region.width, columns), last(region.height, rows)),
        })
    }

    /// How far the cell that moved most between `self` and `other` moved, in
    /// levels of 255, among the cells both know. [`u8::MAX`] when the grids
    /// differ (the area was resized), which is as different as two readings
    /// get.
    #[must_use]
    pub fn moved(&self, other: &Self) -> u8 {
        if (self.columns, self.rows) != (other.columns, other.rows) {
            return u8::MAX;
        }
        self.cells
            .iter()
            .zip(&other.cells)
            .zip(self.hidden.iter().zip(&other.hidden))
            .filter(|(_, (a_hidden, b_hidden))| !**a_hidden && !**b_hidden)
            .map(|((&a, &b), _)| a.abs_diff(b))
            .max()
            .unwrap_or(0)
    }

    /// Whether `other` shows a different region: some cell moved by more than
    /// [`CELL_DELTA`], or the grid changed. The tolerance at its lowest,
    /// *any change*; [`Self::differs_by`] takes the area's own.
    #[must_use]
    pub fn differs(&self, other: &Self) -> bool {
        self.differs_by(other, CELL_DELTA, 0)
    }

    /// Whether enough of the region moved by more than `delta` levels
    /// between `self` and `other`: the cells that moved hold at least
    /// `share_permille` thousandths of the pixels in the cells both know,
    /// and at least one cell moved. Each cell weighs the pixels it holds, so
    /// a part-cell at the region's edge counts for its part (review of
    /// `#168`, round 2: counted as a whole cell, a 1 by 8 edge on a 9 by 8
    /// area passed *half the area*). A changed grid (the area was resized)
    /// always differs.
    #[must_use]
    pub fn differs_by(&self, other: &Self, delta: u8, share_permille: u16) -> bool {
        if (self.columns, self.rows, self.edge) != (other.columns, other.rows, other.edge) {
            return true;
        }
        let (mut known, mut moved) = (0_u64, 0_u64);
        for (at, ((&a, &b), (&a_hidden, &b_hidden))) in self
            .cells
            .iter()
            .zip(&other.cells)
            .zip(self.hidden.iter().zip(&other.hidden))
            .enumerate()
        {
            if a_hidden || b_hidden {
                continue;
            }
            let weight = self.weight(at);
            known += weight;
            if a.abs_diff(b) > delta {
                moved += weight;
            }
        }
        moved > 0 && moved * 1000 >= known * u64::from(share_permille.min(SHARE_PERMILLE_MAX))
    }

    /// How many of the region's pixels cell `at` holds.
    fn weight(&self, at: usize) -> u64 {
        let at = at as u32;
        let (column, row) = (at % self.columns, at / self.columns);
        let across = if column + 1 == self.columns {
            self.edge.0
        } else {
            CELL
        };
        let down = if row + 1 == self.rows {
            self.edge.1
        } else {
            CELL
        };
        u64::from(across) * u64::from(down)
    }

    /// How many cells across and down.
    #[must_use]
    pub const fn grid(&self) -> (u32, u32) {
        (self.columns, self.rows)
    }
}

/// Which cell along one side a bitmap pixel at `at` belongs to, for a
/// bitmap `side` pixels long of a region `region` pixels long with `cells`
/// cells. A full-size picture (`side >= region`) is cut into true [`CELL`]
/// pixel cells, so the last cell holds what is left (review of `#168`,
/// round 1: a proportional split made cells of uneven size); a picture
/// already one texel per cell (the GPU route) maps texel to cell.
fn cell_on(at: u32, side: u32, region: u32, cells: u32) -> u32 {
    let cell = if side >= region {
        at / CELL
    } else if side == cells {
        at
    } else {
        (u64::from(at) * u64::from(cells) / u64::from(side.max(1))) as u32
    };
    cell.min(cells.saturating_sub(1))
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

/// Which of an area's events become a notification card (roadmap `1.65`,
/// the maintainer's choice of 2026-11-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notify {
    /// No card. The area still watches, and its events still count.
    Never,
    /// One card per burst: the first event after [`Tuning::calm_ms`] without
    /// one.
    Once,
    /// A card, or a count on the card already shown, for every event.
    #[default]
    Each,
}

/// How a watch area reads and reports, set per area in its panel (roadmap
/// `1.65`, the maintainer's choices of 2026-11-07). Kept in milliseconds so
/// it crosses to the page as plain numbers; [`Self::clamped`] keeps each in
/// its range, whatever the page sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    /// Which events become a card.
    pub notify: Notify,
    /// For [`Notify::Once`]: how long without an event ends a burst, so the
    /// next event notifies again. His wish: *"the option to set a custom
    /// calm (reset) duration"*.
    pub calm_ms: u32,
    /// The minimum gap after an event before the next may fire; `0` is no
    /// limit. An event inside the gap is dropped, not delayed.
    pub gap_ms: u32,
    /// The least time between two readings (the sampling rate); `0` is
    /// realtime, every frame that touches the area, up to about 30 a second.
    pub sample_ms: u32,
    /// The tolerance (roadmap `1.61`, the maintainer's choice of
    /// 2026-11-03): the share of the area, in thousandths, that must change
    /// before it counts. `0` is *any change* (one [`CELL`]), the default;
    /// [`SHARE_PERMILLE_MAX`] is the whole area. The same share decides when
    /// a change has ended: the region is still once fewer cells than that
    /// are still moving, so a caret blinking inside a larger tolerance
    /// neither starts a change nor keeps one from settling.
    pub share_permille: u16,
    /// For [`Watch::Settles`]: how long the region must hold still before it
    /// counts as stopped (the maintainer, 2026-11-10: *"it should behave
    /// based on the settings that are set"*). Until then a fixed six seconds
    /// nobody could see or change, and at his rig it never seemed to fire.
    pub settle_ms: u32,
    /// For [`Watch::Fills`]: which end the bar fills from.
    pub direction: Direction,
    /// For [`Watch::Fills`]: the percentages at which the area notifies on
    /// the way, besides the full bar (the maintainer, 2026-11-10: *"the user
    /// may also set percentages where he wants to get additional
    /// notifications, at 20% or 50% for example"*). `0` is an unused slot;
    /// each other value is 1 to 99. At most [`MARKS`].
    pub marks: [u8; MARKS],
}

/// Which end a progress bar fills from, for [`Watch::Fills`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// From the left edge to the right: most bars, so the default.
    #[default]
    LeftToRight,
    /// From the right edge to the left.
    RightToLeft,
    /// From the top edge down.
    TopToBottom,
    /// From the bottom edge up.
    BottomToTop,
}

/// How many percentages an area may notify at on the way to a full bar
/// ([`Tuning::marks`]). Four covers his examples (*"at 20% or 50%"*) with
/// room to spare, and keeps [`Tuning`] a plain value the page sends whole.
pub const MARKS: usize = 4;

/// The highest percentage a mark may name: 100 is the full bar itself, which
/// always notifies.
pub const MARK_MAX: u8 = 99;

impl Default for Tuning {
    fn default() -> Self {
        Self {
            notify: Notify::Each,
            calm_ms: 200,
            gap_ms: 0,
            sample_ms: 0,
            share_permille: 0,
            settle_ms: 1_000,
            direction: Direction::LeftToRight,
            marks: [0; MARKS],
        }
    }
}

/// [`Tuning::calm_ms`]'s range: 100 ms to ten seconds. Events arrive at most
/// once a captured frame (about 34 ms), so a calm shorter than that makes
/// *Once* do what *Every time* does (the maintainer, 2026-11-10: *"0ms is
/// NOT valid in this case"*), and the slider moves in 100 ms steps whose
/// lowest must be a whole step (*"the min must be either 0, 100ms or
/// multiple of 100ms"*).
pub const CALM_MS: (u32, u32) = (100, 10_000);
/// [`Tuning::gap_ms`]'s longest: a minute.
pub const GAP_MS_MAX: u32 = 60_000;
/// [`Tuning::sample_ms`]'s longest: one reading every five seconds.
pub const SAMPLE_MS_MAX: u32 = 5_000;
/// [`Tuning::share_permille`]'s largest: the whole area. Half the area until
/// 2026-11-10, his first word (*"from any pixel to half the area"*), until
/// his rig pass that day: *"A slider range must always provide as much
/// flexibility as possible, lowest valid value to highest valid value"*.
pub const SHARE_PERMILLE_MAX: u16 = 1_000;
/// [`Tuning::settle_ms`]'s range: 100 ms to half a minute. While a change
/// settles the host reads every 50 ms (`SETTLE_POLL`), so a shorter calm
/// waits the same, and the slider's lowest whole step is 100 ms.
pub const SETTLE_MS: (u32, u32) = (100, 30_000);

impl Tuning {
    /// `self` with every value in its range.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            notify: self.notify,
            calm_ms: self.calm_ms.clamp(CALM_MS.0, CALM_MS.1),
            gap_ms: self.gap_ms.min(GAP_MS_MAX),
            sample_ms: self.sample_ms.min(SAMPLE_MS_MAX),
            share_permille: self.share_permille.min(SHARE_PERMILLE_MAX),
            settle_ms: self.settle_ms.clamp(SETTLE_MS.0, SETTLE_MS.1),
            direction: self.direction,
            marks: self.marks.map(|mark| mark.min(MARK_MAX)),
        }
    }

    /// How long after a reading at `last` the next may be taken, at `at`:
    /// `None` when it may be taken now.
    #[must_use]
    pub fn sample_wait(self, last: Option<Duration>, at: Duration) -> Option<Duration> {
        let step = Duration::from_millis(u64::from(self.sample_ms));
        let since = at.saturating_sub(last?);
        (since < step).then(|| step - since)
    }
}

/// What [`EventGate::admit`] made of one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admitted {
    /// The event fires: it was outside the minimum gap.
    pub fires: bool,
    /// It fires and becomes a card.
    pub notifies: bool,
}

/// Applies an area's [`Tuning`] to the events its [`Detector`] reports: the
/// minimum gap, then the notification choice. One per area.
#[derive(Debug, Clone, Copy, Default)]
pub struct EventGate {
    /// When the last event fired.
    fired: Option<Duration>,
    /// When the last event arrived, fired or not: a burst is measured on
    /// what the screen did, not on what the gap let through.
    arrived: Option<Duration>,
    /// Whether the current burst has had its card.
    noticed: bool,
}

impl EventGate {
    /// One event at `at` (a clock that only moves forward).
    pub fn admit(&mut self, tuning: Tuning, at: Duration) -> Admitted {
        let tuning = tuning.clamped();
        let calm = Duration::from_millis(u64::from(tuning.calm_ms));
        if self
            .arrived
            .is_none_or(|last| at.saturating_sub(last) >= calm)
        {
            self.noticed = false;
        }
        self.arrived = Some(at);
        let gap = Duration::from_millis(u64::from(tuning.gap_ms));
        if self.fired.is_some_and(|last| at.saturating_sub(last) < gap) {
            return Admitted {
                fires: false,
                notifies: false,
            };
        }
        self.fired = Some(at);
        let notifies = match tuning.notify {
            Notify::Never => false,
            Notify::Each => true,
            Notify::Once => !std::mem::replace(&mut self.noticed, true),
        };
        Admitted {
            fires: true,
            notifies,
        }
    }
}

/// One of the moments a watch area can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The region was still and now differs from its last reading.
    Changed,
    /// The region had been changing and has now held still for the
    /// detector's settle time ([`Watch::settle_time`]).
    Settled,
    /// For [`Watch::Fills`]: the bar is full.
    Filled,
    /// For [`Watch::Fills`]: the bar reached one of the area's percentages
    /// ([`Tuning::marks`]), the percentage it reached.
    Reached(u8),
}

/// Turns a run of readings into [`Event`]s, one area's worth.
///
/// Two comparisons, one for each moment. **A change starts** when a reading
/// differs by more than [`CELL_DELTA`] from the *baseline*, the reading the
/// region was last still at, so a slow change adds up until it counts. **A
/// change ends** when the region has held still for the settle time
/// ([`Watch::settle_time`]: [`REARM_TIME`] for *Notify when it changes*, and
/// for *Notify when it stops changing* the area's own [`Tuning::settle_ms`]
/// once [`Detector::tune`] has run, six seconds before), every reading in that time
/// within [`STILL_DELTA`] of the first reading of the still run; the last
/// becomes the new baseline, wherever the region stopped.
///
/// **A detector made for *Notify when it changes* ([`Self::for_watch`])
/// also reports every further step while a change is under way**: a reading
/// that differs by more than [`CELL_DELTA`] from the one the region last held
/// is another [`Event::Changed`] (the maintainer's choice of 2026-11-07, after
/// his rig pass: about ten changes a second notified once, since the
/// [`REARM_TIME`] calm never came). The host reads at most once a frame, so
/// that is at most one a frame, and a video under the area counts up its card
/// while it plays. A step smaller than [`CELL_DELTA`] (a fade, a slow bar)
/// adds nothing. Any other detector reports a change once and its end once.
#[derive(Debug, Clone)]
pub struct Detector {
    baseline: Option<Fingerprint>,
    /// The first reading of the current still run. Stillness is measured
    /// against it, not only against the reading before, so a change too slow
    /// to show between two readings still adds up and keeps the run from
    /// counting as still (which matters more the more often the host reads:
    /// up to every 50 ms while a change settles).
    still_from: Option<Fingerprint>,
    changing: bool,
    /// When `still_from` was read: the calm is measured from it.
    still_since: Duration,
    /// How long a calm ends a change.
    settle: Duration,
    /// Whether each further full step of a change under way is reported too
    /// ([`Self::for_watch`] with [`Watch::Changes`]).
    every_step: bool,
    /// The area's tolerance ([`Tuning::share_permille`]), set by the host
    /// before each reading ([`Self::set_share`]).
    share_permille: u16,
    /// The time [`Self::observe`] gives a reading: [`READING_STEP`] after
    /// the last one.
    clock: Duration,
    /// For [`Watch::Fills`]: the bar's state, in place of the comparisons
    /// above. `None` for every other choice.
    bar: Option<Bar>,
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
    /// one) at [`READING_STEP`]: [`Watch::settle_readings`] for the area's
    /// choice.
    #[must_use]
    pub fn with_settle(settle: u32) -> Self {
        Self::with_settle_time(READING_STEP * settle.max(1))
    }

    /// A detector whose change ends after `settle` of calm:
    /// [`Watch::settle_time`] for the area's choice.
    #[must_use]
    pub fn with_settle_time(settle: Duration) -> Self {
        Self {
            baseline: None,
            still_from: None,
            changing: false,
            still_since: Duration::ZERO,
            settle,
            every_step: false,
            share_permille: 0,
            clock: Duration::ZERO,
            bar: None,
        }
    }

    /// The detector an area set to `watch` reads with: its settle time
    /// ([`Watch::settle_time`]), and for [`Watch::Changes`] every full step
    /// of a change reported, not only its start.
    #[must_use]
    pub fn for_watch(watch: Watch) -> Self {
        Self {
            every_step: watch == Watch::Changes,
            bar: (watch == Watch::Fills).then(Bar::default),
            ..Self::with_settle_time(watch.settle_time())
        }
    }

    /// Takes the area's tolerance ([`Tuning::share_permille`]) for the
    /// readings from now on. A change already under way keeps going, and
    /// ends by the new share.
    pub fn set_share(&mut self, share_permille: u16) {
        self.share_permille = share_permille.min(SHARE_PERMILLE_MAX);
    }

    /// Takes an area's [`Tuning`] for the readings from now on: its
    /// tolerance, and for [`Watch::Settles`] how long a calm ends a change
    /// ([`Tuning::settle_ms`]). *Notify when it changes* keeps its own short
    /// re-arm ([`REARM_TIME`]).
    pub fn tune(&mut self, watch: Watch, tuning: Tuning) {
        let tuning = tuning.clamped();
        self.set_share(tuning.share_permille);
        if watch == Watch::Settles {
            self.settle = Duration::from_millis(u64::from(tuning.settle_ms));
        }
        if let Some(bar) = &mut self.bar {
            bar.tune(tuning);
        }
    }

    /// Whether a change is under way, so the host must keep reading while the
    /// screen is still, or the calm that ends it is never seen. A bar that has
    /// reached one of its percentages and has not yet held it for
    /// [`MARK_CALM`] is under way in the same sense.
    #[must_use]
    pub fn is_settling(&self) -> bool {
        self.changing || self.bar.as_ref().is_some_and(Bar::is_waiting)
    }

    /// The host now reads this region by another route, whose pictures cannot
    /// be compared with the last ones (another size, so every cell differs):
    /// `reading`, made at `at`, becomes both the baseline and the start of the
    /// calm, and nothing is reported. A change under way stays under way, so
    /// it still ends with [`Event::Settled`] once the region holds still
    /// (review of `up-take` #162, rounds 5 and 6). A change made in the very
    /// reading that switched cannot be told from the switch, and is not
    /// reported.
    pub fn rebase_at(&mut self, reading: Fingerprint, at: Duration) {
        self.clock = at;
        if let Some(bar) = &mut self.bar {
            bar.learn(reading);
            return;
        }
        self.baseline = Some(reading.clone());
        self.still_from = Some(reading);
        self.still_since = at;
    }

    /// The host could not read the region for a while, for a reason that
    /// says nothing about what is under it (the page has not confirmed the
    /// area's mark, as when another program's window lies over the overlay):
    /// for the bar mode, what the bar was learned to be is kept, and the next
    /// readings wait, as after a cover, until the page around the bar looks as
    /// it did at the last clear reading ([`Bar::page_back`]), or until the
    /// area has held wholly still for [`RELEARN_AFTER`] and the bar is learned
    /// again from it (`I-493`). Learning the
    /// bar again from the first reading after the wait learned Win+Shift+S's
    /// darkened screen as the empty bar, and a selection dragged over it then
    /// read as a full bar (the maintainer's rig trace of `#178`, 2026-11-20).
    /// Nothing for the other choices, which the host starts again instead.
    pub fn hold(&mut self) {
        if let Some(bar) = &mut self.bar
            && bar.empty.is_some()
        {
            bar.covering = true;
            bar.held = [None; MARKS];
            bar.attached = None;
        }
    }

    /// Whether this detector keeps what it learned over a wait the host could
    /// not read through ([`Self::hold`]): the bar mode's, once it has learned
    /// the bar.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.bar.as_ref().is_some_and(|bar| bar.empty.is_some())
    }

    /// Takes one reading [`READING_STEP`] after the last one and says whether
    /// it is a moment worth reporting.
    pub fn observe(&mut self, reading: Fingerprint) -> Option<Event> {
        let at = self.clock + READING_STEP;
        self.observe_at(reading, at)
    }

    /// Takes one reading made at `at` (any clock that only moves forward,
    /// the same for every reading) and says whether it is a moment worth
    /// reporting. A change ends once the region has held still for the
    /// settle time, so the host reads again while a change is settling,
    /// frames or not.
    pub fn observe_at(&mut self, reading: Fingerprint, at: Duration) -> Option<Event> {
        self.clock = at;
        if let Some(bar) = &mut self.bar {
            return bar.observe(reading, at);
        }
        for known in [&mut self.baseline, &mut self.still_from]
            .into_iter()
            .flatten()
        {
            known.learn(&reading);
        }
        let Some(baseline) = &self.baseline else {
            self.baseline = Some(reading.clone());
            self.still_from = Some(reading);
            return None;
        };
        let share = self.share_permille;
        if !self.changing {
            if !baseline.differs_by(&reading, CELL_DELTA, share) {
                return None;
            }
            self.changing = true;
            self.still_from = Some(reading);
            self.still_since = at;
            return Some(Event::Changed);
        }
        let (moving, stepped) = self.still_from.as_ref().map_or((true, true), |from| {
            (
                from.differs_by(&reading, STILL_DELTA, share),
                from.differs_by(&reading, CELL_DELTA, share),
            )
        });
        if moving {
            self.still_from = Some(reading);
            self.still_since = at;
            // A full step from what the region last held: another change,
            // for an area that asked to hear of every one.
            return (self.every_step && stepped).then_some(Event::Changed);
        }
        if at.saturating_sub(self.still_since) < self.settle {
            return None;
        }
        self.changing = false;
        // A cell the settling reading could not see keeps the old baseline's
        // value: what was under a card when the region settled is the last
        // thing known there, so a change made under the card shows once the
        // card goes. Left unknown, the card's going would teach the baseline
        // that change as if it had always been (review of `#160`, round 3).
        let mut settled = reading;
        if let Some(previous) = &self.baseline {
            settled.learn(previous);
        }
        self.baseline = Some(settled);
        Some(Event::Settled)
    }
}

/// A progress bar's state, for [`Watch::Fills`] (roadmap `1.61`, the
/// maintainer's design of 2026-11-10, corrected after his rig passes of
/// 2026-11-11 and 2026-11-19).
///
/// # The rule
///
/// The area is cut into *lines* across the way the bar fills: columns of
/// cells for a bar that fills sideways, rows for one that fills up or down
/// ([`Direction`]). The first reading is the bar as the user placed the area
/// over it, and four things are learned from it:
///
/// - **where the bar is**: an area is rarely drawn exactly on a bar, so the
///   lines at either end that look like that end's outermost line (the page
///   around the bar) are *margin*, and the cells of the remaining lines that
///   differ from the page in their own row are the bar's *track*. Only the
///   track is read, so the page above and below a bar and the corners of a
///   round end take no part. Rows that hold almost no track (a one-pixel
///   shadow) are left out, and of the stretches of lines that hold track,
///   separated by more than [`GAP_LINES`] lines of page, only the one with
///   the most track is the bar: a label beside the bar, or a second bar, is
///   not part of it. A gap of up to [`GAP_LINES`] lines inside it (the break
///   between the blocks of a segmented bar) is part of the bar and is passed
///   over. An area drawn tightly over the bar, or over a bar whose track
///   looks like the page, has no margin, and every cell is read;
/// - **what each line looks like empty**: the line as that reading shows it;
/// - **which end lines are not the bar's**: a border or a round end cap
///   looks unlike the lines inside it, whether or not it ever fills. Such an
///   *end line* (at most [`CAP_LINES`] at each end) that has not changed is
///   passed over, and one that has changed counts as filled;
/// - **how far the bar was already filled**: the lines from the start end
///   that do not look like the bar's far end, which a bar that is not yet
///   full still shows empty. End lines do not decide it.
///
/// From then on a line counts as filled when it no longer looks like its
/// empty self (more than half of its track in cells that moved by more than
/// [`CELL_DELTA`]), or, for a line already filled at the first reading, when
/// it still looks as it did then. The bar's progress is the run of filled
/// lines from the bar's start end, as a share of the bar's length, so a
/// percentage label inside the bar that changes ahead of the fill does not
/// count until the fill reaches it, and a bar whose start is not filled has
/// no progress, whatever else moves in it. The bar is **full** when every
/// line that can fill has filled: nothing is asked of what the far end looks
/// like, since a gradient, stripes or a shimmer make it unlike the start
/// (the maintainer's rig pass of 2026-11-19).
///
/// **A reading in which the page around the bar changed is not read at all.**
/// A fill never changes the page; anything drawn over the whole area does.
/// Observed once, on the maintainer's rig on 2026-11-11 and not probed
/// since: with Win+Shift+S open, an area without this rule reported his
/// percentages and the full bar at once. **And once covered, the bar stays
/// unread until the page looks again as it did before**
/// ([`Bar::page_back`]): on 2026-11-20 a snipping selection dragged over part
/// of the area brought most of the page back bright and left the rest of the
/// bar dark, which read as a fill. **Or until the area has held wholly still
/// for [`RELEARN_AFTER`]** (five seconds), when the bar is learned again from
/// it: a page changed for good (a theme switch) otherwise left the area deaf
/// (`I-493`).
///
/// # What notifies, once per fill
///
/// [`Event::Filled`] when the bar is full, and [`Event::Reached`] the first
/// time the progress reaches one of the area's percentages
/// ([`Tuning::marks`]); several reached in one reading report only the
/// highest. A percentage is never reported early, and is reported once the
/// bar has held it for [`MARK_CALM`] (half a second), so that the block of an
/// indeterminate bar, which grows into the track from the start end as a fill
/// does and then leaves it, reports nothing; or at once, when every reading
/// for [`STEADY_FILL`] (three seconds) has shown the bar filled from its start
/// end, which the block never stays. The host keeps reading while one
/// waits ([`Detector::is_settling`]). The full bar is not held. Nothing at or
/// below the first reading's progress
/// notifies: an area placed over a bar at 60% does not report 20% and 50%. A
/// fill ends when the progress falls below half of the highest it reported
/// (a bar that empties for the next file), and the next fill notifies again.
///
/// ⚠️ **Known limits.** With no margin (an area drawn tightly over the bar),
/// a window moved over the bar or a darkened screen changes every line,
/// which reads as a full bar; and with *Show UP-TAKE in screen recordings*
/// on, the area's own border covers the bar's last 4 pixels, so the bar is
/// full once the last line UP-TAKE can see is filled (review of `#170`,
/// round 3). A label beside the bar is left out only when at least
/// [`GAP_LINES`] + 1 lines of page (a dozen pixels) lie between them. A fill
/// within [`CELL_DELTA`] of the empty colour is never seen. A bar that is
/// already full at the first reading reads as empty, and so does one filled
/// past the [`FAR_LINES`] lines before its far cap (the last two dozen
/// pixels or so, about 94% of a 200-pixel bar): with every line it reads
/// the empty level from already filled, one picture cannot tell that bar
/// from an empty one in another colour, and it does not report that fill's
/// end (review of `#178`, round 6). Its next fill, after it empties, is
/// reported. An indeterminate bar reports a percentage below its block's
/// length on every sweep when the block, its front past that percentage,
/// stays joined to the start end for longer than [`MARK_CALM`]: a fill and
/// that block look alike until the block leaves the start end (reviews of
/// `#178`, rounds 11 and 12). On the test page, a block a third of the bar
/// long and a 20% mark, that is a sweep slower than about six and a half
/// seconds; its 1.5, 3 and 6 second sweeps report nothing
/// (`tests/bar_fixtures.rs`). Placed anywhere short of that, it is read correctly; and a bar that shows a
/// block of one colour at its far end then (the sweep of an indeterminate
/// bar, caught at the wrong moment) reads as filled up to it: both are the
/// same picture as a half-filled bar.
#[derive(Debug, Clone, Default)]
struct Bar {
    /// The first reading: what each line looks like empty, and, before
    /// [`Self::start`], filled.
    empty: Option<Fingerprint>,
    /// The bar's first and last line, in fill order; the lines outside them
    /// are not the bar.
    ends: (u32, u32),
    /// Per cell, whether it is on the bar's track. Empty when the area has
    /// no margin: every cell is.
    track: Vec<bool>,
    /// Per cell, whether it is page the bar never covers: known, off the
    /// track, and looking like the page at the first reading. A darkened
    /// screen changes it; a fill does not. Empty when the area has no margin.
    page: Vec<bool>,
    /// Per line, whether it holds any track: the others are gaps. Empty when
    /// the area has no margin: every line does.
    live: Vec<bool>,
    /// Per line, whether it is a line at the start end ([`CAP_LINES`]) that
    /// looks unlike the lines inside it. Empty when there is none.
    odd: Vec<bool>,
    /// The last lines of the bar that are not round caps or borders, in fill
    /// order: the full bar's last line is one of them, and must be filled to
    /// its edge ([`Self::reaches_the_end`]).
    finish: Vec<u32>,
    /// What the empty track looks like: the level of the lines before the
    /// bar's far end.
    empty_level: i32,
    /// The line the first reading showed the fill reaching, from the start
    /// end: the bar's lines before it were already filled, and count as filled
    /// while they look unlike the empty track.
    start: u32,
    /// The highest percentage reported in this fill, or the first reading's
    /// progress: nothing at or below it notifies.
    reported: u8,
    /// The progress the last reading showed.
    seen: u8,
    /// Which end the bar fills from.
    direction: Direction,
    /// The area's percentages ([`Tuning::marks`]).
    marks: [u8; MARKS],
    /// For each, since when the progress has been at it without a break; `None`
    /// while it is below it. A percentage is reported once it has been held
    /// for [`MARK_CALM`].
    held: [Option<Duration>; MARKS],
    /// The last reading whose page was not covered: while [`Self::covering`],
    /// the page must look like it again before the bar is read.
    clear: Option<Fingerprint>,
    /// Whether the page around the bar was covered at a reading and has not
    /// yet come back ([`Self::page_back`]).
    covering: bool,
    /// While [`Self::covering`]: the reading the area has held still since,
    /// and when. An area whose page stays changed, and that holds wholly still
    /// for [`RELEARN_AFTER`], shows a new page, and the bar is learned again
    /// from it.
    still: Option<(Duration, Fingerprint)>,
    /// Since when every reading has shown the bar filled from its start end,
    /// with nothing over it: a fill that has done so for [`STEADY_FILL`] is
    /// not an indeterminate bar's sweep, and its percentages need no calm.
    attached: Option<Duration>,
}

/// How long the bar must stay at a percentage before the area reports it:
/// half a second. The sweep of an indeterminate bar (a block that runs along the
/// track) grows from the start end like a fill for as long as it takes to
/// come in, and then leaves it again; a fill that reaches a percentage keeps
/// it. A block of a third of the track crosses 20% within a few tenths of a
/// second. The full bar is not held: nothing short of a bar filled to the end
/// reads as full.
pub const MARK_CALM: Duration = Duration::from_millis(500);

/// How long a fill must have grown from the bar's start end, seen at every
/// reading, before a percentage it reaches is reported at once, without
/// [`MARK_CALM`]: three seconds. The sweep of an indeterminate bar stays
/// joined to the start end for well under a second (the test page's, about
/// half a second), and then leaves it; a fill never does. The calm made every
/// percentage half a second late, and at a 20-second fill 2.5% late (the
/// founder's rig pass of `#178`, 2026-11-20: a 50% mark came at about 54%).
pub const STEADY_FILL: Duration = Duration::from_secs(3);

/// How long the page around a bar must stay changed and still before the bar
/// is learned again from it: five seconds, the founder's choice of
/// 2026-11-20 (UP-TAKE backlog `I-493`). A theme switch, a scrolled page or
/// another tab under the area changed the page for good, and the area never
/// read again. Known risk, his trade: a Win+Shift+S selection held still this
/// long is learned as the bar, and dragging it on can report one false full
/// bar.
pub const RELEARN_AFTER: Duration = Duration::from_secs(5);

/// How many lines of page may lie between two stretches of a bar and still
/// leave them one bar: the break between the blocks of a segmented bar (4
/// pixels, one line or two by where the cells fall). A label beside a bar is
/// further off than that.
const GAP_LINES: u32 = 2;

/// How many lines at each end of a bar may be a border or a round cap, which
/// look unlike the lines inside it: a round end of a 34-pixel bar is nine
/// pixels deep, at a scale of 1.5 thirteen, three lines of four.
const CAP_LINES: usize = 3;

/// How many lines before the far end's cap the empty track's level is read
/// from, at the first reading: enough that a segmented bar's half-gap line is
/// outvoted, few enough that a bar placed nearly full still shows them empty.
const FAR_LINES: usize = 3;

/// How far, as a share of its neighbours' movement, the last line of a bar
/// must have moved for the fill to have reached the end. A bar that ends in
/// a border the same colour as its track (a dark theme's) cannot be told from
/// one a few pixels short, and a border is a quarter of a four-pixel cell at
/// a scale of 1, half of it at 2, and more where the area's edge cuts the
/// bar (measured on the rig's renders at 1.25: a last line 38% covered), and
/// a round tip has a narrow last line that is covered late (a vertical bar at
/// 97% had one 29% covered, and must not be full): a line that moved by less
/// than 35% of its neighbours' share is short, and one that moved by that
/// much is full.
const REACHES_THE_END_PERCENT: u64 = 35;

/// How far a cell must be from the page, in levels, to be the bar's track:
/// twice [`CELL_DELTA`], so that the one-pixel border of a bar and the
/// anti-aliased edge of a round end, which are a quarter of a cell, are not.
const TRACK_DELTA: u8 = 2 * CELL_DELTA;

/// How far, in levels, a line may be from the empty track and still look like
/// it: a line of a segmented bar that is half block and half gap is half the
/// track's difference from the page off it, and a fill is further than that
/// from the track (the maintainer's two themes: 89 and 114).
const EMPTY_DELTA: u32 = 3 * CELL_DELTA as u32;

/// How far from the middle cell of a line, in levels, a cell may be for it to
/// count as the same flat colour, as a border's are.
const BORDER_SPREAD: u8 = CELL_DELTA / 2;

/// How far a cell may be from the page, in levels, and still be page the
/// bar's fill never reaches. A cell with a pixel of the bar's edge in it moves
/// when the fill comes, by about four times what it was off the page (a fill
/// differs from the track about as much as the track from the page, or more),
/// so it must be within a level or two to stay under [`CELL_DELTA`].
const PAGE_DELTA: u8 = 1;

/// How far apart, in hundredths of a level, a line at the bar's far end may
/// be from the lines inside it before it counts as a border or a round cap
/// that does not fill to the edge: 1.5 levels. A flat bar's lines are the
/// same to the level; a one-pixel border in a four-pixel cell is two.
const CAP_APART: u64 = 150;

impl Bar {
    /// Takes the area's direction and percentages. A new direction starts
    /// again from the next reading, since every line changes meaning. New
    /// percentages count from where the bar already is: one the bar has
    /// passed is not reported late (review of `#170`, round 4).
    fn tune(&mut self, tuning: Tuning) {
        let tuning = tuning.clamped();
        if tuning.direction != self.direction {
            self.direction = tuning.direction;
            self.empty = None;
        }
        if tuning.marks != self.marks {
            self.reported = self.reported.max(self.seen);
            self.held = [None; MARKS];
        }
        self.marks = tuning.marks;
    }

    /// Whether a percentage has been reached and is waiting out its calm.
    fn is_waiting(&self) -> bool {
        self.held.iter().any(Option::is_some)
    }

    /// Whether cell `at` is on the bar's track.
    fn on_track(&self, at: usize) -> bool {
        self.track.get(at).copied().unwrap_or(true)
    }

    /// Whether line `line` holds any track.
    fn is_live(&self, line: u32) -> bool {
        self.live.get(line as usize).copied().unwrap_or(true)
    }

    /// Whether line `line` is an end line that looks unlike its neighbours.
    fn is_odd(&self, line: u32) -> bool {
        self.odd.get(line as usize).copied().unwrap_or(false)
    }

    /// Whether the track of line `line` of `reading` looks different from
    /// that of `empty`: more than half of it, by pixels, in cells that moved
    /// by more than [`CELL_DELTA`]. `None` when no cell of it is known in
    /// both.
    fn changed(&self, empty: &Fingerprint, line: u32, reading: &Fingerprint) -> Option<bool> {
        empty
            .line_share(line, reading, line, self.direction, |x, y| {
                self.on_track(x) && self.on_track(y)
            })
            .map(|(moved, known)| moved * 2 > known)
    }

    /// How far, in hundredths of a level, the track of line `line` of
    /// `reading` is from that of line `other`, on average over the cells both
    /// know: 0 for two lines of a flat bar, more for one with a border or a
    /// round end at it.
    fn apart(&self, reading: &Fingerprint, line: u32, other: u32) -> u64 {
        reading
            .line_share_by(line, reading, other, self.direction, |x, y| {
                self.on_track(x) && self.on_track(y)
            })
            .map_or(0, |(sum, weight)| sum * 100 / weight)
    }

    /// Whether most of the track of line `line` of `reading` is one flat
    /// colour: at least two thirds of its cells within [`BORDER_SPREAD`]
    /// levels of the middle one. A border is, with the corners of a rounded
    /// one left over; a round cap, whose cells run from track to page, is not.
    fn is_flat(&self, reading: &Fingerprint, line: u32) -> bool {
        let direction = self.direction;
        let mut cells = Vec::new();
        for k in 0..reading.across(direction) {
            let (column, row) = reading.line_cell(line, k, direction);
            let at = (row * reading.columns + column) as usize;
            if let Some(&cell) = reading.cells.get(at)
                && !reading.hidden.get(at).copied().unwrap_or(true)
                && self.on_track(at)
            {
                cells.push(cell);
            }
        }
        cells.sort_unstable();
        let Some(&middle) = cells.get(cells.len() / 2) else {
            return false;
        };
        let near = cells
            .iter()
            .filter(|&&cell| cell.abs_diff(middle) <= BORDER_SPREAD)
            .count();
        near * 3 >= cells.len() * 2
    }

    /// The average level of the track of line `line` of `reading`, over the
    /// cells it knows; `None` when it knows none.
    fn level(&self, reading: &Fingerprint, line: u32) -> Option<i32> {
        let direction = self.direction;
        let (mut sum, mut weight) = (0_u64, 0_u64);
        for k in 0..reading.across(direction) {
            let (column, row) = reading.line_cell(line, k, direction);
            let at = (row * reading.columns + column) as usize;
            if let Some(&cell) = reading.cells.get(at)
                && !reading.hidden.get(at).copied().unwrap_or(true)
                && self.on_track(at)
            {
                let w = reading.weight(at);
                sum += w * u64::from(cell);
                weight += w;
            }
        }
        (weight > 0).then(|| i32::try_from(sum / weight).unwrap_or(0))
    }

    /// Learns the bar from `reading`, as if it were the first: the area was
    /// just made, turned, or is read by another route.
    fn learn(&mut self, reading: Fingerprint) {
        // The reading the bar is learned from is its first clear one.
        self.clear = Some(reading.clone());
        self.covering = false;
        self.attached = None;
        self.still = None;
        let direction = self.direction;
        let lines = reading.lines(direction);
        let across = reading.across(direction);
        let index = |line: u32, k: u32| {
            let (column, row) = reading.line_cell(line, k, direction);
            (row * reading.columns + column) as usize
        };
        let is_known = |at: usize| !reading.hidden.get(at).copied().unwrap_or(true);
        let known = |line: u32| (0..across).any(|k| is_known(index(line, k)));
        // Margin: a line that looks like its end's outermost known line, to
        // within one part in sixty-four of its pixels: a bar a cell thick in
        // an area many cells deep is a line's one part in ten.
        let page = |line: u32, edge: u32| {
            reading
                .line_share(line, &reading, edge, direction, |_, _| true)
                .is_none_or(|(moved, known)| moved * 64 <= known)
        };
        self.ends = (0, lines.saturating_sub(1));
        self.track = Vec::new();
        self.page = Vec::new();
        self.live = Vec::new();
        self.odd = Vec::new();
        self.finish = Vec::new();
        let first = (0..lines).find(|&line| known(line));
        let last = (0..lines).rev().find(|&line| known(line));
        if let (Some(first), Some(last)) = (first, last) {
            // With no margin the bar is every known line: the lines under
            // the area's own border, with UP-TAKE in captures, are not its
            // ends, and nothing can be compared with them.
            self.ends = (first, last);
            let low = (first..=last).find(|&line| !page(line, first));
            let high = (first..=last).rev().find(|&line| !page(line, last));
            // A bar is most of its area. Margins that meet, or that leave
            // less than half of it, are not margins: a tight area over a bar
            // whose fill and track are each one flat colour looks like that.
            if let (Some(low), Some(high)) = (low, high)
                && low <= high
                && (low > first || high < last)
                && (high - low + 1) * 2 >= lines
            {
                let edge = if low > first { first } else { last };
                // The page is one flat colour where the bar has not started:
                // an edge line with a cell of the bar's end in it (an area
                // drawn closer than a cell to the bar) is not page.
                let (mut least, mut most) = (u8::MAX, u8::MIN);
                for k in 0..across {
                    let at = index(edge, k);
                    if let (true, Some(&cell)) = (is_known(at), reading.cells.get(at)) {
                        least = least.min(cell);
                        most = most.max(cell);
                    }
                }
                if most.saturating_sub(least) <= CELL_DELTA {
                    self.find_track(&reading, (low, high), edge);
                }
            }
        }
        let (low, high) = self.ends;
        // The end lines, and how far the bar was already filled, from the
        // lines that hold track.
        let live: Vec<u32> = (low..=high)
            .filter(|&line| self.is_live(line) && self.level(&reading, line).is_some())
            .collect();
        let mut odd = vec![false; lines as usize];
        let mut finish = Vec::new();
        let mut empty_level = 0;
        let mut start = low;
        if let Some(&last_live) = live.last() {
            let level = |line: u32| self.level(&reading, line).unwrap_or(0);
            let like = |a: i32, b: i32| a.abs_diff(b) <= u32::from(CELL_DELTA);
            // Looks like the empty track: a line of half track and half gap
            // is that much off it.
            let empty_like = |a: i32, b: i32| a.abs_diff(b) <= EMPTY_DELTA;
            let deep = live.len() >= 2 * CAP_LINES + 2;
            // What the empty track looks like, read from the last FAR_LINES
            // lines before the far end's cap. When they look alike, their
            // middle level (one line would do for a flat bar, but a segmented
            // one has lines that are half gap). When they do not, the fill's
            // front is among them, the bar is nearly full, and the far-most of
            // them is the empty track. (Review of `#178`, rounds 5 and 6: the
            // far third, then a plain median of these lines, read a bar placed
            // past about 80%, then past about 90%, as empty, and it never read
            // full.)
            let far = if deep {
                let end = live.len() - CAP_LINES;
                let window = &live[end.saturating_sub(FAR_LINES).max(CAP_LINES)..end];
                let mut levels: Vec<i32> = window.iter().map(|&line| level(line)).collect();
                levels.sort_unstable();
                match (levels.first(), levels.last(), window.last()) {
                    (Some(&low), Some(&high), Some(&far_most)) if !empty_like(low, high) => {
                        level(far_most)
                    }
                    _ => levels
                        .get(levels.len() / 2)
                        .copied()
                        .unwrap_or_else(|| level(last_live)),
                }
            } else {
                level(last_live)
            };
            empty_level = far;
            let mut cap = vec![false; lines as usize];
            if deep {
                // By the finer measure of the lines themselves, cell by cell:
                // a round end or a one-pixel border is a level or two off in
                // the lines it touches, which the average level does not show.
                let near = live[CAP_LINES];
                for &line in &live[..CAP_LINES] {
                    odd[line as usize] = self.apart(&reading, line, near) > CAP_APART;
                }
                let inner = live[live.len() - 1 - CAP_LINES];
                for &line in &live[live.len() - CAP_LINES..] {
                    cap[line as usize] = self.apart(&reading, line, inner) > CAP_APART;
                    // One that looks unlike the track by a whole step, in one
                    // flat colour across the bar, is a border, not a cap
                    // (whose cells run from track to page): it does not fill,
                    // and is passed over when it has not changed.
                    odd[line as usize] = !like(level(line), far) && self.is_flat(&reading, line);
                }
            }
            finish = live
                .iter()
                .rev()
                .copied()
                .filter(|&line| !cap[line as usize])
                .take(4)
                .collect();
            finish.reverse();
            // The first line that is not an end line and starts a stretch of
            // lines that look like the far end is where the fill stops; the
            // lines of the bar's body before it were filled. A stretch is
            // longer than a gap, so a break between blocks does not stop it.
            let body: Vec<u32> = live
                .iter()
                .copied()
                .filter(|&line| !odd[line as usize])
                .collect();
            let stretch = GAP_LINES as usize + 1;
            let stops_at = (0..body.len()).find(|&at| {
                body[at..]
                    .iter()
                    .take(stretch)
                    .all(|&line| empty_like(level(line), far))
            });
            if let Some(at) = stops_at {
                if at > 0 {
                    start = body[at];
                }
            } else if let Some(&line) = body.last() {
                start = line;
            }
        }
        if odd.iter().any(|&odd| odd) {
            self.odd = odd;
        }
        self.finish = finish;
        self.empty_level = empty_level;
        self.start = start;
        let (percent, _) = self.progress(&reading, &reading).unwrap_or((0, false));
        self.reported = percent;
        self.seen = percent;
        self.held = [None; MARKS];
        self.empty = Some(reading);
    }

    /// Finds the track of an area with margin: the cells between `ends` that
    /// differ from the page row they sit in, taken from line `edge`. Sets
    /// the track, the page, the live lines and the bar's own ends, and leaves
    /// all four alone when no stretch of lines is most of the area.
    fn find_track(&mut self, reading: &Fingerprint, (low, high): (u32, u32), edge: u32) {
        let direction = self.direction;
        let lines = reading.lines(direction);
        let across = reading.across(direction);
        let index = |line: u32, k: u32| {
            let (column, row) = reading.line_cell(line, k, direction);
            (row * reading.columns + column) as usize
        };
        let is_known = |at: usize| !reading.hidden.get(at).copied().unwrap_or(true);
        // The page in each row: its first known cell from the edge inwards.
        let inwards: i64 = if edge < low { 1 } else { -1 };
        let rows: Vec<Option<u8>> = (0..across)
            .map(|k| {
                (0..=CAP_LINES as i64)
                    .filter_map(|step| u32::try_from(i64::from(edge) + inwards * step).ok())
                    .filter(|&line| line < lines)
                    .map(|line| index(line, k))
                    .find(|&at| is_known(at))
                    .and_then(|at| reading.cells.get(at).copied())
            })
            .collect();
        let unlike = |at: usize, k: u32| {
            rows.get(k as usize).copied().flatten().is_some_and(|page| {
                reading
                    .cells
                    .get(at)
                    .is_some_and(|&cell| cell.abs_diff(page) > TRACK_DELTA)
            })
        };
        // Which rows hold the bar: those with at least a third of the most
        // track any row holds.
        let mut per_row = vec![0_u64; across as usize];
        for line in low..=high {
            for k in 0..across {
                let at = index(line, k);
                if is_known(at) && unlike(at, k) {
                    per_row[k as usize] += 1;
                }
            }
        }
        let most = per_row.iter().copied().max().unwrap_or(0);
        let in_bar = |k: u32| most > 0 && per_row[k as usize] * 3 >= most;
        let mut track = vec![false; reading.cells.len()];
        let mut weight = vec![0_u64; lines as usize];
        for line in low..=high {
            for k in 0..across {
                let at = index(line, k);
                if is_known(at) && in_bar(k) && unlike(at, k) {
                    track[at] = true;
                    weight[line as usize] += 1;
                }
            }
        }
        // The stretch of lines with the most track is the bar; stretches
        // further apart than a gap are something else (a label).
        let mut best: Option<(u64, u32, u32)> = None;
        let mut current: Option<(u64, u32, u32, u32)> = None;
        for line in low..=high {
            let w = weight[line as usize];
            if w > 0 {
                current = Some(match current {
                    Some((sum, from, _, _)) => (sum + w, from, line, 0),
                    None => (w, line, line, 0),
                });
            } else if let Some((sum, from, to, dead)) = current {
                if dead + 1 > GAP_LINES {
                    if best.is_none_or(|(most, _, _)| sum > most) {
                        best = Some((sum, from, to));
                    }
                    current = None;
                } else {
                    current = Some((sum, from, to, dead + 1));
                }
            }
        }
        if let Some((sum, from, to, _)) = current
            && best.is_none_or(|(most, _, _)| sum > most)
        {
            best = Some((sum, from, to));
        }
        let Some((_, from, to)) = best else {
            return;
        };
        if (to - from + 1) * 2 < lines {
            return;
        }
        self.ends = (from, to);
        let mut live = vec![false; lines as usize];
        for line in 0..lines {
            if line < from || line > to {
                for k in 0..across {
                    track[index(line, k)] = false;
                }
            } else {
                live[line as usize] = weight[line as usize] > 0;
            }
        }
        // The page the bar never covers: off the track, the page's own level
        // (the most common one off the track) to a level or two, and not next
        // to the bar, whose own edge, and the anti-aliased pixels of its round
        // ends, a fill moves. An area drawn closer than a cell to the bar has
        // no such page, and a darkened screen is not seen there.
        let mut counts = [0_u64; 256];
        for (at, &cell) in reading.cells.iter().enumerate() {
            if is_known(at) && !track[at] {
                counts[usize::from(cell)] += reading.weight(at);
            }
        }
        let page_level = (0..256_usize)
            .max_by_key(|&level| counts[level])
            .unwrap_or(0);
        // The page is not what it looks like when it is the track's own level
        // (most of what is off the track is the bar's edge rows), or a blend
        // of the two: it is taken only well away from the track.
        let (mut sum, mut weight_sum) = (0_u64, 0_u64);
        for (at, &cell) in reading.cells.iter().enumerate() {
            if track[at] {
                sum += reading.weight(at) * u64::from(cell);
                weight_sum += reading.weight(at);
            }
        }
        let track_level = usize::try_from(sum / weight_sum.max(1)).unwrap_or(0);
        let page_known = page_level.abs_diff(track_level) > 2 * usize::from(CELL_DELTA);
        let is_page = |at: usize| {
            page_known
                && reading.cells.get(at).is_some_and(|&cell| {
                    usize::from(cell).abs_diff(page_level) <= usize::from(PAGE_DELTA)
                })
        };
        let in_rows: Vec<u32> = (0..across).filter(|&k| in_bar(k)).collect();
        let near = |k: u32| {
            in_rows
                .first()
                .zip(in_rows.last())
                .is_some_and(|(&a, &b)| k + 1 >= a && k <= b + 1)
        };
        let mut page = vec![false; track.len()];
        for line in 0..lines {
            for k in 0..across {
                let at = index(line, k);
                page[at] = is_known(at)
                    && !track[at]
                    && is_page(at)
                    && (line + 1 < from || line > to + 1 || !near(k));
            }
        }
        self.track = track;
        self.page = page;
        self.live = live;
    }

    /// How far the track of line `line` moved between `empty` and `reading`,
    /// in hundredths of a level, on average over the cells known in both that
    /// looked like the empty track at the first reading (a cell of a round
    /// cap or a border is part page or part line, and moves by less when it
    /// fills); `None` when there is none.
    fn drift(&self, empty: &Fingerprint, reading: &Fingerprint, line: u32) -> Option<u64> {
        let direction = self.direction;
        let (mut sum, mut weight) = (0_u64, 0_u64);
        for k in 0..empty.across(direction) {
            let (column, row) = empty.line_cell(line, k, direction);
            let at = (row * empty.columns + column) as usize;
            if let (Some(&a), Some(&b)) = (empty.cells.get(at), reading.cells.get(at))
                && !empty.hidden.get(at).copied().unwrap_or(true)
                && !reading.hidden.get(at).copied().unwrap_or(true)
                && self.on_track(at)
                && i32::from(a).abs_diff(self.empty_level) <= u32::from(CELL_DELTA)
            {
                let w = empty.weight(at);
                sum += w * u64::from(a.abs_diff(b));
                weight += w;
            }
        }
        (weight > 0).then(|| sum * 100 / weight)
    }

    /// Whether the fill has reached the bar's very end: the last line that is
    /// not a round cap or a border has moved nearly as far as the lines
    /// before it. A last line only partly filled has moved by its covered
    /// share, a pixel short of four being three quarters of the way, and a
    /// bar is not full until its fill reaches the end (his words: *"fill
    /// reaches the end"*). Whatever colour the fill is, and however it
    /// changes along the bar, a line that is covered moves about as far as
    /// its neighbours do.
    fn reaches_the_end(&self, empty: &Fingerprint, reading: &Fingerprint) -> bool {
        let mut drifts = [0_u64; 4];
        let mut count = 0;
        for &line in &self.finish {
            // Lines the fill had already filled at the first reading have
            // not moved, and say nothing of how far this one did.
            if line < self.start {
                return true;
            }
            let Some(drift) = self.drift(empty, reading, line) else {
                continue;
            };
            if let Some(slot) = drifts.get_mut(count) {
                *slot = drift;
                count += 1;
            }
        }
        let Some((&last, before)) = drifts[..count].split_last() else {
            return true;
        };
        if before.len() < 2 {
            return true;
        }
        let mut before = before.to_vec();
        before.sort_unstable();
        let middle = before.get(before.len() / 2).copied().unwrap_or(0);
        last * 100 >= middle * REACHES_THE_END_PERCENT
    }

    /// Whether the page around the bar of `reading` changed against `empty`:
    /// more than half of it, by pixels, in cells that moved by more than
    /// [`CELL_DELTA`]. A fill never changes it.
    fn covered(&self, empty: &Fingerprint, reading: &Fingerprint) -> bool {
        let (mut moved, mut known) = (0_u64, 0_u64);
        for (at, _) in self.page.iter().enumerate().filter(|(_, page)| **page) {
            if let (Some(&a), Some(&b)) = (empty.cells.get(at), reading.cells.get(at))
                && !empty.hidden.get(at).copied().unwrap_or(true)
                && !reading.hidden.get(at).copied().unwrap_or(true)
            {
                let weight = empty.weight(at);
                known += weight;
                if a.abs_diff(b) > CELL_DELTA {
                    moved += weight;
                }
            }
        }
        known > 0 && moved * 2 > known
    }

    /// Whether the page around the bar of `reading` looks again as it did in
    /// the last reading before it was covered ([`Self::clear`]): all but a
    /// sixteenth of it, by pixels, within [`CELL_DELTA`]. A cover that goes
    /// in parts is not gone until it is gone: Win+Shift+S darkens the whole
    /// screen and then shows a selection dragged over it bright, and with
    /// most of the page around the bar inside the selection, the darkened
    /// rest of the bar read as a fill (the founder's rig pass of `#178`,
    /// 2026-11-20). The page beside a line of the bar whose track changed
    /// since is passed over, since the fill may have moved on under the cover
    /// and an area drawn with less than a cell of page beside the bar has
    /// page cells that hold the bar's edge; the page at the bar's ends, and
    /// beside the lines that did not change, must be back.
    fn page_back(&self, reading: &Fingerprint) -> bool {
        let Some(clear) = &self.clear else {
            return true;
        };
        let direction = self.direction;
        let (low, high) = self.ends;
        let (mut moved, mut known) = (0_u64, 0_u64);
        for line in 0..clear.lines(direction) {
            if (low..=high).contains(&line) && self.changed(clear, line, reading) == Some(true) {
                continue;
            }
            for k in 0..clear.across(direction) {
                let (column, row) = clear.line_cell(line, k, direction);
                let at = (row * clear.columns + column) as usize;
                if !self.page.get(at).copied().unwrap_or(false) {
                    continue;
                }
                if let (Some(&a), Some(&b)) = (clear.cells.get(at), reading.cells.get(at))
                    && !clear.hidden.get(at).copied().unwrap_or(true)
                    && !reading.hidden.get(at).copied().unwrap_or(true)
                {
                    let weight = clear.weight(at);
                    known += weight;
                    if a.abs_diff(b) > CELL_DELTA {
                        moved += weight;
                    }
                }
            }
        }
        moved * 16 <= known
    }

    /// How far `reading` shows the bar filled against `empty`, in percent of
    /// its length, and whether it is full; `None` when the page around it
    /// changed ([`Self::covered`]). A line with nothing known in it (under
    /// the area's own border or mark, with *Show UP-TAKE in screen
    /// recordings* on, or under one of UP-TAKE's windows) is passed over: it
    /// neither stops the run nor counts in the length.
    fn progress(&self, empty: &Fingerprint, reading: &Fingerprint) -> Option<(u8, bool)> {
        if self.covered(empty, reading) {
            return None;
        }
        let (low, high) = self.ends;
        let direction = self.direction;
        let (mut run, mut total, mut gap) = (0_u64, 0_u64, 0_u64);
        // The last line the run took: the fill's front, which a cell counts
        // as filled once part of it moved.
        let mut front = 0_u64;
        // How many lines in a row since the last filled one have not moved:
        // a few are a gap in the bar the first reading could not show (the
        // break between blocks, when there was no margin to tell it from),
        // more are the unfilled rest of it.
        let mut unfilled = 0_u32;
        let mut stopped = false;
        let mut any = false;
        // The front line's drift as a share of the line before it, in
        // percent, and the last filled line's drift.
        let (mut front_share, mut behind) = (None::<u64>, None::<u64>);
        // The last line of the bar the first reading knew, and the last the
        // reading knows (review of `#170`, round 6: a card over the far end
        // must not let the bar read full early).
        let (mut last_empty, mut last_now) = (None, None);
        for line in low..=high {
            let length = empty.length(line, direction);
            if !self.is_live(line) {
                // A gap in the bar: it counts in the length, and in the run
                // when a filled line follows.
                total += length;
                gap += length;
                continue;
            }
            if empty
                .line_share(line, empty, line, direction, |_, _| true)
                .is_some()
            {
                last_empty = Some(line);
            }
            let Some(changed) = self.changed(empty, line, reading) else {
                continue;
            };
            last_now = Some(line);
            total += length;
            // A line filled at the first reading while it still looks unlike
            // the empty track (a gradient or a stripe moves under a growing
            // fill, so it need not look as it did); any other when it no
            // longer looks like its empty self.
            let filled = if line < self.start {
                self.level(reading, line)
                    .is_none_or(|level| level.abs_diff(self.empty_level) > EMPTY_DELTA)
            } else {
                changed
            };
            if filled {
                if !stopped {
                    run += gap + length;
                    front = length;
                    // How far the front line moved against the line before
                    // it: the share of it the fill covers.
                    let drift = self.drift(empty, reading, line);
                    front_share = match (drift, behind) {
                        (Some(now), Some(before)) if before > 0 => {
                            Some((now * 100 / before).min(100))
                        }
                        _ => None,
                    };
                    behind = drift;
                    any = true;
                }
                gap = 0;
                unfilled = 0;
            } else if self.is_odd(line) {
                // A border or a cap that has not changed is not the bar's
                // fill: passed over, as a gap is.
                gap += length;
            } else {
                unfilled += 1;
                gap += length;
                if unfilled > GAP_LINES {
                    stopped = true;
                }
            }
        }
        // A percentage is never reported early: the front line may be only
        // partly filled (review of `#170`, round 5: at 97 of 200 pixels the
        // front cell made 50%), so it counts by the share of it the fill
        // covers, rounded down to whole pixels and less one, and not at all
        // when that share cannot be told. At most a pixel or two late on a
        // flat fill, a cell on any other (the founder's rig pass of `#178`,
        // 2026-11-20: a 50% mark came at about 54%).
        let front_part = front_share.map_or(0, |share| (front * share / 100).saturating_sub(1));
        let counted = run.saturating_sub(front) + front_part;
        let full = !stopped
            && unfilled == 0
            && any
            && last_empty == last_now
            && self.reaches_the_end(empty, reading);
        let percent = u8::try_from((counted * 100 / total.max(1)).min(100)).unwrap_or(100);
        Some((if full { 100 } else { percent.min(99) }, full))
    }

    /// Takes one reading, made at `at`, and says whether it is a moment worth
    /// reporting.
    fn observe(&mut self, reading: Fingerprint, at: Duration) -> Option<Event> {
        let Some(mut empty) = self.empty.take() else {
            self.learn(reading);
            return None;
        };
        if (empty.columns, empty.rows, empty.edge) != (reading.columns, reading.rows, reading.edge)
        {
            // Resized: another bar, as far as the lines go.
            self.learn(reading);
            return None;
        }
        // What lay under one of UP-TAKE's own windows at the first reading is
        // learned at its first sight, as the other choices do.
        empty.learn(&reading);
        let covered = if self.covering {
            !self.page_back(&reading)
        } else {
            self.covered(&empty, &reading)
        };
        let progress = if covered {
            None
        } else {
            self.progress(&empty, &reading)
        };
        self.covering = covered;
        self.empty = Some(empty);
        if covered {
            // A page that stays changed and still is a new page (`I-493`).
            let since = match &self.still {
                // The whole area, the bar too: a snipping selection dragged
                // slowly along the bar leaves the page around it as it was,
                // and was learned as the bar (a fill and that drag look
                // alike). A theme switched in the middle of a fill is learned
                // once the bar stops.
                Some((since, from)) if !from.differs_by(&reading, CELL_DELTA, 0) => *since,
                _ => {
                    self.still = Some((at, reading.clone()));
                    at
                }
            };
            if at.saturating_sub(since) >= RELEARN_AFTER {
                self.learn(reading);
                self.held = [None; MARKS];
                return None;
            }
        } else {
            self.still = None;
            self.clear = Some(reading);
        }
        // Something over the area, not a fill: nothing is taken from it, and
        // a percentage waiting out its calm starts it again, since the bar was
        // not seen holding it (review of `#178`, finding 2).
        let Some((percent, full)) = progress else {
            self.held = [None; MARKS];
            self.attached = None;
            return None;
        };
        self.seen = percent;
        if percent == 0 {
            self.attached = None;
        } else if self.attached.is_none() {
            self.attached = Some(at);
        }
        let steady = self
            .attached
            .is_some_and(|since| at.saturating_sub(since) >= STEADY_FILL);
        if u16::from(percent) * 2 < u16::from(self.reported) {
            // The bar emptied: a new fill.
            self.reported = percent;
        }
        if full {
            self.held = [None; MARKS];
            if self.reported < 100 {
                self.reported = 100;
                return Some(Event::Filled);
            }
            return None;
        }
        // Each percentage not yet reported that the bar is at: since when it
        // has been, and whether that is long enough.
        let reported = self.reported;
        let mut reach = None;
        for (mark, held) in self.marks.iter().copied().zip(self.held.iter_mut()) {
            if mark <= reported || mark > percent {
                *held = None;
            } else {
                let since = *held.get_or_insert(at);
                if (steady || at.saturating_sub(since) >= MARK_CALM)
                    && reach.is_none_or(|best| mark > best)
                {
                    reach = Some(mark);
                }
            }
        }
        let mark = reach?;
        self.reported = mark;
        // Only the percentages at or below the one reported are done: a higher
        // one already waiting keeps its own calm (review of `#178`, round 8).
        for (other, held) in self.marks.iter().copied().zip(self.held.iter_mut()) {
            if other <= mark {
                *held = None;
            }
        }
        Some(Event::Reached(mark))
    }
}

impl Fingerprint {
    /// How many lines a bar filling in `direction` has: columns of cells for
    /// one that fills sideways, rows for one that fills up or down.
    const fn lines(&self, direction: Direction) -> u32 {
        match direction {
            Direction::LeftToRight | Direction::RightToLeft => self.columns,
            Direction::TopToBottom | Direction::BottomToTop => self.rows,
        }
    }

    /// How many cells across one line.
    const fn across(&self, direction: Direction) -> u32 {
        match direction {
            Direction::LeftToRight | Direction::RightToLeft => self.rows,
            Direction::TopToBottom | Direction::BottomToTop => self.columns,
        }
    }

    /// The column and row of the `k`th cell across line `line`, lines
    /// counted from the end the bar fills from.
    const fn line_cell(&self, line: u32, k: u32, direction: Direction) -> (u32, u32) {
        match direction {
            Direction::LeftToRight => (line, k),
            Direction::RightToLeft => (self.columns - 1 - line, k),
            Direction::TopToBottom => (k, line),
            Direction::BottomToTop => (k, self.rows - 1 - line),
        }
    }

    /// How many pixels long line `line` is along the bar: [`CELL`], less for
    /// the region's edge line.
    fn length(&self, line: u32, direction: Direction) -> u64 {
        let (column, row) = self.line_cell(line, 0, direction);
        let length = match direction {
            Direction::LeftToRight | Direction::RightToLeft if column + 1 == self.columns => {
                self.edge.0
            }
            Direction::TopToBottom | Direction::BottomToTop if row + 1 == self.rows => self.edge.1,
            _ => CELL,
        };
        u64::from(length)
    }

    /// The pixels-weighted sum of how far the cells of line `line` of `self`
    /// are from those of line `theirs` of `other`, and the pixels compared:
    /// the cells known in both that `include` takes. `None` when none was.
    fn line_share_by(
        &self,
        line: u32,
        other: &Self,
        theirs: u32,
        direction: Direction,
        include: impl Fn(usize, usize) -> bool,
    ) -> Option<(u64, u64)> {
        let (mut known, mut sum) = (0_u64, 0_u64);
        for k in 0..self.across(direction) {
            let index =
                |(column, row): (u32, u32), grid: &Self| (row * grid.columns + column) as usize;
            let (a, b) = (
                index(self.line_cell(line, k, direction), self),
                index(other.line_cell(theirs, k, direction), other),
            );
            let (Some(&a_cell), Some(&b_cell)) = (self.cells.get(a), other.cells.get(b)) else {
                continue;
            };
            if self.hidden.get(a).copied().unwrap_or(true)
                || other.hidden.get(b).copied().unwrap_or(true)
                || !include(a, b)
            {
                continue;
            }
            let weight = self.weight(a);
            known += weight;
            sum += weight * u64::from(a_cell.abs_diff(b_cell));
        }
        (known > 0).then_some((sum, known))
    }

    /// How many pixels of line `line` of `self` sit in cells that moved by
    /// more than [`CELL_DELTA`] against line `theirs` of `other`, and how
    /// many were compared: the cells known in both that `include` takes,
    /// given each side's cell index. `None` when none was compared.
    fn line_share(
        &self,
        line: u32,
        other: &Self,
        theirs: u32,
        direction: Direction,
        include: impl Fn(usize, usize) -> bool,
    ) -> Option<(u64, u64)> {
        let (mut known, mut moved) = (0_u64, 0_u64);
        for k in 0..self.across(direction) {
            let index =
                |(column, row): (u32, u32), grid: &Self| (row * grid.columns + column) as usize;
            let (a, b) = (
                index(self.line_cell(line, k, direction), self),
                index(other.line_cell(theirs, k, direction), other),
            );
            let (Some(&a_cell), Some(&b_cell)) = (self.cells.get(a), other.cells.get(b)) else {
                continue;
            };
            if self.hidden.get(a).copied().unwrap_or(true)
                || other.hidden.get(b).copied().unwrap_or(true)
                || !include(a, b)
            {
                continue;
            }
            let weight = self.weight(a);
            known += weight;
            if a_cell.abs_diff(b_cell) > CELL_DELTA {
                moved += weight;
            }
        }
        (known > 0).then_some((moved, known))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CELL, CELL_DELTA, Detector, Event, Fingerprint, SETTLE_READINGS, Watch, leave_out,
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

    /// `ADR-0053` decision 1: a watch area starts in "notify when it changes".
    #[test]
    fn changes_is_the_default() {
        assert_eq!(Watch::default(), Watch::Changes);
    }

    #[test]
    fn each_choice_is_told_about_its_own_moment_only() {
        assert!(Watch::Changes.notifies(Event::Changed));
        assert!(!Watch::Changes.notifies(Event::Settled));
        assert!(Watch::Settles.notifies(Event::Settled));
        assert!(!Watch::Settles.notifies(Event::Changed));
        assert!(!Watch::Settles.notifies(Event::Filled));
        assert!(Watch::Fills.notifies(Event::Filled));
        assert!(Watch::Fills.notifies(Event::Reached(20)));
        assert!(!Watch::Fills.notifies(Event::Changed));
        assert!(!Watch::Fills.notifies(Event::Settled));
        assert!(!Watch::Changes.notifies(Event::Reached(20)));
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

    /// A caret: 2 by 16 pixels turning black on white, in a 960 by 640
    /// region. Since `1.61` it counts at the lowest tolerance, on any area
    /// size: it fills half of four 4-pixel cells. A tolerance of a
    /// thousandth (ten of this region's 9,600 cells) leaves it out.
    #[test]
    fn a_blinking_caret_counts_unless_the_tolerance_leaves_it_out() {
        let before = grey(960, 640, 250);
        let after = with_patch(before.clone(), (300, 200, 2, 16), 0);
        assert!(print(&before).differs(&print(&after)));
        assert!(!print(&before).differs_by(&print(&after), CELL_DELTA, 1));
    }

    /// His rig pass of 2026-11-07: on an area the size of a monitor a small
    /// change did not fire, because each of the 16 by 16 cells was about
    /// 14,400 pixels. A 6 by 6 icon at a contrast of 100 levels on a 1080p
    /// area must count at the lowest tolerance.
    #[test]
    fn a_small_change_on_a_monitor_sized_area_counts() {
        let before = grey(1920, 1080, 30);
        let after = with_patch(before.clone(), (1000, 500, 6, 6), 130);
        assert!(print(&before).differs(&print(&after)));
    }

    /// Review of `#168`, round 4: *Any change* must see one pixel. A single
    /// pixel changing by more than about 100 levels counts on any area, and
    /// one changing by less does not (the floor [`CELL`] documents).
    #[test]
    fn any_change_sees_a_single_pixel() {
        let before = grey(1920, 1080, 30);
        let bright = with_patch(before.clone(), (1001, 503, 1, 1), 255);
        assert!(print(&before).differs(&print(&bright)));
        let faint = with_patch(before.clone(), (1001, 503, 1, 1), 30 + 90);
        assert!(!print(&before).differs(&print(&faint)));
    }

    /// Fixed-size cells: a larger region has more cells, not larger ones,
    /// rounded up so the region's edge is a cell too.
    #[test]
    fn a_cell_is_four_pixels_on_any_region() {
        assert_eq!(CELL, 4);
        assert_eq!(print(&grey(1920, 1080, 0)).grid(), (480, 270));
        assert_eq!(print(&grey(106, 62, 0)).grid(), (27, 16));
        assert_eq!(print(&grey(5, 3, 0)).grid(), (2, 1));
        assert_eq!(print(&grey(3, 3, 0)).grid(), (1, 1));
    }

    /// The GPU route reads the region averaged down to one texel per cell
    /// (`uptake-capture`'s `watch_capture_small`); a picture read that way
    /// has the same grid, and the same values, as the full picture.
    #[test]
    fn a_picture_averaged_down_to_cells_reads_as_the_full_one() {
        let full = with_patch(grey(64, 48, 40), (8, 16, 16, 8), 200);
        let mut small = grey(16, 12, 40);
        small = with_patch(small, (2, 4, 4, 2), 200);
        let region = Size::new(64, 48);
        let read = Fingerprint::of_region(&small, region, &[])
            .unwrap_or_else(|| unreachable!("a non-empty bitmap"));
        assert_eq!(read, print(&full));
    }

    /// True 4-pixel cells on a full-size picture whose sides are not a
    /// multiple of four (review of `#168`, round 1): a 106-pixel side is
    /// twenty-six cells of 4 and one of 2, so pixel column 7 is in cell 1,
    /// column 8 in cell 2, and the last two columns in cell 26 alone.
    #[test]
    fn a_full_picture_is_cut_into_true_four_pixel_cells() {
        let base = grey(106, 62, 100);
        let changed_cells = |x: u32| {
            let after = with_patch(base.clone(), (x, 0, 1, 62), 255);
            let (a, b) = (print(&base), print(&after));
            let mut columns: Vec<u32> = a
                .cells
                .iter()
                .zip(&b.cells)
                .enumerate()
                .filter(|(_, (x, y))| x != y)
                .map(|(at, _)| at as u32 % a.columns)
                .collect();
            columns.dedup();
            columns.sort_unstable();
            columns.dedup();
            columns
        };
        assert_eq!(changed_cells(7), vec![1]);
        assert_eq!(changed_cells(8), vec![2]);
        assert_eq!(changed_cells(104), vec![26]);
        assert_eq!(changed_cells(103), vec![25]);
    }

    /// Both routes read a region whose sides are not a multiple of four the
    /// same way: the GPU's padding repeats the region's last column and row,
    /// so its last texels are the edge's own pixels, and the grid and the
    /// values are the full picture's. (The GPU itself is tested in
    /// `uptake-capture`'s `a_one_pixel_edge_reaches_the_gpu_average`.) 106
    /// by 62 is 26 cells and 2 pixels across, 15 cells and 2 pixels down.
    #[test]
    fn both_routes_agree_on_a_region_not_a_multiple_of_four() {
        let full = with_patch(grey(106, 62, 192), (8, 8, 16, 8), 0);
        let mut small = grey(27, 16, 192);
        small = with_patch(small, (2, 2, 4, 2), 0);
        let read = Fingerprint::of_region(&small, Size::new(106, 62), &[])
            .unwrap_or_else(|| unreachable!("a non-empty bitmap"));
        assert_eq!(read, print(&full));
    }

    /// The tolerance counts cells: at half the area, a change to under half
    /// the cells is not a change and one to half of them is; at the whole
    /// area only a change to all of it counts.
    #[test]
    fn the_tolerance_is_a_share_of_the_cells() {
        // 64 by 64: 16 by 16 cells; half the area is the top 32 pixel rows.
        let before = grey(64, 64, 100);
        let three_rows = with_patch(before.clone(), (0, 0, 64, 24), 200);
        let four_rows = with_patch(before.clone(), (0, 0, 64, 32), 200);
        let half = 500;
        assert!(!print(&before).differs_by(&print(&three_rows), CELL_DELTA, half));
        assert!(print(&before).differs_by(&print(&four_rows), CELL_DELTA, half));
        // At the whole area, half is not enough and all of it is; above the
        // whole area is held at the whole area.
        let all = super::SHARE_PERMILLE_MAX;
        assert!(!print(&before).differs_by(&print(&four_rows), CELL_DELTA, all));
        let everything = with_patch(before.clone(), (0, 0, 64, 64), 200);
        assert!(print(&before).differs_by(&print(&everything), CELL_DELTA, all));
        assert!(print(&before).differs_by(&print(&everything), CELL_DELTA, u16::MAX));
    }

    /// Review of `#168`, round 2: a part-cell counts for the pixels it
    /// holds. On a 9 by 8 area (two columns of 4 by 4 cells and one of 1 by
    /// 4), changing the 1 by 8 edge is 8 of 72 pixels: a change at *any change*, not at
    /// half the area, nor at a tenth.
    #[test]
    fn a_part_cell_weighs_what_it_holds() {
        let before = grey(9, 8, 100);
        let edge = with_patch(before.clone(), (8, 0, 1, 8), 200);
        assert!(print(&before).differs(&print(&edge)));
        assert!(!print(&before).differs_by(&print(&edge), CELL_DELTA, 500));
        assert!(!print(&before).differs_by(&print(&edge), CELL_DELTA, 112));
        assert!(print(&before).differs_by(&print(&edge), CELL_DELTA, 111));
        // The first 8 columns are 64 of 72: past half.
        let body = with_patch(before.clone(), (0, 0, 8, 8), 200);
        assert!(print(&before).differs_by(&print(&body), CELL_DELTA, 500));
    }

    /// Cells under a window of UP-TAKE's own count toward neither side of
    /// the share: half of what is known, not half of the region.
    #[test]
    fn the_tolerance_counts_only_known_cells() {
        let before = grey(64, 64, 100);
        let after = with_patch(before.clone(), (0, 0, 64, 16), 200);
        // Hide the bottom half: 32 known cells, 16 of them changed.
        let hide = [Rect::new(0, 32, 64, 32)];
        let (a, b) = (hiding(&before, &hide), hiding(&after, &hide));
        assert!(a.differs_by(&b, CELL_DELTA, 500));
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
        let before = grey(16, 16, 100);
        let after = grey(16, 16, 112);
        assert_eq!(print(&before).moved(&print(&after)), 12);
        assert!(print(&before).differs(&print(&after)));
    }

    /// Exactly the line: a whole cell moving by `CELL_DELTA` is not a change,
    /// and one level more is.
    #[test]
    fn the_line_is_cell_delta_exclusive() {
        let base = grey(16, 16, 100);
        assert!(!print(&base).differs(&print(&grey(16, 16, 100 + CELL_DELTA))));
        assert!(print(&base).differs(&print(&grey(16, 16, 101 + CELL_DELTA))));
    }

    #[test]
    fn a_resized_region_differs() {
        assert!(print(&grey(320, 200, 100)).differs(&print(&grey(8, 8, 100))));
    }

    /// A region smaller than one cell is one cell, and a pixel at full
    /// contrast in it still counts.
    #[test]
    fn a_tiny_region_is_one_cell() {
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

    /// A reading of `bitmap` with `hide` under one of UP-TAKE's windows.
    fn hiding(bitmap: &RgbaBitmap, hide: &[Rect]) -> Fingerprint {
        Fingerprint::of_hiding(bitmap, hide).unwrap_or_else(|| unreachable!("a non-empty bitmap"))
    }

    /// Review of `#160`, round 1 (`I-480`): with *Show UP-TAKE in screen
    /// recordings* on, a notification card over a watch area was read as a
    /// change, and could raise the next card. Its cells are unknown while it
    /// is there, so neither its coming, its animation nor its going is a
    /// change, in either mode.
    #[test]
    fn a_window_of_up_takes_own_coming_and_going_is_no_change() {
        let card = Rect::new(16, 0, 32, 12);
        for watch in [Watch::Changes, Watch::Settles] {
            let mut detector = Detector::with_settle(watch.settle_readings());
            let screen = grey(64, 64, 40);
            assert_eq!(detector.observe(print(&screen)), None, "the baseline");
            for level in [250, 200, 120, 30] {
                let drawn = with_patch(screen.clone(), (16, 0, 32, 12), level);
                assert_eq!(detector.observe(hiding(&drawn, &[card])), None, "{watch:?}");
            }
            for _ in 0..SETTLE_READINGS + 1 {
                assert_eq!(detector.observe(print(&screen)), None, "{watch:?}: gone");
            }
        }
    }

    /// While a card is up, the rest of the area is still watched.
    #[test]
    fn a_change_beside_a_window_of_up_takes_own_is_still_a_change() {
        let mut detector = Detector::new();
        let screen = grey(64, 64, 40);
        detector.observe(print(&screen));
        let changed = with_patch(screen, (0, 40, 64, 24), 200);
        assert_eq!(
            detector.observe(hiding(&changed, &[Rect::new(16, 0, 32, 12)])),
            Some(Event::Changed)
        );
    }

    /// What lay under a card when the baseline was taken is learned at its
    /// first sight, not reported, and watched from then on: a change there
    /// after the card has gone is a change.
    #[test]
    fn what_a_window_hid_is_learned_then_watched() {
        let card = Rect::new(0, 0, 64, 16);
        let mut detector = Detector::new();
        let under = with_patch(grey(64, 64, 40), (0, 0, 64, 16), 90);
        let covered = with_patch(under.clone(), (0, 0, 64, 16), 250);
        assert_eq!(
            detector.observe(hiding(&covered, &[card])),
            None,
            "baseline"
        );
        assert_eq!(detector.observe(print(&under)), None, "first sight");
        assert_eq!(detector.observe(print(&under)), None, "learned");
        let changed = with_patch(under, (0, 0, 64, 16), 220);
        assert_eq!(detector.observe(print(&changed)), Some(Event::Changed));
    }

    /// Review of `#160`, round 3: a region that settles while a card covers
    /// part of it, then changes under the card, reports that change once the
    /// card has gone, instead of learning it as the baseline.
    #[test]
    fn a_change_made_under_a_card_shows_when_the_card_goes() {
        let card = Rect::new(0, 0, 64, 16);
        let mut detector = Detector::with_settle(Watch::Changes.settle_readings());
        let screen = grey(64, 64, 40);
        assert_eq!(detector.observe(print(&screen)), None, "baseline");
        let moved = with_patch(screen.clone(), (0, 40, 64, 24), 200);
        assert_eq!(
            detector.observe(hiding(&moved, &[card])),
            Some(Event::Changed)
        );
        let mut events = Vec::new();
        for _ in 0..Watch::Changes.settle_readings() {
            events.extend(detector.observe(hiding(&moved, &[card])));
        }
        assert_eq!(events, vec![Event::Settled], "settled with the card up");
        // Under the card, the screen changes; then the card goes.
        let under = with_patch(moved, (0, 0, 64, 16), 220);
        assert_eq!(detector.observe(hiding(&under, &[card])), None);
        assert_eq!(detector.observe(print(&under)), Some(Event::Changed));
    }

    /// The event-driven watcher reads at no fixed rate: a calm is measured in
    /// time. Readings every 50 ms re-arm *changes* after [`REARM_TIME`] of
    /// calm, not after two readings, and one reading long after the last
    /// still ends a *stops changing* by itself.
    #[test]
    fn a_calm_is_measured_in_time_whatever_the_rate() {
        use super::REARM_TIME;
        use std::time::Duration;
        let ms = Duration::from_millis;
        let mut detector = Detector::with_settle_time(Watch::Changes.settle_time());
        let still = grey(64, 64, 40);
        let moved = grey(64, 64, 200);
        assert_eq!(detector.observe_at(print(&still), ms(0)), None);
        assert_eq!(
            detector.observe_at(print(&moved), ms(50)),
            Some(Event::Changed)
        );
        assert!(detector.is_settling());
        assert_eq!(detector.observe_at(print(&moved), ms(100)), None);
        assert_eq!(
            detector.observe_at(print(&moved), ms(150)),
            None,
            "100 ms calm"
        );
        assert_eq!(
            detector.observe_at(print(&moved), ms(50) + REARM_TIME),
            Some(Event::Settled)
        );
        assert!(!detector.is_settling());

        let mut detector = Detector::with_settle_time(Watch::Settles.settle_time());
        detector.observe_at(print(&still), ms(0));
        detector.observe_at(print(&moved), ms(10));
        assert_eq!(detector.observe_at(print(&moved), ms(20)), None);
        assert_eq!(
            detector.observe_at(print(&moved), ms(10) + Watch::Settles.settle_time()),
            Some(Event::Settled)
        );
    }

    /// The maintainer's rig, 2026-11-05: icons switched on and off by hand,
    /// faster than once a second, notified only the first time. Read the way
    /// the event-driven host reads (on each change, then every 50 ms while it
    /// settles), every switch is a change.
    #[test]
    fn a_region_switched_back_and_forth_reports_every_switch() {
        use std::time::Duration;
        let shown = with_patch(grey(64, 64, 30), (8, 24, 48, 16), 220);
        let empty = grey(64, 64, 30);
        let mut detector = Detector::with_settle_time(Watch::Changes.settle_time());
        let mut changes = 0;
        let mut at = 0_u64;
        detector.observe_at(print(&empty), Duration::from_millis(at));
        for switch in 0..10 {
            let picture = if switch % 2 == 0 { &shown } else { &empty };
            // A switch every 300 ms: the read on the frame, then the polls.
            for _ in 0..6 {
                at += 50;
                if detector.observe_at(print(picture), Duration::from_millis(at))
                    == Some(Event::Changed)
                {
                    changes += 1;
                }
            }
        }
        assert_eq!(changes, 10);
    }

    /// The maintainer's rig pass of 2026-11-07: about ten changes a second
    /// notified once, since the region never held still for [`REARM_TIME`].
    /// Read once a frame (33 ms), as the host does while frames arrive, an
    /// area set to *Notify when it changes* reports every switch; the old
    /// rule, still what a plain detector does, reports the first only.
    #[test]
    fn ten_switches_a_second_are_ten_changes_a_second() {
        use super::REARM_TIME;
        use std::time::Duration;
        let shown = with_patch(grey(64, 64, 30), (8, 24, 48, 16), 220);
        let empty = grey(64, 64, 30);
        let count = |mut detector: Detector| {
            let mut changes = 0;
            detector.observe_at(print(&empty), Duration::ZERO);
            for frame in 1..=99_u64 {
                let at = frame * 33;
                // Switched every 100 ms for 3.3 s: 32 switches, ten a second.
                let picture = if (at / 100) % 2 == 0 { &empty } else { &shown };
                if detector.observe_at(print(picture), Duration::from_millis(at))
                    == Some(Event::Changed)
                {
                    changes += 1;
                }
            }
            changes
        };
        assert!(
            REARM_TIME > Duration::from_millis(100),
            "the old calm never comes"
        );
        assert_eq!(count(Detector::for_watch(Watch::Changes)), 32);
        assert_eq!(
            count(Detector::with_settle_time(Watch::Changes.settle_time())),
            1
        );
    }

    /// A fade under an area set to *Notify when it changes*, a few levels a
    /// frame, is one change, not one a frame: only a full step counts again.
    #[test]
    fn a_fade_is_one_change() {
        use std::time::Duration;
        let mut detector = Detector::for_watch(Watch::Changes);
        let mut changes = 0;
        for frame in 0..=40_u8 {
            let picture = grey(64, 64, 20 + frame * 5);
            if detector.observe_at(
                print(&picture),
                Duration::from_millis(u64::from(frame) * 33),
            ) == Some(Event::Changed)
            {
                changes += 1;
            }
        }
        assert_eq!(changes, 1);
    }

    /// *Notify when it stops changing* is not told of every step: its
    /// detector reports one start and one end, as before.
    #[test]
    fn stops_changing_is_not_told_of_every_step() {
        use std::time::Duration;
        let (a, b) = (print(&grey(64, 64, 10)), print(&grey(64, 64, 200)));
        let mut detector = Detector::for_watch(Watch::Settles);
        let mut reported = Vec::new();
        detector.observe_at(a.clone(), Duration::ZERO);
        for frame in 1..=30_u64 {
            let picture = if frame % 2 == 0 { &a } else { &b };
            reported
                .extend(detector.observe_at(picture.clone(), Duration::from_millis(frame * 33)));
        }
        assert_eq!(reported, vec![Event::Changed]);
    }

    /// *Stops changing* keeps its six seconds; *changes* re-arms after a
    /// sixth of a second (the maintainer's rig, 2026-11-05).
    #[test]
    fn the_settle_times_are_the_counts_at_half_a_second() {
        use std::time::Duration;
        assert_eq!(Watch::Changes.settle_time(), Duration::from_millis(150));
        assert_eq!(Watch::Settles.settle_time(), Duration::from_secs(6));
    }

    /// A rebase keeps a change under way: it still ends with `Settled` after
    /// the calm, measured from the rebase; with nothing under way it reports
    /// nothing, and a later change is a change.
    #[test]
    fn a_rebase_keeps_a_change_under_way() {
        use std::time::Duration;
        let ms = Duration::from_millis;
        let mut detector = Detector::with_settle_time(Watch::Settles.settle_time());
        detector.observe_at(print(&grey(64, 64, 40)), ms(0));
        assert_eq!(
            detector.observe_at(print(&grey(64, 64, 200)), ms(500)),
            Some(Event::Changed)
        );
        let other = print(&grey(32, 20, 90));
        detector.rebase_at(other.clone(), ms(1_000));
        assert!(detector.is_settling(), "still under way");
        assert_eq!(
            detector.observe_at(other.clone(), ms(6_500)),
            None,
            "5.5 s calm"
        );
        assert_eq!(
            detector.observe_at(other.clone(), ms(7_000)),
            Some(Event::Settled),
            "6 s after the rebase"
        );
        let mut calm = Detector::with_settle_time(Watch::Changes.settle_time());
        calm.observe_at(print(&grey(64, 64, 40)), ms(0));
        calm.rebase_at(other.clone(), ms(100));
        assert!(!calm.is_settling());
        assert_eq!(calm.observe_at(other, ms(200)), None);
        assert_eq!(
            calm.observe_at(print(&grey(32, 20, 220)), ms(300)),
            Some(Event::Changed)
        );
    }

    /// Hiding marks every cell the rectangle touches and no other.
    #[test]
    fn hiding_marks_the_cells_a_rectangle_touches() {
        let bitmap = grey(64, 64, 40);
        let hidden = hiding(
            &bitmap,
            &[Rect::new(5, 5, 10, 2), Rect::new(100, 100, 5, 5)],
        );
        // 64 px over 16 cells: 4 px a cell, so x 5..=14 is cells 1..=3, y
        // 5..=6 is row 1.
        let marked: Vec<usize> = hidden
            .hidden
            .iter()
            .enumerate()
            .filter_map(|(at, &is)| is.then_some(at))
            .collect();
        assert_eq!(marked, vec![17, 18, 19]);
    }

    /// The tolerance ends a change too: a caret blinking inside a larger
    /// share neither starts a change nor keeps a finished one from settling
    /// (*Notify when it stops changing* over a render with a text field).
    #[test]
    fn a_caret_inside_the_tolerance_does_not_keep_a_change_going() {
        let blank = grey(960, 640, 250);
        let caret = |on: bool| {
            if on {
                with_patch(blank.clone(), (300, 200, 2, 16), 0)
            } else {
                blank.clone()
            }
        };
        let done = with_patch(blank.clone(), (0, 400, 960, 40), 0);
        let done_caret = with_patch(done.clone(), (300, 200, 2, 16), 0);
        let mut detector = Detector::for_watch(Watch::Settles);
        detector.set_share(1);
        let mut at = 0;
        let mut step = |reading: &RgbaBitmap| {
            at += 100;
            detector.observe_at(print(reading), std::time::Duration::from_millis(at))
        };
        assert_eq!(step(&caret(false)), None);
        assert_eq!(step(&caret(true)), None, "the caret alone is no change");
        assert_eq!(step(&done), Some(Event::Changed), "a bar of 960 by 40 is");
        let mut events = Vec::new();
        for n in 0..80 {
            events.push(step(if n % 2 == 0 { &done_caret } else { &done }));
        }
        assert_eq!(
            events.iter().flatten().collect::<Vec<_>>(),
            vec![&Event::Settled],
            "the caret blinks on, and the change still ends once"
        );
    }
}

#[cfg(test)]
mod tuning_tests {
    use super::{Admitted, EventGate, Notify, Tuning};
    use std::time::Duration;

    const fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    /// Feeds events at `times` (ms) and returns which fired and which
    /// notified.
    fn run(tuning: Tuning, times: &[u64]) -> (Vec<u64>, Vec<u64>) {
        let mut gate = EventGate::default();
        let (mut fired, mut noticed) = (Vec::new(), Vec::new());
        for &at in times {
            let Admitted { fires, notifies } = gate.admit(tuning, ms(at));
            if fires {
                fired.push(at);
            }
            if notifies {
                noticed.push(at);
            }
        }
        (fired, noticed)
    }

    /// Every change at ten a second.
    const BURST: [u64; 6] = [0, 100, 200, 300, 400, 500];

    #[test]
    fn the_default_fires_and_notifies_every_event() {
        assert_eq!(Tuning::default().notify, Notify::Each);
        let (fired, noticed) = run(Tuning::default(), &BURST);
        assert_eq!(fired, BURST);
        assert_eq!(noticed, BURST);
    }

    #[test]
    fn never_fires_and_shows_no_card() {
        let tuning = Tuning {
            notify: Notify::Never,
            ..Tuning::default()
        };
        let (fired, noticed) = run(tuning, &BURST);
        assert_eq!(fired, BURST);
        assert!(noticed.is_empty());
    }

    /// Once: one card per burst, a burst ending after the chosen calm.
    #[test]
    fn once_is_one_card_per_burst_with_his_calm() {
        let tuning = Tuning {
            notify: Notify::Once,
            calm_ms: 150,
            ..Tuning::default()
        };
        // A burst at ten a second, a calm of 400 ms, a second burst.
        let times = [0, 100, 200, 600, 700];
        assert_eq!(run(tuning, &times).1, vec![0, 600]);
        // With a calm of a second, the 400 ms pause does not end the burst.
        let long = Tuning {
            calm_ms: 1_000,
            ..tuning
        };
        assert_eq!(run(long, &times).1, vec![0]);
    }

    /// The minimum gap drops what arrives inside it, and a steady stream
    /// fires once per gap.
    #[test]
    fn the_gap_drops_events_inside_it() {
        let tuning = Tuning {
            gap_ms: 1_000,
            ..Tuning::default()
        };
        let times: Vec<u64> = (0..=25).map(|n| n * 100).collect();
        let (fired, noticed) = run(tuning, &times);
        assert_eq!(fired, vec![0, 1_000, 2_000]);
        assert_eq!(noticed, fired);
    }

    /// A burst is what the screen did: events the gap dropped still keep a
    /// burst going, so Once does not notify again just because the gap let
    /// one through.
    #[test]
    fn once_counts_the_burst_from_every_event_not_only_fired_ones() {
        let tuning = Tuning {
            notify: Notify::Once,
            calm_ms: 150,
            gap_ms: 1_000,
            ..Tuning::default()
        };
        let times: Vec<u64> = (0..=25).map(|n| n * 100).collect();
        let (fired, noticed) = run(tuning, &times);
        assert_eq!(fired, vec![0, 1_000, 2_000]);
        assert_eq!(noticed, vec![0]);
    }

    #[test]
    fn values_from_the_page_are_kept_in_range() {
        let wild = Tuning {
            notify: Notify::Each,
            calm_ms: u32::MAX,
            gap_ms: u32::MAX,
            sample_ms: u32::MAX,
            share_permille: u16::MAX,
            settle_ms: u32::MAX,
            direction: super::Direction::BottomToTop,
            marks: [0, 1, 99, u8::MAX],
        }
        .clamped();
        assert_eq!(wild.calm_ms, super::CALM_MS.1);
        assert_eq!(wild.gap_ms, super::GAP_MS_MAX);
        assert_eq!(wild.sample_ms, super::SAMPLE_MS_MAX);
        assert_eq!(wild.share_permille, super::SHARE_PERMILLE_MAX);
        assert_eq!(wild.settle_ms, super::SETTLE_MS.1);
        assert_eq!(wild.direction, super::Direction::BottomToTop);
        assert_eq!(wild.marks, [0, 1, 99, super::MARK_MAX]);
    }

    /// The sampling rate: realtime never waits; a slower rate waits out
    /// what is left of its step.
    #[test]
    fn the_sampling_rate_spaces_readings() {
        let realtime = Tuning::default();
        assert_eq!(realtime.sample_wait(Some(ms(1_000)), ms(1_001)), None);
        let slow = Tuning {
            sample_ms: 500,
            ..Tuning::default()
        };
        assert_eq!(slow.sample_wait(None, ms(0)), None);
        assert_eq!(slow.sample_wait(Some(ms(1_000)), ms(1_200)), Some(ms(300)));
        assert_eq!(slow.sample_wait(Some(ms(1_000)), ms(1_500)), None);
    }
}

/// [`Watch::Fills`], the bar mode (roadmap `1.61`, the maintainer's design of
/// 2026-11-10).
#[cfg(test)]
mod bar_tests {
    use super::{Detector, Direction, Event, Fingerprint, MARKS, Tuning, Watch};
    use crate::bitmap::RgbaBitmap;
    use crate::geometry::Size;

    /// The empty part of a bar, and its fill.
    const EMPTY: u8 = 40;
    const FULL: u8 = 200;

    /// A `width` by `height` bar, `filled` pixels of it filled from the end
    /// `direction` names.
    fn bar(width: u32, height: u32, filled: u32, direction: Direction) -> Fingerprint {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let inside = match direction {
                    Direction::LeftToRight => x < filled,
                    Direction::RightToLeft => x >= width.saturating_sub(filled),
                    Direction::TopToBottom => y < filled,
                    Direction::BottomToTop => y >= height.saturating_sub(filled),
                };
                let level = if inside { FULL } else { EMPTY };
                pixels.extend_from_slice(&[level, level, level, 255]);
            }
        }
        let bitmap = RgbaBitmap::from_pixels(Size::new(width, height), pixels)
            .unwrap_or_else(|| unreachable!("a well-formed bitmap"));
        Fingerprint::of(&bitmap).unwrap_or_else(|| unreachable!("a non-empty bitmap"))
    }

    /// A bar detector with `marks` and `direction`.
    fn detector(marks: [u8; MARKS], direction: Direction) -> Detector {
        let mut detector = Detector::for_watch(Watch::Fills);
        detector.tune(
            Watch::Fills,
            Tuning {
                marks,
                direction,
                ..Tuning::default()
            },
        );
        detector
    }

    /// The events a 200 by 20 bar filling left to right reports, two
    /// readings per entry of `fills` (pixels filled), half a second apart: a
    /// percentage is reported once the bar has held it for
    /// [`super::MARK_CALM`], which the second reading shows.
    fn run(marks: [u8; MARKS], fills: &[u32]) -> Vec<Event> {
        let mut detector = detector(marks, Direction::LeftToRight);
        fills
            .iter()
            .flat_map(|&filled| {
                [
                    detector.observe(bar(200, 20, filled, Direction::LeftToRight)),
                    detector.observe(bar(200, 20, filled, Direction::LeftToRight)),
                ]
            })
            .flatten()
            .collect()
    }

    /// His words: *"the area notifies only once the bar is filled to
    /// 100%"*. A bar filling step by step says nothing on the way and one
    /// thing when it is full, and stays quiet while it stays full.
    #[test]
    fn a_bar_notifies_once_when_it_is_full() {
        let fills: Vec<u32> = (0..=20).map(|step| step * 10).chain([200, 200]).collect();
        assert_eq!(run([0; MARKS], &fills), vec![Event::Filled]);
    }

    /// Review of `#170`, round 4: a percentage added in the panel after the
    /// bar has passed it is not reported late; one still ahead is.
    #[test]
    fn a_percentage_added_after_the_bar_passed_it_is_not_reported() {
        let mut detector = detector([0; MARKS], Direction::LeftToRight);
        assert_eq!(
            detector.observe(bar(200, 20, 0, Direction::LeftToRight)),
            None
        );
        assert_eq!(
            detector.observe(bar(200, 20, 140, Direction::LeftToRight)),
            None
        );
        detector.tune(
            Watch::Fills,
            Tuning {
                marks: [50, 90, 0, 0],
                ..Tuning::default()
            },
        );
        assert_eq!(
            detector.observe(bar(200, 20, 140, Direction::LeftToRight)),
            None,
            "50% was passed before it was set"
        );
        assert_eq!(
            detector.observe(bar(200, 20, 188, Direction::LeftToRight)),
            None,
            "held for half a second first"
        );
        assert_eq!(
            detector.observe(bar(200, 20, 188, Direction::LeftToRight)),
            Some(Event::Reached(90))
        );
        assert_eq!(
            detector.observe(bar(200, 20, 200, Direction::LeftToRight)),
            Some(Event::Filled)
        );
    }

    /// Review of `#170`, round 5: a cell at the fill's front counts as filled
    /// once part of it moved, so 97 of 200 pixels read as 50%. A percentage
    /// is never reported before the bar reaches it.
    #[test]
    fn a_percentage_is_not_reported_before_the_bar_reaches_it() {
        assert_eq!(run([50, 0, 0, 0], &[0, 97, 99]), vec![]);
        assert_eq!(run([50, 0, 0, 0], &[0, 97, 104]), vec![Event::Reached(50)]);
    }

    /// A percentage is reported once the bar has held it for half a second
    /// ([`super::MARK_CALM`]), and until then the host must keep reading, since
    /// a still screen sends no frames to read the second half-second on.
    #[test]
    fn a_percentage_waits_out_its_calm_and_keeps_the_host_reading() {
        let mut detector = detector([50, 0, 0, 0], Direction::LeftToRight);
        let at = std::time::Duration::from_millis;
        assert_eq!(
            detector.observe_at(bar(200, 20, 0, Direction::LeftToRight), at(0)),
            None
        );
        assert!(!detector.is_settling());
        let half = bar(200, 20, 110, Direction::LeftToRight);
        assert_eq!(detector.observe_at(half.clone(), at(1_000)), None);
        assert!(detector.is_settling(), "it reached 50% and is waiting");
        assert_eq!(detector.observe_at(half.clone(), at(1_300)), None);
        assert_eq!(
            detector.observe_at(half, at(1_500)),
            Some(Event::Reached(50))
        );
        assert!(!detector.is_settling());
    }

    /// Two percentages reached a little apart, their calms overlapping: the
    /// higher one keeps its own calm when the lower is reported, and the host
    /// keeps reading for it (review of `#178`, round 8).
    #[test]
    fn a_later_percentage_keeps_its_calm_when_an_earlier_one_reports() {
        let mut detector = detector([20, 50, 0, 0], Direction::LeftToRight);
        let at = std::time::Duration::from_millis;
        assert_eq!(
            detector.observe_at(bar(200, 20, 0, Direction::LeftToRight), at(0)),
            None
        );
        let past_20 = bar(200, 20, 60, Direction::LeftToRight);
        let past_50 = bar(200, 20, 110, Direction::LeftToRight);
        assert_eq!(detector.observe_at(past_20, at(100)), None);
        assert_eq!(detector.observe_at(past_50.clone(), at(400)), None);
        assert_eq!(
            detector.observe_at(past_50.clone(), at(600)),
            Some(Event::Reached(20))
        );
        assert!(detector.is_settling(), "50% is still waiting out its calm");
        assert_eq!(
            detector.observe_at(past_50, at(900)),
            Some(Event::Reached(50))
        );
    }

    /// A block that comes in from the start end and leaves again (an
    /// indeterminate bar) reaches a percentage and does not hold it.
    #[test]
    fn a_percentage_a_block_passes_through_is_not_reported() {
        let at = std::time::Duration::from_millis;
        let mut detector = detector([20, 0, 0, 0], Direction::LeftToRight);
        assert_eq!(
            detector.observe_at(bar(200, 20, 0, Direction::LeftToRight), at(0)),
            None
        );
        // 24% for a third of a second, then gone.
        let block = bar(200, 20, 48, Direction::LeftToRight);
        assert_eq!(detector.observe_at(block.clone(), at(100)), None);
        assert_eq!(detector.observe_at(block, at(400)), None);
        assert_eq!(
            detector.observe_at(bar(200, 20, 0, Direction::LeftToRight), at(500)),
            None
        );
        assert!(!detector.is_settling());
        assert_eq!(
            detector.observe_at(bar(200, 20, 48, Direction::LeftToRight), at(900)),
            None,
            "its calm starts again"
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(48))]

        /// Whatever pictures it is given (any size, any levels, any area
        /// hidden, any direction), the bar mode reads them without a panic,
        /// and says no more than one moment per reading.
        #[test]
        fn any_pictures_read_without_a_panic(
            width in 1_u32..70,
            height in 1_u32..70,
            seeds in proptest::collection::vec(proptest::num::u64::ANY, 1..5),
            direction in 0_u8..4,
        ) {
            let direction = [
                Direction::LeftToRight,
                Direction::RightToLeft,
                Direction::TopToBottom,
                Direction::BottomToTop,
            ][usize::from(direction)];
            let mut detector = detector([20, 50, 80, 0], direction);
            for (n, seed) in seeds.iter().enumerate() {
                // A picture of bands, from the seed: flat stretches and edges.
                let mut state = *seed | 1;
                let mut next = move || {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state
                };
                let mut pixels = Vec::new();
                let mut level = 128_u8;
                for _ in 0..height {
                    for _ in 0..width {
                        if next() % 9 == 0 {
                            level = u8::try_from(next() % 256).unwrap_or(0);
                        }
                        pixels.extend_from_slice(&[level, level, level, 255]);
                    }
                }
                if let Some(bitmap) = RgbaBitmap::from_pixels(Size::new(width, height), pixels)
                    && let Some(mut print) = Fingerprint::of(&bitmap)
                {
                    for (at, hidden) in print.hidden.iter_mut().enumerate() {
                        *hidden = at % 11 == 3 && n % 2 == 1;
                    }
                    let _ = detector
                        .observe_at(print, std::time::Duration::from_millis(300 * n as u64));
                }
            }
        }
    }

    /// A bar short of its last cell is not full. Not one pixel short, as
    /// this said until 2026-11-19: a bar that ends in a border of its own
    /// track's colour (a dark theme's) is up to a cell of border short of
    /// full when it is full, and a last line with two pixels of fill in it is
    /// as full as one can tell.
    #[test]
    fn a_bar_a_cell_short_is_not_full() {
        assert_eq!(run([0; MARKS], &[0, 100, 197]), vec![]);
        assert_eq!(run([0; MARKS], &[0, 100, 195]), vec![]);
        assert_eq!(run([0; MARKS], &[0, 197, 200]), vec![Event::Filled]);
    }

    /// Review of `#170`, round 6: one of UP-TAKE's own windows (a card) over
    /// the bar's far end hides lines the area has already read. The bar is
    /// not full until that end is seen again.
    #[test]
    fn a_far_end_hidden_by_a_card_is_not_full_until_seen() {
        let mut detector = detector([0; MARKS], Direction::LeftToRight);
        assert_eq!(
            detector.observe(bar(200, 20, 0, Direction::LeftToRight)),
            None
        );
        let mut covered = bar(200, 20, 200, Direction::LeftToRight);
        for (at, hidden) in covered.hidden.iter_mut().enumerate() {
            if at as u32 % covered.columns >= covered.columns - 2 {
                *hidden = true;
            }
        }
        assert_eq!(detector.observe(covered), None, "its far end is hidden");
        assert_eq!(
            detector.observe(bar(200, 20, 200, Direction::LeftToRight)),
            Some(Event::Filled)
        );
    }

    /// His addition: *"the user may also set percentages where he wants to
    /// get additional notifications, at 20% or 50% for example"*. Each is
    /// reported once, on the way up, and the full bar after them.
    #[test]
    fn each_percentage_notifies_once_on_the_way() {
        let fills = [0, 20, 40, 44, 60, 100, 120, 160, 200];
        assert_eq!(
            run([20, 50, 0, 0], &fills),
            vec![Event::Reached(20), Event::Reached(50), Event::Filled]
        );
    }

    /// A bar that jumps past two percentages in one reading reports the
    /// higher one only: one card, not two at once.
    #[test]
    fn a_jump_past_two_percentages_reports_the_higher() {
        assert_eq!(
            run([20, 50, 0, 0], &[0, 120, 200]),
            vec![Event::Reached(50), Event::Filled]
        );
    }

    /// An area placed over a bar already at 60% does not report the
    /// percentages it was placed past, and does report the full bar.
    #[test]
    fn a_bar_placed_part_full_skips_what_it_already_passed() {
        assert_eq!(
            run([20, 50, 70, 0], &[120, 120, 150, 200]),
            vec![Event::Reached(70), Event::Filled]
        );
    }

    /// An area placed over a bar already 80% to 93% full, the front on a
    /// cell's edge or inside one, still reports the full bar (review of
    /// `#178`, rounds 5 and 6: the empty level came from lines a bar that far
    /// along shows filled, and the bar then never read full).
    #[test]
    fn a_bar_placed_far_along_still_reports_full() {
        for placed in [160, 170, 180, 181, 182, 183, 184, 185, 186, 187] {
            assert_eq!(
                run([0, 0, 0, 0], &[placed, placed, 190, 200]),
                vec![Event::Filled],
                "placed at {placed} of 200"
            );
        }
    }

    /// A bar placed part full whose fill then goes back to empty and fills
    /// again: the lines that were filled at the first reading count as
    /// filled only while they look as they did then.
    #[test]
    fn a_bar_placed_part_full_sees_it_empty_and_fill_again() {
        assert_eq!(
            run([20, 0, 0, 0], &[120, 0, 50, 200]),
            vec![Event::Reached(20), Event::Filled]
        );
    }

    /// A bar that empties for the next file is a new fill, and notifies
    /// again.
    #[test]
    fn a_bar_that_empties_and_fills_again_notifies_again() {
        assert_eq!(
            run([50, 0, 0, 0], &[0, 200, 0, 120, 200]),
            vec![Event::Filled, Event::Reached(50), Event::Filled]
        );
    }

    /// A percentage label in the middle of the bar that changes ahead of
    /// the fill does not count as fill: progress is the run of filled lines
    /// from the start end.
    #[test]
    fn a_label_ahead_of_the_fill_does_not_count() {
        let mut detector = detector([20, 50, 0, 0], Direction::LeftToRight);
        let empty = bar(200, 20, 0, Direction::LeftToRight);
        assert_eq!(detector.observe(empty.clone()), None);
        // The label: every cell of the middle 40 pixels changed.
        let mut labelled = empty;
        for (at, cell) in labelled.cells.iter_mut().enumerate() {
            let column = at as u32 % labelled.columns;
            if (20..30).contains(&column) {
                *cell = FULL;
            }
        }
        assert_eq!(detector.observe(labelled), None);
    }

    /// Each direction fills from its own end, and a fill from the other end
    /// is not progress.
    #[test]
    fn each_direction_fills_from_its_own_end() {
        for direction in [
            Direction::LeftToRight,
            Direction::RightToLeft,
            Direction::TopToBottom,
            Direction::BottomToTop,
        ] {
            let opposite = match direction {
                Direction::LeftToRight => Direction::RightToLeft,
                Direction::RightToLeft => Direction::LeftToRight,
                Direction::TopToBottom => Direction::BottomToTop,
                Direction::BottomToTop => Direction::TopToBottom,
            };
            let mut detector = detector([50, 0, 0, 0], direction);
            assert_eq!(detector.observe(bar(80, 80, 0, direction)), None);
            assert_eq!(
                detector.observe(bar(80, 80, 60, opposite)),
                None,
                "{direction:?}: a fill from the far end"
            );
            assert_eq!(detector.observe(bar(80, 80, 0, direction)), None);
            assert_eq!(detector.observe(bar(80, 80, 44, direction)), None);
            assert_eq!(
                detector.observe(bar(80, 80, 44, direction)),
                Some(Event::Reached(50)),
                "{direction:?}"
            );
            assert_eq!(
                detector.observe(bar(80, 80, 80, direction)),
                Some(Event::Filled),
                "{direction:?}"
            );
        }
    }

    /// A bar whose length is not a whole number of cells: the last, partial
    /// line counts for the pixels it holds, and the bar is full only once it
    /// is filled too.
    #[test]
    fn a_bar_of_uneven_length_counts_its_edge_line() {
        let mut detector = detector([0; MARKS], Direction::LeftToRight);
        assert_eq!(
            detector.observe(bar(201, 20, 0, Direction::LeftToRight)),
            None
        );
        assert_eq!(
            detector.observe(bar(201, 20, 200, Direction::LeftToRight)),
            None
        );
        assert_eq!(
            detector.observe(bar(201, 20, 201, Direction::LeftToRight)),
            Some(Event::Filled)
        );
    }

    /// Turning the direction in the panel starts again from the next
    /// reading: what was learned for one direction means nothing for the
    /// other, and is not reported as a fill.
    #[test]
    fn a_new_direction_learns_the_bar_again() {
        let mut detector = detector([0; MARKS], Direction::LeftToRight);
        assert_eq!(
            detector.observe(bar(200, 20, 0, Direction::LeftToRight)),
            None
        );
        assert_eq!(
            detector.observe(bar(200, 20, 100, Direction::LeftToRight)),
            None
        );
        detector.tune(
            Watch::Fills,
            Tuning {
                direction: Direction::RightToLeft,
                ..Tuning::default()
            },
        );
        // The first reading after the turn is learned, not compared.
        assert_eq!(
            detector.observe(bar(200, 20, 0, Direction::RightToLeft)),
            None
        );
        assert_eq!(
            detector.observe(bar(200, 20, 200, Direction::RightToLeft)),
            Some(Event::Filled)
        );
    }

    /// A resized area, or a reading by another route, is another bar as far
    /// as its lines go: learned, not reported.
    #[test]
    fn a_resized_or_rebased_bar_is_learned_not_reported() {
        let mut detector = detector([0; MARKS], Direction::LeftToRight);
        assert_eq!(
            detector.observe(bar(200, 20, 0, Direction::LeftToRight)),
            None
        );
        assert_eq!(
            detector.observe(bar(100, 20, 100, Direction::LeftToRight)),
            None
        );
        detector.rebase_at(
            bar(200, 20, 0, Direction::LeftToRight),
            std::time::Duration::ZERO,
        );
        assert_eq!(
            detector.observe(bar(200, 20, 200, Direction::LeftToRight)),
            Some(Event::Filled)
        );
    }

    /// A bar as a real screen shows it, from the maintainer's rig pass of
    /// 2026-11-11: a 240 by 40 area with an 8 pixel margin of page around
    /// the bar, the bar a pill with round ends (radius 12), a light track,
    /// a darker fill, and a white `45%` label fixed near its far end.
    /// `filled` is pixels of the track's 224 filled; `dim` is the darkened
    /// screen Windows' snipping overlay (Win+Shift+S) draws over everything.
    fn pill(filled: u32, dim: bool) -> Fingerprint {
        const PAGE: u8 = 250;
        const TRACK: u8 = 225;
        const FILL: u8 = 100;
        const TEXT: u8 = 250;
        let (width, height, margin) = (240_u32, 40_u32, 8_u32);
        let radius = f64::from(height - 2 * margin) / 2.0;
        let middle = f64::from(height) / 2.0;
        let (left, right) = (
            f64::from(margin) + radius,
            f64::from(width - margin) - radius,
        );
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                let dx = px - px.clamp(left, right);
                let on_track = dx * dx + (py - middle) * (py - middle) <= radius * radius;
                let label = (width - margin - 50..width - margin - 30).contains(&x)
                    && (15..25).contains(&y);
                let mut level = if !on_track {
                    PAGE
                } else if label {
                    TEXT
                } else if x < margin + filled {
                    FILL
                } else {
                    TRACK
                };
                if dim {
                    level = level / 10 * 6;
                }
                pixels.extend_from_slice(&[level, level, level, 255]);
            }
        }
        let bitmap = RgbaBitmap::from_pixels(Size::new(width, height), pixels)
            .unwrap_or_else(|| unreachable!("a well-formed bitmap"));
        Fingerprint::of(&bitmap).unwrap_or_else(|| unreachable!("a non-empty bitmap"))
    }

    /// The maintainer's rig pass, 2026-11-11: an area with page around a
    /// round-ended bar never reported its percentages or the full bar. The
    /// margin never changes, so a run counted from the area's own edge
    /// never started, and a round end never had half its line filled.
    #[test]
    fn a_bar_with_page_around_it_and_round_ends_notifies() {
        let mut detector = detector([20, 50, 0, 0], Direction::LeftToRight);
        let events: Vec<Event> = (0..=28)
            .map(|step| step * 8)
            .chain([224, 224])
            .filter_map(|filled| detector.observe(pill(filled, false)))
            .collect();
        assert_eq!(
            events,
            vec![Event::Reached(20), Event::Reached(50), Event::Filled]
        );
    }

    /// The same rig pass: Win+Shift+S darkened the screen, every line of the
    /// area changed, and the area reported its percentages and the full bar
    /// at once. The page around the bar changed too, which a fill never does,
    /// so such a reading is not a fill; the fill after it still reads.
    #[test]
    fn a_darkened_screen_over_a_bar_is_not_a_fill() {
        let mut detector = detector([20, 50, 0, 0], Direction::LeftToRight);
        assert_eq!(detector.observe(pill(0, false)), None);
        assert_eq!(detector.observe(pill(0, true)), None);
        assert_eq!(detector.observe(pill(30, true)), None);
        assert_eq!(detector.observe(pill(0, false)), None);
        assert_eq!(detector.observe(pill(120, false)), None);
        assert_eq!(detector.observe(pill(120, false)), Some(Event::Reached(50)));
        assert_eq!(detector.observe(pill(150, true)), None);
        assert_eq!(detector.observe(pill(224, false)), Some(Event::Filled));
    }

    /// A round-ended bar a few pixels short of full is not full.
    #[test]
    fn a_round_ended_bar_short_of_its_end_is_not_full() {
        let mut detector = detector([0; MARKS], Direction::LeftToRight);
        assert_eq!(detector.observe(pill(0, false)), None);
        assert_eq!(detector.observe(pill(214, false)), None);
        assert_eq!(detector.observe(pill(224, false)), Some(Event::Filled));
    }

    /// Only the bar mode reads as a bar: the same readings in *Notify when
    /// it changes* report a change and never a full bar.
    #[test]
    fn only_the_bar_mode_reports_a_full_bar() {
        let mut detector = Detector::for_watch(Watch::Changes);
        let events: Vec<Event> = [0, 200]
            .iter()
            .filter_map(|&filled| detector.observe(bar(200, 20, filled, Direction::LeftToRight)))
            .collect();
        assert_eq!(events, vec![Event::Changed]);
        assert!(!Detector::for_watch(Watch::Fills).is_settling());
    }
}

/// The bar mode against bars drawn as real pages draw them, for the failures
/// of the maintainer's rig pass of 2026-11-19 (roadmap `1.61`; the same bars
/// as real renders are `tools/bar-harness`). Each test names one: a style, a
/// theme, a direction, a place in the area, or text near the bar. Nothing
/// here is a picture on disk: the bars are drawn pixel by pixel, which keeps
/// these tests running on every build.
#[cfg(test)]
mod real_bars {
    use super::{Detector, Direction, Event, Fingerprint, MARKS, Tuning, Watch};
    use crate::bitmap::RgbaBitmap;
    use crate::geometry::Size;
    use std::time::Duration;

    #[derive(Clone, Copy)]
    struct Theme {
        page: u8,
        track: u8,
        fill: u8,
        line: u8,
    }

    /// Luma of the test page's light and dark themes.
    const LIGHT: Theme = Theme {
        page: 255,
        track: 227,
        fill: 113,
        line: 218,
    };
    const DARK: Theme = Theme {
        page: 23,
        track: 54,
        fill: 143,
        line: 53,
    };

    #[derive(Clone, Copy, PartialEq)]
    enum Fill {
        Flat,
        Gradient,
        Stripes,
        Segments,
    }

    #[derive(Clone, Copy)]
    struct Look {
        /// The bar's length and thickness, pixels.
        length: u32,
        thickness: u32,
        /// Page between the area's edge and the bar, along the bar and across.
        margin: (u32, u32),
        theme: Theme,
        round: bool,
        /// A border round the track, pixels.
        border: u32,
        fill: Fill,
        /// Text drawn in the middle of the bar that changes with the fill.
        text_inside: bool,
        /// A percentage beside the bar that changes with the fill.
        label_beside: bool,
        direction: Direction,
    }

    const ROUND: Look = Look {
        length: 240,
        thickness: 18,
        margin: (11, 7),
        theme: LIGHT,
        round: true,
        border: 0,
        fill: Fill::Flat,
        text_inside: false,
        label_beside: false,
        direction: Direction::LeftToRight,
    };

    /// One frame of `look`, `filled` pixels along: `block` is the left edge of
    /// an indeterminate bar's moving block (a third of the length wide)
    /// instead of a fill; `phase` moves the stripes; `dim` is the screen
    /// Windows' snipping overlay darkens.
    fn render(look: &Look, filled: u32, block: Option<i32>, phase: u32, dim: bool) -> Fingerprint {
        render_dimmed(look, filled, block, phase, &|_, _| dim)
    }

    /// [`render`], with the snipping overlay's darkening on the pixels `dim`
    /// takes (area pixels, before the bar is turned for its direction): a
    /// selection dragged over part of the area shows that part bright.
    fn render_dimmed(
        look: &Look,
        filled: u32,
        block: Option<i32>,
        phase: u32,
        dim: &dyn Fn(u32, u32) -> bool,
    ) -> Fingerprint {
        let label = if look.label_beside { 14 + 28 + 30 } else { 0 };
        let (w, h) = (
            look.margin.0 * 2 + look.length + label,
            look.margin.1 * 2 + look.thickness,
        );
        let theme = look.theme;
        let radius = f64::from(look.thickness) / 2.0;
        let length = f64::from(look.length);
        let segments = 20_u32;
        let gap = 4_u32;
        let block_width = i32::try_from(look.length / 3).unwrap_or(0);
        let lighter: i32 = if theme.track > theme.fill { 1 } else { -1 };
        let mut grey = vec![theme.page; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let (bx, by) = (
                    i64::from(x) - i64::from(look.margin.0),
                    i64::from(y) - i64::from(look.margin.1),
                );
                let in_rect = (0..i64::from(look.length)).contains(&bx)
                    && (0..i64::from(look.thickness)).contains(&by);
                let (cx, cy) = (bx as f64 + 0.5, by as f64 + 0.5);
                let in_shape = in_rect
                    && (!look.round || {
                        let dx = if cx < radius {
                            radius - cx
                        } else if cx > length - radius {
                            cx - (length - radius)
                        } else {
                            0.0
                        };
                        dx * dx + (cy - radius) * (cy - radius) <= radius * radius
                    });
                let mut level = theme.page;
                if in_shape {
                    let (bar, across) = (bx as u32, by as u32);
                    let b = i64::from(look.border);
                    let on_border = look.border > 0
                        && (bx < b
                            || bx >= i64::from(look.length) - b
                            || by < b
                            || by >= i64::from(look.thickness) - b);
                    let width = (look.length - (segments - 1) * gap) / segments;
                    let in_gap = look.fill == Fill::Segments && bar % (width + gap) >= width;
                    level = if in_gap {
                        theme.page
                    } else if on_border {
                        theme.line
                    } else {
                        theme.track
                    };
                    let is_filled = match (look.fill, block) {
                        (_, Some(left)) => (left..left + block_width).contains(&(bar as i32)),
                        (Fill::Segments, None) => {
                            bar / (width + gap) < filled * segments / look.length
                        }
                        (_, None) => bar < filled,
                    };
                    if is_filled && !on_border && !in_gap {
                        level = match look.fill {
                            Fill::Gradient if block.is_none() => {
                                let t = f64::from(bar) / f64::from(filled.max(1));
                                (f64::from(theme.fill) + f64::from(lighter) * 40.0 * (1.0 - t))
                                    as u8
                            }
                            Fill::Stripes if ((bar + across + phase * 3) / 6) % 2 == 1 => {
                                (i32::from(theme.fill) + lighter * 30) as u8
                            }
                            _ => theme.fill,
                        };
                    }
                    // Text in the middle, changing with every step.
                    let (mid, around) = (look.length / 2, look.thickness / 2);
                    if look.text_inside
                        && (mid - 12..mid + 12).contains(&bar)
                        && across + 5 >= around
                        && across <= around + 5
                        && (bar + across + filled / 2).is_multiple_of(3)
                    {
                        level = theme.page;
                    }
                }
                // A percentage beside the bar.
                let beside = i64::from(look.length) + 14;
                if look.label_beside
                    && (beside..beside + 28).contains(&bx)
                    && (i64::from(look.thickness) / 2 - 5..=i64::from(look.thickness) / 2 + 5)
                        .contains(&by)
                    && (bx + by + i64::from(filled / 2)) % 3 == 0
                {
                    level = theme.track;
                }
                if dim(x, y) {
                    level = level / 10 * 6;
                }
                grey[(y * w + x) as usize] = level;
            }
        }
        // The bar was drawn left to right; turned for the others.
        let (cols, rows) = match look.direction {
            Direction::LeftToRight | Direction::RightToLeft => (w, h),
            Direction::TopToBottom | Direction::BottomToTop => (h, w),
        };
        let at = |x: u32, y: u32| match look.direction {
            Direction::LeftToRight => (y * w + x) as usize,
            Direction::RightToLeft => (y * w + (w - 1 - x)) as usize,
            Direction::TopToBottom => (x * w + y) as usize,
            Direction::BottomToTop => (x * w + (w - 1 - y)) as usize,
        };
        let mut pixels = Vec::with_capacity((cols * rows * 4) as usize);
        for y in 0..rows {
            for x in 0..cols {
                let level = grey[at(x, y)];
                pixels.extend_from_slice(&[level, level, level, 255]);
            }
        }
        let bitmap = RgbaBitmap::from_pixels(Size::new(cols, rows), pixels)
            .unwrap_or_else(|| unreachable!("a well-formed bitmap"));
        Fingerprint::of(&bitmap).unwrap_or_else(|| unreachable!("a non-empty bitmap"))
    }

    fn detector(direction: Direction, marks: [u8; MARKS]) -> Detector {
        let mut detector = Detector::for_watch(Watch::Fills);
        detector.tune(
            Watch::Fills,
            Tuning {
                marks,
                direction,
                ..Tuning::default()
            },
        );
        detector
    }

    /// The events of a fill, one pixel a frame, `repeat` readings of each
    /// frame 33 ms apart (about 30 a second), with the pixels filled when each
    /// came. The area is placed over the empty bar.
    fn fill(look: &Look, marks: [u8; MARKS], repeat: u32) -> Vec<(Event, u32)> {
        let mut detector = detector(look.direction, marks);
        let mut at = Duration::ZERO;
        detector.observe_at(render(look, 0, None, 0, false), at);
        let mut seen = Vec::new();
        for filled in 1..=look.length {
            let print = render(look, filled, None, filled, false);
            for _ in 0..repeat {
                at += Duration::from_millis(33);
                if let Some(event) = detector.observe_at(print.clone(), at) {
                    seen.push((event, filled));
                }
            }
        }
        // And holds there, as a finished bar does.
        let print = render(look, look.length, None, 0, false);
        for _ in 0..40 {
            at += Duration::from_millis(33);
            if let Some(event) = detector.observe_at(print.clone(), at) {
                seen.push((event, look.length));
            }
        }
        seen
    }

    /// A fill reports 50% once, not before the bar is half full and soon
    /// after, and then the full bar once, when it is.
    #[track_caller]
    fn notifies(look: &Look) {
        for repeat in [1, 12] {
            let seen = fill(look, [50, 0, 0, 0], repeat);
            let [(Event::Reached(50), half), (Event::Filled, full)] = seen[..] else {
                panic!("expected a half and a full bar, saw {seen:?} (repeat {repeat})");
            };
            let length = look.length;
            // Not early, and late by the half second a percentage is held
            // (a pixel a reading at 30 readings a second is 15 pixels; at
            // twelve readings a pixel, one or two) and by a segment's width.
            let late = if repeat == 1 { 56 } else { 28 };
            assert!(
                half * 2 >= length,
                "50% reported early, at {half} of {length}"
            );
            assert!(
                half * 2 <= length + late,
                "50% reported late, at {half} of {length}"
            );
            // The last pixels of a round tip are narrower than a cell, and a
            // bar is full when the last line that holds track is.
            assert!(
                full + 6 >= length,
                "full reported early, at {full} of {length}"
            );
        }
    }

    /// The failure that began it, in his words: *"round ends: everything
    /// correct at 20 s; at 60 s nothing notified"*. Nothing the reading sees
    /// depends on how fast the bar grows, but on where the area's cells fall
    /// on it, and a round bar's first line, a cap, looked unlike the rest of
    /// the empty bar: it was taken for already filled, and a filled line that
    /// then changed stopped the run for good. Every alignment of the bar in
    /// its cells, both speeds.
    #[test]
    fn a_round_bar_notifies_wherever_it_falls_in_the_area() {
        for along in 5..=12 {
            for across in 5..=9 {
                notifies(&Look {
                    margin: (along, across),
                    ..ROUND
                });
            }
        }
    }

    /// His 60 s run, as readings: a bar that grows by a few hundredths of a
    /// pixel between readings reads as the bar that grows by a pixel, with
    /// every picture repeated forty times.
    #[test]
    fn a_slow_fill_reads_like_a_fast_one() {
        let fast = fill(&ROUND, [50, 0, 0, 0], 1);
        let slow = fill(&ROUND, [50, 0, 0, 0], 40);
        assert_eq!(
            fast.iter().map(|(event, _)| *event).collect::<Vec<_>>(),
            slow.iter().map(|(event, _)| *event).collect::<Vec<_>>()
        );
    }

    /// *"Square with 1px border: 50% notified, full never."* A border is
    /// where the fill does not go, and a bar that ends in one is full when its
    /// last cell is part border: in a dark theme the border is the track's own
    /// colour, and cannot be told from a pixel the fill missed.
    #[test]
    fn a_square_bar_with_a_border_is_full() {
        for theme in [LIGHT, DARK] {
            for along in [8, 9, 10, 11] {
                notifies(&Look {
                    round: false,
                    border: 1,
                    theme,
                    margin: (along, 8),
                    ..ROUND
                });
            }
        }
    }

    /// *"Thin, 4 px: nothing."* A bar one cell thick in an area ten cells
    /// deep is a line's tenth, which the margin test of an eighth took for
    /// page.
    #[test]
    fn a_thin_bar_in_a_tall_area_notifies() {
        for across in [8, 12, 24] {
            for theme in [LIGHT, DARK] {
                notifies(&Look {
                    thickness: 4,
                    margin: (11, across),
                    theme,
                    ..ROUND
                });
            }
        }
    }

    /// *"Thick, with the percentage inside: nothing."* The text changes in
    /// the middle of the bar with every step and is not the fill.
    #[test]
    fn a_thick_bar_with_text_inside_notifies() {
        for theme in [LIGHT, DARK] {
            notifies(&Look {
                thickness: 34,
                text_inside: true,
                theme,
                ..ROUND
            });
        }
    }

    /// *"Gradient fill: nothing."* The fill's start and end differ in colour,
    /// and the gradient stretches as the fill grows.
    #[test]
    fn a_gradient_fill_notifies() {
        for theme in [LIGHT, DARK] {
            notifies(&Look {
                fill: Fill::Gradient,
                theme,
                ..ROUND
            });
        }
    }

    /// *"Striped and animated: worked fully."* It still must: the stripes
    /// move on every frame and the far end is not the start's colour.
    #[test]
    fn a_striped_fill_notifies() {
        for theme in [LIGHT, DARK] {
            notifies(&Look {
                fill: Fill::Stripes,
                theme,
                ..ROUND
            });
        }
    }

    /// *"Segmented 20 blocks with 4 px gaps: nothing."* A line in a gap has
    /// no track and never changes; it is part of the bar and is passed over.
    #[test]
    fn a_segmented_bar_notifies() {
        for theme in [LIGHT, DARK] {
            for along in [8, 9, 10, 11] {
                notifies(&Look {
                    fill: Fill::Segments,
                    round: false,
                    thickness: 20,
                    margin: (along, 8),
                    theme,
                    ..ROUND
                });
            }
        }
    }

    /// *"Fills from the right, and vertical from the bottom: nothing."*
    #[test]
    fn a_bar_filling_from_the_right_or_the_bottom_notifies() {
        for direction in [
            Direction::RightToLeft,
            Direction::TopToBottom,
            Direction::BottomToTop,
        ] {
            for theme in [LIGHT, DARK] {
                notifies(&Look {
                    direction,
                    theme,
                    ..ROUND
                });
            }
        }
    }

    /// *"Dark mode doesn't seem to work."* A dark page and a track a little
    /// lighter than it; every style above runs in both themes too, and here
    /// the round ends and the thin bar in particular, whose edge cells are the
    /// least different from the page.
    #[test]
    fn a_bar_on_a_dark_page_notifies() {
        notifies(&Look {
            theme: DARK,
            ..ROUND
        });
        notifies(&Look {
            theme: DARK,
            thickness: 4,
            ..ROUND
        });
    }

    /// A percentage beside the bar that changes with every step is not part of
    /// it: 50% is the bar's own half, and the bar is full at its own end.
    #[test]
    fn a_percentage_beside_the_bar_is_not_the_bar() {
        for theme in [LIGHT, DARK] {
            for margin in [(11, 7), (13, 9)] {
                notifies(&Look {
                    label_beside: true,
                    theme,
                    margin,
                    ..ROUND
                });
            }
        }
    }

    /// An area drawn tight over the bar, with no page round it.
    #[test]
    fn a_tight_area_over_the_bar_notifies() {
        for round in [false, true] {
            notifies(&Look {
                round,
                margin: (0, 0),
                ..ROUND
            });
        }
    }

    /// *"Marquee: notified 50% repeatedly; it must notify nothing."* An
    /// indeterminate bar's block grows into the track from the start end as a
    /// fill does, and leaves it again. At any speed from a sweep of three
    /// seconds, whatever the percentages: a fill keeps what it reached, a block
    /// does not (a percentage is reported once held for half a second).
    #[test]
    fn a_marquee_notifies_nothing() {
        for pace in [3_i32, 6, 12] {
            let look = ROUND;
            let mut detector = detector(look.direction, [20, 50, 80, 0]);
            let mut at = Duration::ZERO;
            detector.observe_at(render(&look, 0, None, 0, false), at);
            let third = i32::try_from(look.length / 3).unwrap_or(0);
            let mut events = Vec::new();
            for _ in 0..3 {
                let mut left = -third;
                while left < i32::try_from(look.length).unwrap_or(0) {
                    at += Duration::from_millis(33);
                    let print = render(&look, 0, Some(left), 0, false);
                    events.extend(detector.observe_at(print, at));
                    left += pace;
                }
            }
            assert_eq!(events, vec![], "a block moving {pace} pixels a reading");
        }
    }

    /// The limit of [`Bar`]'s calm, pinned (reviews of `#178`, rounds 11 and
    /// 12): a block a third of the bar long moving a pixel a reading, a sweep
    /// of about ten seconds, stays joined to the start end with its front past
    /// 20% for over a second, longer than [`MARK_CALM`], and 20% is reported
    /// on each sweep. Nothing above the block's length is, and never the full
    /// bar.
    #[test]
    fn a_marquee_slower_than_the_calm_reports_only_below_its_block() {
        let look = ROUND;
        let mut detector = detector(look.direction, [20, 50, 80, 0]);
        let mut at = Duration::ZERO;
        detector.observe_at(render(&look, 0, None, 0, false), at);
        let third = i32::try_from(look.length / 3).unwrap_or(0);
        let mut events = Vec::new();
        for _ in 0..3 {
            let mut left = -third;
            while left < i32::try_from(look.length).unwrap_or(0) {
                at += Duration::from_millis(33);
                let print = render(&look, 0, Some(left), 0, false);
                events.extend(detector.observe_at(print, at));
                left += 1;
            }
        }
        assert_eq!(events, vec![Event::Reached(20); 3]);
    }

    /// Win+Shift+S darkens everything, the page round the bar too; a fill
    /// never changes that. The reading is not a fill, whatever the bar does
    /// under it.
    #[test]
    fn a_darkened_screen_is_not_a_fill_for_any_style() {
        for (which, look) in [
            ROUND,
            Look {
                thickness: 4,
                ..ROUND
            },
            Look {
                theme: DARK,
                ..ROUND
            },
        ]
        .into_iter()
        .enumerate()
        {
            let mut detector = detector(look.direction, [50, 0, 0, 0]);
            let mut at = Duration::ZERO;
            detector.observe_at(render(&look, 0, None, 0, false), at);
            let mut events = Vec::new();
            for filled in [0, 60, 120, 240] {
                for _ in 0..3 {
                    at += Duration::from_millis(333);
                    events.extend(detector.observe_at(render(&look, filled, None, 0, true), at));
                }
            }
            assert_eq!(events, vec![], "the screen is dim, look {which}");
        }
    }

    /// The maintainer's rig trace of `#178`, 2026-11-20, replayed: during
    /// Win+Shift+S the host lost the page's word on the area's mark, skipped
    /// a reading, and learned the bar again from the next one, which was the
    /// darkened screen; a selection dragged over it then read as a full bar,
    /// ten times in a minute. The synthetic darkening of the tests above does
    /// not reproduce it, so these are the rig's own readings
    /// (`tests/fixtures/snip-skip-2026-11-20.txt`). Held over the skip
    /// ([`Detector::hold`]), the bar keeps what it learned and reports
    /// nothing; learned again, as the host did, the same readings report a
    /// full bar.
    #[test]
    fn a_bar_held_over_a_skip_does_not_learn_the_darkened_screen() {
        let readings: Vec<(Duration, Fingerprint)> =
            include_str!("../tests/fixtures/snip-skip-2026-11-20.txt")
                .lines()
                .filter(|line| !line.starts_with('#'))
                .map(|line| {
                    let parts: Vec<&str> = line.split(' ').collect();
                    let number = |at: usize| parts[at].parse::<u32>().unwrap_or(0);
                    let cells = (0..parts[5].len() / 2)
                        .map(|at| {
                            u8::from_str_radix(&parts[5][at * 2..at * 2 + 2], 16).unwrap_or(0)
                        })
                        .collect();
                    let hidden = parts[6].chars().map(|digit| digit == '1').collect();
                    (
                        Duration::from_millis(u64::from(number(0))),
                        Fingerprint {
                            columns: number(1),
                            rows: number(2),
                            cells,
                            hidden,
                            edge: (number(3), number(4)),
                        },
                    )
                })
                .collect();
        assert_eq!(readings.len(), 39, "the fixture");
        let marks = [25, 50, 75, 0];
        // Held: learned from the empty bar, read up to the skip, held.
        let mut held = detector(Direction::LeftToRight, marks);
        let mut events = Vec::new();
        for (at, print) in &readings[..2] {
            events.extend(held.observe_at(print.clone(), *at));
        }
        held.hold();
        for (at, print) in &readings[2..] {
            events.extend(held.observe_at(print.clone(), *at));
        }
        assert_eq!(events, vec![], "held over the skip");
        // Learned again from the first reading after the skip.
        let mut again = detector(Direction::LeftToRight, marks);
        let events: Vec<Event> = readings[2..]
            .iter()
            .filter_map(|(at, print)| again.observe_at(print.clone(), *at))
            .collect();
        assert_eq!(
            events,
            vec![Event::Filled],
            "learned again, as the host did"
        );
    }

    /// Feeds `look` at `filled` for `ms` milliseconds, a reading every 33.
    fn feed(
        detector: &mut Detector,
        look: &Look,
        filled: u32,
        dim: bool,
        ms: u64,
        at: &mut Duration,
    ) -> Vec<Event> {
        let print = render(look, filled, None, 0, dim);
        let mut events = Vec::new();
        for _ in 0..ms / 33 {
            *at += Duration::from_millis(33);
            events.extend(detector.observe_at(print.clone(), *at));
        }
        events
    }

    /// `I-493`, found by the founder on 2026-11-20: an area placed in a light
    /// page that was then switched to dark never read again, since every
    /// reading looked covered. A page that stays changed and still for
    /// [`RELEARN_AFTER`] is a new page: the bar is learned again from it and
    /// read, an idle bar and one switched in the middle of its fill alike.
    #[test]
    fn a_bar_reads_again_after_its_page_changes_for_good() {
        let dark = Look {
            theme: DARK,
            ..ROUND
        };
        // Switched while idle, then filled.
        let mut area = detector(ROUND.direction, [50, 0, 0, 0]);
        let mut at = Duration::ZERO;
        area.observe_at(render(&ROUND, 0, None, 0, false), at);
        let mut events = feed(&mut area, &dark, 0, false, 5_400, &mut at);
        for filled in (0..=ROUND.length).step_by(4).chain([ROUND.length]) {
            events.extend(feed(&mut area, &dark, filled, false, 66, &mut at));
        }
        assert_eq!(
            events,
            vec![Event::Reached(50), Event::Filled],
            "switched while idle"
        );
        // Switched at 10% while it fills: that fill is lost (the bar moves,
        // so the area is never still); the bar empties for the next file and
        // waits, is learned after five seconds, and the next fill is read
        // whole. (A full bar held still that long is learned full: a bar
        // already full at the first reading, a limit of [`Bar`].)
        let mut area = detector(ROUND.direction, [25, 50, 0, 0]);
        let mut at = Duration::ZERO;
        area.observe_at(render(&ROUND, 0, None, 0, false), at);
        let mut events = feed(&mut area, &ROUND, 24, false, 330, &mut at);
        for filled in (24..=ROUND.length).step_by(2) {
            events.extend(feed(&mut area, &dark, filled, false, 66, &mut at));
        }
        events.extend(feed(&mut area, &dark, ROUND.length, false, 1_000, &mut at));
        events.extend(feed(&mut area, &dark, 0, false, 5_400, &mut at));
        assert_eq!(events, vec![], "the fill switched under it is lost");
        for filled in (0..=ROUND.length).step_by(4).chain([ROUND.length]) {
            events.extend(feed(&mut area, &dark, filled, false, 66, &mut at));
        }
        assert_eq!(
            events,
            vec![Event::Reached(25), Event::Reached(50), Event::Filled],
            "the next fill"
        );
    }

    /// The other side of [`RELEARN_AFTER`]: a darkened screen (Win+Shift+S)
    /// held a little short of it, then gone, is a cover and nothing more. The
    /// bar is not learned from it and nothing is reported.
    #[test]
    fn a_cover_held_short_of_the_relearn_wait_is_only_a_cover() {
        let mut detector = detector(ROUND.direction, [50, 0, 0, 0]);
        let mut at = Duration::ZERO;
        detector.observe_at(render(&ROUND, 0, None, 0, false), at);
        let mut events = feed(&mut detector, &ROUND, 60, false, 330, &mut at);
        events.extend(feed(&mut detector, &ROUND, 60, true, 4_900, &mut at));
        events.extend(feed(&mut detector, &ROUND, 60, false, 330, &mut at));
        for filled in (60..=ROUND.length).step_by(4).chain([ROUND.length]) {
            events.extend(feed(&mut detector, &ROUND, filled, false, 66, &mut at));
        }
        assert_eq!(events, vec![Event::Reached(50), Event::Filled]);
    }

    /// A page that changed while the host could not read ([`Detector::hold`])
    /// is a new page too, once it has stayed so for [`RELEARN_AFTER`].
    #[test]
    fn a_page_changed_over_a_skip_is_read_again() {
        let dark = Look {
            theme: DARK,
            ..ROUND
        };
        let mut detector = detector(ROUND.direction, [50, 0, 0, 0]);
        let mut at = Duration::ZERO;
        detector.observe_at(render(&ROUND, 0, None, 0, false), at);
        detector.hold();
        let mut events = feed(&mut detector, &dark, 0, false, 5_400, &mut at);
        for filled in (0..=ROUND.length).step_by(4).chain([ROUND.length]) {
            events.extend(feed(&mut detector, &dark, filled, false, 66, &mut at));
        }
        assert_eq!(events, vec![Event::Reached(50), Event::Filled]);
    }

    /// The founder's rig pass of `#178`, 2026-11-20: Win+Shift+S darkens the
    /// screen, and a selection dragged over the bar shows the part inside it
    /// bright again. With most of the page around the bar inside the
    /// selection, the darkened rest of the bar looked like a fill from its
    /// start end, and both the 50% mark and the full bar notified. No
    /// selection, from either side, at any size, over a bar in either theme
    /// and at any place in its fill, is a fill.
    #[test]
    fn a_snipping_selection_over_the_bar_is_not_a_fill() {
        for look in [
            ROUND,
            Look {
                theme: DARK,
                ..ROUND
            },
            Look {
                round: false,
                border: 1,
                ..ROUND
            },
        ] {
            let (w, h) = (
                look.margin.0 * 2 + look.length,
                look.margin.1 * 2 + look.thickness,
            );
            for filled in [0, 60, 130] {
                // Selections: from the left edge, from the right edge, and a
                // band across the middle, each as tall as the area or only
                // the bar and a little page.
                for (rows, from_left) in [
                    ((0, h), true),
                    ((0, h), false),
                    ((4, h - 4), true),
                    ((4, h - 4), false),
                ] {
                    let mut detector = detector(look.direction, [50, 0, 0, 0]);
                    let mut at = Duration::ZERO;
                    detector.observe_at(render(&look, filled, None, 0, false), at);
                    let mut events = Vec::new();
                    for step in 0..=w / 4 {
                        let edge = step * 4;
                        let bright = move |x: u32, y: u32| {
                            let across = (rows.0..rows.1).contains(&y);
                            let along = if from_left { x < edge } else { x >= w - edge };
                            across && along
                        };
                        let print = render_dimmed(&look, filled, None, 0, &|x, y| !bright(x, y));
                        for _ in 0..20 {
                            at += Duration::from_millis(33);
                            events.extend(detector.observe_at(print.clone(), at));
                        }
                    }
                    assert_eq!(
                        events,
                        vec![],
                        "theme page {}, filled {filled}, rows {rows:?}, from the left {from_left}",
                        look.theme.page
                    );
                }
            }
        }
    }

    /// A bar that fills on while the screen is dark reads again once the
    /// cover goes: the page must look as it did before the cover, and a fill
    /// that moved on under it must not leave the area waiting for good. (An
    /// area drawn tighter than a cell around the bar has no page here, and a
    /// darkened screen is its known limit, [`Bar`]; the real renders at a
    /// 3-pixel margin, whose page cells hold the bar's edge, are run by
    /// `tests/bar_fixtures.rs`.)
    #[test]
    fn a_bar_that_filled_on_under_a_cover_reads_again_after_it() {
        for margin in [(11, 7), (16, 9), (24, 12)] {
            for theme in [LIGHT, DARK] {
                let look = Look {
                    margin,
                    theme,
                    ..ROUND
                };
                let mut detector = detector(look.direction, [50, 0, 0, 0]);
                let mut at = Duration::ZERO;
                let mut events = Vec::new();
                let mut read =
                    |detector: &mut Detector, filled: u32, dim: bool, at: &mut Duration| {
                        for _ in 0..20 {
                            *at += Duration::from_millis(33);
                            events.extend(
                                detector.observe_at(render(&look, filled, None, 0, dim), *at),
                            );
                        }
                    };
                detector.observe_at(render(&look, 0, None, 0, false), at);
                read(&mut detector, 60, false, &mut at);
                // Covered from 25% to 75%, past the 50% mark, for about four
                // seconds: shorter than RELEARN_AFTER.
                for filled in (60..=180).step_by(24) {
                    read(&mut detector, filled, true, &mut at);
                }
                for filled in (180..=look.length).step_by(4).chain([look.length]) {
                    read(&mut detector, filled, false, &mut at);
                }
                assert_eq!(
                    events,
                    vec![Event::Reached(50), Event::Filled],
                    "margin {margin:?}, page {}",
                    theme.page
                );
            }
        }
    }

    /// The founder's rig pass of `#178`, 2026-11-20: a 50% mark on a 60-second
    /// fill came at about 54%. Measured here before the fix, on the test
    /// page's 440-pixel bar read 30 times a second: 25% at 26.8%, 50% at
    /// 51.3%, 75% at 76.8%, and at a 20-second fill up to 3.6% late (the
    /// half-second calm, and the front line not counted). A steady fill's
    /// percentages are now reported within a percent, never early, at any
    /// speed and at a reading every half second.
    #[test]
    fn a_steady_fills_percentages_come_within_a_percent() {
        for (secs, step_ms) in [(20_u64, 33_u64), (60, 33), (120, 33), (60, 500)] {
            for style in [
                ROUND,
                Look {
                    round: false,
                    ..ROUND
                },
            ] {
                let look = Look {
                    length: 440,
                    ..style
                };
                let mut detector = detector(look.direction, [25, 50, 75, 0]);
                detector.observe_at(render(&look, 0, None, 0, false), Duration::ZERO);
                let mut t = 0_u64;
                let mut seen = Vec::new();
                while t < secs * 1000 + 2000 {
                    t += step_ms;
                    let filled = u32::try_from((440 * t) / (secs * 1000))
                        .unwrap_or(440)
                        .min(440);
                    let print = render(&look, filled, None, 0, false);
                    if let Some(Event::Reached(mark)) =
                        detector.observe_at(print, Duration::from_millis(t))
                    {
                        seen.push(mark);
                        // In tenths of a percent.
                        let at = filled * 1000 / 440;
                        let mark = u32::from(mark) * 10;
                        assert!(
                            at >= mark && at <= mark + 10,
                            "{secs} s, a reading every {step_ms} ms, round {}: {} at {at}",
                            look.round,
                            mark / 10
                        );
                    }
                }
                assert_eq!(seen, vec![25, 50, 75], "{secs} s, round {}", look.round);
            }
        }
    }

    /// A reading with the page covered (Win+Shift+S) breaks a percentage's
    /// calm: the first reading after it is not reported at once on the strength
    /// of the time before the cover (review of `#178`, finding 2).
    #[test]
    fn a_covered_reading_starts_a_percentage_calm_again() {
        let mut detector = detector(ROUND.direction, [20, 0, 0, 0]);
        let ms = Duration::from_millis;
        assert_eq!(
            detector.observe_at(render(&ROUND, 0, None, 0, false), ms(0)),
            None
        );
        assert_eq!(
            detector.observe_at(render(&ROUND, 72, None, 0, false), ms(100)),
            None
        );
        assert!(detector.is_settling(), "30% reached, waiting out the calm");
        assert_eq!(
            detector.observe_at(render(&ROUND, 72, None, 0, true), ms(300)),
            None
        );
        assert_eq!(
            detector.observe_at(render(&ROUND, 72, None, 0, false), ms(700)),
            None,
            "the calm starts again after the cover"
        );
        assert_eq!(
            detector.observe_at(render(&ROUND, 72, None, 0, false), ms(1_200)),
            Some(Event::Reached(20))
        );
    }

    /// A bar placed over when it is already part full, in each style: the
    /// 20% it passed is not reported, the rest is.
    #[test]
    fn a_bar_placed_part_full_reports_the_rest_in_any_style() {
        for (which, fill) in [Fill::Flat, Fill::Gradient, Fill::Stripes, Fill::Segments]
            .into_iter()
            .enumerate()
        {
            let look = Look {
                fill,
                round: false,
                ..ROUND
            };
            let mut detector = detector(look.direction, [20, 50, 0, 0]);
            let mut at = Duration::ZERO;
            detector.observe_at(render(&look, 72, None, 0, false), at);
            let mut events = Vec::new();
            for (frame, filled) in (73..=look.length)
                .step_by(3)
                .chain([look.length])
                .enumerate()
            {
                for _ in 0..20 {
                    at += Duration::from_millis(33);
                    let print = render(&look, filled, None, frame as u32, false);
                    events.extend(detector.observe_at(print, at));
                }
            }
            assert_eq!(
                events,
                vec![Event::Reached(50), Event::Filled],
                "style {which}"
            );
        }
    }
}
