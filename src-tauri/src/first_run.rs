//! The first-run tour on the live overlay (roadmap 1.18, ADR-0043).
//!
//! [`uptake_core::first_run`] decides what the tour's events mean. This module
//! is the half that needs the app: it decides whether the tour runs, feeds it
//! the events the overlay produces, tells the frontend what to draw, and
//! hit-tests the coach panel the frontend drew.
//!
//! # The coach is hit-tested here, against rectangles the WebView MEASURED
//!
//! ADR-0043 decision 4 says the coach "is hit-tested Rust-side the same way" as
//! the area menu, and it is: every press goes through the mouse hook, the
//! overlay stays click-through, and nothing is added to the input model. What
//! differs from the menu is where the rectangles come from. The menu is a fixed
//! grid of rows, so Rust lays it out and the frontend draws what it is told. The
//! coach is wrapped prose, and its height depends on the font, the language
//! (`1.38`) and the WebView's scale, none of which Rust knows. So the frontend
//! draws the panel, measures the panel and its buttons, and reports them through
//! [`overlay_report_coach`], and this module tests presses against the last
//! report. Laying the panel out here would have meant a second, guessed copy of
//! the frontend's text layout.
//!
//! **Every report is tied to the emit it answers** by a generation number, so a
//! report for a step the tour has already left cannot move the buttons of the
//! step that replaced it.
//!
//! ⚠️ **Until the first report lands the panel is not hit-testable at all**, and
//! a press on it in that window does whatever it would have done without the
//! coach. The window is one render, and the failure is a drag starting over the
//! panel rather than a click being lost, so it is stated rather than engineered
//! away.
//!
//! # When it runs
//!
//! From the first launch until the user finishes it or presses `Skip`, which is
//! recorded in [`crate::config`]. **Leaving Placement does not end it:** `Esc`
//! puts the coach away with the overlay and the next summon brings it back at
//! the same step, because a reflexive `Esc` in the first second of a new app is
//! not a decision never to be shown how it works. Quitting before the end starts
//! it again from step 1 on the next launch.
//!
//! In a debug build `UPTAKE_DEV_FIRST_RUN` runs it whatever the file says; see
//! `dev_harness`.

use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uptake_core::area::AreaType;
use uptake_core::first_run::{Tour, TourEvent};
use uptake_core::geometry::{Point, Rect};

use crate::config;
use crate::diagnostics;
use crate::overlay;
use crate::overlay_state::OverlayState;

/// The Tauri event carrying the coach to draw, or `null` for none.
const COACH_EVENT: &str = "overlay://coach";

/// One of the coach's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoachButton {
    /// `Next`, or the finish button on the last step.
    Next,
    /// `Skip`.
    Skip,
}

/// Where the frontend drew the coach, physical virtual-desktop px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    panel: Rect,
    next: Rect,
    /// Absent on the last step, which has no `Skip`.
    skip: Option<Rect>,
}

/// The tour in progress.
struct Live {
    tour: Tour,
    /// The state the overlay last settled in.
    surface: OverlayState,
    /// The monitor the coach is drawn on, resolved when it is first shown and
    /// again at every change of step, so it follows the user's attention
    /// between steps without jumping under the pointer during one.
    monitor: Option<Rect>,
    /// Bumped by every emit. A layout report must name the current one.
    generation: u64,
    /// The last accepted layout report, cleared by every emit.
    layout: Option<Layout>,
}

/// The tour, or `None` when it is not running this session.
static LIVE: Mutex<Option<Live>> = Mutex::new(None);

/// The payload of `overlay://coach`.
#[derive(Serialize, Clone)]
struct CoachPayload {
    /// The coach to draw, or `null` when nothing is shown.
    coach: Option<CoachView>,
}

/// The coach as the frontend draws it.
#[derive(Serialize, Clone)]
struct CoachView {
    /// 1 to 4.
    step: u8,
    /// Whether the overlay is in Living, which changes step 3's closing line.
    living: bool,
    /// The monitor to draw on, physical virtual-desktop px.
    monitor: (i32, i32, u32, u32),
    /// Echoed back with the layout report.
    generation: u64,
}

