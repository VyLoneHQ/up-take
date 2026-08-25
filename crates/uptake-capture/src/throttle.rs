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
    // On an even count this is the upper-middle element rather than the mean of
    // the middle two. Deliberate: it biases `NotBinding` toward firing, which
    // is the conservative direction, because a run wrongly called binding is a
    // wrong point on the curve while one wrongly called not-binding is only a
    // re-run.
    let median = sorted[sorted.len() / 2].as_secs_f64() * 1000.0;

    if shortest < requested_ms * HONOURED_FLOOR {
        ThrottleVerdict::NotHonoured
    } else if median > requested_ms * BINDING_CEILING {
        ThrottleVerdict::NotBinding
    } else {
        ThrottleVerdict::HonouredAndBinding
    }
}

/// A silence this large a fraction of the cost window is a candidate for the
/// content having stopped partway.
const SILENCE_FRACTION: f64 = 0.25;

/// Below this rate over the window's ACTIVE portion, an UNTHROTTLED run is not
/// producing frames continuously, so its silences are its nature rather than a
/// change in conditions.
///
/// The number is a judgement and is named as one. A static desktop sits far
/// below it: `UT-F-41` measured WGC delivering **2 frames in 8.25 s**, which is
/// under 0.5 fps even counting only the active stretch. A paused video sits far
/// above it: the run this check exists for averaged **14.5 fps** across the
/// 40.3 s it was actually playing. Two orders of magnitude separate the two
/// cases, so the threshold is not load-bearing anywhere near its own value.
const MIN_CONTINUOUS_FPS: f64 = 2.0;

/// A throttled run counts as continuous at this fraction of the rate its own
/// throttle allows.
///
/// Half, so that a run delivering most of what it asked for is continuous and
/// one delivering a fraction of it is not.
const CONTINUOUS_FRACTION_OF_ASKED: f64 = 0.5;

/// Whether the captured content behaved the same way for the whole cost window.
///
/// Returns `false` only when the content was **demonstrably continuous and then
/// stopped** (or started late): one silence swallowed [`SILENCE_FRACTION`] of
/// the window, and outside that silence frames were arriving at a continuous
/// rate.
///
/// # Why a static desktop is NOT a failure, which a first version got wrong
///
/// Run 2 of this instrument's documented four-run matrix is *"default, static
/// desktop"*, described there as *"the row the decision turns on"*. A static
/// desktop produces almost no frames by definition, so its longest silence is
/// most of the window. **Refusing that run would refuse the most important
/// measurement the program takes**, and the first version of this check did
/// exactly that: pointed at a quiet monitor it voided a perfectly good run.
///
/// So silence alone cannot be the test. The test is silence *plus* evidence
/// that the content was capable of better, which is what the active-rate term
/// supplies.
///
/// # The case it does catch
///
/// The founder paused a video 40 s into a 60 s window. Every other line of the
/// report read clean, because the frames that did arrive were correctly spaced,
/// and he caught it by remembering what he had done. Active rate 14.5 fps,
/// silence 19.7 s of 60: refused.
///
/// # A CONTAMINATED static run is refused, and that is intended
///
/// A genuinely static desktop delivers no frames, has an active rate of zero,
/// and is held. But a static run during which a notification appears, or a
/// window flashes, delivers a short burst and then falls silent, and that is
/// refused. It should be: the run claims to measure a static desktop and the
/// window was not homogeneous. Observed once in six runs against a quiet
/// monitor while verifying this change.
///
/// The asymmetry is deliberate. A spurious refusal costs a re-run; a silent
/// accept puts a wrong number into a backlog row that arguments then rest on.
///
/// # What it still does not catch
///
/// Content that slows without stopping, and content that stops for less than
/// [`SILENCE_FRACTION`] of the window. Both change the measurement and neither
/// leaves a silence long enough to see. Stated rather than left to be found.
///
/// # There is deliberately no explicit "static desktop" branch
///
/// There was one, and a mutation proved it could not fail: a run with no frames
/// has an active rate of zero, which is below [`MIN_CONTINUOUS_FPS`] by any
/// value it could take, so the general term already answers the static case.
/// An untestable branch was removed rather than left in to look reassuring.
#[must_use]
pub fn conditions_held(
    longest_silence: Duration,
    window: Duration,
    frames: usize,
    asked: Option<Duration>,
) -> bool {
    if window.is_zero() {
        return false;
    }
    if longest_silence.as_secs_f64() < window.as_secs_f64() * SILENCE_FRACTION {
        return true;
    }
    let active = window.as_secs_f64() - longest_silence.as_secs_f64();
    if active <= 0.0 {
        return true;
    }
    // What counts as "continuous" has to be measured against what this run
    // could ACHIEVE, not against a fixed rate. A throttle caps the rate by
    // design, so a fixed 2 fps floor silently switches this guard off for any
    // interval slower than about 500 ms: at `--min-update-interval-ms 600` the
    // run tops out near 1.67 fps, lands under the floor, and a genuine freeze
    // is waved through as "not continuous content". Found by `PR #67` round 2.
    // No recorded run uses an interval that slow, so nothing already measured
    // is implicated, but the guard was blind exactly where a slower run would
    // have needed it.
    //
    // A zero interval needs no special case and deliberately does not have one:
    // `1.0 / 0.0` is infinity in IEEE arithmetic, infinity times a half is
    // infinity, and `min` clamps it straight back to the absolute floor. A
    // guard for it was written, and a mutation proved it could not fail. The
    // example refuses `--min-update-interval-ms 0` at parse time in any case.
    let floor = match asked {
        Some(interval) => {
            let achievable = 1.0 / interval.as_secs_f64();
            (achievable * CONTINUOUS_FRACTION_OF_ASKED).min(MIN_CONTINUOUS_FPS)
        }
        None => MIN_CONTINUOUS_FPS,
    };
    (frames as f64 / active) < floor
}

