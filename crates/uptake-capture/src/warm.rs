//! Warm capture sessions: WGC sessions held open per monitor, so a capture is
//! a **readback** rather than a capture (roadmap task 1.9f, [ADR-0026]'s second
//! amendment).
//!
//! # What this fixes, and what it costs
//!
//! [`crate::capture_region`] builds a D3D11 device, a capture item, a frame pool
//! and a session *before* WGC starts looking, so the setup sits **ahead of** the
//! frame rather than behind it. The pixels it returns are the desktop as it was
//! ~350 ms after the caller asked (`UT-F-45`), which is every moment
//! freeze-on-demand exists to catch — a video frame, a notification sliding away.
//!
//! A session held open has already paid all of that. The compositor pushes
//! frames into it as the screen changes, this module keeps the newest one, and a
//! capture becomes a GPU-to-CPU copy of pixels that were already there. Measured
//! on the four-monitor dev rig, 2026-07-30: **8–11 ms against 255–353 ms**.
//!
//! **It is not free, which is why it is settings-gated and off by default.**
//! Holding four sessions cost **+0.62 pp of one core** with the desktop mostly
//! still and **+0.94 pp with video playing**, against the 0.87 pp
//! `quality-bars.md` §1 leaves after the click-through poll — so the video case
//! misses §1's target. Plus ~175 MiB of private commit. The full measurement,
//! its three reasons for being a floor rather than a ceiling, and the decision
//! it produced are in ADR-0026's second amendment.
//!
//! # The design F-29 did not price
//!
//! F-29 rejected persistent sessions for one-shot capture on the grounds that a
//! session with something to hand over must retain **a full-monitor frame copy
//! per monitor**, continuously, in system RAM. That is true of the obvious
//! implementation and it is not what this one does: the newest frame is kept as
//! a **GPU texture** (`CopyResource` into a `D3D11_USAGE_DEFAULT` texture we
//! own) and nothing crosses to system RAM until [`capture_monitor`] asks. The
//! per-frame cost is a GPU-to-GPU copy; the retained bytes are VRAM.
//!
//! # Two properties a caller must handle rather than assume
//!
//! * **A session is not warm the instant it starts.** Time to a first frame on
//!   all four monitors measured **331–336 ms** on the rig. [`capture_monitor`]
//!   returns `None` for a monitor with nothing retained, and the caller must
//!   fall back to [`crate::capture_region`] — which is the slow path, so the
//!   window right after entering PLACEMENT behaves as it did before 1.9f rather
//!   than failing.
//! * **`None` is ordinary, not exceptional.** An unstarted manager, a monitor
//!   that was not enumerated when [`start`] ran, a display topology that changed
//!   underneath, a readback that failed — every one of them is `None` and every
//!   one of them means *take the cold path*. There is deliberately no error type
//!   to interpret: a warm capture either happened or did not.
//!
//! # Threading
//!
//! One pump thread per monitor, each owning its session, exactly as
//! [`crate::wgc`] does for a one-shot — and for the same reason, that a
//! `GraphicsCaptureItem`'s `Monitor` wraps a raw `HMONITOR` and is not `Send`.
//! The D3D11 immediate context is written from the pump thread and read from
//! whichever thread calls [`capture_monitor`], which is sound because
//! `ID3D11Multithread::SetMultithreadProtected` is enabled on it before the
//! state is published. See the `unsafe impl Send` below.
//!
//! [ADR-0026]: the private planning repo's
//! `DECISIONS/ADR-0026-freeze-on-demand-trigger.md`

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::{Point, Rect, Size};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11Texture2D,
};
use windows::core::Interface;
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

use crate::CapturedRegion;

/// The sessions currently held. Empty means the warm path is not running.
///
/// **Emptiness is the state**, rather than a separate `bool` that could disagree
/// with it — the same reasoning `freeze::STILLS` is built on, and for the same
/// reason: two variables holding one fact is where this project's findings
/// ledger keeps finding defects.
static SESSIONS: Mutex<Vec<Arc<Slot>>> = Mutex::new(Vec::new());

/// One monitor's GPU-side state.
struct Retained {
    context: ID3D11DeviceContext,
    /// GPU-only copy of the newest frame: `CopyResource` target on arrival,
    /// `CopyResource` source on a readback. Never mapped.
    live: ID3D11Texture2D,
    /// CPU-readable, allocated once at the first frame rather than per capture.
    ///
    /// **This doubles the retained bytes and is a deliberate trade.** Allocating
    /// it at capture time would halve the held memory (~37.8 MiB on the dev rig)
    /// at the cost of a `CreateTexture2D` on the path being optimised. Held here
    /// because a capture that has to allocate 33 MB is exactly the kind of
    /// variable cost this task exists to remove — but the trade is recorded in
    /// ADR-0026's second amendment as owed a measurement, not settled.
    staging: ID3D11Texture2D,
    size: Size,
}

