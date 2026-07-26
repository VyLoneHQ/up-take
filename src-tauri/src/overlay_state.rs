//! The overlay's three-state interaction model (roadmap task 1.6,
//! [ADR-0012](../../../Projects/UP-TAKE/DECISIONS/ADR-0012-overlay-interaction-model.md)).
//!
//! The overlay is always in exactly one of three states:
//!
//! - **`Hidden`** — nothing on screen; the user's apps have input.
//! - **`Placement`** — UP-TAKE has input focus: a light tint dims the screen,
//!   the whole surface is interactive, and the user drags to create areas.
//! - **`Living`** — the user's apps have input; areas float and stay
//!   interactive, while everything between them is click-through.
//!
//! The global hotkey **toggles input focus** between UP-TAKE and the real
//! screen; `Esc` backs out of `Placement`; an explicit summon (tray, relaunch,
//! startup) always lands in `Placement`.
//!
//! This module holds only the **pure** transition — a total function of the
//! current state, the event, and whether any areas exist. The window, IPC and
//! poll effects live in [`crate::overlay`], driven by the state this produces.
//! Keeping the decision separate from the effects is what makes the table below
//! unit-testable without a window.

/// Which of the three interaction states the overlay is in (ADR-0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayState {
    /// Nothing on screen; the user's apps have input.
    Hidden,
    /// UP-TAKE has input focus: dim surface, drag to create, hotkeys live.
    Placement,
    /// Apps have input; areas float and are click-through between.
    Living,
}

/// What can drive a state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The global hotkey (`Win+Shift+U`): toggles input focus.
    Toggle,
    /// `Esc` from the overlay. Only reachable while the overlay holds focus —
    /// in `Living` the apps have focus, so the overlay's key handler never sees
    /// it there (the platform truth ADR-0012 turns on).
    ///
    /// The two discriminators are the rungs of ADR-0018 §4's ladder: `Esc` peels
    /// **one layer at a time, innermost first**, so it cancels a drag before it
    /// disarms, and disarms before it backs out of `Placement`.
    Escape {
        /// Whether a placement drag is currently in progress.
        mid_drag: bool,
        /// Whether a type is armed for the next drag (ADR-0018 §1). Read only
        /// when `mid_drag` is false — a drag is the innermost rung, and
        /// cancelling one leaves the arming intact so the retry is still armed.
        armed: bool,
    },
    /// An explicit summon — the tray Show item, a single-instance relaunch, or
    /// the debug startup show. Always ends in `Placement`, so the user lands
    /// ready to place an area.
    Summon,
    /// An area was just created, carrying that type's answer to ADR-0018 §6's
    /// "and then what?" ([`uptake_core::area::AreaType::after_create`]).
    ///
    /// The polarity lives in the table below rather than at the call site: a
    /// caller that decides *whether to send an event* can get the condition
    /// backwards silently, where a caller that always reports what happened
    /// cannot.
    AreaCreated {
        /// Whether this type's creation ends `Placement` (`Screenshot` does;
        /// `Default` does not).
        exits_placement: bool,
    },
}