/// Decides at startup whether the tour runs this session. Call once, before the
/// first summon, so the summon's transition is the tour's first event.
pub fn init() {
    let loaded = config::path().map_or(config::Loaded::Missing, |path| config::load_from(&path));
    if let config::Loaded::Unreadable(reason) = &loaded {
        diagnostics::trouble(
            "settings: the settings file could not be read, so it is treated as absent",
            reason,
        );
    }
    #[cfg(debug_assertions)]
    let forced = crate::dev_harness::first_run_forced();
    #[cfg(not(debug_assertions))]
    let forced = false;
    if loaded.first_run_completed() && !forced {
        return;
    }
    diagnostics::note(if forced {
        "first run: the tour is forced by UPTAKE_DEV_FIRST_RUN"
    } else {
        "first run: starting the tour"
    });
    *lock(&LIVE) = Some(Live {
        tour: Tour::start(),
        surface: OverlayState::Hidden,
        monitor: None,
        generation: 0,
        layout: None,
    });
}

/// The overlay has settled in `state`. Called by `overlay::drive` after every
/// transition, including one to the state it was already in.
pub fn on_state(app: &AppHandle, state: OverlayState) {
    let event = match state {
        OverlayState::Placement => Some(TourEvent::EnteredPlacement),
        OverlayState::Living => Some(TourEvent::EnteredLiving),
        OverlayState::Hidden => None,
    };
    update(app, |live| {
        live.surface = state;
        event
    });
}

/// A drag has created an area of `kind`.
pub fn on_area_created(app: &AppHandle, kind: AreaType) {
    let typed = !matches!(kind, AreaType::Default);
    update(app, |_| Some(TourEvent::AreaCreated { typed }));
}

/// What a press at `point` lands on: `None` when it is not on the coach, and
/// `Some` with the button, if any, when it is.
pub fn hit(point: Point) -> Option<Option<CoachButton>> {
    lock(&LIVE)
        .as_ref()
        .and_then(|live| live.layout)
        .and_then(|layout| hit_in(layout, point))
}

/// Performs `pressed`, if the release landed on the button the press started
/// on: the press-and-release contract every other control here keeps.
pub fn activate(app: &AppHandle, pressed: CoachButton, release: Point) {
    if hit(release) != Some(Some(pressed)) {
        return;
    }
    let event = match pressed {
        CoachButton::Next => TourEvent::Next,
        CoachButton::Skip => TourEvent::Skip,
    };
    update(app, |_| Some(event));
}

/// IPC surface: where the frontend drew the coach for emit `generation`.
#[tauri::command]
pub fn overlay_report_coach(
    generation: u64,
    panel: (i32, i32, u32, u32),
    next: (i32, i32, u32, u32),
    skip: Option<(i32, i32, u32, u32)>,
) {
    let mut guard = lock(&LIVE);
    let Some(live) = guard.as_mut() else {
        return;
    };
    if let Some(layout) = accepted(live.generation, generation, live.monitor, panel, next, skip) {
        live.layout = Some(layout);
    }
}

/// Emits the coach to draw, or its absence. Also called by
/// `overlay_request_state`, for a WebView that mounted after the last emit.
pub fn emit(app: &AppHandle) {
    let payload = {
        let mut guard = lock(&LIVE);
        CoachPayload {
            coach: guard.as_mut().and_then(|live| view(app, live)),
        }
    };
    let _ = app.emit(COACH_EVENT, payload);
}

/// Applies whatever `event_for` returns to the tour, ends the tour if that
/// finished it, and emits the result.
fn update(app: &AppHandle, event_for: impl FnOnce(&mut Live) -> Option<TourEvent>) {
    let ended = {
        let mut guard = lock(&LIVE);
        let Some(live) = guard.as_mut() else {
            return;
        };
        let before = live.tour.step();
        let after = match event_for(live) {
            Some(event) => live.tour.advance(event),
            None => Some(live.tour),
        };
        if let Some(tour) = after {
            if tour.step() != before {
                live.monitor = None;
            }
            live.tour = tour;
            false
        } else {
            *guard = None;
            true
        }
    };
    if ended {
        record_completion();
    }
    emit(app);
}