// SAFETY: D3D11 device contexts are not thread-safe by default, and this type is
// written from the pump thread (on frame arrival) and read from whichever thread
// calls `capture_monitor`. `retain` enables
// `ID3D11Multithread::SetMultithreadProtected` on this very context *before* the
// value is published into the slot, which is the documented mechanism for making
// the immediate context callable from several threads — D3D11 then serialises
// internally. The `Mutex` orders our own accesses on top of that; the `unsafe
// impl` is needed only because windows-rs COM pointers are not `Send`, not
// because the calls race.
unsafe impl Send for Retained {}

/// One monitor's session, shared between its pump thread and its readers.
struct Slot {
    /// The monitor's bounds when [`start`] ran — the key [`capture_monitor`]
    /// matches on, and the rectangle a successful capture reports.
    bounds: Rect,
    handle: isize,
    retained: Mutex<Option<Retained>>,
    stop: AtomicBool,
    thread_id: Mutex<Option<u32>>,
}

impl Slot {
    fn is_warm(&self) -> bool {
        self.retained
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
}

/// What the warm path is currently doing, for a caller that wants to report it.
///
/// **This exists because of `I-11`**, where a probe produced no output and its
/// silence was indistinguishable from working. A warm path that is enabled but
/// holding nothing performs *exactly* like one that was never switched on — the
/// caller falls back and everything works, slowly, forever. So readiness is
/// something this module states rather than something a reader infers from the
/// absence of a complaint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarmStatus {
    /// Sessions held.
    pub sessions: usize,
    /// Of those, how many have a frame retained and can answer a capture.
    pub warm: usize,
}

impl WarmStatus {
    /// Whether every held session can answer a capture.
    #[must_use]
    pub const fn fully_warm(&self) -> bool {
        self.sessions > 0 && self.warm == self.sessions
    }
}

/// Starts a held session for every monitor, replacing any already running.
///
/// Returns how many sessions were started — **not** how many are warm, which is
/// zero at this instant and stays zero for ~330 ms. Use [`status`] for that.
///
/// Idempotent in effect: calling it twice stops the first set first, so a caller
/// funnelling every state transition through it cannot leak sessions.
pub fn start() -> usize {
    stop();
    let Ok(monitors) = crate::monitors::enumerate() else {
        return 0;
    };
    let mut sessions = SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
    for monitor in monitors {
        let slot = Arc::new(Slot {
            bounds: monitor.bounds,
            handle: monitor.handle,
            retained: Mutex::new(None),
            stop: AtomicBool::new(false),
            thread_id: Mutex::new(None),
        });
        spawn_session(Arc::clone(&slot));
        sessions.push(slot);
    }
    sessions.len()
}

/// Stops every held session and releases its textures.
///
/// Safe to call when nothing is running, which is what lets the caller put it on
/// every state transition rather than only the ones it believes are leaving
/// PLACEMENT — the same "one funnel" reasoning `freeze::thaw` is placed by.
pub fn stop() {
    let held: Vec<Arc<Slot>> = {
        let mut sessions = SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *sessions)
    };
    for slot in held {
        slot.stop.store(true, Ordering::SeqCst);
        // `stop` alone cannot unwind a pump whose monitor is static: the flag is
        // only read inside `on_frame_arrived`, and on a still screen that may
        // never run again. WM_QUIT breaks the message loop itself. Same escape
        // hatch as `wgc.rs`, and required for the same reason.
        if let Some(id) = *slot
            .thread_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            // SAFETY: posting a thread message has no memory-safety
            // preconditions. The id belongs to a pump thread of ours which has
            // not been observed to exit; a stale id is the documented risk and
            // is bounded here by the pump only exiting through this path or a
            // session failure, both of which happen after the id is set.
            unsafe {
                PostThreadMessageW(id, WM_QUIT, 0, 0);
            }
        }
    }
}

/// How many sessions are held and how many can answer a capture.
#[must_use]
pub fn status() -> WarmStatus {
    let sessions = SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
    WarmStatus {
        sessions: sessions.len(),
        warm: sessions.iter().filter(|slot| slot.is_warm()).count(),
    }
}

