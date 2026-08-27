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
    /// The worker has stopped and will answer nothing further.
    ///
    /// Delivered **once**, and its absence is not a promise of health: a
    /// receiver that has been dropped cannot be told anything. Callers that need
    /// to know the worker is alive should ask [`Service::is_running`].
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
                run(&worker_shared, &sender, make_engine);
                worker_running.store(STOPPED, Ordering::SeqCst);
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
        self.signal_stop();
        self.worker.take()
    }

    fn signal_stop(&self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopping = true;
            // Queued-but-unstarted work is dropped rather than drained. Its
            // answers have nowhere to go once the receiver is gone, and running
            // them would be exactly the delay `shutdown` exists to avoid.
            state.queued.clear();
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

/// The worker body. Builds the engine, then serves until told to stop.
fn run<E, F>(shared: &Arc<Shared>, sender: &Sender<Outcome>, make_engine: F)
where
    E: Engine,
    F: FnOnce() -> Result<E, EngineError>,
{
    let mut engine = match make_engine() {
        Ok(engine) => engine,
        Err(error) => {
            // Ignoring the send result is correct rather than lazy: a dropped
            // receiver means nobody is listening, and there is no other party to
            // report to. It is the only reasonable action, so it is taken
            // explicitly instead of through an `unwrap` the workspace lints deny
            // anyway.
            drop(sender.send(Outcome::Stopped(StopReason::EngineUnavailable(error))));
            return;
        }
    };

    loop {
        let Some(request) = take_next(shared) else {
            drop(sender.send(Outcome::Stopped(StopReason::Requested)));
            return;
        };
        let result = engine.recognise(&request.frame);
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
            drop(sender.send(Outcome::Stopped(StopReason::Fatal(error))));
            return;
        }
    }
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
