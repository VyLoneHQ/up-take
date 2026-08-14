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
//!   v1.0?) was closed by ADR-0013 *after* this module was first written:
//!   stacking is implicit recency plus a per-area [`Layer`] tier. The tier and
//!   the ordering rule live here; the gesture that sets it — the per-area Layer
//!   menu — is task 1.6's.
//! - **No wiring, and no longer any prospect of it.** This bullet used to read
//!   "Nothing here is connected to `ClickThrough` yet; that is task 1.6c", with
//!   [`AreaStore::interactive_regions`] shaped to feed
//!   `overlay_set_interactive_regions`. **That consumer was deleted**, not
//!   deferred: ADR-0016 routes per-area input through the global mouse hook and
//!   removed the frontend-reported region store entirely. `interactive_regions`
//!   survives as the thing the hit test is checked against, not as anybody's
//!   input — see its own docs.

use serde::{Deserialize, Serialize};

use crate::geometry::{Point, Rect, Size};
use crate::interaction;

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
    /// Every variant, in PRODUCT-VISION §3.2's Type-row order.
    ///
    /// For callers that have to *offer* the types rather than answer a question
    /// about one, where a `match` cannot help: the area menu's conversion rows
    /// (roadmap task 1.27) iterate this and ask a per-type predicate what to do
    /// with each. Declaration order is the spec's order, and it is also the
    /// order those rows appear in.
    ///
    /// ⚠️ **Hand-maintained, and the array length is the only thing that says
    /// so.** Adding a variant fails to compile in every exhaustive `match` in
    /// this file and in the host, so the author is stopped and made to decide;
    /// nothing but the `7` stops them leaving this list short. Kept because the
    /// alternative is a derive-macro dependency for one list of seven names.
    pub const ALL: [Self; 7] = [
        Self::Default,
        Self::Screenshot,
        Self::Record,
        Self::Ocr,
        Self::Upscale,
        Self::Analysis,
        Self::Filter,
    ];

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

    /// What happens to PLACEMENT once an area of this type has been created —
    /// the per-type axis ADR-0018 §6 added.
    ///
    /// Unlike [`default_visual`](Self::default_visual) and
    /// [`default_input`](Self::default_input) this is **not** a starting value
    /// for a field on the area. It is a property of the *creating gesture*, and
    /// it exists because the two decided types genuinely differ: placing several
    /// `Default` areas in a row is the normal case, while a `Screenshot` has
    /// finished the moment its capture is pinned and the user wants input back
    /// in their own applications.
    ///
    /// ADR-0018 names the cost of this axis out loud: every future type must now
    /// answer "and then what?", and getting it wrong strands the user in the
    /// wrong state. So the five unbuilt types below are **not** answered here —
    /// they take the conservative value and say so.
    ///
    /// # Every type stays, as of ADR-0023
    ///
    /// ADR-0018 §6 had `Screenshot` **exit** on create. Driving it on real
    /// hardware immediately read as wrong: the capture lands and the overlay
    /// drops you out from under your own hands, before you can nudge or resize
    /// the area you just drew. Reversed by
    /// [ADR-0023](ADR-0023-screenshot-stays-in-placement.md); the exit becomes
    /// an opt-in setting (task 1.14).
    ///
    /// **The axis is still worth having, and this is the evidence for it.** The
    /// reversal was a one-value change with no structural edit anywhere — which
    /// is exactly what a per-type property is for. Had "and then what?" been
    /// inlined at the call site instead, this would have been surgery.
    #[must_use]
    pub const fn after_create(self) -> AfterCreate {
        match self {
            // Every type stays in PLACEMENT today (ADR-0023). Written as one
            // arm rather than seven because there is currently one answer; the
            // *return type* is what keeps a second answer cheap to add, not the
            // shape of this match.
            Self::Default
            | Self::Screenshot
            | Self::Record
            | Self::Ocr
            | Self::Upscale
            | Self::Analysis
            | Self::Filter => AfterCreate::StayInPlacement,
        }
    }

    /// Whether scrolling over an area of this type magnifies it (§3.4).
    ///
    /// **Only `Default` today, and the narrowness is the decision.** §3.4 is
    /// written about the Default area alone (*"scrolling over a Default area
    /// scales its contents"*), and §3.1 names zoom as the thing that makes
    /// Default more than an empty rectangle. Every other type already owns what
    /// its own region means: a Screenshot holds a pinned still, an Upscale is
    /// magnification by definition, and an OCR area shows a result rather than
    /// pixels. Giving them all a second, conflicting magnifier is a decision
    /// nothing has taken, so this answers `false` and leaves it takeable.
    ///
    /// Written as an explicit match rather than `matches!` so an eighth
    /// [`AreaType`] has to answer here, the way [`default_visual`] and
    /// [`default_input`] already make it answer.
    ///
    /// [`default_visual`]: Self::default_visual
    /// [`default_input`]: Self::default_input
    #[must_use]
    pub const fn supports_zoom(self) -> bool {
        match self {
            Self::Default => true,
            Self::Screenshot
            | Self::Record
            | Self::Ocr
            | Self::Upscale
            | Self::Analysis
            | Self::Filter => false,
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

/// Whether creating an area of a given type leaves PLACEMENT (ADR-0018 §6).
///
/// Deliberately a two-variant enum rather than a `bool`: `after_create(t) ==
/// AfterCreate::ExitPlacement` reads as the question it answers, where
/// `exits_placement(t) == true` invites a caller to get the polarity backwards
/// at the one call site where it matters.
///
/// The exit lands in **LIVING, not HIDDEN** — an area now exists, and
/// `overlay_state::next` already collapses an arealess LIVING to HIDDEN, so no
/// rule is needed here (ADR-0018 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum AfterCreate {
    /// PLACEMENT continues — the user is placing several areas in a row.
    #[default]
    StayInPlacement,
    /// PLACEMENT ends and input returns to the user's own applications.
    ExitPlacement,
}

/// Which stacking tier an area is pinned to (ADR-0013).
///
/// Recency — "the area you last touched is on top" (§3.2a) — is right for the
/// common case and wrong for two specific ones: a Filter tint is only useful
/// *above* what it tints, and a reference area is often wanted *behind* the
/// work. Under pure recency both get re-buried by the next click somewhere
/// else, forever. A tier is the smallest thing that fixes that: three values,
/// no per-area z-index, and recency intact inside each tier.
///
/// The effective order is **tier first, then recency within the tier** — every
/// [`Layer::Front`] area sits above every [`Layer::Auto`] area, which sits above
/// every [`Layer::Back`] area. [`AreaStore::bring_to_front`] therefore raises an
/// area **within its own tier** and can never lift it across one.
///
/// # Variant order is load-bearing
///
/// The derived [`Ord`] follows declaration order, and [`AreaStore`] relies on it
/// being bottom-to-top: `Back < Auto < Front`. Reordering these variants would
/// silently invert the stack rather than fail to compile.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub enum Layer {
    /// Below every `Auto` area, however recently this one was touched.
    Back,
    /// Obeys recency. The default for every area of every type — ADR-0013 pins
    /// the tier to the *area*, not to its [`AreaType`], because the cases that
    /// want pinning are about what the user is doing with a particular area
    /// rather than about what kind of area it is.
    #[default]
    Auto,
    /// Above every `Auto` area, however recently they were touched.
    Front,
}

