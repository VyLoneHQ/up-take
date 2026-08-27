//! The background inference thread: roadmap 1.10's third clause.
//!
//! `architecture.md` section 3.2: *"Runs on a dedicated thread. Models load once
//! at startup and stay resident. The UI thread never blocks -- if OCR takes 3
//! seconds, the overlay must still close instantly."* This module is that
//! sentence, and each clause below names the mechanism that keeps it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use uptake_core::bitmap::RgbaBitmap;

use crate::engine::{Engine, EngineError, Recognition};

/// Identifies a request, and is the key coalescing works on.
///
/// **The host should use the area's own id.** Coalescing is "a newer request for
/// the same id replaces an older one that has not started yet", which is the
/// right policy exactly when the id means *this area*: a resize drag emits a
/// request per frame, and recognising the intermediate sizes is work whose
/// answer is thrown away. Using a fresh id per request instead turns this into
/// an unbounded queue, which is a decision a caller is allowed to make and
/// should make knowingly.
///
/// # The contract: ONE outcome per id, not one per accepted `submit`
///
/// **Stated here because it was never stated anywhere, and round 5 of `PR #73`'s
/// review found the two halves of this module answering it differently.**
///
/// Coalescing has always collapsed to one: a queued frame replaced by a newer
/// one for the same id produces **no** outcome of its own, and the replacement
/// answers for both. The in-flight path briefly did the opposite -- a request
/// in flight plus a second queued under the same id produced **two**
/// `Abandoned` for one id when the engine panicked, reproduced by the reviewer
/// as `[Abandoned{1}, Abandoned{1}, Stopped(Panicked)]`. So the same caller
/// action, re-submitting one area, yielded zero or two signals depending purely
/// on whether the first submission had been picked off the queue yet.
///
/// The rule is now one, everywhere: `close` reports an in-flight id only when
/// the queue does not already carry it.
///
/// ⚠️ **This is the consistent default, not a decided answer.** "One per id" is
/// what the older half already did and is therefore the change that breaks
/// nothing; "one per accepted `submit`" is defensible too, and would mean
/// coalescing announcing every superseded frame -- dozens during a resize drag,
/// which is noise the caller did not ask for. Choosing between them is a product
/// call about what a host wants to hear, and it is recorded as a backlog row
/// rather than settled here by whoever happened to be fixing the defect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

impl RequestId {
    /// Wraps a raw id. The host passes its area id.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw id back out.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// What comes back from the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// One request finished, successfully or not.
    ///
    /// A `Recognition` with no blocks is a success: the frame had no text in it.
    Done {
        /// The request this answers.
        id: RequestId,
        /// What the engine made of it.
        result: Result<Recognition, EngineError>,
    },
    /// A request was accepted and will never be answered, because the worker
    /// stopped before reaching it.
    ///
    /// **This variant exists because the alternative is silence.** A `submit`
    /// that returned `Ok` has told its caller the work is in hand; if the worker
    /// then dies, dropping that request without a word leaves the caller waiting
    /// for a result that is not coming, which is precisely what
    /// [`Service::submit`]'s documentation promises will not happen. Delivered
    /// before the [`Outcome::Stopped`] that follows it, so a caller draining in
    /// order learns what it lost before it learns why.
    ///
    /// # ⚠️ Reachable on the worker's own stops, and NOT on a caller's
    ///
    /// This is sent whenever the worker ends by itself: the engine fails to
    /// build, an error is fatal, or it panics. It is also *sent* on
    /// [`Service::shutdown`] and on `Drop`, because `close` is the single drain
    /// point, but **nobody can hear it there**: both consume the `Service`, and
    /// [`Service::results`] needs one to borrow, so the receiver is going away in
    /// the same breath. The send fails and is discarded.
    ///
    /// **Stated rather than quietly true.** Round 2 of `PR #73`'s review found
    /// `signal_stop` clearing the queue with no report at all, which broke this
    /// promise in code as well as in reach. The clearing is gone and there is now
    /// one drain point that always reports; what is left is a limit of the API
    /// shape, not an inconsistency in it. Making a caller-initiated stop
    /// observable would mean `shutdown` handing back the drained outcomes, which
    /// is a signature change and belongs to whoever first needs it.
    Abandoned {
        /// The request that will go unanswered.
        id: RequestId,
    },
    /// The worker has stopped and will answer nothing further.
    ///
    /// Delivered **once**, and after any [`Outcome::Abandoned`] it caused. Its
    /// absence is not a promise of health: a receiver that has been dropped
    /// cannot be told anything. Callers that need to know the worker is alive
    /// should ask [`Service::is_running`].
    Stopped(StopReason),
}