/// The longest stretch of the cost window during which no frame arrived.
///
/// `arrival_offsets` are measured from the start of the window and must be
/// ascending. The answer considers **three** kinds of silence and the first and
/// last are the ones that get forgotten:
///
/// - from the start of the window to the first frame,
/// - between consecutive frames,
/// - from the last frame to the end of the window.
///
/// # Why this is a function and not two lines at the call site
///
/// It was two lines at the call site, and they were wrong. The first version
/// built its gap list from `windows(2)` over the arrivals alone, which sees
/// only the middle kind. A run whose content froze for the first 58 seconds of
/// a 60 second window and then delivered four frames 500 ms apart reported a
/// longest silence of **500 ms** and passed as healthy.
///
/// That is not a hypothetical: the run this whole check was written for had its
/// video paused so that the freeze ran to the END of the window, which the
/// author's own wiring could not see either. An independent review of `PR #67`
/// found it. The lesson is the one this project keeps re-learning: the guard
/// was correct in isolation and was fed the wrong data, and the guard's unit
/// tests could not see the wiring because the wiring lived in an example, which
/// `cargo test` builds without running.
///
/// With no arrivals at all the whole window was silent, which is what is
/// returned.
#[must_use]
pub fn longest_silence(window: Duration, arrival_offsets: &[Duration]) -> Duration {
    let Some(first) = arrival_offsets.first() else {
        return window;
    };
    let leading = *first;
    let between = arrival_offsets
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .max()
        .unwrap_or_default();
    let trailing = arrival_offsets
        .last()
        .map_or(window, |last| window.saturating_sub(*last));
    leading.max(between).max(trailing)
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
    fn no_arrivals_means_the_whole_window_was_silent() {
        assert_eq!(
            longest_silence(Duration::from_secs(60), &[]),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn a_freeze_at_the_start_of_the_window_is_seen() {
        // The review's scenario: frozen for 58 s, then four frames 500 ms apart.
        // The wiring this replaces reported 500 ms and passed as healthy.
        let arrivals = ms(&[58_000, 58_500, 59_000, 59_500]);
        assert_eq!(
            longest_silence(Duration::from_secs(60), &arrivals),
            Duration::from_millis(58_000)
        );
    }

    #[test]
    fn a_freeze_at_the_end_of_the_window_is_seen() {
        // The run this check was written for: the founder paused a video and
        // the silence ran to the end of the window.
        let arrivals = ms(&[0, 10_000, 20_000, 30_000, 40_269]);
        assert_eq!(
            longest_silence(Duration::from_secs(60), &arrivals),
            Duration::from_millis(19_731)
        );
    }

    #[test]
    fn a_freeze_in_the_middle_is_still_seen() {
        let arrivals = ms(&[100, 30_100, 30_200, 59_900]);
        assert_eq!(
            longest_silence(Duration::from_secs(60), &arrivals),
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn the_largest_of_the_three_kinds_wins() {
        // Leading 5 s, middle 9 s, trailing 7 s.
        let arrivals = ms(&[5_000, 14_000, 23_000]);
        assert_eq!(
            longest_silence(Duration::from_secs(30), &arrivals),
            Duration::from_secs(9)
        );
    }

    #[test]
    fn a_steady_run_is_silent_only_between_its_frames() {
        let arrivals = ms(&[100, 200, 300, 400]);
        // All three terms tie at 100 ms here, which is the point: whichever
        // one is taken the answer is the same.
        assert_eq!(
            longest_silence(Duration::from_millis(500), &arrivals),
            Duration::from_millis(100)
        );
    }

    #[test]
    fn the_leading_freeze_reaches_conditions_held() {
        // End to end: the composition that was broken, now going red.
        let arrivals = ms(&[58_000, 58_500, 59_000, 59_500]);
        let window = Duration::from_secs(60);
        assert!(!conditions_held(
            longest_silence(window, &arrivals),
            window,
            arrivals.len(),
            None
        ));
    }

    #[test]
    fn a_steady_run_held_its_conditions() {
        assert!(conditions_held(
            Duration::from_millis(114),
            Duration::from_secs(60),
            584,
            None
        ));
    }

    #[test]
    fn a_paused_video_did_not_hold_its_conditions() {
        // The observed run: 584 frames, 19.7 s of silence inside 60 s, so the
        // 40.3 s it was actually playing averaged 14.5 fps.
        assert!(!conditions_held(
            Duration::from_millis(19_731),
            Duration::from_secs(60),
            584,
            None
        ));
    }

    #[test]
    fn a_static_desktop_is_not_a_changed_run() {
        // Run 2 of this program's own four-run matrix. Voiding it would refuse
        // the most important measurement the instrument takes, and the first
        // version of this check did exactly that.
        assert!(conditions_held(
            Duration::from_secs(60),
            Duration::from_secs(60),
            0,
            None
        ));
    }

    #[test]
    fn a_nearly_static_desktop_is_not_a_changed_run() {
        // UT-F-41: WGC delivered 2 frames in 8.25 s on a static desktop. The
        // silence is most of the window and the content was never continuous.
        assert!(conditions_held(
            Duration::from_millis(4_150),
            Duration::from_millis(8_250),
            2,
            None
        ));
    }

    #[test]
    fn a_late_start_on_busy_content_is_a_changed_run() {
        // The review's scenario: frozen 58 s, then four frames 500 ms apart.
        // Active portion is 2 s carrying 4 frames, which is 2 fps.
        assert!(!conditions_held(
            Duration::from_millis(58_000),
            Duration::from_secs(60),
            4,
            None
        ));
    }

    #[test]
    fn a_long_silence_alone_is_not_enough() {
        // Same silence as the case above, but only ONE frame arrived, so
        // nothing demonstrates the content could do better.
        assert!(conditions_held(
            Duration::from_millis(58_000),
            Duration::from_secs(60),
            1,
            None
        ));
    }

    #[test]
    fn a_slow_throttle_does_not_switch_the_guard_off() {
        // `PR #67` round 2's scenario. --min-update-interval-ms 600 tops out
        // near 1.67 fps by design, so a fixed 2 fps floor would call this
        // content "not continuous" and wave the freeze through. Measured
        // against what the run could ACHIEVE, 1.68 fps is most of 1.67 and the
        // 20 s freeze is refused.
        assert!(!conditions_held(
            Duration::from_secs(20),
            Duration::from_secs(60),
            67,
            Some(Duration::from_millis(600))
        ));
    }

    #[test]
    fn a_slow_throttle_delivering_almost_nothing_is_still_static() {
        // Same slow throttle, but the content produced a handful of frames in
        // the active window rather than the ~67 the interval allows. Nothing
        // demonstrates continuity, so the silence is its nature.
        assert!(conditions_held(
            Duration::from_secs(20),
            Duration::from_secs(60),
            5,
            Some(Duration::from_millis(600))
        ));
    }

    #[test]
    fn a_fast_throttle_keeps_the_absolute_floor() {
        // At 100 ms the achievable rate is 10 fps and half of that is 5, which
        // is above MIN_CONTINUOUS_FPS, so the floor stays at 2 rather than
        // rising with the throttle and refusing healthy slow content.
        assert!(conditions_held(
            Duration::from_millis(4_150),
            Duration::from_millis(8_250),
            2,
            Some(Duration::from_millis(100))
        ));
    }

    #[test]
    fn the_paused_video_is_still_refused_with_its_throttle_known() {
        assert!(!conditions_held(
            Duration::from_millis(19_731),
            Duration::from_secs(60),
            584,
            Some(Duration::from_millis(100))
        ));
    }

    #[test]
    fn a_zero_interval_falls_back_to_the_absolute_floor() {
        assert!(conditions_held(
            Duration::from_millis(4_150),
            Duration::from_millis(8_250),
            2,
            Some(Duration::ZERO)
        ));
    }

    #[test]
    fn a_contaminated_static_run_is_refused() {
        // Something appeared on an otherwise quiet screen: 40 frames in the
        // first 2 s of a 10 s window, then nothing. The run claims to measure a
        // static desktop and did not. Re-run it rather than record it.
        assert!(!conditions_held(
            Duration::from_secs(8),
            Duration::from_secs(10),
            40,
            None
        ));
    }

    #[test]
    fn a_zero_window_holds_nothing() {
        assert!(!conditions_held(Duration::ZERO, Duration::ZERO, 0, None));
    }
}