/// How far an area's contents are magnified (§3.4).
///
/// **Steps, not a factor.** The stored value is a count of scroll notches above
/// natural size, and the factor is computed from it. Two reasons, and the first
/// is structural: [`Area`] derives [`Eq`] and [`Hash`], which an `f32` field
/// would take away from it and from everything that holds one. The second is
/// that §3.4's floor is *"as far out as its natural size"*, and a floor
/// expressed as `step == 0` is exact, where `factor <= 1.0` is a float
/// comparison a sequence of multiplications can land just underneath.
///
/// The increment is a quarter, exactly representable in binary, so
/// [`Zoom::factor`] returns the same value for the same step on every machine
/// and the tests can assert equality rather than a tolerance.
///
/// # Why there is a ceiling at all
///
/// §3.4 names only the floor. The ceiling is this module's, and it exists
/// because [`Zoom::source_rect`] divides by the factor: without one, a
/// sufficiently zoomed area asks for a source rectangle of zero pixels and the
/// magnifier stops being able to show anything at all.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct Zoom(u8);

impl Zoom {
    /// Natural size. §3.4's floor, and the value every area starts at.
    pub const NATURAL: Self = Self(0);

    /// What one scroll notch adds to the factor.
    pub const STEP: f32 = 0.25;

    /// The largest step, giving [`Zoom::MAX_FACTOR`].
    const MAX_STEP: u8 = 28;

    /// The factor at [`Zoom::MAX_STEP`].
    pub const MAX_FACTOR: f32 = 8.0;

    /// The magnification this zoom applies. `1.0` at [`Zoom::NATURAL`].
    #[must_use]
    pub fn factor(self) -> f32 {
        Self::STEP.mul_add(f32::from(self.0), 1.0)
    }

    /// True at natural size, where the area shows the live screen underneath
    /// rather than a magnified capture of it.
    ///
    /// The caller that matters is the one deciding whether an area needs a
    /// capture at all: at natural size it must not have one, or live content
    /// stops being live (ADR-0014) on the type that exists to demonstrate it.
    #[must_use]
    pub const fn is_natural(self) -> bool {
        self.0 == Self::NATURAL.0
    }

    /// Applies `notches` of scroll, saturating at the floor and the ceiling.
    ///
    /// Positive zooms in. Windows reports a forward wheel rotation as a
    /// positive `WHEEL_DELTA`, and that is the direction every magnifier in
    /// this class magnifies on. Saturating rather than wrapping is §3.4's
    /// guarantee: scrolling out is *"always a way back to normal rather than a
    /// way to get lost"*, which a wrap at either end would break in the worst
    /// way available.
    #[must_use]
    pub fn stepped(self, notches: i32) -> Self {
        let stepped = i32::from(self.0).saturating_add(notches);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=MAX_STEP on the line above, and MAX_STEP is a u8"
        )]
        Self(stepped.clamp(0, i32::from(Self::MAX_STEP)) as u8)
    }

    /// The screen rectangle whose pixels fill an area of `bounds` at this zoom.
    ///
    /// A magnifier shows *less* screen, larger: at 2× the area is a window onto
    /// half its own width and half its own height, centred on itself, and the
    /// renderer stretches those pixels to fill it. So this shrinks about the
    /// centre rather than scaling about the origin, and the result is always
    /// inside `bounds`.
    ///
    /// **Never empty.** Each axis floors at one pixel, so the result can always
    /// be captured. The ceiling makes that clamp unreachable for any area at or
    /// above ADR-0015's minimum size; it is kept because `bounds` arrives from
    /// callers this type does not control, and a zero-width capture request
    /// fails a long way from its cause.
    #[must_use]
    pub fn source_rect(self, bounds: Rect) -> Rect {
        if self.is_natural() {
            return bounds;
        }
        let factor = self.factor();
        let shrink = |extent: u32| -> u32 {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "screen extents are far inside f32's exactly-representable \
                          integer range, and the quotient is clamped to at least 1"
            )]
            let scaled = (extent as f32 / factor) as u32;
            scaled.max(1)
        };
        let size = Size::new(shrink(bounds.size.width), shrink(bounds.size.height));
        // Integer division truncates, so the two margins can differ by a pixel.
        // Taking the margin as the halved *difference* keeps the source inside
        // `bounds` on both axes rather than assuming it is symmetric.
        let inset = |extent: u32, inner: u32| -> i32 {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "half the difference of two screen extents"
            )]
            let inset = (extent.saturating_sub(inner) / 2) as i32;
            inset
        };
        // `saturating_add`, not `+`, and geometry.rs's own policy is the
        // reason: rectangle edges are computed in `i64` there precisely because
        // `origin + size` can overflow `i32`. An area with a large positive
        // origin makes this the same sum, and the consequence is a debug panic
        // inside the mouse hook or a silently wrapped negative origin in
        // release. Not reachable from real screen coordinates -- and this
        // function's own contract is that `bounds` arrives from callers it does
        // not control, which is the argument the one-pixel floor below already
        // rests on. Found by an independent review, which also noted that
        // `any_rect` is bounded to +/-200 and so could never have caught it.
        Rect::new(
            bounds
                .origin
                .x
                .saturating_add(inset(bounds.size.width, size.width)),
            bounds
                .origin
                .y
                .saturating_add(inset(bounds.size.height, size.height)),
            size.width,
            size.height,
        )
    }
}

/// One area: an identity, a rectangle, and the three orthogonal properties.
///
/// `Serialize`/`Deserialize` are derived deliberately even though nothing
/// serialises an area yet. §9.1 decided areas do **not** survive a restart —
/// auto-restore is actively bad — but named layouts saved and recalled on
/// purpose are a strong v1.1 feature, and deriving this now is the difference
/// between *adding* layouts later and *rewriting* the model later.
///
/// Depth is deliberately **not** a field. An area's position in the stack is
/// the store's ordering, so there is no way to hold two areas whose recorded
/// depths disagree. [`Layer`] is not an exception to that: it is a *constraint
/// on* the ordering (which tier the area belongs to), not a copy of it — an
/// area's depth within its tier still exists only as its index in the store.
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
    /// Which stacking tier it is pinned to. [`Layer::Auto`] — plain recency —
    /// unless the user has said otherwise.
    pub layer: Layer,
    /// How far its contents are magnified (§3.4). [`Zoom::NATURAL`] on every
    /// area of every type, including the types [`AreaType::supports_zoom`]
    /// refuses: the field is what the area *is*, and the predicate is what the
    /// gesture is allowed to change. Keeping them separate means a type that
    /// gains zoom later gains it by answering the predicate, with no migration
    /// of the model.
    pub zoom: Zoom,
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
/// Ordering is **bottom-first**: the last element is the topmost area. Areas
/// are few — tens, not thousands — so the linear scans here are cheaper than
/// any index that would have to be kept coherent with them.
///
/// # The ordering invariant
///
/// The vector is **sorted by [`Layer`], ascending, and by recency within each
/// tier** — i.e. `Back`s at the bottom, then `Auto`s, then `Front`s, each group
/// in the order its members were last created, raised or re-tiered. Every
/// mutation here preserves that, so the *effective* order ADR-0013 defines and
/// the *stored* order are the same thing rather than two views that could
/// disagree. That is what lets [`AreaStore::iter`], [`AreaStore::hit_test`] and
/// [`AreaStore::interactive_regions`] stay plain traversals: tiering is not
/// applied on read, it is maintained on write.
///
/// The invariant also makes [`slice::partition_point`] valid for locating the
/// top of a tier, which every insertion here uses.
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