/// Why the worker stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// The engine could not be built at all, typically a missing or unreadable
    /// model. No request was ever attempted.
    EngineUnavailable(EngineError),
    /// A request failed in a way the engine does not survive.
    Fatal(EngineError),
    /// [`Service::shutdown`] was called, or the service was dropped.
    Requested,
    /// The engine **panicked**, so the worker is gone and no further request can
    /// be served.
    ///
    /// [`Engine::recognise`] and the closure that builds the engine are
    /// caller-supplied, and 1.11 puts an FFI binding behind them. A panic there
    /// used to leave the service reporting itself healthy while silently
    /// swallowing every later request; it now ends the worker like any other
    /// exit, and says which one it was. The panic message itself goes to stderr
    /// through the standard hook, which this module does not intercept.
    Panicked,
}

/// What went wrong submitting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ServiceError {
    /// The worker is gone. Submitting cannot succeed and retrying will not help.
    #[error("the OCR worker is not running")]
    NotRunning,
}

/// One queued frame.
struct Request {
    id: RequestId,
    frame: RgbaBitmap,
}

/// The state the submitter and the worker share.
struct State {
    /// Queued, not yet started. Small by construction: coalescing bounds it at
    /// one entry per distinct id, and a screen holds few areas.
    ///
    /// A `Vec` rather than a `VecDeque` or a map because coalescing needs a
    /// linear scan for a matching id on every push, and at this length the scan
    /// is cheaper than a hash. If it ever holds enough entries for that to be
    /// false, the queue has stopped being coalesced, and that is the defect to
    /// look at rather than the container.
    queued: Vec<Request>,
    /// Set by [`Service::shutdown`] and by `Drop`. The worker checks it before
    /// waiting and before taking work, never during inference: there is no way
    /// to interrupt a running `recognise`, and an in-flight cancel API would be
    /// claiming otherwise.
    stopping: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// A handle on the OCR worker thread.
///
/// # The three clauses of the contract, and what keeps each
///
/// **"Runs on a dedicated thread"**: [`Service::spawn`] starts one and owns it.
/// The engine never leaves that thread, which is what lets [`Engine::recognise`]
/// take `&mut self` without any locking.
///
/// **"Models load once at startup and stay resident"**: the engine is built by a
/// closure that runs **on the worker thread**, not on the caller's. Loading a
/// PP-OCRv4 model is slow, and building it in `spawn` would move that cost onto
/// whichever thread called it, which on the host is the one that must stay free.
/// The engine is then kept and reused for every request; nothing reloads it.
///
/// **"The UI thread never blocks"**: [`Service::submit`] takes a lock only long
/// enough to push, then returns. [`Service::results`] is polled, never awaited.
/// And `Drop` **does not join the worker**, which is the clause with real teeth:
/// see [`Service::shutdown`].
pub struct Service {
    shared: Arc<Shared>,
    results: Receiver<Outcome>,
    worker: Option<JoinHandle<()>>,
    /// Mirrors "the worker has stopped" without needing the receiver, so
    /// [`Service::is_running`] does not have to consume an [`Outcome`] to
    /// answer.
    running: Arc<AtomicU64>,
}

/// `running` is a counter rather than a flag so a caller can tell "not started
/// yet" from "started and stopped" without racing the thread.
const RUNNING: u64 = 1;
const STOPPED: u64 = 2;

impl Service {
    /// Starts the worker, building the engine on it.
    ///
    /// `make_engine` runs **on the new thread**. If it fails, the worker sends
    /// [`StopReason::EngineUnavailable`] and exits; `spawn` itself still returns
    /// a `Service`, because the alternative is for `spawn` to block until the
    /// model has loaded, and that is the cost this design exists to move.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NotRunning`] if the thread could not be created
    /// at all. That is an operating-system refusal rather than an engine
    /// problem, and it is the one failure that happens before there is anywhere
    /// to report failures to.
    pub fn spawn<E, F>(make_engine: F) -> Result<Self, ServiceError>
    where
        E: Engine,
        F: FnOnce() -> Result<E, EngineError> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queued: Vec::new(),
                stopping: false,
            }),
            wake: Condvar::new(),
        });
        let (sender, results) = mpsc::channel();
        let running = Arc::new(AtomicU64::new(0));

        let worker_shared = Arc::clone(&shared);
        let worker_running = Arc::clone(&running);
        let worker = thread::Builder::new()
            .name("uptake-ocr".to_owned())
            .spawn(move || {
                worker_running.store(RUNNING, Ordering::SeqCst);
                // **Everything that closes the service lives in this guard, and
                // that is what makes a panic survivable.** See `Exit`.
                let mut exit = Exit {
                    shared: &worker_shared,
                    sender: &sender,
                    running: &worker_running,
                    reason: None,
                    in_flight: None,
                };
                run(&worker_shared, &sender, make_engine, &mut exit);
            })
            .map_err(|_| ServiceError::NotRunning)?;

        Ok(Self {
            shared,
            results,
            worker: Some(worker),
            running,
        })
    }

    /// Queues a frame. Returns immediately; never waits for inference.
    ///
    /// A queued request for the same `id` that has **not started yet** is
    /// replaced. One already running is not: there is no way to interrupt
    /// [`Engine::recognise`], so a design that claimed to cancel it would be
    /// lying about what it can do.
    ///
    /// # Errors
    ///
    /// [`ServiceError::NotRunning`] once the worker has stopped. Submitting to a
    /// dead worker is reported rather than silently queued forever, because a
    /// caller that cannot tell the difference will wait for a result that is
    /// never coming.
    pub fn submit(&self, id: RequestId, frame: RgbaBitmap) -> Result<(), ServiceError> {
        if self.running.load(Ordering::SeqCst) == STOPPED {
            return Err(ServiceError::NotRunning);
        }
        let Ok(mut state) = self.shared.state.lock() else {
            // A poisoned lock means the worker panicked while holding it. The
            // queue's contents cannot be trusted, and there is nobody left to
            // serve them.
            return Err(ServiceError::NotRunning);
        };
        if state.stopping {
            return Err(ServiceError::NotRunning);
        }
        match state.queued.iter_mut().find(|queued| queued.id == id) {
            Some(existing) => existing.frame = frame,
            None => state.queued.push(Request { id, frame }),
        }
        drop(state);
        self.shared.wake.notify_one();
        Ok(())
    }

    /// Results that have arrived, oldest first. Never blocks.
    ///
    /// Drained rather than awaited so the host can call this from its own poll
    /// without a runtime or a callback crossing the thread boundary.
    pub fn results(&self) -> impl Iterator<Item = Outcome> + '_ {
        // `.ok()` collapses both `TryRecvError` variants to `None`, which is
        // correct for each: Empty means nothing has arrived yet, Disconnected
        // means the worker is gone and nothing ever will. Neither is a state a
        // draining caller acts on differently, and `Service::is_running` is the
        // question that distinguishes them.
        std::iter::from_fn(move || self.results.try_recv().ok())
    }

    /// Whether the worker is still able to answer.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst) != STOPPED
    }

    /// Asks the worker to stop and **returns without waiting for it**.
    ///
    /// # This is the clause with teeth
    ///
    /// *"If OCR takes 3 seconds, the overlay must still close instantly."* A
    /// running [`Engine::recognise`] cannot be interrupted, since no safe API
    /// kills a thread mid-call, so the only way to honour that sentence is to
    /// **not wait**. The flag is set, the worker is woken, and the caller
    /// returns while inference is still running. The thread finishes its current
    /// frame, sees the flag, and exits on its own.
    ///
    /// The returned [`JoinHandle`] is for a caller that genuinely wants to wait,
    /// which in practice means a test. **Dropping it is the normal case** and is
    /// not a leak: the thread holds an `Arc` to the shared state and nothing
    /// else, so it releases everything when it returns.
    pub fn shutdown(mut self) -> Option<JoinHandle<()>> {
        // **Deliberately does not call `signal_stop` itself.** `Service`
        // implements `Drop` and `self` is consumed here, so the destructor runs
        // at the end of this scope and signals exactly once, before the caller
        // receives the handle, which is what makes joining it safe. An earlier
        // version called `signal_stop` here as well and therefore ran it twice on
        // every clean shutdown. Harmless while it stays idempotent, and the
        // independent review of `PR #73` was right that the doc comments read as
        // though only one of the two paths fires. Now only one does.
        self.worker.take()
    }

    fn signal_stop(&self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopping = true;
            // **The queue is deliberately NOT cleared here, and it used to be.**
            // That made this the second place queued work was discarded, and the
            // only one that discarded it without a word -- which contradicted
            // [`Outcome::Abandoned`]'s own stated purpose, as round 2 of
            // `PR #73`'s review pointed out. `close` is now the single drain
            // point: the worker wakes, `take_next` sees `stopping` and returns
            // `None` before touching the queue, and `close` reports every entry.
            //
            // **Nothing is run that was not already running.** The concern the
            // old comment named -- that draining would cost the delay `shutdown`
            // exists to avoid -- was about *recognising* those frames, and
            // nothing here recognises them. They are reported and dropped.
        }
        self.shared.wake.notify_all();
    }
}

