//! One-shot Windows Graphics Capture of a single monitor's frame.
//!
//! # Threading model
//!
//! WGC is a session API built for streams, wrapped here into "give me one
//! frame": each shot spawns a dedicated thread that runs `windows-capture`'s
//! blocking message pump, and the handler crops the first frame, sends it back
//! over a channel, and stops the session — so the pump thread ends itself on
//! the happy path.
//!
//! The pump thread is spawned per shot (rather than using the library's
//! `start_free_threaded`) because a `GraphicsCaptureItem`'s `Monitor` wraps a
//! raw `HMONITOR` pointer and is not `Send`; the handle crosses threads as an
//! `isize` and becomes a `Monitor` only on the thread that uses it.
//!
//! # The timeout escape hatch
//!
//! If no frame ever arrives (a stalled compositor, a session that silently
//! died), nothing inside the pump would ever break the `GetMessageW` loop. So
//! every pump thread creates its message queue *before* reporting its thread
//! id, and the waiting side can post `WM_QUIT` to that id to unwind the pump.
//! `WM_QUIT` is only ever posted while the thread is provably alive (its
//! channel sender not yet dropped) — a thread id may be recycled by the OS
//! once its thread exits, and posting a quit message to a recycled id would
//! sabotage an unrelated thread.
//!
//! # Degraded retry
//!
//! The first attempt asks for no cursor and no capture border. Both knobs
//! hard-fail on Windows builds that predate them (`windows-capture` returns
//! `CursorConfigUnsupported`/`BorderConfigUnsupported` rather than ignoring
//! the setting), so a session that fails before producing a frame is retried
//! once with the system defaults — a capture that may include the cursor or
//! flash a border beats no capture (architecture.md §5: degrade gracefully).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use uptake_core::geometry::Size;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_QUIT, WM_USER,
};

use crate::blit::extract_rect;
use crate::error::CaptureError;
use crate::plan::Shot;

/// How long a shot may wait for its first frame before the capture gives up.
///
/// The spike measured ~90 ms to first frame on the dev rig; the budget for
/// the whole selection→clipboard path is 300 ms (quality-bars.md §1). This
/// bound exists for the *failure* path — a loaded system should get the
/// benefit of the doubt rather than a spurious error, so it is deliberately
/// far above both numbers.
pub(crate) const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

/// How a single shot went wrong, before monitor context is attached.
enum ShotFailure {
    /// The frame's dimensions disagree with the plan — the display topology
    /// changed between planning and capture.
    DisplayChanged,
    /// The session failed or closed without delivering a usable frame.
    Session(String),
}

type ShotResult = Result<Vec<u8>, ShotFailure>;

/// What the capture handler needs to crop, report, and stop.
struct OneShotFlags {
    source_x: u32,
    source_y: u32,
    size: Size,
    tx: Sender<ShotResult>,
    /// Set before anything is sent on `tx`, so the pump thread can tell "the
    /// handler already reported" from "the session died silently".
    sent: Arc<AtomicBool>,
}

/// Captures exactly one frame, crops it, reports it, and stops the session.
struct OneShot {
    flags: OneShotFlags,
    done: bool,
}

impl GraphicsCaptureApiHandler for OneShot {
    type Flags = OneShotFlags;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            flags: ctx.flags,
            done: false,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame<'_>,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        // A second frame can slip in before the stop request lands; the first
        // one already answered.
        if self.done {
            capture_control.stop();
            return Ok(());
        }
        self.done = true;

        let frame_size = Size::new(frame.width(), frame.height());
        let outcome = match frame.buffer() {
            Ok(mut buffer) => {
                let row_pitch = buffer.row_pitch() as usize;
                extract_rect(
                    buffer.as_raw_buffer(),
                    row_pitch,
                    frame_size,
                    self.flags.source_x,
                    self.flags.source_y,
                    self.flags.size,
                )
                .ok_or(ShotFailure::DisplayChanged)
            }
            Err(error) => Err(ShotFailure::Session(format!(
                "could not read the captured frame: {error}"
            ))),
        };

        self.flags.sent.store(true, Ordering::SeqCst);
        let _ = self.flags.tx.send(outcome);
        capture_control.stop();
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        // `swap` rather than `load`: closing races nothing here (the pump is
        // single-threaded), but claiming the flag before sending keeps the
        // invariant "at most one report per attempt" locally obvious.
        if !self.flags.sent.swap(true, Ordering::SeqCst) {
            let _ = self.flags.tx.send(Err(ShotFailure::Session(
                "the capture session closed before a frame arrived".into(),
            )));
        }
        Ok(())
    }
}

/// One spawned capture attempt: its result channel, its pump thread's id, and
/// the flag saying whether the handler already reported.
struct Attempt {
    rx: Receiver<ShotResult>,
    thread_id: u32,
    sent: Arc<AtomicBool>,
}

