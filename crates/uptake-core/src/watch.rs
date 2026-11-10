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
}

impl Watch {
    /// How many still readings end a change for this choice: a short calm
    /// re-arms *changes*, a long one is needed to call something finished.
    #[must_use]
    pub const fn settle_readings(self) -> u32 {
        match self {
            Self::Changes => REARM_READINGS,
            Self::Settles => SETTLE_READINGS,
        }
    }

    /// How long a calm ends a change for this choice, for the event-driven
    /// host: [`REARM_TIME`] re-arms *changes*, and [`SETTLE_READINGS`]
    /// readings at [`READING_STEP`] (six seconds) call something finished.
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
            (Self::Changes, Event::Changed) | (Self::Settles, Event::Settled)
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
        // The GPU route's last texel on a side averages the region's last
        // part-cell with the black padding beyond it (`padded_for_level` in
        // `uptake-capture`). Scaled back by the share that was real, it reads
        // as the full picture's part-cell does, which averages only its real
        // pixels (review of `#168`, round 1).
        let real = |side: u32, region: u32, cells: u32, at: u32| -> u64 {
            if side == cells && side < region && at + 1 == cells {
                u64::from(region - (cells - 1) * CELL)
            } else {
                u64::from(CELL)
            }
        };
        let cells = sums
            .iter()
            .zip(&counts)
            .enumerate()
            .map(|(at, (&sum, &count))| {
                let at = at as u32;
                let (column, row) = (at % columns, at / columns);
                let shown = u64::from(CELL) * u64::from(CELL);
                let seen = real(width, region.width, columns, column)
                    * real(height, region.height, rows, row);
                let average = sum / count.max(1);
                u8::try_from((average * shown + seen / 2) / seen).unwrap_or(u8::MAX)
            })
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
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            notify: Notify::Each,
            calm_ms: 150,
            gap_ms: 0,
            sample_ms: 0,
            share_permille: 0,
            settle_ms: 1_000,
        }
    }
}

/// [`Tuning::calm_ms`]'s range.
pub const CALM_MS: (u32, u32) = (50, 10_000);
/// [`Tuning::gap_ms`]'s longest: a minute.
pub const GAP_MS_MAX: u32 = 60_000;
/// [`Tuning::sample_ms`]'s longest: one reading every five seconds.
pub const SAMPLE_MS_MAX: u32 = 5_000;
/// [`Tuning::share_permille`]'s largest: the whole area. Half the area until
/// 2026-11-10, his first word (*"from any pixel to half the area"*), until
/// his rig pass that day: *"A slider range must always provide as much
/// flexibility as possible, lowest valid value to highest valid value"*.
pub const SHARE_PERMILLE_MAX: u16 = 1_000;
/// [`Tuning::settle_ms`]'s range: a tenth of a second to half a minute.
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

/// One of the two moments a watch area can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The region was still and now differs from its last reading.
    Changed,
    /// The region had been changing and has now held still for the
    /// detector's settle time ([`Watch::settle_time`]).
    Settled,
}

/// Turns a run of readings into [`Event`]s, one area's worth.
///
/// Two comparisons, one for each moment. **A change starts** when a reading
/// differs by more than [`CELL_DELTA`] from the *baseline*, the reading the
/// region was last still at, so a slow change adds up until it counts. **A
/// change ends** when the region has held still for the settle time
/// ([`Watch::settle_time`]: [`REARM_TIME`] for *Notify when it changes*, six
/// seconds for *Notify when it stops changing*), every reading in that time
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
        }
    }

    /// The detector an area set to `watch` reads with: its settle time
    /// ([`Watch::settle_time`]), and for [`Watch::Changes`] every full step
    /// of a change reported, not only its start.
    #[must_use]
    pub fn for_watch(watch: Watch) -> Self {
        Self {
            every_step: watch == Watch::Changes,
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
    }

    /// Whether a change is under way, so the host must keep reading while the
    /// screen is still, or the calm that ends it is never seen.
    #[must_use]
    pub const fn is_settling(&self) -> bool {
        self.changing
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
        self.baseline = Some(reading.clone());
        self.still_from = Some(reading);
        self.still_since = at;
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
    /// same way: the GPU's last texels, diluted by black padding, are scaled
    /// back to what their real pixels hold. 106 by 62 is 26 cells and 2
    /// pixels across, 15 cells and 2 pixels down; the grey is chosen so every
    /// diluted texel is a whole level, as the GPU would store it.
    #[test]
    fn both_routes_agree_on_a_region_not_a_multiple_of_four() {
        let full = with_patch(grey(106, 62, 192), (8, 8, 16, 8), 0);
        let mut small = grey(27, 16, 192);
        small = with_patch(small, (2, 2, 4, 2), 0);
        small = with_patch(small, (26, 0, 1, 15), 96); // 192 * 2 / 4
        small = with_patch(small, (0, 15, 26, 1), 96); // 192 * 2 / 4
        small = with_patch(small, (26, 15, 1, 1), 48); // 192 * 4 / 16
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
            calm_ms: 0,
            gap_ms: u32::MAX,
            sample_ms: u32::MAX,
            share_permille: u16::MAX,
            settle_ms: u32::MAX,
        }
        .clamped();
        assert_eq!(wild.calm_ms, super::CALM_MS.0);
        assert_eq!(wild.gap_ms, super::GAP_MS_MAX);
        assert_eq!(wild.sample_ms, super::SAMPLE_MS_MAX);
        assert_eq!(wild.share_permille, super::SHARE_PERMILLE_MAX);
        assert_eq!(wild.settle_ms, super::SETTLE_MS.1);
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