impl Drop for Service {
    /// Signals the worker and returns. **Deliberately does not join.**
    ///
    /// Joining here would make dropping the service take as long as the frame
    /// being recognised, which is the exact behaviour section 3.2 forbids, and
    /// it would do it in a destructor where a caller cannot opt out. A caller
    /// that wants the wait asks for it with [`Service::shutdown`].
    fn drop(&mut self) {
        self.signal_stop();
    }
}

/// Closes the service on **every** way out of the worker, including a panic.
///
/// # The hole this fills, found by round 2 of `PR #73`'s independent review
///
/// The worker used to store `STOPPED` and call `close` as ordinary statements
/// after `run` returned. Neither runs when `run` **unwinds**, and unwinding is
/// not hypothetical here: [`Engine::recognise`] and the `make_engine` closure are
/// **caller-supplied code**, and 1.11 puts an ONNX/PP-OCRv4 FFI binding behind
/// exactly that trait. A panic there produced the worst state this module can be
/// in, and the reviewer reproduced it: [`Service::is_running`] answered **`true`
/// forever**, `submit` accepted **40 further requests**, and **zero** outcomes
/// were ever delivered. That is the same silent-swallow the previous round fixed,
/// reached by a door the fix did not cover.
///
/// A `Drop` guard is used rather than `catch_unwind` because it needs no
/// `UnwindSafe` bound on caller-supplied types, it cannot be forgotten at a new
/// `return`, and it keeps one exit path instead of two. `run` records **why** it
/// is leaving; the guard decides what to do about it. A `reason` still `None`
/// when this drops means nobody recorded one, which can only be a panic.
struct Exit<'a> {
    shared: &'a Arc<Shared>,
    sender: &'a Sender<Outcome>,
    running: &'a AtomicU64,
    reason: Option<StopReason>,
    /// The request the engine is working on right now, if any.
    ///
    /// **The one request `close` cannot see.** It has already been taken out of
    /// the queue, so draining the queue does not reach it, and if `recognise`
    /// panics on it the `Done` send is skipped by the unwind. Round 4 of
    /// `PR #73`'s review reproduced the result: a caller that got `Ok(())` waits
    /// for an outcome that never comes, which is the exact failure the previous
    /// three rounds were each closing somewhere else.
    in_flight: Option<RequestId>,
}

