//! Hardware-bound integration tests: they drive real WGC sessions and need a
//! real, interactive desktop, which CI runners do not have. All `#[ignore]`d;
//! run on the rig with:
//!
//! ```text
//! cargo test -p uptake-capture --test hardware -- --ignored --nocapture
//! ```
//!
//! quality-bars.md §2 scopes this crate to "thin integration tests only" for
//! exactly this reason — the pure planning/compositing logic is unit-tested,
//! the WGC path is verified here and via `examples/grab.rs` on the rig.

#![cfg(windows)]
// `expect` alongside `unwrap` for the same reason architecture §5 permits both
// inside tests: a failed setup should abort loudly. It earns its place here
// specifically — every `expect` message in this file names the *precondition*
// that was not met, so a rig run that cannot establish one says so instead of
// reporting it as the invariant failing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use uptake_core::geometry::{Rect, Size};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Serialises the tests that drive `warm`'s process-global `SESSIONS`.
///
/// **libtest runs tests in one process, concurrently.** Two tests calling
/// `warm::start`/`warm::stop` therefore share one set of sessions: one test's
/// `stop` tears down the sessions the other is mid-assertion on, and one's
/// `start` can satisfy the other's warm-up wait. The failure would be
/// intermittent and would read as a defect in the feature.
///
/// This became reachable on 2026-07-30, when a second warm rig test was added;
/// with only one there was nothing to collide with. It is `F-33`'s family — a
/// test reaching another through process-global state — with the difference
/// that `SESSIONS` is the production design rather than a testing seam, so the
/// fix is to serialise rather than to parameterise.
///
/// Taken with `unwrap_or_else(PoisonError::into_inner)` so one failing test
/// does not cascade into a poisoned-lock panic in the next, which would report
/// the wrong test as broken.
static WARM_SESSIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Physical coordinates require per-monitor-DPI awareness (see the crate
/// docs). Idempotent: the second call in the same process fails harmlessly.
fn ensure_dpi_aware() {
    // SAFETY: no memory-safety preconditions.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[test]
#[ignore = "needs a real desktop: drives a live WGC session"]
fn captures_a_small_region_at_the_primary_origin() {
    ensure_dpi_aware();
    // The primary monitor's top-left is (0, 0) by definition, so this region
    // exists on every machine.
    let captured = uptake_capture::capture_region(Rect::new(0, 0, 64, 48)).unwrap();
    assert_eq!(captured.rect, Rect::new(0, 0, 64, 48));
    assert_eq!(captured.bitmap.size(), Size::new(64, 48));
    // A real desktop frame is opaque — WGC reports full alpha. All-zero
    // pixels would mean we composited nothing and called it success.
    assert!(
        captured
            .bitmap
            .pixels()
            .chunks_exact(4)
            .all(|px| px[3] == 0xFF)
    );
}

#[test]
#[ignore = "needs a real desktop: drives a live WGC session"]
fn off_screen_and_empty_regions_error_without_capturing() {
    ensure_dpi_aware();
    assert!(matches!(
        uptake_capture::capture_region(Rect::new(1_000_000, 1_000_000, 10, 10)),
        Err(uptake_capture::CaptureError::Offscreen)
    ));
    assert!(matches!(
        uptake_capture::capture_region(Rect::new(0, 0, 0, 10)),
        Err(uptake_capture::CaptureError::EmptyRegion)
    ));
}

/// The GDI fallback (task 1.8), driven directly since it does not fire on its
/// own on a healthy desktop. Proves the DIB is captured and the BGRA→RGBA-opaque
/// conversion is right end to end: a real desktop frame comes back fully opaque,
/// and an all-zero result would mean nothing was blitted.
///
/// Selects the path by argument, not by an environment variable: libtest runs
/// these tests concurrently, so a process-global switch set here would be read
/// by `captures_a_small_region_at_the_primary_origin` running beside it, which
/// would then silently capture via GDI and still pass — leaving the crate's
/// only live-WGC assertion testing the wrong path.
#[test]
#[ignore = "needs a real desktop: drives a real GDI screen BitBlt"]
fn the_forced_gdi_fallback_captures_an_opaque_frame() {
    ensure_dpi_aware();
    let captured = uptake_capture::capture_region_via_gdi(Rect::new(0, 0, 64, 48)).unwrap();

    assert_eq!(captured.rect, Rect::new(0, 0, 64, 48));
    assert_eq!(captured.bitmap.size(), Size::new(64, 48));
    assert!(
        captured
            .bitmap
            .pixels()
            .chunks_exact(4)
            .all(|px| px[3] == 0xFF)
    );
}

/// **1B exit-gate row 2, the capture half.** Two *real* captures of one
/// rectangle, reached by the two routes the app actually uses, must agree
/// byte-for-byte.
///
/// The transformation half is unit-tested with a synthetic frame on both sides
/// (`freeze::frozen_and_held_crops_are_byte_identical`), and since task 1.9d put
/// both frame sources on `RgbaBitmap::crop_screen` it is true by construction.
/// **That test cannot see this one's failure mode.** The two routes differ
/// *before* the crop: a live capture asks `plan` for a small region and reads
/// the source at that offset, while the held and frozen paths capture a large
/// region and subtract their way back to it. Those are two separate pieces of
/// coordinate arithmetic, and nothing has ever compared their output on real
/// pixels.
///
/// # Both preconditions are established rather than assumed
///
/// Byte equality over real screen pixels means nothing on its own, and this
/// test needs two separate things to be true first. Each is checked, and each
/// fails with a message saying it is the precondition rather than the invariant
/// — a test that cannot tell "the crop is wrong" from "a clock ticked" reports
/// the second as the first.
///
/// 1. **The region held still**, shown by re-capturing the surrounding frame
///    *after* the direct capture and requiring the same crop back. Checking two
///    frames taken before it would leave the window that matters unchecked.
/// 2. **The region has texture**, established by walking the raw pixel buffer
///    for a candidate whose rows differ from the rows below them. Over flat
///    wallpaper every crop equals every other, so the assertion below passes for
///    a crop reading entirely the wrong place — observed, not theorised. The
///    walk deliberately avoids `crop_screen`: using the function under test to
///    decide whether the function under test can be tested is how the first two
///    attempts at this both passed under a one-row mutation.
///
/// UT-F-40 is the reason for the shape: a test that constructs its own
/// precondition cannot discover that the precondition never held.
#[test]
#[ignore = "needs a real desktop and a static, textured region: drives three live WGC sessions"]
fn a_cropped_capture_and_a_direct_capture_agree_byte_for_byte() {
    ensure_dpi_aware();
    // Deliberately **not** at the origin: the top-left of a desktop is where
    // notifications, taskbar chrome and window furniture live, so something
    // there is nearly always mid-animation. The first cut of this test used
    // (0, 0), its staticness check fired every run at up to 58% of bytes, and
    // that read as "WGC is non-deterministic" — which was wrong. Measured
    // 2026-07-29 away from the origin: two WGC captures, two GDI captures, and
    // WGC against GDI are all byte-identical.
    let surrounding = Rect::new(640, 480, 640, 480);
    let before = uptake_capture::capture_region(surrounding).unwrap();
    let settled = uptake_capture::capture_region(surrounding).unwrap();

    // The rectangle is **found, not hard-coded.** A fixed choice depends on what
    // the desktop happens to be showing: at (800, 600) this rig showed a window
    // one minute and flat wallpaper the next, and over flat wallpaper every crop
    // equals every other, so a deliberate one-row error in `crop_screen` left
    // the comparison below green. Scanning for a sub-rectangle whose pixels
    // change when the crop moves one row is the test establishing its own
    // discriminating power instead of assuming it — backlog I-1 / UT-F-44's
    // lesson, which this test has now repeated twice.
    let wanted = textured_subrect(&before, &settled, surrounding).expect(
        "precondition failed, not the invariant: no region found that is both \
         textured and unchanged across two captures. Every textured part of this \
         screen is animating — Windows' mica and acrylic materials shimmer by a \
         few levels continuously — so no comparison here could mean anything. \
         Re-run with a still, detailed window in the scanned area.",
    );
    let cropped = before
        .bitmap
        .crop_screen(before.rect.origin, wanted)
        .expect("the found rectangle lies wholly inside the frame it came from");

    let direct = uptake_capture::capture_region(wanted).unwrap();

    // Sandwiched: the frame is re-captured *after* the direct capture, and the
    // same crop must come back. That is what shows the screen held still across
    // the direct capture in between — comparing two captures taken before it
    // would leave the window that actually matters unchecked (UT-F-40: a test
    // that constructs its own precondition cannot discover it never held).
    let after = uptake_capture::capture_region(surrounding).unwrap();
    let cropped_after = after
        .bitmap
        .crop_screen(after.rect.origin, wanted)
        .expect("the found rectangle lies wholly inside the frame it came from");
    assert_eq!(
        differing_bytes(cropped.pixels(), cropped_after.pixels()),
        None,
        "precondition failed, not the invariant: the region changed while the \
         direct capture was taken, so byte equality below would prove nothing. \
         Re-run with that part of the screen holding still."
    );

    assert_eq!(cropped.size(), direct.bitmap.size());
    assert_eq!(
        differing_bytes(cropped.pixels(), direct.bitmap.pixels()),
        None,
        "a capture cropped out of a larger frame must equal a direct capture of \
         the same rectangle — 1B exit-gate row 2"
    );
}

/// A 64×48 rectangle inside `frame` whose contents change when the crop moves
/// one row, or `None` if the whole frame is that uniform.
///
/// # It reads the raw buffer, and that is the whole point
///
/// The obvious implementation calls [`RgbaBitmap::crop_screen`] twice and
/// compares — and it is wrong, because `crop_screen` is the function the caller
/// is about to test. With a deliberate one-row error injected into it, both
/// crops shift together, the comparison still finds a difference, and the
/// caller's assertion then passes over a crop reading the wrong place.
/// **Observed on the rig 2026-07-29**, after which the region a fixed choice had
/// hidden turned out to be vertically uniform for exactly as many rows as the
/// check looked at.
///
/// Walking the pixel buffer by hand keeps the selection independent of
/// everything under test. The condition is exactly the failure mode being
/// guarded against: some row inside the candidate differs from the row below it,
/// which is what makes a one-row displacement visible.
fn textured_subrect(
    frame: &uptake_capture::CapturedRegion,
    settled: &uptake_capture::CapturedRegion,
    bounds: Rect,
) -> Option<Rect> {
    let size = Size::new(64, 48);
    let frame_size = frame.bitmap.size();
    let stride = frame_size.width as usize * 4;
    let pixels = frame.bitmap.pixels();
    let settled_pixels = settled.bitmap.pixels();
    let span = size.width as usize * 4;
    if settled.bitmap.size() != frame_size {
        return None;
    }

    for row in 0..8_i32 {
        for column in 0..8_i32 {
            let candidate = Rect::new(
                bounds.origin.x + 32 + column * 64,
                bounds.origin.y + 32 + row * 48,
                size.width,
                size.height,
            );
            // Frame-local, via the frame's *reported* origin: it clamps to the
            // virtual desktop, so the requested origin is not always the one it
            // captured.
            let local_x = (candidate.origin.x - frame.rect.origin.x) as usize;
            let local_y = (candidate.origin.y - frame.rect.origin.y) as usize;
            // One row of slack, because the comparison below reads the row under
            // the candidate's last.
            if local_x + size.width as usize > frame_size.width as usize
                || local_y + size.height as usize + 1 > frame_size.height as usize
            {
                continue;
            }
            let rows = || {
                (0..size.height as usize)
                    .map(move |offset| (local_y + offset) * stride + local_x * 4)
            };
            // Textured: a one-row displacement is visible somewhere in it.
            let textured = rows().any(|here| {
                let below = here + stride;
                pixels[here..here + span] != pixels[below..below + span]
            });
            // ...and still, across two captures. The two conditions pull against
            // each other on a modern desktop — the detailed regions are windows,
            // and Windows' mica and acrylic materials shimmer by a few levels
            // continuously — so a candidate has to be checked for both or the
            // test trades one precondition failure for the other.
            let still =
                rows().all(|here| pixels[here..here + span] == settled_pixels[here..here + span]);
            if textured && still {
                return Some(candidate);
            }
        }
    }
    None
}

/// How two pixel buffers differ, or `None` when they are identical.
///
/// A summary rather than the buffers themselves: `assert_eq!` on two
/// full-monitor pixel slices prints megabytes of numbers, which is
/// indistinguishable from having no diagnostic at all. The first differing
/// offset and the count are what separate "one small element animated" from
/// "the whole crop is at the wrong offset".
fn differing_bytes(left: &[u8], right: &[u8]) -> Option<String> {
    if left.len() != right.len() {
        return Some(format!("lengths differ: {} vs {}", left.len(), right.len()));
    }
    let differing = left.iter().zip(right).filter(|(a, b)| a != b).count();
    if differing == 0 {
        return None;
    }
    let first = left
        .iter()
        .zip(right)
        .position(|(a, b)| a != b)
        .unwrap_or_default();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a percentage for a human reading a failure message"
    )]
    let share = differing as f64 * 100.0 / left.len() as f64;
    // The magnitude is what separates the two explanations: a handful of levels
    // is capture noise or dithering, and a byte-equality criterion built on top
    // of it is unachievable in principle. Large deltas mean the content moved,
    // or the crop is reading the wrong place.
    let worst = left
        .iter()
        .zip(right)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or_default();
    Some(format!(
        "{differing} of {} bytes differ ({share:.3}%), worst delta {worst} levels, \
         first at byte {first} (pixel {})",
        left.len(),
        first / 4
    ))
}