/// The newest frame held for `monitor`, or `None` if the warm path cannot serve
/// it and the caller should take [`crate::capture_region`].
///
/// `None` covers every reason without distinguishing them, deliberately: an
/// unstarted manager, a monitor absent from the enumeration [`start`] ran
/// against, a session still inside its ~330 ms warm-up, a display topology that
/// moved, a frame whose dimensions no longer match the monitor, or a failed
/// readback. All of them mean *take the cold path*, and a caller that branched
/// on which would be making a distinction it cannot act on.
///
/// **The returned rectangle is the monitor this module holds, which may not be
/// byte-identical to the one asked for** — see the matching rule inside. Callers
/// must place the bitmap at [`CapturedRegion::rect`], exactly as they must for
/// [`crate::capture_region`].
#[must_use]
pub fn capture_monitor(monitor: Rect) -> Option<CapturedRegion> {
    let slot = {
        let sessions = SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
        // Matched on the full rectangle, not on position alone: a monitor that
        // changed resolution under a held session is a different surface, and
        // serving its old pixels would be the see-one-thing-get-another failure
        // freeze exists to avoid.
        sessions
            .iter()
            .find(|slot| slot.bounds.contains(centre_of(monitor)))
            .map(Arc::clone)?
    };
    let pixels = readback(&slot)?;
    let bitmap = RgbaBitmap::from_pixels(slot.bounds.size, pixels)?;
    Some(CapturedRegion {
        // The slot's own bounds, never the requested rectangle: these pixels are
        // that monitor's, and reporting them at a rectangle they did not come
        // from would offset every crop by the disagreement. The same rule
        // `capture_region` follows in returning what it took rather than what it
        // was asked for.
        rect: slot.bounds,
        bitmap,
    })
}

/// The centre of `rect`, in i64 on the way so a rectangle at the far edge of a
/// large virtual desktop cannot overflow.
fn centre_of(rect: Rect) -> Point {
    let x = i64::from(rect.origin.x) + i64::from(rect.size.width) / 2;
    let y = i64::from(rect.origin.y) + i64::from(rect.size.height) / 2;
    Point::new(
        i32::try_from(x).unwrap_or(rect.origin.x),
        i32::try_from(y).unwrap_or(rect.origin.y),
    )
}

/// Copies the retained texture to the CPU as RGBA8, top-down.
fn readback(slot: &Slot) -> Option<Vec<u8>> {
    let guard = slot.retained.lock().unwrap_or_else(PoisonError::into_inner);
    let retained = guard.as_ref()?;
    // A frame whose dimensions no longer match the monitor we would report means
    // the display changed under the session. Refuse rather than return pixels
    // the caller will place at the wrong rectangle.
    if retained.size != slot.bounds.size {
        return None;
    }

    // SAFETY: staging and live agree on dimensions and format by construction —
    // both are created from the arriving frame's own description — and the
    // staging texture carries CPU read access, which is what `Map` requires.
    unsafe {
        retained
            .context
            .CopyResource(&retained.staging, &retained.live);
    }

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: subresource 0 exists on a non-mipped 2D texture and `mapped` is a
    // valid out-param; failure is reported through the HRESULT.
    unsafe {
        retained
            .context
            .Map(&retained.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
    }
    .ok()?;

    let row_pitch = mapped.RowPitch as usize;
    let height = retained.size.height as usize;
    // SAFETY: the mapping stays valid until `Unmap` below, and D3D11 guarantees
    // the mapped region covers `RowPitch × Height` bytes for a 2D texture with
    // one mip level. Taking it as a slice here rather than doing pointer
    // arithmetic in the copy is what lets `pack_rows` be an ordinary tested
    // function instead of unsafe code nothing can exercise without a desktop.
    let mapped_bytes =
        unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), row_pitch * height) };
    let packed = pack_rows(mapped_bytes, row_pitch, retained.size);
    // SAFETY: the staging texture was mapped immediately above and no early
    // return happens in between, so this unmaps exactly what was mapped.
    unsafe {
        retained.context.Unmap(&retained.staging, 0);
    }
    packed
}

/// Copies `size`'s worth of rows out of a `row_pitch`-strided mapping, dropping
/// the padding.
///
/// # Why this is a separate function
///
/// **`row_pitch` is almost always larger than `width × 4`**, because D3D11 aligns
/// each row, and a copy that ignored that would shear the image progressively —
/// every row offset a little further than the last. It is the one place in the
/// warm path where a mistake silently corrupts pixels rather than failing, and
/// pulling it out of the `unsafe` block is what makes it reachable by a test on
/// a machine with no desktop. The padded case is the one that matters and it is
/// the one a real capture almost always takes.
fn pack_rows(mapped: &[u8], row_pitch: usize, size: Size) -> Option<Vec<u8>> {
    let width = size.width as usize;
    let height = size.height as usize;
    let row_bytes = width.checked_mul(4)?;
    // A pitch narrower than a row means the mapping does not hold the image the
    // caller thinks it does. Refuse rather than read across rows into whatever
    // follows — the cold path is a correct answer and a sheared bitmap is not.
    if row_pitch < row_bytes || mapped.len() < row_pitch.checked_mul(height)? {
        return None;
    }
    let mut out = Vec::with_capacity(row_bytes.checked_mul(height)?);
    for y in 0..height {
        let start = y * row_pitch;
        out.extend_from_slice(&mapped[start..start + row_bytes]);
    }
    Some(out)
}