impl Drop for Exit<'_> {
    fn drop(&mut self) {
        let reason = self.reason.take().unwrap_or(StopReason::Panicked);
        close(self.shared, self.sender, reason, self.in_flight.take());
        // Stored **after** `close`, so a caller that sees `is_running() == false`
        // can already drain every outcome the shutdown produced. The other order
        // is a window where the service reports itself dead and its explanation
        // has not been sent yet.
        //
        // ⚠️ **CORRECT BY CONSTRUCTION AND NOT ENFORCED BY ANY TEST, which is
        // stated here because a previous attempt claimed otherwise.** Round 3 of
        // `PR #73`'s review found this ordering had no coverage; the test written
        // for it queued 4096 requests to "widen the window", and round 4 showed
        // that test **cannot go red**: it passed 50 out of 50 runs against the
        // reversed order. The measurement explains why, and it is worth keeping:
        // `close` completes in **13 microseconds**, while the submit loop that
        // was supposed to slow it down costs **679 microseconds on the observing
        // thread itself**. Widening the producer's cost does not widen the
        // observer's reaction window when they are the same thread.
        //
        // The test was **deleted rather than kept green**, because a check that
        // cannot fail is worse than no check: it reports coverage that does not
        // exist. `UT-F-75` and `I-314` are this project's record of that class,
        // and keeping the test would have been a third entry. What is left is a
        // two-statement ordering a reader can verify by looking at it, and an
        // honest note that nothing guards it.
        self.running.store(STOPPED, Ordering::SeqCst);
    }
}

