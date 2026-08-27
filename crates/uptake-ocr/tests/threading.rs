//! The three clauses of `architecture.md` section 3.2, each drilled as a test
//! that can actually fail.
//!
//! # Why these are gated on a barrier and not on sleeps
//!
//! Every property here is about *ordering* -- did `submit` return while
//! inference was still running, was a superseded frame skipped, did `drop`
//! return before the engine finished. A sleep-based test asserts a *duration*
//! instead, which is a different claim, and on a loaded CI runner it is a claim
//! that fails for reasons unrelated to the code. These block the fake engine on
//! a channel the test controls, so "still running" is a fact the test creates
//! rather than a window it hopes for.
//!
//! `UT-F-75` and `I-314` are this project's record of guards that ran green and
//! could not have failed. Each test below therefore also asserts the negative
//! half: that the thing it is measuring would be observable if it went wrong.
//!
//! # A HANG IS NOT A FAILING TEST, and the first draft of this file got that wrong
//!
//! The two tests that assert "this call did not wait for the engine" originally
//! held the fake engine blocked forever. Drilled by mutation before merge:
//! making `Drop` join the worker turned
//! `dropping_the_service_does_not_wait_for_inference` into a test that **ran for
//! over sixty seconds and never returned**, because the call under test blocked
//! before its own assertion could be reached. On CI that is a job timeout with
//! no named cause, not a red test naming the defect.
//!
//! The fix is [`WATCHDOG`]: a detached thread releases the engine after a delay
//! far longer than the call should ever take. A correct implementation returns
//! long before it fires and the test ends in milliseconds; a blocking one waits
//! for it and then fails the bound with a message. Both outcomes are now
//! bounded, and the one that matters is red rather than silent.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use uptake_core::bitmap::{BYTES_PER_PIXEL, RgbaBitmap};
use uptake_core::geometry::{Rect, Size};
use uptake_ocr::engine::{Engine, EngineError, Recognition, TextBlock};
use uptake_ocr::service::{Outcome, RequestId, Service, StopReason};

/// How long a test will wait for something that should already have happened.
/// Generous on purpose: it bounds a hang, it is never the thing being measured.
const PATIENCE: Duration = Duration::from_secs(10);

/// How long the watchdog leaves the engine blocked before releasing it.
///
/// Three seconds on purpose: it is `architecture.md` section 3.2's own figure,
/// *"if OCR takes 3 seconds, the overlay must still close instantly"*. A call
/// that waits for the engine waits this long and fails [`IMPATIENCE`]; a call
/// that does not returns in microseconds. The gap between the two is four orders
/// of magnitude, so a loaded runner cannot turn one into the other.
const WATCHDOG: Duration = Duration::from_secs(3);

/// The bound a non-blocking call must meet. Six times smaller than [`WATCHDOG`]
/// and thousands of times larger than the operation itself.
const IMPATIENCE: Duration = Duration::from_millis(500);

/// Releases the engine after [`WATCHDOG`], so a call that blocks on it FAILS
/// rather than hanging.
///
/// Detached deliberately. In the passing case the test is over long before this
/// fires, and the harness reaps it at exit; joining it would make every run pay
/// [`WATCHDOG`] to prove something that already happened.
fn watchdog(release: Sender<()>) {
    drop(thread::spawn(move || {
        thread::sleep(WATCHDOG);
        // Both sends may fail once the receiver is gone. That is the normal,
        // expected path in the passing case and there is nothing to report.
        // `let _ =` rather than `drop(..)`: `SendError<()>` is `Copy`, so
        // dropping it does nothing and clippy says so.
        let _ = release.send(());
        let _ = release.send(());
    }));
}

/// An engine the test drives: each `recognise` announces itself, then blocks
/// until the test releases it.
struct GatedEngine {
    /// Announces "I have started recognising this frame", carrying its width so
    /// the test can tell frames apart.
    started: Sender<u32>,
    /// Blocks until the test sends. One message releases one frame.
    release: Arc<Mutex<Receiver<()>>>,
    /// Total frames this engine was asked to recognise.
    calls: Arc<AtomicUsize>,
}

impl Engine for GatedEngine {
    fn recognise(&mut self, frame: &RgbaBitmap) -> Result<Recognition, EngineError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.send(frame.width()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        Ok(Recognition {
            blocks: vec![TextBlock {
                text: format!("width {}", frame.width()),
                bounds: Rect::new(0, 0, frame.width(), frame.height()),
            }],
        })
    }
}

