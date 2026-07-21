//! The area model: what an area *is*, and the store that holds them (roadmap
//! task 1.6b).
//!
//! An **area** is a rectangle of screen the user has claimed. UP-TAKE is a
//! persistent screen workspace rather than a screenshot tool
//! (ADR-0009), and the area is the whole product's noun — everything else is
//! an action performed on one.
//!
//! Spec references here are to the workspace documents (`PRODUCT-VISION.md`,
//! `DECISIONS/`), which live in the private planning repo rather than beside
//! this source. Cited by section, not linked, for that reason.
//!
//! # The three orthogonal properties
//!
//! PRODUCT-VISION §3.2 is explicit that [`AreaType`], [`Visual`] and [`Input`]
//! are independent: **any combination is valid**. They are modelled as three
//! separate fields rather than folded into the type for exactly that reason. A
//! type only supplies the *starting* values ([`AreaType::default_visual`],
//! [`AreaType::default_input`]); nothing here prevents a passive Record area or
//! an interactive Filter, because the spec says nothing should.
//!
//! # Coordinates
//!
//! Area bounds are [`Rect`]s and therefore physical pixels in virtual-desktop
//! space, like everything else on the Rust side (see the crate docs). Areas
//! outlive the window they were drawn over and may straddle monitors, so no
//! part of this module may assume a single monitor or a single scale factor.
//!
//! # What this module deliberately does not do
//!
//! - **No focus model.** PRODUCT-VISION §4.3 gives `Delete` a "focused area" to
//!   close, but focus and z-order are not obviously the same thing (a
//!   pass-through area can be topmost and can never be clicked), and the
//!   roadmap puts the interaction that would settle it in task 1.6. Deciding it
//!   here, unused, would be guessing.
//! - **No minimum size policy.** [`AreaStore::create`] rejects *empty*
//!   rectangles, because a zero-pixel area can never be seen, hit-tested or
//!   dismissed — that is a model invariant. Whether a 3×3 drag should also be
//!   refused is a UX decision belonging to task 1.6.
//! - **No z-order gesture.** Open question V-8 (is z-order user-adjustable in
//!   v1.0?) is assigned to task 1.6. [`AreaStore::bring_to_front`] exists
//!   regardless, because §3.2a's implicit rule — most recently interacted-with
//!   on top — needs it either way.
//! - **No wiring.** Nothing here is connected to `ClickThrough` yet; that is
//!   task 1.6c. [`AreaStore::interactive_regions`] is shaped to be the input to
//!   `overlay_set_interactive_regions`'s physical side when it is.

use serde::{Deserialize, Serialize};

use crate::geometry::{Point, Rect};

/// A stable identity for an area, unique within the [`AreaStore`] that issued
/// it.
///
/// Opaque on purpose: callers compare and store these, they do not compute with
/// them. Ids are **never reused** — removing an area does not free its id — so
/// a stale id held across a removal fails to resolve rather than silently
/// addressing whichever area took its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AreaId(u64);

impl AreaId {
    /// The underlying number, for logging and for crossing the IPC boundary.
    ///
    /// Not a constructor: ids come from [`AreaStore::create`] only, so that
    /// uniqueness is the store's to guarantee rather than every caller's.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// What an area *does* — the third of the three orthogonal properties.
///
/// The variants are PRODUCT-VISION §3.2's Type row verbatim. Note that
/// ADR-0009 caps v1.0 at roughly three of these (open question V-4), and task
/// 1.6 ships exactly one, [`AreaType::Default`]. The rest are modelled now so
/// that adding one later is a match arm rather than a schema change.
///
/// ## Spec discrepancy, recorded rather than silently resolved
///
/// §3.2's second table illustrates the input rule with **Zoom** and **Notes**
/// rows, which are not in the Type row above it. They are not modelled here —
/// the enumerated list wins over the illustrative one — but the two lists
/// should be reconciled in the spec rather than left for the next reader to
/// notice. Zoom in particular is described as its own behaviour in §3.4, where
/// it is a gesture *on a `Default` area* rather than a type of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AreaType {
    /// A plain claimed rectangle: scroll to zoom (§3.4), drop files onto it.
    /// The only type task 1.6 ships.
    Default,
    /// A still capture, pinned in place.
    Screenshot,
    /// A region being recorded to video.
    Record,
    /// Text recognition over the region.
    Ocr,
    /// An upscaled live view of the region.
    Upscale,
    /// A region handed to an analysis pipeline.
    Analysis,
    /// A visual treatment — a tint, a dim, a mask — applied over the region.
    Filter,
}

