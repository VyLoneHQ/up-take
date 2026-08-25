//! Judging whether a frame-rate throttle did anything, from the arrival times
//! it produced.
//!
//! # Why this is a module and not ten lines inside the example
//!
//! `warm_session` grew a `--min-update-interval-ms` flag on 2026-08-25 so that
//! `I-41`'s parked question could be measured rather than modelled. The flag
//! needs three refusals, and refusals are exactly what this project has just
//! been bitten by shipping undrilled: `I-301` is open because two of them
//! reached `main` in `PR #62` with no test behind either.
//!
//! An example is the wrong home for the drill. CI runs `cargo test
//! --all-features`, which **builds** examples but does not run `#[test]`
//! functions inside them, so a drill written there is a drill nobody runs.
//! That is `I-11`'s shape: a check whose silence is indistinguishable from
//! working. So the judgement lives here, where `cargo test` reaches it, and the
//! example is left holding only the printing.
//!
//! # The distinction the first run of the instrument taught
//!
//! Asking WGC for a minimum interval can fail in three different ways, and two
//! of them look like success:
//!
//! - **Not honoured.** WGC ignores the request and frames keep arriving at the
//!   untuned rate. The cost measured is the untuned cost wearing a throttled
//!   label, and it is the CHEAP-looking wrong answer.
//! - **Not binding.** The floor is respected but nothing ever reaches it,
//!   because the screen was changing more slowly than the throttle allows. The
//!   run measures the content's frame rate, not the throttle's. Observed on the
//!   very first use: 55 ms and 28 ms were both asked for, both honoured, and
//!   both returned about 13 fps, because the video only produced that many
//!   distinct frames. The two runs looked like two points on a curve and were
//!   one point twice.
//! - **Unverifiable.** Too few frames arrived to say anything either way.
//!
//! Only the fourth case, honoured *and* binding, is a point on the curve.

use std::time::Duration;

/// What a run's observed inter-frame gaps say about the throttle it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleVerdict {
    /// Fewer than two gaps: nothing can be concluded.
    Unverifiable {
        /// How many gaps were actually observed.
        gaps: usize,
    },
    /// A frame arrived materially inside the floor that was requested.
    NotHonoured,
    /// The floor held, but the content never reached it, so the content set the
    /// rate. The run is a valid measurement of something else.
    NotBinding,
    /// The floor held and the content was fast enough to be limited by it.
    HonouredAndBinding,
}

/// A frame arriving closer than this fraction of the requested interval means
/// the request was not honoured. Slack because the clock and the compositor
/// disagree by a millisecond or two: the first real run asked 28 ms and saw a
/// shortest gap of 27.
const HONOURED_FLOOR: f64 = 0.8;

/// A median gap this many times the requested interval means nothing was ever
/// limited by the request.
const BINDING_CEILING: f64 = 1.5;

/// Judge a throttled run from the gaps between the frames it received.
///
/// `gaps` need not be sorted.
#[must_use]
pub fn verdict(requested: Duration, gaps: &[Duration]) -> ThrottleVerdict {
    if gaps.len() < 2 {
        return ThrottleVerdict::Unverifiable { gaps: gaps.len() };
    }
    let requested_ms = requested.as_secs_f64() * 1000.0;
    let mut sorted: Vec<Duration> = gaps.to_vec();
    sorted.sort_unstable();
    let shortest = sorted[0].as_secs_f64() * 1000.0;
    let median = sorted[sorted.len() / 2].as_secs_f64() * 1000.0;

    if shortest < requested_ms * HONOURED_FLOOR {
        ThrottleVerdict::NotHonoured
    } else if median > requested_ms * BINDING_CEILING {
        ThrottleVerdict::NotBinding
    } else {
        ThrottleVerdict::HonouredAndBinding
    }
}

/// A silence this large a fraction of the cost window means the thing being
/// captured stopped changing partway through, so the two halves of the run were
/// not the same experiment.
const SILENCE_FRACTION: f64 = 0.25;

