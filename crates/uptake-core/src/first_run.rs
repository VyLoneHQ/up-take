//! The first-run tour: its four steps and what moves the user between them
//! (roadmap 1.18, ADR-0043).
//!
//! Pure on purpose. The tour is taught on the live overlay, so every input to
//! it is an event something else already produces: an area being created, the
//! overlay changing state, a press on one of the coach's own buttons. Deciding
//! what those events MEAN for the tour is this module's whole job, and keeping
//! it free of Tauri is what lets that table be tested without a window.
//!
//! # The steps, in ADR-0043 decision 2's order
//!
//! 1. **Draw an area.** Moves on when any area is created.
//! 2. **An area has a type.** Moves on when an area is created with a type
//!    armed, because that is the gesture the step teaches. A plain area drawn
//!    here is the user practising step 1 again and says nothing about step 2.
//! 3. **Placing and Living.** Moves on after a round trip: into Living and
//!    back into Placement. Entering Living is half of the lesson (the screen is
//!    yours again) and coming back is the other half (the same chord returns
//!    it), so neither alone counts.
//! 4. **The reference sheet.** Nothing moves it on except the user saying so.
//!
//! `Next` moves on from any step without the gesture (decision 3), and `Skip`
//! ends the tour from any step.
//!
//! # What this does NOT decide
//!
//! Whether the tour runs at all (a stored flag, in `src-tauri`), where the coach
//! is drawn, and what `Esc` does. `Esc` is not an event here, and that follows
//! decision 3 rather than leaving a gap: it leaves Placement "as it does
//! everywhere else", and the tour sees only the state change that causes.

/// How many steps the tour has, as the coach prints it ("Step 2 of 4").
pub const STEP_COUNT: u8 = 4;

/// One step of the tour, in the order they are taught.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Drag anywhere to draw an area.
    DrawArea,
    /// Press a key first, then drag: the area's type.
    AreaTypes,
    /// Placing and Living, and the chord between them.
    Modes,
    /// The keybind reference, which is also the tour's last screen.
    Reference,
}

impl Step {
    /// The step's position, counted from 1.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::DrawArea => 1,
            Self::AreaTypes => 2,
            Self::Modes => 3,
            Self::Reference => 4,
        }
    }

    /// The step after this one, or `None` after the last.
    const fn following(self) -> Option<Self> {
        match self {
            Self::DrawArea => Some(Self::AreaTypes),
            Self::AreaTypes => Some(Self::Modes),
            Self::Modes => Some(Self::Reference),
            Self::Reference => None,
        }
    }
}

/// Something that happened which the tour may care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TourEvent {
    /// A drag created an area. `typed` is whether a type was armed for it, so
    /// anything other than a plain area.
    AreaCreated {
        /// Whether the area was created with a type armed.
        typed: bool,
    },
    /// The overlay settled in Placement. Sent for a summon that was already in
    /// Placement too, which is why step 3 needs a trip to Living first.
    EnteredPlacement,
    /// The overlay settled in Living.
    EnteredLiving,
    /// The coach's `Next` button (or, on the last step, its finish button).
    Next,
    /// The coach's `Skip` button.
    Skip,
}

/// Where the user is in the tour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tour {
    step: Step,
    /// Whether the user has been into Living during the current step. Read only
    /// on step 3, and reset by every change of step, so a trip to Living taken
    /// during step 2 cannot count toward step 3 later.
    been_living: bool,
}

impl Tour {
    /// A tour at its first step.
    #[must_use]
    pub const fn start() -> Self {
        Self {
            step: Step::DrawArea,
            been_living: false,
        }
    }

    /// The step the user is on.
    #[must_use]
    pub const fn step(self) -> Step {
        self.step
    }

    /// Whether the coach is drawn while the overlay is in Living.
    ///
    /// Only on step 3, the one step whose lesson happens in Living. Every other
    /// step teaches a Placement gesture, and a panel describing one over a
    /// screen that has been handed back would be a control the user cannot act
    /// on, sitting on top of their own apps.
    #[must_use]
    pub const fn shows_in_living(self) -> bool {
        matches!(self.step, Step::Modes)
    }