/// Spawns a pump thread capturing `shot`'s monitor with the given settings.
fn spawn_attempt(
    shot: Shot,
    cursor: CursorCaptureSettings,
    border: DrawBorderSettings,
) -> Result<Attempt, CaptureError> {
    let (tx, rx) = mpsc::channel::<ShotResult>();
    let (tid_tx, tid_rx) = mpsc::channel::<u32>();
    let sent = Arc::new(AtomicBool::new(false));
    let flags = OneShotFlags {
        source_x: shot.source_x,
        source_y: shot.source_y,
        size: shot.size,
        tx: tx.clone(),
        sent: Arc::clone(&sent),
    };
    let handle = shot.monitor.handle;
    let thread_sent = Arc::clone(&sent);

    std::thread::spawn(move || {
        // Force this thread's message queue into existence *before* reporting
        // the thread id: the parent may post WM_QUIT the moment it has the id,
        // and PostThreadMessageW fails against a thread with no queue yet.
        // SAFETY: PeekMessageW with PM_NOREMOVE only inspects the queue (and
        // creates it as a side effect); the MSG out-param is a plain struct.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            PeekMessageW(
                &mut msg,
                std::ptr::null_mut(),
                WM_USER,
                WM_USER,
                PM_NOREMOVE,
            );
            let _ = tid_tx.send(GetCurrentThreadId());
        }

        // The handle re-becomes a Monitor only here, on the thread that uses
        // it — this is the manual Send the raw pointer could not provide.
        let monitor = Monitor::from_raw_hmonitor(handle as *mut std::ffi::c_void);
        let settings = Settings::new(
            monitor,
            cursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            flags,
        );
        // start() blocks pumping messages until the handler stops the session
        // or WM_QUIT arrives. An error with nothing yet reported is the
        // session failing to start — report it, since the handler never can.
        if let Err(error) = OneShot::start(settings)
            && !thread_sent.swap(true, Ordering::SeqCst)
        {
            let _ = tx.send(Err(ShotFailure::Session(format!(
                "the capture session could not start: {error}"
            ))));
        }
    });

    // The queue-creation preamble runs in microseconds; a second is not a
    // tuning knob but a "the thread never ran at all" detector.
    let thread_id =
        tid_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| CaptureError::Failed {
                monitor: shot.monitor.bounds,
                reason: "the capture thread did not start".into(),
            })?;

    Ok(Attempt {
        rx,
        thread_id,
        sent,
    })
}

/// A shot in flight. Obtain one with [`spawn`], collect it with
/// [`PendingShot::wait`]; dropping it un-collected unwinds its pump thread.
pub(crate) struct PendingShot {
    shot: Shot,
    attempt: Option<Attempt>,
    retried: bool,
}

/// Starts capturing `shot` in the background (no cursor, no border, with one
/// degraded retry handled inside [`PendingShot::wait`]).
pub(crate) fn spawn(shot: Shot) -> Result<PendingShot, CaptureError> {
    let attempt = spawn_attempt(
        shot,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
    )?;
    Ok(PendingShot {
        shot,
        attempt: Some(attempt),
        retried: false,
    })
}

impl PendingShot {
    /// The shot this capture is executing, for compositing its result.
    pub(crate) fn shot(&self) -> Shot {
        self.shot
    }

    /// Waits for the shot's pixels until `deadline`.
    ///
    /// A session that fails before producing a frame is retried once with
    /// default capture settings (see the module docs); the retry shares the
    /// same deadline, so the two attempts together still respect it.
    pub(crate) fn wait(mut self, deadline: Instant) -> Result<Vec<u8>, CaptureError> {
        loop {
            let Some(attempt) = self.attempt.take() else {
                // Unreachable by construction — attempt is always re-installed
                // before looping. Refuse rather than panic.
                return Err(CaptureError::Failed {
                    monitor: self.shot.monitor.bounds,
                    reason: "internal: capture attempt missing".into(),
                });
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            match attempt.rx.recv_timeout(remaining) {
                Ok(Ok(pixels)) => return Ok(pixels),
                Ok(Err(ShotFailure::DisplayChanged)) => return Err(CaptureError::DisplayChanged),
                Ok(Err(ShotFailure::Session(reason))) => {
                    // The pump thread reported and is exiting on its own; do
                    // not post to its (soon recyclable) thread id.
                    if self.retried {
                        return Err(CaptureError::Failed {
                            monitor: self.shot.monitor.bounds,
                            reason,
                        });
                    }
                    self.retried = true;
                    self.attempt = Some(spawn_attempt(
                        self.shot,
                        CursorCaptureSettings::Default,
                        DrawBorderSettings::Default,
                    )?);
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Channel open ⇒ senders alive ⇒ the pump thread still
                    // runs, so posting to its id is safe — and necessary, or
                    // the pump would leak.
                    post_quit(attempt.thread_id);
                    return Err(CaptureError::Timeout {
                        monitor: self.shot.monitor.bounds,
                        timeout_ms: u64::try_from(FIRST_FRAME_TIMEOUT.as_millis())
                            .unwrap_or(u64::MAX),
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // The thread died without reporting (a panic in the pump).
                    // It is gone — nothing to unwind, nothing to retry into.
                    return Err(CaptureError::Failed {
                        monitor: self.shot.monitor.bounds,
                        reason: "the capture thread ended unexpectedly".into(),
                    });
                }
            }
        }
    }
}

impl Drop for PendingShot {
    /// Unwinds the pump thread of a shot that was never collected — the
    /// early-return path when a *sibling* shot of the same capture failed.
    fn drop(&mut self) {
        if let Some(attempt) = self.attempt.take() {
            // Only post while the thread is provably alive (sender not yet
            // dropped, nothing reported). `sent` first: once it is true the
            // handler has reported and the pump is already unwinding itself.
            if !attempt.sent.load(Ordering::SeqCst)
                && !matches!(attempt.rx.try_recv(), Err(mpsc::TryRecvError::Disconnected))
            {
                post_quit(attempt.thread_id);
            }
        }
    }
}

/// Posts `WM_QUIT` to a pump thread, breaking its `GetMessageW` loop.
fn post_quit(thread_id: u32) {
    // SAFETY: posting a thread message has no memory-safety preconditions;
    // callers guarantee the id belongs to a live pump thread of ours.
    unsafe {
        PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
    }
}