/// What a completed [`AreaStore::set_kind`] leaves for its caller to finish.
///
/// The store owns the *model*: the type, and the zoom only some types may hold.
/// It does not own the *pixels*, which live in the host's capture store and in
/// whatever magnification work is in flight, neither of which is reachable from
/// this crate. A conversion that invalidates those pixels therefore has to say
/// so, and the caller has to act on it.
///
/// Returned rather than left implicit, because the alternative is every caller
/// working out for itself whether a conversion stranded a capture. That is the
/// invariant roadmap task 1.27 exists to enforce: an area converted away from a
/// magnified [`AreaType::Default`] goes on rendering a pin of somewhere else,
/// under a badge reporting a magnification [`AreaType::supports_zoom`] no longer
/// lets the user scroll away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conversion {
    /// Whether the area's pinned pixels are now wrong for it, so the caller must
    /// drop them along with any capture still in flight for the same area.
    ///
    /// True when the area was showing something its new type cannot mean: a
    /// magnified still it can no longer be scrolled out of, or the pinned
    /// capture that *was* its entire content while it was a
    /// [`AreaType::Screenshot`].
    pub discard_capture: bool,
    /// Whether anything actually changed. False when the area already had this
    /// type, which is an ordinary request rather than an error: the menu row for
    /// the current type is drawn ticked and is still clickable.
    pub changed: bool,
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

    /// Creates an area of `kind` at `bounds`, on top of every existing area in
    /// its tier, with the type's default properties.
    ///
    /// New areas start [`Layer::Auto`], so in an unpinned workspace — the
    /// ordinary case — this is ADR-0013's rule 1 exactly: a new area renders
    /// above anything it covers. An existing `Front` area still outranks it,
    /// which is the point of having pinned it.
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
        let area = Area {
            id,
            kind,
            bounds,
            visual: kind.default_visual(),
            input: kind.default_input(),
            layer: Layer::default(),
            zoom: Zoom::NATURAL,
        };
        let index = self.top_of_tier(area.layer);
        self.areas.insert(index, area);
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

    /// Applies `notches` of scroll to an area's magnification (§3.4), returning
    /// the zoom it ended at.
    ///
    /// Returns `None` for an unknown id **and for a type whose
    /// [`AreaType::supports_zoom`] is false**. The two are one return value on
    /// purpose. A caller that has to distinguish them is a caller deciding
    /// whether to tell the user off for scrolling, and §3.4's model is that a
    /// scroll the product has no use for belongs to whatever is underneath.
    /// Both answers mean *this scroll was not ours*.
    ///
    /// Returns `Some` even when the zoom did not move, which is the saturating
    /// case: scrolling out at natural size is a no-op the area still owns, and
    /// the caller must not pass that notch through to the application beneath.
    /// Everything downstream compares the returned zoom with the previous one
    /// to decide whether a re-capture is needed, so a no-op costs nothing.
    pub fn zoom_by(&mut self, id: AreaId, notches: i32) -> Option<Zoom> {
        let area = self.area_mut(id)?;
        if !area.kind.supports_zoom() {
            return None;
        }
        area.zoom = area.zoom.stepped(notches);
        Some(area.zoom)
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

    /// Converts an area to another [`AreaType`] (roadmap task 1.27). `None` for
    /// an unknown id; otherwise a [`Conversion`] saying what the caller still
    /// owes.
    ///
    /// # Conversion is not creation, and the axes do not all carry over
    ///
    /// ADR-0018's warning is that every type has to answer *"and then what?"*.
    /// Three answers, and the third is the one that is not obvious:
    ///
    /// - **[`AfterCreate`] does not apply at all.** It is a property of the
    ///   creating gesture, and a conversion is not one. There is no drag in
    ///   progress and no mode to return from.
    /// - **[`Zoom`] resets whenever the new type does not support it**, and this
    ///   is the invariant the row is *for*. [`AreaType::supports_zoom`] is
    ///   `Default`-only, so a converted area left holding a non-natural zoom
    ///   could never be scrolled back: [`AreaStore::zoom_by`] refuses on the
    ///   type before it ever reaches the value.
    /// - **[`Visual`] and [`Input`] are re-taken from the new type's defaults.**
    ///   §3.2 calls all three properties orthogonal and says a type supplies a
    ///   *starting* value rather than a constraint, so carrying the old values
    ///   over would also be defensible. Taking the defaults is chosen because
    ///   the menu row says "Filter", and a Filter that still swallows every
    ///   click is the one thing §3.2's own test says a Filter must not be.
    ///   Little is lost either way: `Input` has its own menu row a few lines
    ///   below, so a user who wanted the old value can put it back.
    ///
    /// [`Layer`] is deliberately untouched. It is not derived from the type, and
    /// "always on top" is a placement the user set on this rectangle rather than
    /// on what the rectangle currently does.
    pub fn set_kind(&mut self, id: AreaId, kind: AreaType) -> Option<Conversion> {
        let area = self.area_mut(id)?;
        if area.kind == kind {
            return Some(Conversion {
                discard_capture: false,
                changed: false,
            });
        }
        // Read before the write, and both halves matter. A magnified area holds
        // a still it can no longer scroll out of; a Screenshot's pin *was* its
        // content, and the type replacing it has content of its own.
        let was_magnified = !area.zoom.is_natural();
        let was_pinned_capture = area.kind == AreaType::Screenshot;
        area.kind = kind;
        area.visual = kind.default_visual();
        area.input = kind.default_input();
        if !kind.supports_zoom() {
            area.zoom = Zoom::NATURAL;
        }
        // `was_magnified && supports_zoom` is unreachable today, because only
        // `Default` zooms and `Default` to `Default` returned above. It is
        // written as a conjunction anyway, so that widening `supports_zoom` to a
        // second type cannot silently start throwing away a magnification the
        // new type would have kept.
        Some(Conversion {
            discard_capture: (was_magnified && !kind.supports_zoom()) || was_pinned_capture,
            changed: true,
        })
    }

    /// Raises an area to the top of **its own tier**. Returns `false` for an
    /// unknown id.
    ///
    /// This is §3.2a's implicit rule made callable: whatever the user last
    /// interacted with ends up on top. ADR-0013 bounds it — an [`Layer::Auto`]
    /// area can never reach above a [`Layer::Front`] one by being clicked,
    /// because otherwise "always on top" would mean "on top until you touch
    /// something else", which is the failure the tiers exist to fix.
    ///
    /// Raising the area that is already topmost in its tier is a no-op, not a
    /// reshuffle.
    pub fn bring_to_front(&mut self, id: AreaId) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let area = self.areas.remove(index);
        // Computed against the vector *without* this area, so the target index
        // is the one it will actually occupy.
        let target = self.top_of_tier(area.layer);
        self.areas.insert(target, area);
        true
    }

    /// Pins an area to a stacking tier, raising it to the top of that tier.
    /// Returns `false` for an unknown id.
    ///
    /// The raise is deliberate rather than incidental: every path that reaches
    /// here is a user picking Layer from an area's own menu, and "put this in
    /// front" that leaves the area buried under its new tier-mates would look
    /// like the setting had not taken. The same applies downward — a
    /// [`Layer::Back`] area goes to the top of the `Back` tier, still beneath
    /// every `Auto` area, so the user sees it sink exactly one step rather than
    /// vanish under every other pinned-back area.
    pub fn set_layer(&mut self, id: AreaId, layer: Layer) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let mut area = self.areas.remove(index);
        area.layer = layer;
        let target = self.top_of_tier(layer);
        self.areas.insert(target, area);
        true
    }

    /// The area that should receive a mouse event at `point`: the topmost
    /// **interactive** area containing it (§3.2a).
    ///
    /// "Topmost" is the tier-aware order (ADR-0013), so a [`Layer::Front`] area
    /// takes the click over an [`Layer::Auto`] area that was created or touched
    /// later. Pass-through areas are skipped entirely regardless of depth *or*
    /// tier, so a Filter tint never steals a click from an area beneath it —
    /// including a Filter the user has pinned to `Front`, which is the
    /// combination ADR-0013's motivating case actually produces. `None` means
    /// the click belongs to whatever is behind the overlay.
    /// # Pass-through means body-only, not invisible (ADR-0024 §2)
    ///
    /// A pass-through area is **not** skipped outright any more: its *body* lets
    /// clicks through, and its *chrome* does not. Before this, flipping an area to
    /// pass-through stranded it — `Filter` and `Record` are pass-through by
    /// default, so they could never be grabbed, moved or dismissed without
    /// re-entering PLACEMENT. §3.2a's "skipped entirely regardless of z-order" now
    /// reads as *the body* being skipped.
    ///
    /// The `monitors` argument is what that costs: chrome geometry includes the
    /// close control, which on a small area sits **outside** the bounds and has to
    /// be placed against a monitor.
    #[must_use]
    pub fn hit_test(&self, point: Point, monitors: &[Rect]) -> Option<&Area> {
        self.iter_top_down().find(|area| {
            if area.is_interactive() {
                area.bounds.contains(point)
            } else {
                interaction::is_chrome_at(area.bounds, point, monitors)
            }
        })
    }

    /// [`AreaStore::hit_test`] plus **which part** was hit: the Living grab rule.
    ///
    /// The same question `hit_test` answers, returning the [`Handle`] as well,
    /// because a press needs to know whether it began a move, a resize or a
    /// dismissal. Kept beside `hit_test` rather than open-coded at the call site:
    /// the host had its own copy of *an interactive area answers for any handle, a
    /// pass-through one only for chrome*, and two copies of what takes input
    /// drifting apart is how a click gets swallowed by an area that would not have
    /// handled it.
    ///
    /// [`Handle`]: interaction::Handle
    #[must_use]
    pub fn grab_test(
        &self,
        point: Point,
        monitors: &[Rect],
    ) -> Option<(&Area, interaction::Handle)> {
        self.iter_top_down().find_map(|area| {
            let handle = interaction::handle_at(area.bounds, point, monitors)?;
            (area.is_interactive() || !matches!(handle, interaction::Handle::Body))
                .then_some((area, handle))
        })
    }

    /// The topmost area containing `point`, **whatever its [`Input`]** — the
    /// area a Placement gesture grabs.
    ///
    /// Distinct from [`AreaStore::hit_test`] on purpose, and the difference is
    /// not a subtlety to fold away later. `hit_test` answers a question about
    /// the *user's apps*: who receives this click while the workspace is living
    /// and the overlay is click-through, where a pass-through area must be
    /// invisible to the cursor. This answers a question about *the workspace
    /// itself*: which area is the user reaching for while they are editing the
    /// layout. A Filter tint that no click can reach in Living must still be
    /// movable and dismissable in Placement, or it becomes permanent.
    #[must_use]
    pub fn hit_test_any(&self, point: Point) -> Option<&Area> {
        self.iter_top_down()
            .find(|area| area.bounds.contains(point))
    }

    /// The topmost area the cursor is **over**, for deciding what to draw.
    ///
    /// The third member of this family, and the only one that answers a question
    /// about the display rather than about input. [`AreaStore::hit_test`] answers
    /// who receives a click in Living; [`AreaStore::hit_test_any`] answers what a
    /// Placement gesture grabs; this answers what the user is looking at, which
    /// governs the hover chrome and nothing else.
    ///
    /// # Why it is not one of the other two
    ///
    /// Hover chrome was resolved through the Living input rule until 2026-08-14,
    /// so a pass-through area revealed its close control only when the cursor was
    /// already on the control. On an area below
    /// [`interaction::CHROME_INSIDE_SPAN`] that control is the sole route back to
    /// the menu, and it sits outside the corner, so the way out of a Filter area
    /// was an invisible 18 px target. **Surfaced by** the first hardware pass over
    /// roadmap 1.27, which had already shipped: the row is not driving this, it is
    /// what exposed it. (Said "driving roadmap 1.27" until the independent review
    /// of `#56` read the row and found it marked shipped at this branch's own
    /// base.)
    ///
    /// It is not `hit_test_any` either, because that tests bounds alone. The
    /// close control of a small area is mostly **outside** its bounds, so a
    /// bounds-only rule would drop the hover at the moment the cursor arrived on
    /// the control and the chrome would vanish as it was reached for.
    ///
    /// **Nothing here may be read as permission to take a click.** Drawing a
    /// control and honouring a press are separate questions and this answers only
    /// the first; ADR-0016 decision 3 owns the second, and a pass-through body
    /// still belongs to the application underneath.
    #[must_use]
    pub fn hover_test(&self, point: Point, monitors: &[Rect]) -> Option<&Area> {
        self.iter_top_down().find(|area| {
            area.bounds.contains(point)
                || interaction::close_control(area.bounds, monitors).contains(point)
        })
    }

    /// Every rectangle that takes input, topmost first — an interactive area's
    /// whole bounds, and a pass-through area's **chrome only** (ADR-0024 §2).
    ///
    /// **No longer one rect per area**, and that is the point: a pass-through
    /// area contributes its close control and its four edge bands, so the list
    /// describes an input *surface* rather than a set of areas. Nothing needs the
    /// area→rect correspondence; what needs to hold is that this surface and
    /// [`AreaStore::hit_test`] agree, which
    /// `hit_testing_and_the_region_list_agree` checks.
    ///
    /// **An empty result means no rectangle takes input**, which is a real state
    /// (no areas at all, or only pass-through areas whose chrome is off-screen)
    /// and not a failure. Note that `ClickThrough` reads an empty region list as
    /// its fail-safe — "regions cannot be trusted, take input everywhere" — so a
    /// caller must not hand this straight through without distinguishing the two.
    ///
    /// # It has no production caller today
    ///
    /// Its consumer — the frontend-reported region store the click-through poll
    /// tested the cursor against — was **deleted** by
    /// [ADR-0016](../../../Projects/UP-TAKE/DECISIONS/ADR-0016-living-input-via-the-global-hook.md),
    /// which routes per-area input through the global mouse hook instead. It is
    /// kept because the property test over it is the one place the two halves of
    /// "what takes input" are forced to answer alike; deleting it would remove the
    /// check, not the requirement.
    #[must_use]
    pub fn interactive_regions(&self, monitors: &[Rect]) -> Vec<Rect> {
        self.iter_top_down()
            .flat_map(|area| {
                if area.is_interactive() {
                    vec![area.bounds]
                } else {
                    interaction::chrome_rects(area.bounds, monitors)
                }
            })
            .collect()
    }

    /// Whether any area needs continuous capture — the cheap check for whether
    /// the capture pipeline has to run at all (§3.2's battery concern).
    #[must_use]
    pub fn has_live_area(&self) -> bool {
        self.areas.iter().any(|area| area.is_live())
    }

    /// Every area, bottom-first. This is paint order: later areas draw over
    /// earlier ones. Tier-aware by the store's ordering invariant — no caller
    /// has to sort, and none should.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Area> {
        self.areas.iter()
    }

    /// Every area, topmost first. This is hit-test order, tiers included.
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

    /// The index one past the last area in `layer`'s tier — where a new, raised
    /// or newly re-tiered member of that tier belongs.
    ///
    /// Relies on the store's ordering invariant: `layer <= given` is true for a
    /// prefix of the vector and false for the rest, which is exactly
    /// [`slice::partition_point`]'s precondition. Inserting here is what keeps
    /// the invariant true, so the two are load-bearing for each other.
    fn top_of_tier(&self, layer: Layer) -> usize {
        self.areas.partition_point(|area| area.layer <= layer)
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

    /// One monitor large enough to hold every rectangle these tests use,
    /// including the negative-coordinate ones.
    ///
    /// Hit testing needs a monitor list because a pass-through area's chrome
    /// includes its close control, and a small area's control is placed *outside*
    /// it against a screen ([`interaction::close_control`]). An interactive area's
    /// hit test ignores the list entirely, so most of these tests only need it to
    /// exist.
    fn screens() -> Vec<Rect> {
        vec![Rect::new(-3000, -3000, 6000, 6000)]
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
    fn no_type_exits_placement_on_create() {
        // ADR-0023 reversed ADR-0018 §6 after a rig pass: being dropped out of
        // PLACEMENT the instant a capture lands, before you can nudge the area
        // you just drew, reads as the app taking the tool out of your hands.
        // Pinned as a whole-set assertion rather than deleted, so a type that
        // later wants `ExitPlacement` has to change this test deliberately.
        for kind in ALL_TYPES {
            assert_eq!(
                kind.after_create(),
                AfterCreate::StayInPlacement,
                "{kind:?} after_create"
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
        assert!(!store.set_layer(stale, Layer::Front));
    }

    #[test]
    fn every_area_starts_on_the_auto_tier() {
        // ADR-0013 pins the tier to the area, not to its type: a type-derived
        // default would quietly reintroduce "Filters are special", which is the
        // per-type behaviour the three orthogonal properties exist to avoid.
        for kind in ALL_TYPES {
            let (store, ids) = store_with(&[kind]);
            assert_eq!(store.get(ids[0]).unwrap().layer, Layer::Auto, "{kind:?}");
        }
    }

    #[test]
    fn a_front_area_outranks_an_auto_area_created_after_it() {
        // ADR-0013 rule 3 beating rule 1. The `Front` area is created *first*,
        // so pure recency would bury it.
        let (mut store, ids) = store_with(&[AreaType::Default; 2]);
        assert!(store.set_layer(ids[0], Layer::Front));
        let newest = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        assert_eq!(store.iter_top_down().next().unwrap().id, ids[0]);
        assert_ne!(newest, ids[0]);
    }

    #[test]
    fn raising_an_auto_area_cannot_lift_it_above_a_pinned_front_area() {
        // The invariant ADR-0013 names as the one that must be tested rather
        // than assumed, because task 1.6c's input routing leans on it.
        let (mut store, ids) = store_with(&[AreaType::Default; 3]);
        assert!(store.set_layer(ids[0], Layer::Front));
        assert!(store.bring_to_front(ids[1]));
        assert!(store.bring_to_front(ids[2]));
        assert!(store.bring_to_front(ids[1]));
        assert_eq!(store.iter_top_down().next().unwrap().id, ids[0]);
    }

    #[test]
    fn a_back_area_stays_beneath_every_auto_area_however_recently_touched() {
        let (mut store, ids) = store_with(&[AreaType::Default; 3]);
        assert!(store.set_layer(ids[2], Layer::Back));
        assert!(store.bring_to_front(ids[2]));
        let stacked: Vec<AreaId> = store.iter().map(|area| area.id).collect();
        assert_eq!(stacked, vec![ids[2], ids[0], ids[1]]);
    }

    #[test]
    fn the_three_tiers_stack_in_order_regardless_of_creation_order() {
        let (mut store, ids) = store_with(&[AreaType::Default; 3]);
        assert!(store.set_layer(ids[0], Layer::Front));
        assert!(store.set_layer(ids[1], Layer::Back));
        // ids[2] is left on Auto.
        let stacked: Vec<AreaId> = store.iter().map(|area| area.id).collect();
        assert_eq!(stacked, vec![ids[1], ids[2], ids[0]]);
    }

    #[test]
    fn set_layer_raises_within_the_new_tier() {
        // Picking "Always on top" on an area already behind another `Front` one
        // must visibly do something, or the menu looks broken.
        let (mut store, ids) = store_with(&[AreaType::Default; 2]);
        assert!(store.set_layer(ids[0], Layer::Front));
        assert!(store.set_layer(ids[1], Layer::Front));
        assert_eq!(store.iter_top_down().next().unwrap().id, ids[1]);
    }

    #[test]
    fn returning_an_area_to_auto_puts_it_back_under_recency() {
        let (mut store, ids) = store_with(&[AreaType::Default; 2]);
        assert!(store.set_layer(ids[0], Layer::Front));
        assert!(store.set_layer(ids[0], Layer::Auto));
        assert!(store.bring_to_front(ids[1]));
        assert_eq!(store.iter_top_down().next().unwrap().id, ids[1]);
    }

    #[test]
    fn a_pinned_front_filter_still_does_not_steal_the_click() {
        // The exact combination ADR-0013's motivating case produces: the tint
        // the user pinned on top of the thing it tints. Tier precedence governs
        // paint order; it must not govern input, or the feature is unusable.
        let mut store = AreaStore::new();
        let below = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        let tint = store
            .create(AreaType::Filter, rect(0, 0, 100, 100))
            .unwrap();
        assert!(store.set_layer(tint, Layer::Front));
        assert_eq!(store.iter_top_down().next().unwrap().id, tint);
        assert_eq!(
            store.hit_test(Point::new(50, 50), &screens()).unwrap().id,
            below
        );
    }

    #[test]
    fn a_pass_through_area_is_still_reachable_for_placement_gestures() {
        // The `hit_test` / `hit_test_any` split. A Filter is invisible to a
        // click in Living by design; if it were also invisible while editing the
        // layout it could never be moved or dismissed, i.e. permanent.
        let mut store = AreaStore::new();
        store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        let tint = store
            .create(AreaType::Filter, rect(0, 0, 100, 100))
            .unwrap();
        let point = Point::new(50, 50);
        assert_ne!(store.hit_test(point, &screens()).unwrap().id, tint);
        assert_eq!(store.hit_test_any(point).unwrap().id, tint);
    }

    #[test]
    fn placement_hit_testing_follows_the_same_tier_order() {
        let (mut store, ids) = store_with(&[AreaType::Default; 2]);
        assert!(store.set_layer(ids[0], Layer::Front));
        assert_eq!(store.hit_test_any(Point::new(50, 50)).unwrap().id, ids[0]);
    }

    #[test]
    fn a_sub_50px_pass_through_areas_close_control_answers_close() {
        // Roadmap 1.27 records this as owed: "`handle_at` tests the close control
        // *before* the bounds check, so a pass-through area below
        // `CHROME_INSIDE_SPAN` answers `Handle::Close` there and `hit_test`
        // returns it, which means its 18 px close control already opens the menu.
        // Read from `interaction.rs`, not measured. Owed: a test for it."
        //
        // The branch that made that control visible is the right place to pay it.
        // Asserted at `Handle::Close` specifically rather than "some handle",
        // because `Handle::Body` here would mean the area had a grab it does not
        // have, and `None` would mean it was stranded.
        let mut store = AreaStore::new();
        let bounds = rect(400, 400, 20, 20);
        assert!(
            interaction::chrome_is_outside(bounds),
            "this test is vacuous unless the area is below CHROME_INSIDE_SPAN"
        );
        let small = store.create(AreaType::Filter, bounds).unwrap();
        let control = interaction::close_control(bounds, &screens());
        let on_control = Point::new(control.origin.x + 2, control.origin.y + 2);

        let (area, handle) = store.grab_test(on_control, &screens()).unwrap();
        assert_eq!(area.id, small);
        assert_eq!(
            handle,
            interaction::Handle::Close,
            "the sole route back from a small Filter must be the close control"
        );
    }

    #[test]
    fn grab_test_admits_a_pass_through_areas_chrome_and_refuses_its_body() {
        // `grab_test` is the Living grab rule, extracted from the host so there
        // is one copy rather than two. The rule is ADR-0024 section 2: the body
        // passes clicks through, the chrome does not. Both directions, because a
        // version that admitted everything and a version that admitted nothing
        // would each satisfy a single-sided test.
        let mut store = AreaStore::new();
        let bounds = rect(0, 0, 100, 100);
        let tint = store.create(AreaType::Filter, bounds).unwrap();
        let control = interaction::close_control(bounds, &screens());

        let on_control = Point::new(control.origin.x + 2, control.origin.y + 2);
        let (area, handle) = store.grab_test(on_control, &screens()).unwrap();
        assert_eq!(area.id, tint);
        assert_eq!(handle, interaction::Handle::Close);

        assert!(
            store.grab_test(Point::new(50, 50), &screens()).is_none(),
            "the body of a pass-through area grabs nothing"
        );
    }

    #[test]
    fn a_pass_through_body_is_hovered_even_though_it_takes_no_click() {
        // The defect the founder found on the rig during 1.27's first hardware
        // pass, after that row had shipped: hover was
        // resolved through the Living input rule, so moving across a Filter area
        // revealed no close control and the only route back to its menu was an
        // invisible target. `hit_test` must keep saying no here; `hover_test`
        // must say yes. Both halves, because a fix that made the body take
        // clicks would satisfy the second assertion and break ADR-0016.
        let mut store = AreaStore::new();
        let tint = store
            .create(AreaType::Filter, rect(0, 0, 100, 100))
            .unwrap();
        let body = Point::new(50, 50);
        assert!(
            store.hit_test(body, &screens()).is_none(),
            "a pass-through body must still pass the click to the app underneath"
        );
        assert_eq!(store.hover_test(body, &screens()).unwrap().id, tint);
    }

    #[test]
    fn a_small_areas_close_control_keeps_the_hover_it_is_reached_by() {
        // The reason `hover_test` is not `hit_test_any`. Below
        // `CHROME_INSIDE_SPAN` the control is placed *outside* the bounds, so a
        // bounds-only rule drops the hover exactly as the cursor arrives on the
        // control, and the chrome vanishes as it is reached for.
        let mut store = AreaStore::new();
        let bounds = rect(200, 200, 20, 20);
        let small = store.create(AreaType::Filter, bounds).unwrap();
        let control = interaction::close_control(bounds, &screens());
        let on_control = Point::new(control.origin.x + 2, control.origin.y + 2);
        assert!(
            !bounds.contains(on_control),
            "this test is vacuous unless the control sits outside the bounds"
        );
        assert!(
            store.hit_test_any(on_control).is_none(),
            "the bounds-only rule does not cover the control, which is the point"
        );
        assert_eq!(store.hover_test(on_control, &screens()).unwrap().id, small);
    }

    #[test]
    fn hover_follows_the_same_tier_order_as_everything_else() {
        // A third resolver is a third chance to get "topmost" wrong, and a
        // hover on the wrong area draws chrome on the wrong rectangle.
        let (mut store, ids) = store_with(&[AreaType::Filter; 2]);
        assert!(store.set_layer(ids[0], Layer::Front));
        assert_eq!(
            store.hover_test(Point::new(50, 50), &screens()).unwrap().id,
            ids[0]
        );
    }

    #[test]
    fn tiers_order_the_region_list_the_same_way_they_order_hit_testing() {
        let mut store = AreaStore::new();
        let auto = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        let front = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        assert!(store.set_layer(front, Layer::Front));
        assert!(store.bring_to_front(auto));
        assert_eq!(
            store.hit_test(Point::new(50, 50), &screens()).unwrap().id,
            front
        );
        assert_eq!(store.interactive_regions(&screens()).len(), 2);
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
        assert_eq!(
            store.hit_test(Point::new(50, 50), &screens()).unwrap().id,
            below
        );
    }

    #[test]
    fn converting_away_from_default_resets_a_magnification_the_new_type_cannot_undo() {
        // Roadmap 1.27's invariant, and the reason it is the model's job rather
        // than the menu's. `zoom_by` refuses on the *type*, so a converted area
        // holding a non-natural zoom is not merely odd, it is unrecoverable:
        // nothing the user can do puts it back.
        let mut store = AreaStore::new();
        let id = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        store.zoom_by(id, 4).unwrap();
        assert!(
            !store.get(id).unwrap().zoom.is_natural(),
            "set up magnified"
        );

        let conversion = store.set_kind(id, AreaType::Filter).unwrap();

        assert!(conversion.changed);
        assert!(
            conversion.discard_capture,
            "the magnified pin is a still of somewhere the area no longer is"
        );
        assert!(
            store.get(id).unwrap().zoom.is_natural(),
            "a type that cannot zoom cannot be left holding a zoom"
        );
        // The check that makes the reset load-bearing rather than cosmetic.
        assert_eq!(
            store.zoom_by(id, -4),
            None,
            "zoom_by refuses on the type, so the user has no way back"
        );
    }

    #[test]
    fn converting_takes_the_new_types_visual_and_input_defaults() {
        let mut store = AreaStore::new();
        let id = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        assert_eq!(store.get(id).unwrap().input, Input::Interactive);

        store.set_kind(id, AreaType::Filter).unwrap();

        // §3.2's own test for the two pass-through types: a tint you cannot work
        // underneath is useless. A Filter that still swallows clicks is not one.
        assert_eq!(store.get(id).unwrap().input, Input::PassThrough);
        assert_eq!(store.get(id).unwrap().visual, Visual::Passive);
        assert_eq!(store.get(id).unwrap().kind, AreaType::Filter);
    }

    #[test]
    fn converting_preserves_the_layer_the_user_pinned() {
        // Layer is not a type-derived axis. "Always on top" was set on this
        // rectangle, not on what the rectangle currently does.
        let mut store = AreaStore::new();
        let id = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        store.set_layer(id, Layer::Front);

        store.set_kind(id, AreaType::Filter).unwrap();

        assert_eq!(store.get(id).unwrap().layer, Layer::Front);
    }

    #[test]
    fn converting_away_from_screenshot_discards_the_pin_that_was_its_content() {
        let mut store = AreaStore::new();
        let id = store
            .create(AreaType::Screenshot, rect(0, 0, 100, 100))
            .unwrap();

        let conversion = store.set_kind(id, AreaType::Default).unwrap();

        assert!(conversion.discard_capture);
    }

    #[test]
    fn converting_to_the_type_it_already_is_changes_nothing_and_discards_nothing() {
        // The menu draws the current type's row ticked and leaves it clickable,
        // so this is an ordinary request. Reporting `discard_capture` here would
        // throw away a live Screenshot's pixels for a click that meant nothing.
        let mut store = AreaStore::new();
        let id = store
            .create(AreaType::Screenshot, rect(0, 0, 100, 100))
            .unwrap();

        let conversion = store.set_kind(id, AreaType::Screenshot).unwrap();

        assert!(!conversion.changed);
        assert!(!conversion.discard_capture);
        assert_eq!(store.get(id).unwrap().kind, AreaType::Screenshot);
    }

    #[test]
    fn converting_an_unknown_id_answers_none() {
        let mut store = AreaStore::new();
        let id = store
            .create(AreaType::Default, rect(0, 0, 100, 100))
            .unwrap();
        store.remove(id);

        assert_eq!(store.set_kind(id, AreaType::Filter), None);
    }

    #[test]
    fn every_conversion_leaves_an_area_whose_zoom_its_type_allows() {
        // The invariant stated over the whole matrix rather than on one pair, so
        // an eighth type cannot arrive with a hole in it. Seven by seven, with
        // each source area magnified as far as its own type permits first.
        for from in AreaType::ALL {
            for to in AreaType::ALL {
                let mut store = AreaStore::new();
                let id = store.create(from, rect(0, 0, 100, 100)).unwrap();
                store.zoom_by(id, 4);
                store.set_kind(id, to).unwrap();
                let area = store.get(id).unwrap();
                assert_eq!(area.kind, to, "{from:?} to {to:?}");
                assert!(
                    area.kind.supports_zoom() || area.zoom.is_natural(),
                    "{from:?} to {to:?} left a zoom the type cannot undo"
                );
            }
        }
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
        assert_eq!(
            store.hit_test(Point::new(60, 60), &screens()).unwrap().id,
            top
        );
        // And raising the lower one flips it.
        let lower: AreaId = store.iter().next().unwrap().id;
        assert!(store.bring_to_front(lower));
        assert_eq!(
            store.hit_test(Point::new(60, 60), &screens()).unwrap().id,
            lower
        );
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
        assert_eq!(
            store.hit_test(Point::new(10, 5), &screens()).unwrap().id,
            right
        );
        assert!(store.hit_test(Point::new(20, 5), &screens()).is_none());
    }

    #[test]
    fn areas_live_in_virtual_desktop_space_including_negative_coordinates() {
        // A monitor left of the primary starts at x < 0. An area drawn there is
        // ordinary, not an edge case.
        let mut store = AreaStore::new();
        let id = store
            .create(AreaType::Default, rect(-1920, -200, 300, 300))
            .unwrap();
        assert_eq!(
            store
                .hit_test(Point::new(-1800, -100), &screens())
                .unwrap()
                .id,
            id
        );
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
    fn an_interactive_area_contributes_its_bounds_and_a_pass_through_one_its_chrome() {
        // Rewritten for ADR-0024 §2. This test used to be
        // `interactive_regions_holds_only_the_interactive_areas` and asserted that
        // pass-through areas contribute *nothing* — which was the behaviour that
        // stranded them.
        let mut store = AreaStore::new();
        let interactive = rect(0, 0, 80, 80);
        let passing = rect(200, 0, 80, 80);
        store.create(AreaType::Default, interactive).unwrap();
        store.create(AreaType::Filter, passing).unwrap();

        let regions = store.interactive_regions(&screens());
        // Topmost first: the Filter was created last, so its chrome leads.
        assert_eq!(
            regions.first().copied(),
            Some(interaction::close_control(passing, &screens())),
            "a pass-through area leads with its close control"
        );
        assert!(
            regions.contains(&interactive),
            "an interactive area still contributes its whole bounds"
        );
        // The pass-through area's *body* is in no region.
        let body = Point::new(240, 40);
        assert!(
            !crate::geometry::point_in_any(&regions, body),
            "the body of a pass-through area takes no input"
        );
        // Its edge does.
        let edge = Point::new(201, 40);
        assert!(crate::geometry::point_in_any(&regions, edge));
    }

    #[test]
    fn a_small_pass_through_area_offers_its_close_control_and_nothing_else() {
        // Below `CHROME_INSIDE_SPAN` an area has no resize band at all
        // (`handle_at` answers `Body` everywhere inside it), so its chrome is the
        // close control alone. That is the gap task 1.17(b2)'s outside handles
        // close, recorded here so shrinking it is a deliberate change.
        let mut store = AreaStore::new();
        let small = rect(0, 0, 20, 20);
        store.create(AreaType::Filter, small).unwrap();

        let regions = store.interactive_regions(&screens());
        assert_eq!(regions, vec![interaction::close_control(small, &screens())]);
        assert!(
            store.hit_test(Point::new(10, 10), &screens()).is_none(),
            "its interior is body, so it passes through"
        );
    }

    #[test]
    fn an_empty_store_reports_no_regions_at_all() {
        // The state a caller must not confuse with `ClickThrough`'s fail-safe
        // empty set, which means the opposite ("take input everywhere").
        let store = AreaStore::new();
        assert!(store.interactive_regions(&screens()).is_empty());
        assert!(store.is_empty());
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

    /// §3.4's floor, stated as the test that would fail if it moved: an area at
    /// natural size shows the screen itself, and scrolling further out is a
    /// no-op rather than a way to get lost.
    #[test]
    fn zooming_out_at_natural_size_stays_at_natural_size() {
        let zoom = Zoom::NATURAL.stepped(-1);
        assert!(zoom.is_natural());
        assert!((zoom.factor() - 1.0).abs() < f32::EPSILON);
        // Not merely "still natural after one notch": the floor has to hold
        // against the scroll a user actually produces, which is a fast flick.
        assert!(Zoom::NATURAL.stepped(-400).is_natural());
        assert!(Zoom::NATURAL.stepped(i32::MIN).is_natural());
    }

    /// The ceiling and the factor it is documented to produce are two constants
    /// that must agree. Editing `STEP` or `MAX_STEP` alone turns this red. The
    /// alternative is a doc comment claiming 8× while the code does something
    /// else, which no test would catch.
    #[test]
    fn the_ceiling_matches_the_factor_it_claims() {
        let ceiling = Zoom::NATURAL.stepped(i32::from(Zoom::MAX_STEP));
        assert!((ceiling.factor() - Zoom::MAX_FACTOR).abs() < f32::EPSILON);
        assert_eq!(ceiling, Zoom::NATURAL.stepped(i32::MAX), "must saturate");
    }

    /// A magnifier shows less screen, larger. At 2× that is exactly half of
    /// each extent, centred, and asserted on a rectangle whose halves are even, so
    /// a failure means the arithmetic is wrong rather than that it rounded.
    #[test]
    fn source_rect_halves_and_centres_at_two_times() {
        let zoom = Zoom::NATURAL.stepped(4);
        assert!((zoom.factor() - 2.0).abs() < f32::EPSILON);
        let source = zoom.source_rect(Rect::new(100, 200, 400, 300));
        assert_eq!(source, Rect::new(200, 275, 200, 150));
    }

    /// The identity case is a separate arm in `source_rect`, so it gets its own
    /// test: at natural size the source *is* the area, byte for byte, and
    /// nothing downstream has to special-case a rectangle that drifted by a
    /// rounding error.
    #[test]
    fn source_rect_at_natural_size_is_the_area_itself() {
        let bounds = Rect::new(-33, 17, 101, 57);
        assert_eq!(Zoom::NATURAL.source_rect(bounds), bounds);
    }

    /// The one-pixel floor, driven at the smallest rectangle the model admits
    /// rather than argued about. A zero-extent source would be a capture
    /// request no capture backend can serve.
    #[test]
    fn source_rect_never_asks_for_an_empty_capture() {
        let smallest = Rect::new(0, 0, 1, 1);
        let source = Zoom::NATURAL
            .stepped(i32::from(Zoom::MAX_STEP))
            .source_rect(smallest);
        assert_eq!(source.size, Size::new(1, 1));
    }

    /// `supports_zoom` is a per-type answer, and the one that matters is that
    /// exactly one type says yes today. A future type that says yes has to
    /// change this line, which is where the decision gets noticed.
    #[test]
    fn only_the_default_area_zooms() {
        let zooming: Vec<AreaType> = ALL_TYPES
            .iter()
            .copied()
            .filter(|kind| kind.supports_zoom())
            .collect();
        assert_eq!(zooming, vec![AreaType::Default]);
    }

    /// The store's two refusals are one return value, and this pins which is
    /// which: an unknown id and an unzoomable type both answer `None`, while a
    /// scroll that saturates answers `Some` because the area still owns it.
    #[test]
    fn zoom_by_refuses_the_wrong_type_and_claims_a_saturating_scroll() {
        let (mut store, ids) = store_with(&[AreaType::Default, AreaType::Filter]);
        assert_eq!(store.zoom_by(ids[1], 1), None, "Filter does not zoom");
        assert_eq!(
            store.zoom_by(AreaId(9999), 1),
            None,
            "an unknown id does not zoom"
        );
        assert_eq!(
            store.zoom_by(ids[0], -3),
            Some(Zoom::NATURAL),
            "a scroll that hits the floor is still this area's scroll"
        );
        assert_eq!(store.zoom_by(ids[0], 2), Some(Zoom::NATURAL.stepped(2)));
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

    fn any_layer() -> impl Strategy<Value = Layer> {
        prop::sample::select([Layer::Back, Layer::Auto, Layer::Front].as_slice())
    }

    fn any_store() -> impl Strategy<Value = AreaStore> {
        // Layers are assigned as the user would — after creation, via
        // `set_layer` — so the generated stores exercise the re-tiering path
        // rather than only a hand-built sorted vector.
        prop::collection::vec((any_type(), any_rect(), any_layer()), 0..12).prop_map(|specs| {
            let mut store = AreaStore::new();
            for (kind, bounds, layer) in specs {
                if let Some(id) = store.create(kind, bounds) {
                    store.set_layer(id, layer);
                }
            }
            store
        })
    }

    /// The store's ordering invariant: tiers ascend along the vector.
    ///
    /// Checked as a helper rather than inline because three properties assert
    /// it, and because `partition_point` in [`AreaStore::top_of_tier`] is
    /// *unsound* without it — it would silently return a wrong index rather
    /// than fail, so every mutation has to be pinned against it.
    fn tiers_ascend(store: &AreaStore) -> bool {
        store
            .iter()
            .map(|area| area.layer)
            .collect::<Vec<_>>()
            .windows(2)
            .all(|pair| pair[0] <= pair[1])
    }

    /// ADR-0024 §2's rule, restated independently of [`AreaStore::hit_test`].
    ///
    /// The property tests below must not just call the function they are checking,
    /// so the rule is written out once here: an interactive area takes input
    /// anywhere inside itself, a pass-through area only on its chrome.
    fn takes_input_at(area: &Area, point: Point) -> bool {
        if area.is_interactive() {
            area.bounds.contains(point)
        } else {
            interaction::is_chrome_at(area.bounds, point, &screens())
        }
    }

    proptest! {
        /// The two invariants every caller of `source_rect` relies on, over the
        /// whole zoom range rather than at the three factors a unit test picks:
        /// the source is **inside** the area, so the magnifier never shows
        /// pixels the user did not claim, and it is **never empty**, so the
        /// capture request can always be served.
        ///
        /// Containment is checked on the far edges rather than by `contains`,
        /// which takes a point: a rectangle sitting one pixel outside on the
        /// right passes any single-corner check.
        #[test]
        fn a_source_rect_is_inside_its_area_and_never_empty(
            bounds in any_rect(),
            notches in 0i32..=i32::from(Zoom::MAX_STEP),
        ) {
            let source = Zoom::NATURAL.stepped(notches).source_rect(bounds);
            prop_assert!(source.size.width >= 1 && source.size.height >= 1);
            prop_assert!(source.origin.x >= bounds.origin.x);
            prop_assert!(source.origin.y >= bounds.origin.y);
            prop_assert!(source.right() <= bounds.right());
            prop_assert!(source.bottom() <= bounds.bottom());
        }

        /// Magnification is monotonic: scrolling in never shows *more* screen.
        /// Written as a comparison between adjacent steps because that is the
        /// operation the user performs, and an off-by-one in `stepped` that
        /// re-used the previous step would pass a test of the endpoints alone.
        #[test]
        fn scrolling_in_never_widens_the_source(
            bounds in any_rect(),
            notches in 0i32..i32::from(Zoom::MAX_STEP),
        ) {
            let here = Zoom::NATURAL.stepped(notches);
            let closer = here.stepped(1);
            let (wide, narrow) = (here.source_rect(bounds), closer.source_rect(bounds));
            prop_assert!(narrow.size.width <= wide.size.width);
            prop_assert!(narrow.size.height <= wide.size.height);
        }

        #[test]
        fn ids_are_unique_across_the_store(store in any_store()) {
            let mut seen = std::collections::HashSet::new();
            for area in store.iter() {
                prop_assert!(seen.insert(area.id), "duplicate id {:?}", area.id);
            }
        }

        #[test]
        fn a_hit_is_an_interactive_body_or_a_pass_through_areas_chrome(
            store in any_store(),
            x in -250i32..250,
            y in -250i32..250,
        ) {
            // ADR-0024 §2 split this invariant in two. It used to read
            // `prop_assert!(area.is_interactive())` — true only while a
            // pass-through area was invisible to input.
            //
            // Note the asymmetry in the bounds check: an interactive area is hit
            // only *inside* itself, but a pass-through area can be hit outside its
            // bounds, because a small area's close control sits outside them.
            let point = Point::new(x, y);
            if let Some(area) = store.hit_test(point, &screens()) {
                if area.is_interactive() {
                    prop_assert!(area.bounds.contains(point));
                } else {
                    prop_assert!(
                        interaction::is_chrome_at(area.bounds, point, &screens()),
                        "a pass-through hit must be on chrome"
                    );
                    prop_assert!(
                        !matches!(
                            interaction::handle_at(area.bounds, point, &screens()),
                            Some(interaction::Handle::Body)
                        ),
                        "and never on the body"
                    );
                }
            }
        }

        #[test]
        fn a_miss_means_no_interactive_area_contains_the_point(
            store in any_store(),
            x in -250i32..250,
            y in -250i32..250,
        ) {
            let point = Point::new(x, y);
            if store.hit_test(point, &screens()).is_none() {
                prop_assert!(
                    !store.iter().any(|a| takes_input_at(a, point))
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
                .rfind(|a| takes_input_at(a, point))
                .map(|a| a.id);
            prop_assert_eq!(store.hit_test(point, &screens()).map(|a| a.id), expected);
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
            let regions = store.interactive_regions(&screens());
            prop_assert_eq!(
                crate::geometry::point_in_any(&regions, point),
                store.hit_test(point, &screens()).is_some()
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
            let layer = store.get(id).map(|a| a.layer);
            prop_assert!(store.bring_to_front(id));
            let after: std::collections::HashSet<AreaId> =
                store.iter().map(|a| a.id).collect();
            prop_assert_eq!(before, after);
            prop_assert!(tiers_ascend(&store));
            // Top of its own tier — *not* top of the stack. Any area above it
            // must be pinned to a higher tier, which is ADR-0013 rule 3 stated
            // as a property rather than as three hand-picked cases.
            prop_assert!(
                store
                    .iter_top_down()
                    .take_while(|a| a.id != id)
                    .all(|a| Some(a.layer) > layer),
                "a same-or-lower-tier area sits above the raised one"
            );
        }

        #[test]
        fn every_mutation_leaves_the_tiers_ascending(
            store in any_store(),
            index in 0usize..12,
            layer in any_layer(),
        ) {
            // `top_of_tier`'s `partition_point` is only correct while this
            // holds, and it fails silently rather than loudly if it stops.
            let mut store = store;
            prop_assert!(tiers_ascend(&store));
            prop_assume!(!store.is_empty());
            let Some(id) = store.iter().nth(index % store.len()).map(|a| a.id) else {
                return Ok(());
            };
            prop_assert!(store.set_layer(id, layer));
            prop_assert!(tiers_ascend(&store));
            prop_assert_eq!(store.get(id).map(|a| a.layer), Some(layer));
            prop_assert!(store.remove(id).is_some());
            prop_assert!(tiers_ascend(&store));
            prop_assert!(store.create(AreaType::Default, Rect::new(0, 0, 10, 10)).is_some());
            prop_assert!(tiers_ascend(&store));
        }

        #[test]
        fn removing_every_area_empties_the_store(store in any_store()) {
            let mut store = store;
            let ids: Vec<AreaId> = store.iter().map(|a| a.id).collect();
            for id in &ids {
                prop_assert!(store.remove(*id).is_some());
            }
            prop_assert!(store.is_empty());
            prop_assert!(store.interactive_regions(&screens()).is_empty());
            // And every id is now stale rather than pointing at anything.
            for id in &ids {
                prop_assert!(store.get(*id).is_none());
            }
        }
    }
}