/// The worker body. Builds the engine, then serves until told to stop.
fn run<E, F>(shared: &Arc<Shared>, sender: &Sender<Outcome>, make_engine: F, exit: &mut Exit<'_>)
where
    E: Engine,
    F: FnOnce() -> Result<E, EngineError>,
{
    let mut engine = match make_engine() {
        Ok(engine) => engine,
        Err(error) => {
            // Ignoring a send result is correct rather than lazy: a dropped
            // receiver means nobody is listening, and there is no other party to
            // report to. It is the only reasonable action, so it is taken
            // explicitly instead of through an `unwrap` the workspace lints deny
            // anyway.
            exit.reason = Some(StopReason::EngineUnavailable(error));
            return;
        }
    };

    loop {
        let Some(request) = take_next(shared) else {
            exit.reason = Some(StopReason::Requested);
            return;
        };
        // Marked before the call and cleared after, so a panic INSIDE `recognise`
        // leaves it set and the guard reports it. The window is exactly the call.
        exit.in_flight = Some(request.id);
        let result = engine.recognise(&request.frame);
        exit.in_flight = None;
        let fatal = result
            .as_ref()
            .err()
            .filter(|error| error.is_fatal())
            .cloned();
        drop(sender.send(Outcome::Done {
            id: request.id,
            result,
        }));
        if let Some(error) = fatal {
            exit.reason = Some(StopReason::Fatal(error));
            return;
        }
    }
}

/// Shuts the door **under the lock**, then reports everything left behind.
///
/// # The race this exists to close, found by the independent review of `PR #73`
///
/// [`Service::submit`]'s own documentation promises that submitting to a dead
/// worker is *"reported rather than silently queued forever, because a caller
/// that cannot tell the difference will wait for a result that is never
/// coming."* Before this function existed, that promise held only for the
/// **caller-driven** stops, `shutdown` and `Drop`, which go through
/// `signal_stop`. The worker's own exits, an engine that fails to build and a
/// fatal inference error, set nothing at all: the `running` atomic flips to
/// `STOPPED` only *after* `run` has returned, in the spawn closure, so between
/// the worker deciding to die and that store landing, `submit` saw
/// `stopping == false`, pushed, and answered `Ok`.
///
/// **Measured, not theorised.** The reviewer raced a failing `make_engine`
/// against a tight submit loop: **54 submits returned `Ok(())` and not one of
/// them ever produced an outcome**, because the thread that would have drained
/// them had already gone. Thread-creation latency alone was enough, with no
/// tuning needed to reproduce it.
///
/// Two halves, and both are needed. Setting `stopping` **under the same lock
/// `submit` takes** serialises the two: whoever reaches the lock first wins, and
/// a `submit` arriving afterwards is refused. Draining the queue and reporting
/// each entry as [`Outcome::Abandoned`] covers the other order, where a `submit`
/// won the race and pushed, so that request is answered rather than dropped in
/// silence. **Neither half alone is enough, and that is measured rather than
/// argued**: with the flag set under the lock but the queue left undrained, the
/// regression test still lost **81 of 81** accepted requests. The flag closes the
/// door; the drain answers whoever was already through it.
fn close(
    shared: &Arc<Shared>,
    sender: &Sender<Outcome>,
    reason: StopReason,
    in_flight: Option<RequestId>,
) {
    let abandoned = match shared.state.lock() {
        Ok(mut state) => {
            state.stopping = true;
            std::mem::take(&mut state.queued)
        }
        // A poisoned lock means the queue's contents cannot be trusted. There is
        // nothing safe to drain and nothing truthful to say about what was in it.
        Err(_) => Vec::new(),
    };
    // The in-flight request first, because it was accepted first -- but **only if
    // the queue does not already carry its id.** See the contract note on
    // [`RequestId`]: one outcome per id, which is the rule coalescing has always
    // followed and which round 5 caught this function breaking.
    if let Some(id) = in_flight
        && !abandoned.iter().any(|request| request.id == id)
    {
        drop(sender.send(Outcome::Abandoned { id }));
    }
    for request in abandoned {
        drop(sender.send(Outcome::Abandoned { id: request.id }));
    }
    drop(sender.send(Outcome::Stopped(reason)));
}

/// Blocks until there is work or a stop. `None` means stop.
fn take_next(shared: &Arc<Shared>) -> Option<Request> {
    let Ok(mut state) = shared.state.lock() else {
        return None;
    };
    loop {
        if state.stopping {
            return None;
        }
        if !state.queued.is_empty() {
            // Oldest first. The queue is bounded by coalescing, so the O(n)
            // removal is over a handful of entries.
            return Some(state.queued.remove(0));
        }
        let Ok(waited) = shared.wake.wait(state) else {
            return None;
        };
        state = waited;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! In-crate tests, for the two things the integration suite cannot reach.
    //!
    //! # Why this module exists at all, and why that is a correction
    //!
    //! `8786e2c`'s commit message said the `signal_stop` path *"needed a
    //! test-only API to reach"* (attributed to `246e03e` when this module was
    //! written, and corrected here after round 4 read both commits) and that widening the public surface for a test
    //! was not worth it. **The first half was wrong.** Round 3 of `PR #73`'s
    //! independent review demonstrated it by writing the test: a private
    //! `#[cfg(test)]` module inside this file reaches `signal_stop` directly,
    //! which is ordinary Rust and widens nothing. The crate simply had no such
    //! module, and its absence was mistaken for an obstacle.
    //!
    //! The honest shape of the original problem was narrower than stated: the
    //! *public* API cannot observe it, because `shutdown` and `Drop` consume the
    //! `Service` while `results` borrows one. That remains true and is recorded
    //! on [`Outcome::Abandoned`]. It was never a reason the code could not be
    //! tested from inside.

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use uptake_core::bitmap::RgbaBitmap;
    use uptake_core::geometry::Size;

    use super::{Engine, EngineError, Outcome, Recognition, RequestId, Service};

    const PATIENCE: Duration = Duration::from_secs(10);

    fn frame() -> RgbaBitmap {
        RgbaBitmap::from_pixels(Size::new(4, 4), vec![0u8; 4 * 4 * 4]).unwrap()
    }

    /// Blocks in `recognise` until released, so a request can be left queued.
    struct Gated {
        started: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        calls: std::sync::Arc<AtomicUsize>,
    }

    impl Engine for Gated {
        fn recognise(&mut self, _frame: &RgbaBitmap) -> Result<Recognition, EngineError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Ok(Recognition::default())
        }
    }

    #[test]
    fn signal_stop_reports_the_work_it_abandons_rather_than_discarding_it() {
        // **Round 2's finding (b)**, not round 3's -- this comment said "Round 3's
        // finding (d)", which is a different finding and is already correctly
        // used for the test-only-API claim in this module's own header. Round 4
        // caught the collision by reading the file rather than the brief.
        // `signal_stop` used to clear the queue outright,
        // making it the one place `Outcome::Abandoned`'s promise was broken in
        // CODE rather than merely unreachable. Reverting that fix leaves the
        // whole integration suite green, which is why this test is here and why
        // it is here rather than there: `signal_stop` is private, and reaching it
        // needs no public API at all.
        let (started, starts) = std::sync::mpsc::channel();
        let (release, releases) = std::sync::mpsc::channel();
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let service = Service::spawn({
            let calls = std::sync::Arc::clone(&calls);
            move || {
                Ok(Gated {
                    started,
                    release: std::sync::Mutex::new(releases),
                    calls,
                })
            }
        })
        .unwrap();

        // One request occupies the worker; a second is accepted and never starts.
        service.submit(RequestId::new(1), frame()).unwrap();
        starts.recv_timeout(PATIENCE).unwrap();
        service.submit(RequestId::new(2), frame()).unwrap();

        // The private path, which the public one cannot reach without consuming
        // the `Service` and taking the receiver with it.
        service.signal_stop();
        // `let _ =` rather than `drop(..)`: `SendError<()>` is `Copy`, so dropping
        // it does nothing and clippy says so.
        let _ = release.send(());

        let deadline = Instant::now() + PATIENCE;
        let mut abandoned = Vec::new();
        while service.is_running() {
            assert!(Instant::now() < deadline, "the worker never stopped");
            std::thread::sleep(Duration::from_millis(2));
        }
        for outcome in service.results() {
            if let Outcome::Abandoned { id } = outcome {
                abandoned.push(id);
            }
        }
        assert_eq!(
            abandoned,
            vec![RequestId::new(2)],
            "an accepted request the worker never reached must be reported",
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "and reporting it must not mean RUNNING it -- that is the delay a stop exists to avoid",
        );
    }
}