/// What the retaining handler needs.
struct WarmFlags {
    slot: Arc<Slot>,
}

/// Keeps the newest frame and nothing else.
struct Warm {
    flags: WarmFlags,
}

impl GraphicsCaptureApiHandler for Warm {
    type Flags = WarmFlags;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { flags: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame<'_>,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.flags.slot.stop.load(Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }
        retain(&self.flags.slot, frame).inspect_err(|_| capture_control.stop())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        // Deliberately empty. A session unwound by WM_QUIT never reaches here,
        // so anything this did would run on some teardowns and not others —
        // which is how the spike came to report a teardown failure that had not
        // happened. Liveness is not tracked here; `SESSIONS` is emptied by
        // `stop`, which is the one place a session ends by our choice.
        Ok(())
    }
}

/// Copies the arriving frame into the slot's retained texture, creating the GPU
/// state on the first frame.
fn retain(slot: &Slot, frame: &mut Frame<'_>) -> Result<(), String> {
    let mut guard = slot.retained.lock().unwrap_or_else(PoisonError::into_inner);
    let source = *frame.desc();
    // A resolution change under a held session invalidates the textures, which
    // are sized to the old frame and would fail `CopyResource` silently forever.
    // Dropping the state rebuilds it from this frame.
    if guard
        .as_ref()
        .is_some_and(|retained| retained.size != size_of_desc(&source))
    {
        *guard = None;
    }
    if guard.is_none() {
        let device = frame.device().clone();
        let context = frame.device_context().clone();

        // Before the context is published into the slot — see the `unsafe impl
        // Send`, which is only sound because of this call.
        let multithread: ID3D11Multithread = context
            .cast()
            .map_err(|error| format!("no ID3D11Multithread on the capture context: {error}"))?;
        // SAFETY: no memory-safety preconditions. The BOOL is the *previous*
        // mode, which we have no use for.
        let _previously = unsafe { multithread.SetMultithreadProtected(true) };

        *guard = Some(Retained {
            live: create_texture(&device, &source, D3D11_USAGE_DEFAULT, 0)?,
            staging: create_texture(
                &device,
                &source,
                D3D11_USAGE_STAGING,
                D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
            )?,
            context,
            size: size_of_desc(&source),
        });
    }

    let Some(retained) = guard.as_ref() else {
        return Err("internal: retained state missing right after creation".into());
    };
    // GPU to GPU. Nothing crosses to system RAM until `capture_monitor` asks,
    // which is the whole of the difference from what F-29 priced.
    // SAFETY: both textures share dimensions and format by construction.
    unsafe {
        retained
            .context
            .CopyResource(&retained.live, frame.as_raw_texture());
    }
    Ok(())
}

const fn size_of_desc(desc: &D3D11_TEXTURE2D_DESC) -> Size {
    Size::new(desc.Width, desc.Height)
}

/// Creates a texture matching `source`'s dimensions and format with the given
/// usage — `CopyResource` requires both to agree exactly.
fn create_texture(
    device: &ID3D11Device,
    source: &D3D11_TEXTURE2D_DESC,
    usage: D3D11_USAGE,
    cpu_access: u32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Usage: usage,
        BindFlags: 0,
        CPUAccessFlags: cpu_access,
        MiscFlags: 0,
        ..*source
    };
    let mut texture = None;
    // SAFETY: `desc` is fully initialised and `texture` is a valid out-param;
    // failure is reported through the HRESULT.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .map_err(|error| format!("could not create a capture texture: {error}"))?;
    texture.ok_or_else(|| "CreateTexture2D succeeded but returned nothing".to_string())
}