impl AreaType {
    /// The [`Visual`] an area of this type starts with.
    ///
    /// Passive unless the type is meaningless without continuous capture.
    /// §3.2: passive costs compositing only, live costs real CPU, GPU and
    /// battery *per area*, and **live is explicitly opt-in with its cost
    /// visible to the user** — so anything not obviously live starts passive.
    #[must_use]
    pub const fn default_visual(self) -> Visual {
        match self {
            // Both are named as live in §3.2's own prose.
            Self::Record | Self::Upscale => Visual::Live,
            // Screenshot is the "pinned still capture" §3.2 lists as passive;
            // OCR and Analysis run over a captured frame and then display a
            // result; a Filter is a tint; an idle Default area is named
            // passive outright.
            Self::Default | Self::Screenshot | Self::Ocr | Self::Analysis | Self::Filter => {
                Visual::Passive
            }
        }
    }

    /// The [`Input`] an area of this type starts with.
    ///
    /// Interactive unless the type is *useless* while capturing clicks. §3.2
    /// mandates exactly two exceptions and gives the test for both: a tint you
    /// cannot work underneath is useless, and you must be able to use the thing
    /// you are recording. Every other type is a surface the user acts on, so it
    /// takes input.
    #[must_use]
    pub const fn default_input(self) -> Input {
        match self {
            Self::Filter | Self::Record => Input::PassThrough,
            Self::Default | Self::Screenshot | Self::Ocr | Self::Upscale | Self::Analysis => {
                Input::Interactive
            }
        }
    }
}

/// Whether an area's contents update continuously — the first of the three
/// orthogonal properties (§3.2).
///
/// This is the battery-drain boundary the product differentiates itself
/// against, and it is **never** a paywall (ADR-0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Visual {
    /// Compositing only, approximately free. The default, always.
    #[default]
    Passive,
    /// Continuous screen capture at framerate, for this area alone.
    Live,
}

/// Whether an area captures mouse events or lets them fall through — the
/// second of the three orthogonal properties (§3.2).
///
/// This maps onto the click-through primitive task 1.2 already built: a
/// pass-through area simply never enters the interactive-regions list. See
/// [`AreaStore::interactive_regions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Input {
    /// The area receives mouse events when it is the topmost interactive area
    /// under the cursor.
    #[default]
    Interactive,
    /// Mouse events fall through to whatever is beneath, regardless of z-order.
    PassThrough,
}

/// One area: an identity, a rectangle, and the three orthogonal properties.
///
/// `Serialize`/`Deserialize` are derived deliberately even though nothing
/// serialises an area yet. §9.1 decided areas do **not** survive a restart —
/// auto-restore is actively bad — but named layouts saved and recalled on
/// purpose are a strong v1.1 feature, and deriving this now is the difference
/// between *adding* layouts later and *rewriting* the model later.
///
/// Z-order is deliberately **not** a field. It is the store's ordering, not a
/// property of an area, so there is no way to hold two areas whose recorded
/// depths disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Area {
    /// Stable identity, issued by the store.
    pub id: AreaId,
    /// What this area does.
    pub kind: AreaType,
    /// Where it is, in physical pixels, virtual-desktop space.
    pub bounds: Rect,
    /// Whether its contents update continuously.
    pub visual: Visual,
    /// Whether it captures mouse events.
    pub input: Input,
}

impl Area {
    /// True when this area takes mouse events.
    #[must_use]
    pub fn is_interactive(self) -> bool {
        self.input == Input::Interactive
    }

    /// True when this area needs continuous capture.
    #[must_use]
    pub fn is_live(self) -> bool {
        self.visual == Visual::Live
    }
}