/// The next state, given the current one, the event, and whether any areas
/// exist. Total and pure — the single source of truth for the state machine.
///
/// Two rules are worth stating because they are not obvious:
///
/// - **`Escape` while mid-drag does not change state.** It cancels the drag (an
///   effect [`crate::overlay`] performs); the overlay stays in `Placement`.
/// - **`Escape` while armed does not change state either.** It disarms (again an
///   effect, not a state change), which is the middle rung of ADR-0018 §4's
///   ladder. Only an `Esc` that has nothing left to peel backs out of
///   `Placement` — so reaching `Living` from an armed, mid-drag `Placement`
///   takes three presses, and each one visibly undoes something.
/// - **`Living` with no areas collapses to `Hidden`.** A click-through overlay
///   with nothing on it is invisible, and worse than useless: the click-through
///   poll reads an empty region set as its fail-safe "take input everywhere",
///   so a `Living` state with no areas would capture every click instead of
///   passing it through. There is no such state; it is always `Hidden`.
#[must_use]
pub fn next(current: OverlayState, event: Event, has_areas: bool) -> OverlayState {
    let target = match (current, event) {
        // A summon always brings up the placement surface, from any state.
        (_, Event::Summon) => OverlayState::Placement,
        // A type whose creation ends Placement hands input back (ADR-0018 §6).
        // It lands in Living, not Hidden, because an area now exists — and
        // `collapse` below is what guarantees that rather than a rule here
        // (ADR-0018 §7).
        (
            OverlayState::Placement,
            Event::AreaCreated {
                exits_placement: true,
            },
        ) => OverlayState::Living,
        // Creating a Default area changes nothing about the state; neither does
        // any creation outside Placement, which cannot happen today but must
        // still have an answer for the function to stay total.
        (_, Event::AreaCreated { .. }) => current,
        // Cancelling a drag leaves the overlay where it is (Placement). This
        // arm is first so a mid-drag Esc never reaches the arming rung below —
        // the ladder is innermost-first, and arming deliberately survives a
        // cancelled drag (ADR-0018 §4).
        (_, Event::Escape { mid_drag: true, .. }) => current,
        // Disarming is the middle rung: it consumes the Esc without leaving
        // Placement.
        (
            OverlayState::Placement,
            Event::Escape {
                mid_drag: false,
                armed: true,
            },
        ) => current,
        // The hotkey toggles focus; Esc backs out of Placement.
        (OverlayState::Hidden, Event::Toggle) => OverlayState::Placement,
        (OverlayState::Placement, Event::Toggle | Event::Escape { .. }) => OverlayState::Living,
        (OverlayState::Living, Event::Toggle) => OverlayState::Placement,
        // Esc cannot reach an unfocused Living overlay; a no-op if it somehow
        // does, and Esc from Hidden has nothing to do.
        (OverlayState::Living, Event::Escape { .. })
        | (OverlayState::Hidden, Event::Escape { .. }) => current,
    };
    collapse(target, has_areas)
}