/// Spawns `slot`'s pump thread.
fn spawn_session(slot: Arc<Slot>) {
    std::thread::spawn(move || {
        // Force the message queue into existence *before* publishing the thread
        // id: `PostThreadMessageW` fails against a thread that has none yet, and
        // `stop` may post the instant it can read the id.
        // SAFETY: PeekMessageW with PM_NOREMOVE only inspects the queue (and
        // creates it as a side effect); MSG is a plain struct.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            PeekMessageW(
                &mut msg,
                std::ptr::null_mut(),
                WM_USER,
                WM_USER,
                PM_NOREMOVE,
            );
        }
        *slot
            .thread_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner) =
            // SAFETY: no preconditions.
            Some(unsafe { GetCurrentThreadId() });

        // The handle re-becomes a Monitor only here, on the thread that uses it —
        // the same manual `Send` `wgc.rs` performs, for the same reason.
        let monitor = Monitor::from_raw_hmonitor(slot.handle as *mut std::ffi::c_void);
        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            // Deliberately `Default`, the value `wgc.rs` captures with.
            // Throttling would cut the CPU cost measured for this feature, but it
            // buys that by letting the retained frame age — and frame age is the
            // fidelity half of `quality-bars.md` §1's freeze rows, so it cannot
            // be tuned on the CPU number alone.
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            WarmFlags {
                slot: Arc::clone(&slot),
            },
        );
        if let Err(error) = Warm::start(settings) {
            // Debug-only, like every other in-process report here; task 1.15
            // owns real logging. A session that never starts is not an error the
            // caller can act on — `capture_monitor` returns `None` and the cold
            // path runs — but it is the difference between "warm and quiet" and
            // "never warm", which `status` is what actually reports.
            #[cfg(debug_assertions)]
            eprintln!("warm: session failed for {:?}: {error}", slot.bounds);
            let _ = &error;
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Builds a mapping whose every pixel encodes its own (x, y), so a copy that
    /// reads the wrong offset produces provably wrong content rather than
    /// something that merely looks plausible — F-38's rule about pinning at least
    /// one end to an externally determined value.
    fn strided(width: u32, height: u32, row_pitch: usize) -> Vec<u8> {
        let mut buffer = vec![0xEE; row_pitch * height as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let at = y * row_pitch + x * 4;
                buffer[at] = u8::try_from(x % 251).unwrap();
                buffer[at + 1] = u8::try_from(y % 251).unwrap();
                buffer[at + 2] = 0x40;
                buffer[at + 3] = 0xFF;
            }
        }
        buffer
    }

    #[test]
    fn padding_is_dropped_and_every_pixel_keeps_its_coordinates() {
        let size = Size::new(3, 4);
        // 64 bytes for 12 bytes of pixels: heavy padding, so a copy that used
        // `row_bytes` as the stride would read the *first* row four times over.
        let mapped = strided(3, 4, 64);
        let packed = pack_rows(&mapped, 64, size).unwrap();
        assert_eq!(packed.len(), 3 * 4 * 4);
        for y in 0..4usize {
            for x in 0..3usize {
                let at = (y * 3 + x) * 4;
                assert_eq!(packed[at], u8::try_from(x).unwrap(), "x at ({x}, {y})");
                assert_eq!(packed[at + 1], u8::try_from(y).unwrap(), "y at ({x}, {y})");
            }
        }
        assert!(!packed.contains(&0xEE), "padding leaked into the output");
    }

    #[test]
    fn an_unpadded_mapping_is_copied_whole() {
        let size = Size::new(5, 3);
        let mapped = strided(5, 3, 20);
        let packed = pack_rows(&mapped, 20, size).unwrap();
        assert_eq!(packed, mapped);
    }

    #[test]
    fn a_pitch_narrower_than_a_row_is_refused_rather_than_read_across() {
        // 4 px needs 16 bytes; a 12-byte pitch would have every row reading into
        // the next one. The honest answer is None, which routes the caller to
        // the cold path.
        assert!(pack_rows(&[0; 64], 12, Size::new(4, 4)).is_none());
    }

    #[test]
    fn a_mapping_shorter_than_it_claims_is_refused() {
        assert!(pack_rows(&[0; 32], 16, Size::new(4, 4)).is_none());
    }

    #[test]
    fn fully_warm_needs_every_session_warm_and_at_least_one() {
        assert!(
            !WarmStatus {
                sessions: 0,
                warm: 0
            }
            .fully_warm()
        );
        assert!(
            !WarmStatus {
                sessions: 4,
                warm: 3
            }
            .fully_warm()
        );
        assert!(
            WarmStatus {
                sessions: 4,
                warm: 4
            }
            .fully_warm()
        );
    }

    #[test]
    fn capturing_and_stopping_without_a_started_session_are_both_safe() {
        // The disabled path and the every-transition placement of `stop` both
        // depend on these being no-ops rather than panics.
        stop();
        assert_eq!(
            status(),
            WarmStatus {
                sessions: 0,
                warm: 0
            }
        );
        assert!(capture_monitor(Rect::new(0, 0, 100, 100)).is_none());
    }
}