/// The warm path is a **fifth** source of a Screenshot's pixels, so 1B's
/// exit-gate row 2 — every path producing identical results for the same
/// rectangle — has to cover it.
///
/// # What this can and cannot assert
///
/// A warm readback and a fresh capture are taken at **different instants** from
/// **different sessions**, so byte equality is only meaningful over a part of
/// the screen that is genuinely still. That is the same constraint
/// `a_cropped_capture_and_a_direct_capture_agree_byte_for_byte` works under, and
/// this test borrows its machinery: find a sub-rectangle that is both textured
/// and unchanged across two captures, then sandwich the comparison so the region
/// is shown to have held still *across* it rather than merely before it.
///
/// **The staticness failures are reported as preconditions, not as the
/// invariant.** A rig run where the scanned area is animating proves nothing
/// about pixel identity, and saying so is the difference between this test and
/// one that passes for the wrong reason (`UT-F-40`).
#[test]
#[ignore = "needs a real desktop and a static, textured region: holds warm WGC sessions"]
fn a_warm_readback_and_a_fresh_capture_agree_byte_for_byte() {
    // Held for the whole test: `warm`'s sessions are process-global, so a
    // sibling test's `stop` would tear down what this one is asserting on.
    let _serial = WARM_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure_dpi_aware();

    let sessions = uptake_capture::warm::start(uptake_capture::warm::Scope::AllMonitors);
    assert!(sessions > 0, "no monitors were enumerated to warm");

    // The ~330 ms warm-up measured on the rig, with room. Polled rather than
    // slept-through so the test reports how the sessions actually came up.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline && !uptake_capture::warm::status().fully_warm() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let status = uptake_capture::warm::status();
    assert!(
        status.fully_warm(),
        "precondition failed, not the invariant: only {}/{} sessions became warm \
         within 3 s, so there is nothing to compare",
        status.warm,
        status.sessions
    );

    // Away from the origin, for the reason UT-F-45 records: the top-left of a
    // desktop is where notifications and window chrome live.
    let surrounding = Rect::new(640, 480, 640, 480);
    let before = uptake_capture::capture_region(surrounding).unwrap();
    let settled = uptake_capture::capture_region(surrounding).unwrap();
    let wanted = textured_subrect(&before, &settled, surrounding).expect(
        "precondition failed, not the invariant: no region found that is both \
         textured and unchanged across two captures. Re-run with a still, \
         detailed window in the scanned area.",
    );

    // The whole monitor holding that rectangle, read back off the warm session.
    let warm = uptake_capture::warm::capture_monitor(surrounding)
        .expect("a warm session covers the monitor the scanned region is on");
    let warm_crop = warm
        .bitmap
        .crop_screen(warm.rect.origin, wanted)
        .expect("the found rectangle lies inside the monitor the warm frame covers");

    let direct = uptake_capture::capture_region(wanted).unwrap();

    // Sandwiched, exactly as the cold-path test is: re-read the warm session
    // *after* the direct capture and require the same pixels, which is what
    // shows the region held still across the comparison rather than only before
    // it. A warm session updates on every compositor frame, so this is a real
    // check and not a re-read of a frozen buffer.
    let warm_after =
        uptake_capture::warm::capture_monitor(surrounding).expect("the warm session is still held");
    let warm_crop_after = warm_after
        .bitmap
        .crop_screen(warm_after.rect.origin, wanted)
        .expect("the found rectangle lies inside the monitor the warm frame covers");
    assert_eq!(
        differing_bytes(warm_crop.pixels(), warm_crop_after.pixels()),
        None,
        "precondition failed, not the invariant: the region changed while the \
         fresh capture was taken, so byte equality below would prove nothing."
    );

    assert_eq!(warm_crop.size(), direct.bitmap.size());
    assert_eq!(
        differing_bytes(warm_crop.pixels(), direct.bitmap.pixels()),
        None,
        "a warm readback and a fresh capture of the same rectangle must agree — \
         1B exit-gate row 2, extended to the warm path (task 1.9f)"
    );

    uptake_capture::warm::stop();
    assert_eq!(
        uptake_capture::warm::status().sessions,
        0,
        "stop must release every session, or Placement would leak them"
    );
}