/// Whether the captured content kept changing for the whole cost window.
///
/// Returns `false` when one silence swallowed a quarter of the window or more.
/// Added after the instrument's first session, where a paused video produced
/// 19.7 s of silence inside a 60 s window and every other line of the report
/// still read as a clean result: the frames that did arrive were spaced
/// correctly, so the throttle verdict above said honoured and binding. The
/// operator caught it by remembering what he had done, which is not a check.
#[must_use]
pub fn conditions_held(longest_silence: Duration, window: Duration) -> bool {
    if window.is_zero() {
        return false;
    }
    longest_silence.as_secs_f64() < window.as_secs_f64() * SILENCE_FRACTION
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(values: &[u64]) -> Vec<Duration> {
        values.iter().copied().map(Duration::from_millis).collect()
    }

    #[test]
    fn no_gaps_is_unverifiable() {
        assert_eq!(
            verdict(Duration::from_millis(55), &[]),
            ThrottleVerdict::Unverifiable { gaps: 0 }
        );
    }

    #[test]
    fn one_gap_is_unverifiable() {
        assert_eq!(
            verdict(Duration::from_millis(55), &ms(&[55])),
            ThrottleVerdict::Unverifiable { gaps: 1 }
        );
    }

    #[test]
    fn a_frame_inside_the_floor_is_not_honoured() {
        // Asked 55 ms, one frame arrived after 20.
        assert_eq!(
            verdict(Duration::from_millis(55), &ms(&[20, 60, 58, 57])),
            ThrottleVerdict::NotHonoured
        );
    }

    #[test]
    fn the_untuned_rate_surviving_the_request_is_not_honoured() {
        // The failure that looks cheap: ~60 fps throughout despite asking 55 ms.
        assert_eq!(
            verdict(Duration::from_millis(55), &ms(&[16, 17, 16, 17, 16])),
            ThrottleVerdict::NotHonoured
        );
    }

    #[test]
    fn slack_below_the_floor_is_still_honoured() {
        // The real 28 ms run saw a shortest gap of 27: clock slack, not a breach.
        assert_eq!(
            verdict(Duration::from_millis(28), &ms(&[27, 31, 30, 29])),
            ThrottleVerdict::HonouredAndBinding
        );
    }

    #[test]
    fn a_floor_nothing_reaches_is_not_binding() {
        // The real 28 ms run against ~15 fps content: honoured, and pointless.
        assert_eq!(
            verdict(Duration::from_millis(28), &ms(&[65, 63, 66, 64])),
            ThrottleVerdict::NotBinding
        );
    }

    #[test]
    fn a_binding_floor_is_reported_as_such() {
        // The real 100 ms run: median 102, shortest 100.
        assert_eq!(
            verdict(Duration::from_millis(100), &ms(&[100, 102, 104, 101])),
            ThrottleVerdict::HonouredAndBinding
        );
    }

    #[test]
    fn not_honoured_outranks_not_binding() {
        // A breach plus a slow median is still a void run, not a valid one.
        assert_eq!(
            verdict(Duration::from_millis(28), &ms(&[5, 200, 300, 400])),
            ThrottleVerdict::NotHonoured
        );
    }

    #[test]
    fn touching_the_floor_once_is_not_binding() {
        // The distinction the median exists to draw, and the one a mutation
        // survived until this test was written: ONE gap sits exactly on the
        // requested floor while every other is three times it. Judged by the
        // shortest gap this reads as binding; judged by the median it is the
        // content setting the rate, which is what it is. This is the shape of
        // the real 28 ms run, where a fast burst sat inside slow video.
        assert_eq!(
            verdict(Duration::from_millis(28), &ms(&[28, 90, 95, 100])),
            ThrottleVerdict::NotBinding
        );
    }

    #[test]
    fn gaps_need_not_be_sorted() {
        let ascending = verdict(Duration::from_millis(100), &ms(&[100, 101, 102, 104]));
        let shuffled = verdict(Duration::from_millis(100), &ms(&[104, 100, 102, 101]));
        assert_eq!(ascending, shuffled);
    }

    #[test]
    fn a_steady_run_held_its_conditions() {
        assert!(conditions_held(
            Duration::from_millis(114),
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn a_paused_video_did_not_hold_its_conditions() {
        // The observed run: 19.7 s of silence inside a 60 s window.
        assert!(!conditions_held(
            Duration::from_millis(19_731),
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn the_boundary_is_a_quarter_of_the_window() {
        assert!(conditions_held(
            Duration::from_millis(14_999),
            Duration::from_secs(60)
        ));
        assert!(!conditions_held(
            Duration::from_secs(15),
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn a_zero_window_holds_nothing() {
        assert!(!conditions_held(Duration::ZERO, Duration::ZERO));
    }
}