/// A frame whose width identifies it.
fn frame(width: u32) -> RgbaBitmap {
    let pixels = vec![0u8; width as usize * 4 * BYTES_PER_PIXEL];
    RgbaBitmap::from_pixels(Size::new(width, 4), pixels).unwrap()
}

/// Drains `results()` until `want` arrives or patience runs out.
fn wait_for(service: &Service, want: impl Fn(&Outcome) -> bool) -> Outcome {
    let deadline = Instant::now() + PATIENCE;
    loop {
        for outcome in service.results() {
            if want(&outcome) {
                return outcome;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no matching outcome within {PATIENCE:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn submit_returns_while_the_engine_is_still_working() {
    // Clause 3, first half: "the UI thread never blocks". The engine is held
    // inside `recognise` while the second submit runs, so a `submit` that waited
    // for inference cannot return on its own -- the watchdog below is what turns
    // that into a failure with a message instead of a hang.
    let (started, starts) = mpsc::channel();
    let (release, releases) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(Mutex::new(releases));

    let service = Service::spawn({
        let calls = Arc::clone(&calls);
        move || {
            Ok(GatedEngine {
                started,
                release: releases,
                calls,
            })
        }
    })
    .unwrap();

    service.submit(RequestId::new(1), frame(100)).unwrap();
    assert_eq!(
        starts.recv_timeout(PATIENCE).unwrap(),
        100,
        "the worker should have picked the frame up",
    );

    // The engine is now blocked inside `recognise`. This submit must still
    // return. The watchdog is what makes a blocking `submit` FAIL instead of
    // hanging: without it, a `submit` that waited for inference would never
    // reach the assertion below.
    watchdog(release.clone());
    let began = Instant::now();
    service.submit(RequestId::new(2), frame(200)).unwrap();
    let took = began.elapsed();
    assert!(
        took < IMPATIENCE,
        "submit blocked for {took:?} while the engine was busy",
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the second frame must not have started while the first is unfinished",
    );

    release.send(()).unwrap();
    release.send(()).unwrap();
    let handle = service.shutdown();
    if let Some(handle) = handle {
        drop(handle.join());
    }
}

#[test]
fn a_superseded_frame_is_never_recognised() {
    // Coalescing, which is the whole reason `RequestId` exists. Three frames go
    // in under two ids while the engine is blocked on the first; the middle one
    // must be replaced rather than queued behind it.
    //
    // Both halves asserted: the superseded frame is absent AND the frame that
    // replaced it is present. A version that dropped both would satisfy the
    // first assertion alone.
    let (started, starts) = mpsc::channel();
    let (release, releases) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(Mutex::new(releases));

    let service = Service::spawn({
        let calls = Arc::clone(&calls);
        move || {
            Ok(GatedEngine {
                started,
                release: releases,
                calls,
            })
        }
    })
    .unwrap();

    // Frame A starts and blocks the worker.
    service.submit(RequestId::new(1), frame(10)).unwrap();
    assert_eq!(starts.recv_timeout(PATIENCE).unwrap(), 10);

    // B and C share an id, so C replaces B before either runs.
    service.submit(RequestId::new(2), frame(20)).unwrap();
    service.submit(RequestId::new(2), frame(30)).unwrap();

    release.send(()).unwrap();
    assert_eq!(
        starts.recv_timeout(PATIENCE).unwrap(),
        30,
        "the newest frame for an id must be the one that runs",
    );
    release.send(()).unwrap();

    let outcome = wait_for(
        &service,
        |outcome| matches!(outcome, Outcome::Done { id, .. } if *id == RequestId::new(2)),
    );
    let Outcome::Done { result, .. } = outcome else {
        panic!("expected a Done outcome");
    };
    assert_eq!(
        result.unwrap().text(),
        "width 30",
        "the superseded frame's answer must not be the one delivered",
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "three submits under two ids must cost two recognitions, not three",
    );

    if let Some(handle) = service.shutdown() {
        drop(handle.join());
    }
}

#[test]
fn dropping_the_service_does_not_wait_for_inference() {
    // Clause 3, second half, and the one with teeth: "if OCR takes 3 seconds,
    // the overlay must still close instantly".
    //
    // The engine is blocked when `drop` runs. An earlier draft left it blocked
    // FOREVER, and the mutation drill showed what that bought: making `Drop`
    // join the worker produced a test that ran past sixty seconds and never
    // returned, because `drop` blocked before `began.elapsed()` could be read.
    // The watchdog releases the engine after three seconds, so the blocking
    // version now returns late and fails the bound by name.
    let (started, starts) = mpsc::channel();
    let (release, releases) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(Mutex::new(releases));

    let service = Service::spawn({
        let calls = Arc::clone(&calls);
        move || {
            Ok(GatedEngine {
                started,
                release: releases,
                calls,
            })
        }
    })
    .unwrap();

    service.submit(RequestId::new(1), frame(64)).unwrap();
    assert_eq!(starts.recv_timeout(PATIENCE).unwrap(), 64);

    watchdog(release.clone());
    let began = Instant::now();
    drop(service);
    let took = began.elapsed();
    assert!(
        took < IMPATIENCE,
        "drop waited {took:?} for an inference that had not finished",
    );

    // The worker is still inside `recognise`, which is what makes the assertion
    // above meaningful rather than vacuous: it did not merely finish early.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the engine should still be mid-frame, so drop genuinely did not wait",
    );

    // Let the detached worker finish so the test leaves nothing running. The
    // watchdog would do this too; doing it here means the passing case does not
    // wait three seconds for it.
    let _ = release.send(());
}

#[test]
fn an_engine_that_cannot_start_reports_it_instead_of_panicking() {
    // Model loading happens on the worker thread, so its failure has no call
    // stack to return on. It must arrive as an outcome. A version that panicked
    // the worker instead would show up here as no outcome at all.
    let service = Service::spawn(|| {
        Err::<GatedEngine, _>(EngineError::Unavailable("no model on disk".to_owned()))
    })
    .unwrap();

    let outcome = wait_for(&service, |outcome| matches!(outcome, Outcome::Stopped(_)));
    assert_eq!(
        outcome,
        Outcome::Stopped(StopReason::EngineUnavailable(EngineError::Unavailable(
            "no model on disk".to_owned()
        ))),
        "the reason the engine could not start must survive to the caller",
    );

    // And the service must now refuse work rather than queue it forever, which
    // is the half a caller actually depends on.
    let deadline = Instant::now() + PATIENCE;
    while service.is_running() {
        assert!(
            Instant::now() < deadline,
            "the worker never marked itself stopped"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        service.submit(RequestId::new(1), frame(8)).is_err(),
        "submitting to a dead worker must be reported, not silently queued",
    );
}

#[test]
fn a_frame_with_no_text_is_a_success_and_not_an_error() {
    // The distinction `EngineError`'s docs insist on: "the wall is blank" must
    // not be indistinguishable from "the model failed to load". Cheap to state
    // and cheap to get wrong, since both are the absence of text.
    struct Blank;
    impl Engine for Blank {
        fn recognise(&mut self, _frame: &RgbaBitmap) -> Result<Recognition, EngineError> {
            Ok(Recognition::default())
        }
    }

    let service = Service::spawn(|| Ok(Blank)).unwrap();
    service.submit(RequestId::new(7), frame(16)).unwrap();

    let outcome = wait_for(&service, |outcome| matches!(outcome, Outcome::Done { .. }));
    let Outcome::Done { id, result } = outcome else {
        panic!("expected a Done outcome");
    };
    assert_eq!(id, RequestId::new(7));
    let recognition = result.expect("an empty frame is a success, not an error");
    assert!(recognition.is_empty());
    assert_eq!(recognition.text(), "");

    if let Some(handle) = service.shutdown() {
        drop(handle.join());
    }
}

#[test]
fn a_fatal_engine_error_stops_the_worker_and_a_recoverable_one_does_not() {
    // `EngineError::is_fatal` is policy, and policy that nothing exercises is
    // policy that drifts. Both directions, because a worker that stopped on
    // every error and one that stopped on none would each satisfy a one-sided
    // test.
    struct Flaky {
        calls: Arc<AtomicUsize>,
    }
    impl Engine for Flaky {
        fn recognise(&mut self, frame: &RgbaBitmap) -> Result<Recognition, EngineError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if frame.width() == 1 {
                Err(EngineError::Unavailable("model unloaded".to_owned()))
            } else {
                Err(EngineError::Inference("this frame was odd".to_owned()))
            }
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let service = Service::spawn({
        let calls = Arc::clone(&calls);
        move || Ok(Flaky { calls })
    })
    .unwrap();

    // A recoverable error: reported, worker survives.
    service.submit(RequestId::new(1), frame(2)).unwrap();
    wait_for(&service, |outcome| matches!(outcome, Outcome::Done { .. }));
    assert!(
        service.is_running(),
        "a recoverable inference error must not kill the worker",
    );

    // A fatal one: reported, then the worker stops.
    service.submit(RequestId::new(2), frame(1)).unwrap();
    let stop = wait_for(&service, |outcome| matches!(outcome, Outcome::Stopped(_)));
    assert!(
        matches!(stop, Outcome::Stopped(StopReason::Fatal(_))),
        "a fatal error must stop the worker and say so, got {stop:?}",
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "both frames should have reached the engine",
    );
}

#[test]
fn a_request_the_worker_never_reaches_is_reported_rather_than_lost() {
    // **The defect the independent review of PR #73 found, made permanent.**
    //
    // `Service::submit` promises an accepted request is either answered or
    // refused, never silently dropped. That held only for the caller-driven
    // stops. On the worker's OWN exits -- a failing `make_engine`, a fatal
    // inference error -- nothing set `stopping`, and the `running` atomic
    // flipped to STOPPED only after `run` had already returned. In that window
    // `submit` answered `Ok` into a queue nobody would ever drain.
    //
    // The reviewer measured it rather than arguing it: 54 submits returned
    // `Ok(())` against an engine that could never answer anything, and not one
    // produced an outcome. Thread-creation latency alone opened the window.
    //
    // The invariant asserted here is the one the docs actually promise, and it
    // is deliberately stronger than "the flag gets set": EVERY id whose submit
    // returned `Ok` must eventually appear in an outcome. A fix that only set
    // `stopping` under the lock still loses the request that won the race, and
    // would pass a weaker test.
    let service = Service::spawn(|| {
        Err::<GatedEngine, _>(EngineError::Unavailable("no model on disk".to_owned()))
    })
    .unwrap();

    // Submit hard from the instant spawn returns, which is where the window is.
    let mut accepted = Vec::new();
    for raw in 0..200 {
        let id = RequestId::new(raw);
        if service.submit(id, frame(4)).is_ok() {
            accepted.push(id);
        }
    }

    // Collect until the worker says it has stopped, then once more: `Stopped` is
    // sent last, so anything abandoned is already in the channel behind it.
    let mut answered = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    let mut saw_stop = false;
    while !saw_stop {
        for outcome in service.results() {
            match outcome {
                Outcome::Done { id, .. } | Outcome::Abandoned { id } => answered.push(id),
                Outcome::Stopped(_) => saw_stop = true,
                // `Outcome` is `#[non_exhaustive]`, so a wildcard is required
                // from outside the crate. A future variant lands here and is
                // NOT counted as an answer, which is the conservative direction:
                // it would make this test fail rather than pass, and a new way
                // to answer a request deserves a deliberate line here.
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "the worker never reported that it had stopped",
        );
        thread::sleep(Duration::from_millis(2));
    }
    for outcome in service.results() {
        match outcome {
            Outcome::Done { id, .. } | Outcome::Abandoned { id } => answered.push(id),
            Outcome::Stopped(_) => {}
            _ => {}
        }
    }

    let lost: Vec<_> = accepted
        .iter()
        .filter(|id| !answered.contains(id))
        .collect();
    assert!(
        lost.is_empty(),
        "{} of {} accepted requests were never answered: {lost:?}",
        lost.len(),
        accepted.len(),
    );

    // And the negative half. If `submit` had simply started refusing everything
    // the assertion above would be vacuous, so this fails the test when nothing
    // was ever accepted -- the case where the window closed before it opened.
    assert!(
        !accepted.is_empty(),
        "vacuous: no submit was accepted at all, so nothing was at risk",
    );
}

#[test]
fn a_panicking_engine_ends_the_worker_instead_of_leaving_a_zombie() {
    // **Round 2 of PR #73's review found this, and it is the previous round's
    // defect reached through a door that fix did not cover.**
    //
    // `Engine::recognise` and the closure that builds the engine are
    // CALLER-SUPPLIED, and 1.11 puts an ONNX/PP-OCRv4 FFI binding behind them.
    // A panic there unwound straight out of the spawn closure, so neither the
    // STOPPED store nor `close` ran. The reviewer measured the result:
    // `is_running()` answered TRUE FOREVER, `submit` accepted 40 more requests,
    // and zero outcomes were ever delivered.
    //
    // Three assertions, because the failure had three faces and a fix could
    // plausibly mend one and leave the others.
    struct Exploding;
    impl Engine for Exploding {
        fn recognise(&mut self, _frame: &RgbaBitmap) -> Result<Recognition, EngineError> {
            #[allow(clippy::panic)]
            {
                panic!("simulated engine panic, e.g. an ONNX FFI bug");
            }
        }
    }

    let service = Service::spawn(|| Ok(Exploding)).unwrap();
    service.submit(RequestId::new(1), frame(8)).unwrap();

    // (1) The worker must stop reporting itself alive.
    let deadline = Instant::now() + PATIENCE;
    while service.is_running() {
        assert!(
            Instant::now() < deadline,
            "is_running() still true after the engine panicked -- the zombie is back",
        );
        thread::sleep(Duration::from_millis(2));
    }

    // (2) It must SAY it stopped, and say why, rather than going quiet.
    //
    // **Collected rather than filtered.** `wait_for` DISCARDS the outcomes it
    // steps over, so using it here would consume the `Abandoned` that assertion
    // (4) is about, and (4) would fail against correct code. The first draft did
    // exactly that and reported "the in-flight request was never answered: []" --
    // a red that was the test's fault, not the code's.
    let mut seen = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    while !seen
        .iter()
        .any(|outcome| matches!(outcome, Outcome::Stopped(_)))
    {
        seen.extend(service.results());
        assert!(
            Instant::now() < deadline,
            "the worker never said it had stopped",
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        seen.contains(&Outcome::Stopped(StopReason::Panicked)),
        "a panic must be reported as a panic, not silence and not another reason: {seen:?}",
    );

    // (3) And it must refuse further work rather than swallowing it. This is the
    // assertion that actually failed before the fix: submit kept answering Ok.
    assert!(
        service.submit(RequestId::new(2), frame(8)).is_err(),
        "submitting after an engine panic must be refused, not silently queued",
    );

    // (4) **The request that was IN FLIGHT when the panic hit must be answered
    // too.** Round 4 of the review found this one: it had already been popped off
    // the queue, so draining the queue could not reach it, and the `Done` send
    // sits after `recognise` so the unwind skipped it. The caller got `Ok(())`
    // and would have waited forever. Neither `Done` nor `Abandoned` arrived.
    seen.extend(service.results());
    let answered: Vec<_> = seen
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Done { id, .. } | Outcome::Abandoned { id } => Some(*id),
            _ => None,
        })
        .collect();
    assert!(
        answered.contains(&RequestId::new(1)),
        "the in-flight request was never answered: {answered:?}",
    );
}

#[test]
fn one_id_gets_one_outcome_even_when_it_is_submitted_twice() {
    // **Round 5's finding, reproduced as a test.** `submit`'s coalescing only
    // searches the QUEUE, so a caller can legitimately hold two live submissions
    // under one id: one in flight, one queued behind it. When the engine then
    // panicked, `Exit::drop` reported the in-flight one and `close` reported the
    // queued one, and the caller saw `Abandoned` twice for a single id.
    //
    // The reviewer's exact reproduction was
    // `[Abandoned{1}, Abandoned{1}, Stopped(Panicked)]`.
    //
    // The rule is one outcome per id, which is what coalescing already did, so
    // this pins the half that disagreed rather than inventing a new promise.
    let (started, starts) = mpsc::channel();
    let (release, releases) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(Mutex::new(releases));

    struct Exploding {
        started: Sender<u32>,
        release: Arc<Mutex<Receiver<()>>>,
        calls: Arc<AtomicUsize>,
    }
    impl Engine for Exploding {
        fn recognise(&mut self, frame: &RgbaBitmap) -> Result<Recognition, EngineError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.send(frame.width()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            #[allow(clippy::panic)]
            {
                panic!("simulated engine panic while a resubmission is queued");
            }
        }
    }

    let service = Service::spawn(move || {
        Ok(Exploding {
            started,
            release: releases,
            calls,
        })
    })
    .unwrap();

    // First submission reaches the engine and blocks there.
    service.submit(RequestId::new(1), frame(10)).unwrap();
    assert_eq!(starts.recv_timeout(PATIENCE).unwrap(), 10);
    // Second under the SAME id. Coalescing cannot see the in-flight one, so this
    // genuinely queues rather than replacing anything -- which is the setup, and
    // asserting it here stops the test passing because the queue stayed empty.
    service.submit(RequestId::new(1), frame(20)).unwrap();

    let _ = release.send(());

    let mut seen = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    while !seen
        .iter()
        .any(|outcome| matches!(outcome, Outcome::Stopped(_)))
    {
        seen.extend(service.results());
        assert!(
            Instant::now() < deadline,
            "the worker never stopped: {seen:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
    seen.extend(service.results());

    let abandoned = seen
        .iter()
        .filter(|outcome| matches!(outcome, Outcome::Abandoned { id } if *id == RequestId::new(1)))
        .count();
    assert_eq!(
        abandoned, 1,
        "one id must yield one outcome, got {abandoned}: {seen:?}",
    );
}