/// The coach to draw for `live`, or `None` when it is not shown in the current
/// state. Bumps the generation either way, so no report from before this emit
/// can be accepted after it.
fn view(app: &AppHandle, live: &mut Live) -> Option<CoachView> {
    live.generation = live.generation.wrapping_add(1);
    live.layout = None;
    let shown = match live.surface {
        OverlayState::Placement => true,
        OverlayState::Living => live.tour.shows_in_living(),
        OverlayState::Hidden => false,
    };
    if !shown {
        return None;
    }
    let monitor = *live.monitor.get_or_insert_with(|| cursor_monitor(app));
    Some(CoachView {
        step: live.tour.step().number(),
        living: live.surface == OverlayState::Living,
        monitor: overlay::as_tuple(monitor),
        generation: live.generation,
    })
}

/// The monitor under the real cursor, which is where the user is looking.
fn cursor_monitor(app: &AppHandle) -> Rect {
    crate::placement::real_cursor(app).map_or_else(
        || {
            overlay::monitor_rects()
                .first()
                .copied()
                .unwrap_or(Rect::new(0, 0, 1, 1))
        },
        |point| overlay::monitor_bounds_at(app, point),
    )
}

/// Records completion off the calling thread. The last `Next` or a `Skip`
/// arrives inside the `WH_MOUSE_LL` callback, and time spent there counts
/// against `LowLevelHooksTimeout`: Windows removes a hook that overruns it,
/// silently, which is why every other slow action on that path is spawned too
/// (see `activate_menu_item`). File I/O is not bounded by anything this app
/// controls.
fn record_completion() {
    std::thread::spawn(|| match config::mark_first_run_completed() {
        Ok(()) => diagnostics::note("first run: the tour is over and recorded"),
        Err(error) => diagnostics::trouble(
            "first run: the tour is over but could not be recorded, so it will show again next launch",
            &error,
        ),
    });
}

/// What `point` lands on inside `layout`. Pure, so the precedence is testable.
fn hit_in(layout: Layout, point: Point) -> Option<Option<CoachButton>> {
    if layout.next.contains(point) {
        return Some(Some(CoachButton::Next));
    }
    if layout.skip.is_some_and(|skip| skip.contains(point)) {
        return Some(Some(CoachButton::Skip));
    }
    layout.panel.contains(point).then_some(None)
}

/// The layout a report describes, or `None` when it cannot be used.
///
/// Refused when it answers an older emit, or when no coach is shown. Otherwise
/// **clipped rather than trusted**: the panel to the monitor the coach is drawn
/// on, and each button to the panel. A report is WebView input, and a press on
/// the coach is swallowed by the mouse hook, in Living as well as Placement, so
/// an unclipped report is a rectangle of the desktop where clicks stop reaching
/// the user's apps. Clipping bounds that to the one monitor the coach belongs
/// on. A panel with no part on its monitor, or a `Next` with no part on the
/// panel, is refused outright; a `Skip` with no part on the panel is dropped.
///
/// Reaching it with a hostile rectangle needs script running in the overlay
/// WebView already, which is why this is hardening rather than a fix. Raised by
/// the security review of roadmap 1.18 as defence in depth.
fn accepted(
    current: u64,
    reported: u64,
    monitor: Option<Rect>,
    panel: (i32, i32, u32, u32),
    next: (i32, i32, u32, u32),
    skip: Option<(i32, i32, u32, u32)>,
) -> Option<Layout> {
    if current != reported {
        return None;
    }
    let rect = |(x, y, width, height): (i32, i32, u32, u32)| Rect::new(x, y, width, height);
    let panel = rect(panel).intersection(monitor?)?;
    Some(Layout {
        panel,
        next: rect(next).intersection(panel)?,
        skip: skip.and_then(|skip| rect(skip).intersection(panel)),
    })
}