/// Re-entering Placement must not cool the warm path — the defect bug_001 named
/// (PR #28 review, 2026-07-30).
///
/// `apply` funnels **every** overlay transition into `sync_warm_sessions`,
/// including Placement → Placement, which is what `Esc` mid-drag and a summon
/// while already in Placement produce. When `start` began with an unconditional
/// `stop`, those transitions dropped every texture and respawned every pump, so
/// the user landed back in Placement with the path silently cold for ~330 ms —
/// precisely the window `Ctrl+Space` is pressed in.
///
/// **The rig pass could not see it**, which is why this test exists rather than
/// a note: enter Placement fresh, wait, freeze, and the path is warm every time.
/// Only a *second* entry exposes it, and nothing was driving one.
///
/// The assertion is that warmth survives the second `start` **with no wait
/// after it** — a sleep here would let a respawned set warm up and turn the test
/// green over the bug it exists to catch.
#[test]
#[ignore = "needs a real desktop: holds warm WGC sessions across a simulated re-entry"]
fn re_entering_placement_keeps_the_sessions_already_warm() {
    // Held for the whole test: `warm`'s sessions are process-global, so a
    // sibling test's `stop` would tear down what this one is asserting on.
    let _serial = WARM_SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure_dpi_aware();

    let sessions = uptake_capture::warm::start(uptake_capture::warm::Scope::AllMonitors);
    assert!(sessions > 0, "no monitors were enumerated to warm");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline && !uptake_capture::warm::status().fully_warm() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let warmed = uptake_capture::warm::status();
    assert!(
        warmed.fully_warm(),
        "precondition failed, not the invariant: only {}/{} sessions became warm \
         within 3 s, so a second entry has nothing to preserve",
        warmed.warm,
        warmed.sessions
    );

    // The second entry. No sleep follows it, deliberately.
    let held = uptake_capture::warm::start(uptake_capture::warm::Scope::AllMonitors);
    let after = uptake_capture::warm::status();
    assert_eq!(
        held, sessions,
        "a re-entry must hold the same sessions, not a new set"
    );
    assert_eq!(
        after, warmed,
        "re-entering Placement dropped the warm frames: {}/{} warm immediately \
         after the second start, against {}/{} before it. The sessions were \
         torn down and respawned, so `Ctrl+Space` in the next ~330 ms takes the \
         cold path and lands ~350 ms late (UT-F-45).",
        after.warm, after.sessions, warmed.warm, warmed.sessions
    );

    // And the readback still answers, rather than merely reporting warmth.
    let primary = uptake_capture::warm::capture_monitor(Rect::new(0, 0, 1, 1));
    assert!(
        primary.is_some(),
        "a session reported warm after a re-entry must still serve a readback"
    );

    uptake_capture::warm::stop();
}