/// Every area in the session, in z-order.
///
/// This is the store the click-through poll will read (task 1.6c). It owns two
/// things nothing else may duplicate: **identity** (ids are issued here and
/// never reused) and **z-order** (the iteration order of one `Vec`, so there is
/// no second copy of the stacking to fall out of sync).
///
/// Ordering is **bottom-first**: the last element is the topmost area, which
/// makes "most recently created or interacted-with is on top" (§3.2a) a plain
/// push. Areas are few — tens, not thousands — so the linear scans here are
/// cheaper than any index that would have to be kept coherent with them.
///
/// Not `Serialize`: round-tripping the store would have to re-establish the
/// no-duplicate-ids and next-id-is-past-every-id invariants on the way in, and
/// a derive cannot do that. Serialize [`Area`]s and replay them through
/// [`AreaStore::create`] instead. See §9.1 for why nothing does yet.
#[derive(Debug, Clone, Default)]
pub struct AreaStore {
    /// Bottom-first. Ids are unique across this vector.
    areas: Vec<Area>,
    /// The next id to issue. Strictly greater than every id ever issued by this
    /// store, including those since removed.
    next_id: u64,
}

impl AreaStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            areas: Vec::new(),
            next_id: 1,
        }
    }

    /// Creates an area of `kind` at `bounds`, on top of every existing area,
    /// with the type's default properties.
    ///
    /// Returns `None` for an **empty** rectangle — zero width or zero height.
    /// That is not a policy choice: an area with no pixels can never be drawn,
    /// never be hit-tested, and therefore never be dismissed by clicking it, so
    /// admitting one would create an area the user cannot get rid of. A drag
    /// that never moved produces exactly this, so task 1.6 must handle the
    /// `None`.
    pub fn create(&mut self, kind: AreaType, bounds: Rect) -> Option<AreaId> {
        if bounds.size.width == 0 || bounds.size.height == 0 {
            return None;
        }
        let id = AreaId(self.next_id);
        // Saturating rather than wrapping: wrapping would eventually re-issue a
        // live id, which is the one thing `AreaId`'s contract forbids. At one
        // area per nanosecond this is reachable in about 585 years, so the
        // saturated state is unobservable — but a wrong answer here is silent
        // and a saturated one merely stops issuing.
        self.next_id = self.next_id.saturating_add(1);
        self.areas.push(Area {
            id,
            kind,
            bounds,
            visual: kind.default_visual(),
            input: kind.default_input(),
        });
        Some(id)
    }

    /// Removes an area, returning it. `None` if no such area exists.
    ///
    /// The removed id is not recycled — see [`AreaId`].
    pub fn remove(&mut self, id: AreaId) -> Option<Area> {
        let index = self.index_of(id)?;
        Some(self.areas.remove(index))
    }

    /// Removes every area.
    ///
    /// Ids continue where they left off, so nothing held across a clear can
    /// resolve to a new area.
    pub fn clear(&mut self) {
        self.areas.clear();
    }

    /// The area with this id.
    #[must_use]
    pub fn get(&self, id: AreaId) -> Option<&Area> {
        self.areas.iter().find(|area| area.id == id)
    }

    /// Moves or resizes an area. Returns `false` if the id is unknown or
    /// `bounds` is empty (same reasoning as [`AreaStore::create`]).
    ///
    /// One setter for both operations on purpose: a move and a resize differ
    /// only in which corners of the rectangle changed, and two entry points
    /// would be two places for the empty-rectangle check to be forgotten.
    /// Does **not** raise the area — see [`AreaStore::bring_to_front`].
    pub fn set_bounds(&mut self, id: AreaId, bounds: Rect) -> bool {
        if bounds.size.width == 0 || bounds.size.height == 0 {
            return false;
        }
        match self.area_mut(id) {
            Some(area) => {
                area.bounds = bounds;
                true
            }
            None => false,
        }
    }

    /// Sets whether an area updates continuously. Returns `false` for an
    /// unknown id.
    ///
    /// Independent of the area's type, per §3.2 — the type supplied a starting
    /// value, not a constraint.
    pub fn set_visual(&mut self, id: AreaId, visual: Visual) -> bool {
        match self.area_mut(id) {
            Some(area) => {
                area.visual = visual;
                true
            }
            None => false,
        }
    }

    /// Sets whether an area captures mouse events. Returns `false` for an
    /// unknown id.
    pub fn set_input(&mut self, id: AreaId, input: Input) -> bool {
        match self.area_mut(id) {
            Some(area) => {
                area.input = input;
                true
            }
            None => false,
        }
    }

    /// Raises an area to the top of the stack. Returns `false` for an unknown
    /// id.
    ///
    /// This is §3.2a's implicit rule made callable: whatever the user last
    /// interacted with ends up on top. Whether the user can *also* invoke it
    /// deliberately is open question V-8, assigned to task 1.6.
    ///
    /// Raising the area that is already topmost is a no-op, not a reshuffle.
    pub fn bring_to_front(&mut self, id: AreaId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        if index + 1 < self.areas.len() {
            let area = self.areas.remove(index);
            self.areas.push(area);
        }
        true
    }

    /// The area that should receive a mouse event at `point`: the topmost
    /// **interactive** area containing it (§3.2a).
    ///
    /// Pass-through areas are skipped entirely regardless of z-order, so a
    /// Filter tint never steals a click from an area beneath it. `None` means
    /// the click belongs to whatever is behind the overlay.
    #[must_use]
    pub fn hit_test(&self, point: Point) -> Option<&Area> {
        self.iter_top_down()
            .find(|area| area.is_interactive() && area.bounds.contains(point))
    }

    /// The bounds of every interactive area, topmost first — the set the
    /// click-through poll tests the cursor against (task 1.6c).
    ///
    /// Pass-through areas are absent by construction rather than filtered
    /// downstream, which is what makes §3.2a's "skipped entirely regardless of
    /// z-order" true for free.
    ///
    /// **An empty result means no area takes input**, which is a real state
    /// (every area is pass-through, or there are none) and not a failure. Note
    /// that `ClickThrough` reads an empty region list as its fail-safe —
    /// "regions cannot be trusted, take input everywhere" — so task 1.6c must
    /// not hand this straight through without distinguishing the two.
    #[must_use]
    pub fn interactive_regions(&self) -> Vec<Rect> {
        self.iter_top_down()
            .filter(|area| area.is_interactive())
            .map(|area| area.bounds)
            .collect()
    }

    /// Whether any area needs continuous capture — the cheap check for whether
    /// the capture pipeline has to run at all (§3.2's battery concern).
    #[must_use]
    pub fn has_live_area(&self) -> bool {
        self.areas.iter().any(|area| area.is_live())
    }

    /// Every area, bottom-first. This is paint order: later areas draw over
    /// earlier ones.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Area> {
        self.areas.iter()
    }

    /// Every area, topmost first. This is hit-test order.
    pub fn iter_top_down(&self) -> impl Iterator<Item = &Area> {
        self.areas.iter().rev()
    }

    /// How many areas exist.
    #[must_use]
    pub fn len(&self) -> usize {
        self.areas.len()
    }

    /// Whether there are no areas at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }

    fn index_of(&self, id: AreaId) -> Option<usize> {
        self.areas.iter().position(|area| area.id == id)
    }

    fn area_mut(&mut self, id: AreaId) -> Option<&mut Area> {
        self.areas.iter_mut().find(|area| area.id == id)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const ALL_TYPES: [AreaType; 7] = [
        AreaType::Default,
        AreaType::Screenshot,
        AreaType::Record,
        AreaType::Ocr,
        AreaType::Upscale,
        AreaType::Analysis,
        AreaType::Filter,
    ];

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect::new(x, y, w, h)
    }

    fn store_with(kinds: &[AreaType]) -> (AreaStore, Vec<AreaId>) {
        let mut store = AreaStore::new();
        let ids = kinds
            .iter()
            .map(|kind| store.create(*kind, rect(0, 0, 100, 100)).unwrap())
            .collect();
        (store, ids)
    }

    #[test]
    fn the_spec_mandated_pass_through_types_are_exactly_filter_and_record() {
        // §3.2 gives a *test* for pass-through — the type is useless if it
        // captures clicks — and names two types that meet it. Pinning the whole
        // set, not just the two, so adding a type silently to the
        // `default_input` match arm fails here.
        for kind in ALL_TYPES {
            let expected = matches!(kind, AreaType::Filter | AreaType::Record);
            assert_eq!(
                kind.default_input() == Input::PassThrough,
                expected,
                "{kind:?} default_input"
            );
        }
    }

    #[test]
    fn live_is_opt_in_for_every_type_that_is_not_inherently_live() {
        // The battery boundary. A type quietly defaulting to Live is the
        // failure §3.2 names outright, so the default set is pinned whole.
        for kind in ALL_TYPES {
            let expected = matches!(kind, AreaType::Record | AreaType::Upscale);
            assert_eq!(
                kind.default_visual() == Visual::Live,
                expected,
                "{kind:?} default_visual"
            );
        }
    }

    #[test]
    fn an_empty_rectangle_is_not_an_area() {
        let mut store = AreaStore::new();
        assert!(
            store
                .create(AreaType::Default, rect(10, 10, 0, 50))
                .is_none()
        );
        assert!(
            store
                .create(AreaType::Default, rect(10, 10, 50, 0))
                .is_none()
        );
        assert!(
            store
                .create(AreaType::Default, rect(10, 10, 0, 0))
                .is_none()
        );
        assert!(store.is_empty());
    }

    #[test]
    fn a_rejected_area_does_not_consume_an_id() {
        // Otherwise an aborted drag — the common case — would leave a hole in
        // the id sequence, which is harmless but makes ids useless for
        // reasoning about what happened in a log.
        let mut store = AreaStore::new();
        assert!(store.create(AreaType::Default, rect(0, 0, 0, 0)).is_none());
        let id = store.create(AreaType::Default, rect(0, 0, 10, 10)).unwrap();
        assert_eq!(id.get(), 1);
    }

    #[test]
    fn ids_are_never_reused_after_removal() {
        let mut store = AreaStore::new();
        let first = store.create(AreaType::Default, rect(0, 0, 10, 10)).unwrap();
        store.remove(first).unwrap();
        let second = store.create(AreaType::Default, rect(0, 0, 10, 10)).unwrap();
        assert_ne!(first, second);
        assert!(store.get(first).is_none());
    }

    #[test]
    fn a_clear_does_not_recycle_ids_either() {
        let mut store = AreaStore::new();
        let first = store.create(AreaType::Default, rect(0, 0, 10, 10)).unwrap();
        store.clear();
        let second = store.create(AreaType::Default, rect(0, 0, 10, 10)).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn the_newest_area_is_on_top() {
        let (store, ids) = store_with(&[AreaType::Default; 3]);
        let stacked: Vec<AreaId> = store.iter_top_down().map(|area| area.id).collect();
        assert_eq!(stacked, vec![ids[2], ids[1], ids[0]]);
    }

    #[test]
    fn bring_to_front_raises_without_disturbing_the_rest() {
        let (mut store, ids) = store_with(&[AreaType::Default; 3]);
        assert!(store.bring_to_front(ids[0]));
        let stacked: Vec<AreaId> = store.iter().map(|area| area.id).collect();
        assert_eq!(stacked, vec![ids[1], ids[2], ids[0]]);
    }

    #[test]
    fn bring_to_front_on_the_topmost_area_changes_nothing() {
        let (mut store, ids) = store_with(&[AreaType::Default; 3]);
        let before: Vec<AreaId> = store.iter().map(|area| area.id).collect();
        assert!(store.bring_to_front(ids[2]));
        let after: Vec<AreaId> = store.iter().map(|area| area.id).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn unknown_ids_are_rejected_rather_than_addressing_someone_else() {
        let (mut store, ids) = store_with(&[AreaType::Default]);
        let stale = store.remove(ids[0]).unwrap().id;
        assert!(!store.bring_to_front(stale));
        assert!(!store.set_bounds(stale, rect(0, 0, 5, 5)));
        assert!(!store.set_visual(stale, Visual::Live));
        assert!(!store.set_input(stale, Input::PassThrough));
    }

    #[test]
    fn a_pass_through_area_never_takes_a_click_from_the_area_below_it() {
        // §3.2a's flagship case: a Filter tint laid over a Default area. The
        // tint is created second, so it is topmost, and it must still be
        // invisible to the cursor.
        let mut store = AreaStore::new();
        let below = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        store
            .create(AreaType::Filter, rect(0, 0, 100, 100))
            .unwrap();
        assert_eq!(store.hit_test(Point::new(50, 50)).unwrap().id, below);
    }

    #[test]
    fn the_topmost_interactive_area_wins_an_overlap() {
        let mut store = AreaStore::new();
        store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        let top = store
            .create(AreaType::Default, rect(50, 50, 100, 100))
            .unwrap();
        assert_eq!(store.hit_test(Point::new(60, 60)).unwrap().id, top);
        // And raising the lower one flips it.
        let lower: AreaId = store.iter().next().unwrap().id;
        assert!(store.bring_to_front(lower));
        assert_eq!(store.hit_test(Point::new(60, 60)).unwrap().id, lower);
        assert_ne!(lower, top);
    }

    #[test]
    fn hit_testing_uses_half_open_edges() {
        // Inherited from `Rect::contains`, pinned here because two areas laid
        // edge to edge is the ordinary case and a both-contain answer would be
        // a z-order-dependent coin flip.
        let mut store = AreaStore::new();
        store.create(AreaType::Default, rect(0, 0, 10, 10)).unwrap();
        let right = store
            .create(AreaType::Default, rect(10, 0, 10, 10))
            .unwrap();
        assert_eq!(store.hit_test(Point::new(10, 5)).unwrap().id, right);
        assert!(store.hit_test(Point::new(20, 5)).is_none());
    }

    #[test]
    fn areas_live_in_virtual_desktop_space_including_negative_coordinates() {
        // A monitor left of the primary starts at x < 0. An area drawn there is
        // ordinary, not an edge case.
        let mut store = AreaStore::new();
        let id = store
            .create(AreaType::Default, rect(-1920, -200, 300, 300))
            .unwrap();
        assert_eq!(store.hit_test(Point::new(-1800, -100)).unwrap().id, id);
    }

    #[test]
    fn set_bounds_moves_and_resizes_without_raising() {
        let (mut store, ids) = store_with(&[AreaType::Default; 2]);
        assert!(store.set_bounds(ids[0], rect(5, 5, 20, 30)));
        assert_eq!(store.get(ids[0]).unwrap().bounds, rect(5, 5, 20, 30));
        assert_eq!(store.iter_top_down().next().unwrap().id, ids[1]);
    }

    #[test]
    fn set_bounds_refuses_to_shrink_an_area_out_of_existence() {
        let (mut store, ids) = store_with(&[AreaType::Default]);
        assert!(!store.set_bounds(ids[0], rect(5, 5, 0, 30)));
        assert_eq!(store.get(ids[0]).unwrap().bounds, rect(0, 0, 100, 100));
    }

    #[test]
    fn the_three_properties_are_independent_of_the_type() {
        // §3.2: "any combination is valid". The type supplies a starting value
        // and nothing more — a live Filter and a pass-through Default are both
        // constructible.
        let (mut store, ids) = store_with(&[AreaType::Filter]);
        assert!(store.set_visual(ids[0], Visual::Live));
        assert!(store.set_input(ids[0], Input::Interactive));
        let area = store.get(ids[0]).unwrap();
        assert_eq!(area.kind, AreaType::Filter);
        assert!(area.is_live());
        assert!(area.is_interactive());
    }

    #[test]
    fn interactive_regions_holds_only_the_interactive_areas() {
        let mut store = AreaStore::new();
        store.create(AreaType::Default, rect(0, 0, 10, 10)).unwrap();
        store.create(AreaType::Filter, rect(20, 0, 10, 10)).unwrap();
        store.create(AreaType::Record, rect(40, 0, 10, 10)).unwrap();
        let top = store
            .create(AreaType::Default, rect(60, 0, 10, 10))
            .unwrap();
        // Topmost first.
        assert_eq!(
            store.interactive_regions(),
            vec![rect(60, 0, 10, 10), rect(0, 0, 10, 10)]
        );
        assert_eq!(store.hit_test(Point::new(65, 5)).unwrap().id, top);
    }

    #[test]
    fn a_store_of_only_pass_through_areas_reports_no_interactive_regions() {
        // The state task 1.6c must not confuse with `ClickThrough`'s fail-safe
        // empty set, which means the opposite ("take input everywhere").
        let (store, _) = store_with(&[AreaType::Filter, AreaType::Record]);
        assert!(store.interactive_regions().is_empty());
        assert!(!store.is_empty());
    }

    #[test]
    fn has_live_area_tracks_the_capture_cost() {
        let (mut store, ids) = store_with(&[AreaType::Default, AreaType::Default]);
        assert!(!store.has_live_area());
        assert!(store.set_visual(ids[1], Visual::Live));
        assert!(store.has_live_area());
        store.remove(ids[1]).unwrap();
        assert!(!store.has_live_area());
    }

    // Bounded to keep coordinates in a range where overlaps actually occur;
    // `Rect`'s own property tests already cover the extremes of the geometry.
    prop_compose! {
        fn any_rect()(
            x in -200i32..200,
            y in -200i32..200,
            width in 1u32..200,
            height in 1u32..200,
        ) -> Rect {
            Rect::new(x, y, width, height)
        }
    }

    fn any_type() -> impl Strategy<Value = AreaType> {
        prop::sample::select(ALL_TYPES.as_slice())
    }

    fn any_store() -> impl Strategy<Value = AreaStore> {
        prop::collection::vec((any_type(), any_rect()), 0..12).prop_map(|specs| {
            let mut store = AreaStore::new();
            for (kind, bounds) in specs {
                store.create(kind, bounds);
            }
            store
        })
    }

    proptest! {
        #[test]
        fn ids_are_unique_across_the_store(store in any_store()) {
            let mut seen = std::collections::HashSet::new();
            for area in store.iter() {
                prop_assert!(seen.insert(area.id), "duplicate id {:?}", area.id);
            }
        }

        #[test]
        fn a_hit_is_always_an_interactive_area_containing_the_point(
            store in any_store(),
            x in -250i32..250,
            y in -250i32..250,
        ) {
            let point = Point::new(x, y);
            if let Some(area) = store.hit_test(point) {
                prop_assert!(area.is_interactive());
                prop_assert!(area.bounds.contains(point));
            }
        }

        #[test]
        fn a_miss_means_no_interactive_area_contains_the_point(
            store in any_store(),
            x in -250i32..250,
            y in -250i32..250,
        ) {
            let point = Point::new(x, y);
            if store.hit_test(point).is_none() {
                prop_assert!(
                    !store.iter().any(|a| a.is_interactive() && a.bounds.contains(point))
                );
            }
        }

        #[test]
        fn a_hit_is_the_topmost_candidate_and_no_other(
            store in any_store(),
            x in -250i32..250,
            y in -250i32..250,
        ) {
            let point = Point::new(x, y);
            // `rfind` over the bottom-first iterator: the *last* candidate in
            // paint order is the topmost one. Deliberately computed the long
            // way round rather than by reusing `iter_top_down`, so this checks
            // the ordering rule itself instead of restating the implementation.
            let expected = store
                .iter()
                .rfind(|a| a.is_interactive() && a.bounds.contains(point))
                .map(|a| a.id);
            prop_assert_eq!(store.hit_test(point).map(|a| a.id), expected);
        }

        #[test]
        fn hit_testing_and_the_region_list_agree(
            store in any_store(),
            x in -250i32..250,
            y in -250i32..250,
        ) {
            // The invariant task 1.6c depends on: the regions handed to the
            // click-through poll describe exactly the same input surface the
            // hit test does. If these ever disagree, the cursor passes through
            // an area that would have handled the click, or the overlay
            // swallows a click nothing wants.
            let point = Point::new(x, y);
            let regions = store.interactive_regions();
            prop_assert_eq!(
                crate::geometry::point_in_any(&regions, point),
                store.hit_test(point).is_some()
            );
        }

        #[test]
        fn bring_to_front_permutes_without_adding_or_losing_areas(
            store in any_store(),
            index in 0usize..12,
        ) {
            let mut store = store;
            prop_assume!(!store.is_empty());
            let before: std::collections::HashSet<AreaId> =
                store.iter().map(|a| a.id).collect();
            let id = store.iter().nth(index % store.len()).map(|a| a.id);
            let id = match id {
                Some(id) => id,
                None => return Ok(()),
            };
            prop_assert!(store.bring_to_front(id));
            let after: std::collections::HashSet<AreaId> =
                store.iter().map(|a| a.id).collect();
            prop_assert_eq!(before, after);
            prop_assert_eq!(store.iter_top_down().next().map(|a| a.id), Some(id));
        }

        #[test]
        fn removing_every_area_empties_the_store(store in any_store()) {
            let mut store = store;
            let ids: Vec<AreaId> = store.iter().map(|a| a.id).collect();
            for id in &ids {
                prop_assert!(store.remove(*id).is_some());
            }
            prop_assert!(store.is_empty());
            prop_assert!(store.interactive_regions().is_empty());
            // And every id is now stale rather than pointing at anything.
            for id in &ids {
                prop_assert!(store.get(*id).is_none());
            }
        }
    }
}