/// Locks a mutex, treating poisoning as recoverable: the tour is plain data that
/// stays valid after a panic, and architecture §5 forbids `unwrap`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use uptake_core::geometry::{Point, Rect};

    use super::{CoachButton, CoachPayload, CoachView, Layout, accepted, hit_in};
    use crate::payload_keys::{assert_keys, assert_payload_coverage};

    fn layout(skip: Option<Rect>) -> Layout {
        Layout {
            panel: Rect::new(0, 0, 500, 200),
            next: Rect::new(400, 160, 80, 30),
            skip,
        }
    }

    #[test]
    fn a_press_on_a_button_is_that_button() {
        let layout = layout(Some(Rect::new(320, 160, 40, 30)));
        assert_eq!(
            hit_in(layout, Point::new(410, 170)),
            Some(Some(CoachButton::Next))
        );
        assert_eq!(
            hit_in(layout, Point::new(330, 170)),
            Some(Some(CoachButton::Skip))
        );
    }

    #[test]
    fn a_press_on_the_panel_between_buttons_is_the_coach_and_nothing_else() {
        // `Some(None)`: swallowed by the coach, and neither button. Reporting
        // `None` here would let the press fall through to whatever area or
        // empty overlay is underneath the panel.
        assert_eq!(hit_in(layout(None), Point::new(20, 20)), Some(None));
    }

    #[test]
    fn a_press_outside_the_panel_is_not_the_coach() {
        assert_eq!(hit_in(layout(None), Point::new(600, 20)), None);
    }

    #[test]
    fn the_last_step_has_no_skip_to_press() {
        // Where `Skip` would be on the other steps. With no skip rect this is
        // panel, not a button.
        assert_eq!(hit_in(layout(None), Point::new(330, 170)), Some(None));
    }

    #[test]
    fn a_report_for_an_older_emit_is_refused() {
        let monitor = Some(Rect::new(0, 0, 100, 100));
        let rect = (0, 0, 10, 10);
        assert_eq!(accepted(5, 4, monitor, rect, rect, None), None);
        assert!(accepted(5, 5, monitor, rect, rect, None).is_some());
    }

    #[test]
    fn a_report_while_no_coach_is_shown_is_refused() {
        let rect = (0, 0, 10, 10);
        assert_eq!(accepted(5, 5, None, rect, rect, None), None);
    }

    #[test]
    fn a_report_is_clipped_to_the_coach_monitor_and_its_buttons_to_the_panel() {
        // The hardening, drilled with the report the security review described:
        // a panel covering the whole four-monitor virtual desktop. Clipped, it
        // can swallow clicks on the coach's own monitor and nowhere else.
        let monitor = Rect::new(0, 0, 1920, 1080);
        let Some(layout) = accepted(
            1,
            1,
            Some(monitor),
            (-1080, -500, 10_000, 10_000),
            (400, 160, 80, 30),
            Some((-5000, 160, 40, 30)),
        ) else {
            panic!("a report overlapping its monitor is usable once clipped")
        };
        assert_eq!(layout.panel, monitor);
        assert_eq!(layout.next, Rect::new(400, 160, 80, 30));
        assert_eq!(
            layout.skip, None,
            "a Skip with no part on the panel is dropped"
        );
        assert_eq!(hit_in(layout, Point::new(2500, 500)), None);
    }

    #[test]
    fn a_report_with_nothing_on_its_monitor_or_a_next_off_the_panel_is_refused() {
        let monitor = Some(Rect::new(0, 0, 1920, 1080));
        let panel = (100, 100, 500, 200);
        assert_eq!(
            accepted(
                1,
                1,
                monitor,
                (5000, 5000, 10, 10),
                (5000, 5000, 5, 5),
                None
            ),
            None
        );
        assert_eq!(
            accepted(1, 1, monitor, panel, (900, 900, 80, 30), None),
            None
        );
    }

    #[test]
    fn every_payload_this_module_emits_keeps_the_keys_the_frontend_reads() {
        let view = CoachView {
            step: 1,
            living: false,
            monitor: (0, 0, 1920, 1080),
            generation: 3,
        };
        assert_keys(
            "CoachView",
            &view,
            &["step", "living", "monitor", "generation"],
        );
        assert_keys(
            "CoachPayload",
            &CoachPayload { coach: Some(view) },
            &["coach"],
        );
    }

    #[test]
    fn no_payload_in_this_module_escapes_the_key_table() {
        assert_payload_coverage(
            "first_run.rs",
            include_str!("first_run.rs"),
            &["CoachPayload", "CoachView"],
            &[],
        );
    }
}