    /// The tour after `event`, or `None` when the tour has ended.
    #[must_use]
    pub fn advance(self, event: TourEvent) -> Option<Self> {
        let moves_on = match (self.step, event) {
            (_, TourEvent::Skip) => return None,
            (_, TourEvent::Next)
            | (Step::DrawArea, TourEvent::AreaCreated { .. })
            | (Step::AreaTypes, TourEvent::AreaCreated { typed: true }) => true,
            (Step::Modes, TourEvent::EnteredLiving) => {
                return Some(Self {
                    step: self.step,
                    been_living: true,
                });
            }
            (Step::Modes, TourEvent::EnteredPlacement) => self.been_living,
            _ => false,
        };
        if !moves_on {
            return Some(self);
        }
        self.step.following().map(|step| Self {
            step,
            been_living: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{STEP_COUNT, Step, Tour, TourEvent};

    const EVERY_EVENT: [TourEvent; 6] = [
        TourEvent::AreaCreated { typed: false },
        TourEvent::AreaCreated { typed: true },
        TourEvent::EnteredPlacement,
        TourEvent::EnteredLiving,
        TourEvent::Next,
        TourEvent::Skip,
    ];

    /// A tour standing on `step`, reached the way a user reaches it.
    fn at(step: Step) -> Tour {
        let mut tour = Tour::start();
        while tour.step() != step {
            let Some(next) = tour.advance(TourEvent::Next) else {
                panic!("ran off the end of the tour looking for {step:?}")
            };
            tour = next;
        }
        tour
    }

    #[test]
    fn the_tour_starts_at_step_one() {
        assert_eq!(Tour::start().step(), Step::DrawArea);
        assert_eq!(Tour::start().step().number(), 1);
    }

    #[test]
    fn next_walks_every_step_in_order_and_ends_after_the_last() {
        let mut tour = Tour::start();
        let mut numbers = vec![tour.step().number()];
        while let Some(next) = tour.advance(TourEvent::Next) {
            tour = next;
            numbers.push(tour.step().number());
        }
        assert_eq!(numbers, vec![1, 2, 3, 4]);
        assert_eq!(numbers.len(), usize::from(STEP_COUNT));
    }

    #[test]
    fn skip_ends_the_tour_from_every_step() {
        for step in [
            Step::DrawArea,
            Step::AreaTypes,
            Step::Modes,
            Step::Reference,
        ] {
            assert_eq!(at(step).advance(TourEvent::Skip), None, "{step:?}");
        }
    }

    #[test]
    fn step_one_moves_on_for_any_area() {
        for typed in [false, true] {
            let after = Tour::start().advance(TourEvent::AreaCreated { typed });
            assert_eq!(
                after.map(Tour::step),
                Some(Step::AreaTypes),
                "typed: {typed}"
            );
        }
    }

    #[test]
    fn step_two_moves_on_only_for_a_typed_area() {
        let tour = at(Step::AreaTypes);
        assert_eq!(
            tour.advance(TourEvent::AreaCreated { typed: false }),
            Some(tour),
            "a plain area is step 1 practised again, not step 2 learned"
        );
        assert_eq!(
            tour.advance(TourEvent::AreaCreated { typed: true })
                .map(Tour::step),
            Some(Step::Modes)
        );
    }

    #[test]
    fn step_three_needs_the_round_trip_and_not_half_of_it() {
        let tour = at(Step::Modes);
        // Placement alone: a summon while already placing sends this, and it
        // must not count as having come back from anywhere.
        assert_eq!(tour.advance(TourEvent::EnteredPlacement), Some(tour));

        let Some(living) = tour.advance(TourEvent::EnteredLiving) else {
            panic!("entering Living does not end the tour")
        };
        assert_eq!(
            living.step(),
            Step::Modes,
            "Living alone is half the lesson"
        );
        assert!(living.shows_in_living());
        // A second trip into Living before coming back changes nothing.
        assert_eq!(living.advance(TourEvent::EnteredLiving), Some(living));

        assert_eq!(
            living.advance(TourEvent::EnteredPlacement).map(Tour::step),
            Some(Step::Reference)
        );
    }

    #[test]
    fn a_trip_to_living_taken_before_step_three_does_not_count_toward_it() {
        let Some(on_two) = at(Step::AreaTypes).advance(TourEvent::EnteredLiving) else {
            panic!("entering Living on step 2 does not end the tour")
        };
        let Some(on_three) = on_two.advance(TourEvent::AreaCreated { typed: true }) else {
            panic!("a typed area on step 2 does not end the tour")
        };
        assert_eq!(on_three.step(), Step::Modes);
        assert_eq!(
            on_three.advance(TourEvent::EnteredPlacement),
            Some(on_three),
            "step 3 moved on without its own trip to Living"
        );
    }

    #[test]
    fn the_reference_sheet_waits_for_the_user_whatever_else_happens() {
        let tour = at(Step::Reference);
        for event in EVERY_EVENT {
            let after = tour.advance(event);
            match event {
                TourEvent::Next | TourEvent::Skip => assert_eq!(after, None, "{event:?}"),
                _ => assert_eq!(after, Some(tour), "{event:?}"),
            }
        }
    }

    #[test]
    fn no_step_moves_on_for_an_event_that_is_not_its_own() {
        // The negative half of the table, swept rather than sampled: a step
        // that moved on for someone else's gesture would teach nothing.
        let cases = [
            (Step::DrawArea, TourEvent::EnteredPlacement),
            (Step::DrawArea, TourEvent::EnteredLiving),
            (Step::AreaTypes, TourEvent::EnteredPlacement),
            (Step::AreaTypes, TourEvent::EnteredLiving),
            (Step::Modes, TourEvent::AreaCreated { typed: false }),
            (Step::Modes, TourEvent::AreaCreated { typed: true }),
        ];
        for (step, event) in cases {
            let after = at(step).advance(event).map(Tour::step);
            assert_eq!(after, Some(step), "{step:?} moved on for {event:?}");
        }
    }

    #[test]
    fn only_step_three_is_drawn_in_living() {
        let shown: Vec<bool> = [
            Step::DrawArea,
            Step::AreaTypes,
            Step::Modes,
            Step::Reference,
        ]
        .into_iter()
        .map(|step| at(step).shows_in_living())
        .collect();
        assert_eq!(shown, vec![false, false, true, false]);
    }
}