/// Collapses the unreachable `Living`-without-areas state onto `Hidden`.
fn collapse(target: OverlayState, has_areas: bool) -> OverlayState {
    match target {
        OverlayState::Living if !has_areas => OverlayState::Hidden,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOGGLE: Event = Event::Toggle;
    const ESC: Event = Event::Escape {
        mid_drag: false,
        armed: false,
    };
    const ESC_ARMED: Event = Event::Escape {
        mid_drag: false,
        armed: true,
    };
    const ESC_DRAG: Event = Event::Escape {
        mid_drag: true,
        armed: false,
    };
    const ESC_DRAG_ARMED: Event = Event::Escape {
        mid_drag: true,
        armed: true,
    };
    const SUMMON: Event = Event::Summon;

    #[test]
    fn the_hotkey_brings_up_placement_from_hidden() {
        assert_eq!(
            next(OverlayState::Hidden, TOGGLE, false),
            OverlayState::Placement
        );
    }

    #[test]
    fn the_hotkey_toggles_placement_back_to_the_real_screen() {
        // No areas: Living collapses to Hidden, so a toggle out of Placement
        // hands control back and clears the screen.
        assert_eq!(
            next(OverlayState::Placement, TOGGLE, false),
            OverlayState::Hidden
        );
        // With areas: Living is real — areas stay, apps get input.
        assert_eq!(
            next(OverlayState::Placement, TOGGLE, true),
            OverlayState::Living
        );
    }

    #[test]
    fn esc_backs_out_of_placement_exactly_like_the_hotkey() {
        assert_eq!(
            next(OverlayState::Placement, ESC, false),
            OverlayState::Hidden
        );
        assert_eq!(
            next(OverlayState::Placement, ESC, true),
            OverlayState::Living
        );
    }

    #[test]
    fn esc_mid_drag_cancels_without_leaving_placement() {
        // The drag itself is cancelled by the effect layer; the state is
        // unchanged whether or not areas exist.
        assert_eq!(
            next(OverlayState::Placement, ESC_DRAG, false),
            OverlayState::Placement
        );
        assert_eq!(
            next(OverlayState::Placement, ESC_DRAG, true),
            OverlayState::Placement
        );
    }

    #[test]
    fn esc_while_armed_disarms_without_leaving_placement() {
        // The middle rung of ADR-0018 §4. Disarming is an effect; the state
        // must not move, with or without areas — otherwise arming and then
        // changing your mind would dump the user out of Placement.
        assert_eq!(
            next(OverlayState::Placement, ESC_ARMED, false),
            OverlayState::Placement
        );
        assert_eq!(
            next(OverlayState::Placement, ESC_ARMED, true),
            OverlayState::Placement
        );
    }

    #[test]
    fn esc_mid_drag_stays_on_the_drag_rung_even_when_armed() {
        // Innermost-first: a mid-drag Esc must not consume the arming as well,
        // or a cancelled drag would silently lose the type the user selected
        // and the retry would make the wrong kind of area (ADR-0018 §4).
        assert_eq!(
            next(OverlayState::Placement, ESC_DRAG_ARMED, true),
            OverlayState::Placement
        );
    }

    #[test]
    fn the_ladder_takes_three_presses_to_leave_an_armed_mid_drag_placement() {
        // The whole ladder end to end, as the ADR's table reads it. Each press
        // peels exactly one layer, and only the last one moves the state.
        let armed_mid_drag = next(OverlayState::Placement, ESC_DRAG_ARMED, true);
        assert_eq!(armed_mid_drag, OverlayState::Placement, "drag cancelled");
        let armed = next(armed_mid_drag, ESC_ARMED, true);
        assert_eq!(armed, OverlayState::Placement, "disarmed");
        assert_eq!(next(armed, ESC, true), OverlayState::Living, "backed out");
    }

    #[test]
    fn creating_a_screenshot_hands_input_back_but_creating_a_default_does_not() {
        const CREATED_EXITING: Event = Event::AreaCreated {
            exits_placement: true,
        };
        const CREATED_STAYING: Event = Event::AreaCreated {
            exits_placement: false,
        };
        // `has_areas` is true because the area that fired this event exists —
        // that is what keeps the exit in Living instead of collapsing to Hidden
        // (ADR-0018 §7).
        assert_eq!(
            next(OverlayState::Placement, CREATED_EXITING, true),
            OverlayState::Living
        );
        assert_eq!(
            next(OverlayState::Placement, CREATED_STAYING, true),
            OverlayState::Placement
        );
    }

    #[test]
    fn a_creation_that_somehow_reports_no_areas_collapses_rather_than_stranding_living() {
        // Not reachable today — the created area is in the store before this
        // event fires — but the function is total, and a Living state with no
        // areas is the one the click-through poll reads as "take every click".
        assert_eq!(
            next(
                OverlayState::Placement,
                Event::AreaCreated {
                    exits_placement: true
                },
                false
            ),
            OverlayState::Hidden
        );
    }

    #[test]
    fn the_hotkey_takes_control_back_from_living() {
        assert_eq!(
            next(OverlayState::Living, TOGGLE, true),
            OverlayState::Placement
        );
    }

    #[test]
    fn a_summon_always_lands_in_placement() {
        for current in [
            OverlayState::Hidden,
            OverlayState::Placement,
            OverlayState::Living,
        ] {
            for has_areas in [false, true] {
                assert_eq!(next(current, SUMMON, has_areas), OverlayState::Placement);
            }
        }
    }

    #[test]
    fn living_without_areas_is_never_a_reachable_target() {
        // The invariant `desired_ignore`/the poll depends on: whatever event is
        // applied, we never end up in Living with no areas to justify it.
        for current in [
            OverlayState::Hidden,
            OverlayState::Placement,
            OverlayState::Living,
        ] {
            for event in [TOGGLE, ESC, ESC_DRAG, SUMMON] {
                assert_ne!(
                    next(current, event, false),
                    OverlayState::Living,
                    "reached Living with no areas from {current:?} on {event:?}"
                );
            }
        }
    }

    #[test]
    fn esc_and_toggle_are_inert_from_hidden_except_the_summoning_toggle() {
        // Esc from Hidden has nothing to hide; only the toggle summons.
        assert_eq!(next(OverlayState::Hidden, ESC, false), OverlayState::Hidden);
        assert_eq!(next(OverlayState::Hidden, ESC, true), OverlayState::Hidden);
    }
}
