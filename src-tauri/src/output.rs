//! The output pipeline (roadmap task 1.9): `capture_region` → PNG → clipboard,
//! and a separate, explicit save-to-file action.
//!
//! Every decision here was settled before this module was written — see
//! `PRODUCT-VISION.md` §8 in the private planning repo — so nothing below is a
//! product choice: **clipboard only by default**, publishing both `CF_DIBV5`
//! and the registered `"PNG"` format (Office and most desktop apps read DIB;
//! browsers, Discord and Figma prefer PNG); saving is a **separate, explicit**
//! action that writes to `Pictures\UP-TAKE\`, never invoked implicitly by a
//! copy.
//!
//! # Why this never runs on the mouse-hook thread
//!
//! Both actions are reached from the area menu (`placement.rs`), whose
//! release handling normally runs synchronously inside the `WH_MOUSE_LL`
//! callback on the event-loop thread. A capture is ~100–300 ms even warm
//! (`uptake_capture` crate docs, F-29) — far past what a low-level hook
//! callback can spend before Windows silently removes it
//! (`LowLevelHooksTimeout`, the same failure class as F-33). [`copy_to_clipboard`]
//! and [`save_to_file`] are therefore designed to be called from a **freshly
//! spawned thread**, never from the hook callback itself; `placement.rs` owns
//! that spawn. Any thread may call `uptake_capture::capture_region` (its own
//! contract), and the clipboard is opened against the overlay's `HWND` — a
//! plain field read on tao's side, safe from any thread — so nothing below is
//! thread-affine.

use std::fs;
use std::path::PathBuf;
use std::ptr;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use tauri::{AppHandle, Manager};
use uptake_core::area::AreaId;
use uptake_core::bitmap::RgbaBitmap;
use uptake_core::geometry::Rect;

use windows_capture::encoder::{ImageEncoder, ImageEncoderPixelFormat, ImageFormat};
use windows_sys::Win32::Foundation::{GlobalFree, HWND, SYSTEMTIME};
use windows_sys::Win32::Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER, LCS_GM_IMAGES};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

use crate::captures::CaptureStore;

/// The value Windows defines for `LCS_sRGB` (`wingdi.h`) — not exported by
/// `windows-sys`'s `Gdi` module under any name (only the `LCS_GM_*` rendering
/// intents are), so pinned here as the documented, ABI-stable FourCC it is.
const LCS_S_RGB: u32 = 0x7352_4742;

/// §1's selection→clipboard budget: a soft target and a hard fail (ms).
const BUDGET_TARGET_MS: u128 = 300;
const BUDGET_HARD_FAIL_MS: u128 = 600;

/// The pixels an export should use for `area`: its pinned capture if it has one,
/// otherwise a fresh capture of `bounds`.
///
/// # A pinned area exports what it is showing, not what is under it
///
/// **This is the fix for a defect found on the rig 2026-07-27.** Both export
/// actions used to capture live, unconditionally. While areas could not move that
/// was indistinguishable from correct — a passive `Screenshot` pins the instant it
/// is created, so its rectangle still held the same pixels. Task 1.17(a) added
/// move and resize in LIVING, and from then on Copy on a moved Screenshot area
/// returned **the desktop underneath its new position**, cropped to the area's
/// size: a screenshot of whatever the area happened to be sitting on.
///
/// The failure is quiet, which is what makes it bad. The result is a plausible
/// image of the right dimensions, so nothing looks broken until you look at what
/// you pasted.
///
/// A `Default` area has no capture and falls through to the live path, which is
/// the only thing Copy can mean for a bare claimed rectangle.
fn export_source(
    app: &AppHandle,
    area: AreaId,
    bounds: Rect,
    split: &mut Split,
) -> Result<(RgbaBitmap, Vec<u8>), String> {
    if let Some(pinned) = crate::captures::pinned_capture(app, area) {
        // Capture and encode both stay at 0 ms, which is the honest reading: this
        // path does neither. It also keeps the §1 budget lines meaningful — a
        // pinned export is not a measurement of the capture pipeline.
        return Ok(pinned);
    }
    capture(bounds, split)
}

/// Publishes an area's image to the clipboard alone.
///
/// PRODUCT-VISION §8: an area's source is still on screen, so re-copying is
/// one gesture — the justification ShareX/Snipping Tool have for also
/// writing a file (a screenshot is transient) does not transfer here, and the
/// cost (an area copied forty times is forty stray PNGs) stays. Nothing is
/// written to disk by this path.
pub(crate) fn copy_to_clipboard(app: &AppHandle, area: AreaId, bounds: Rect) {
    let started = Instant::now();
    let mut split = Split::default();
    let outcome = export_source(app, area, bounds, &mut split).and_then(|(bitmap, png)| {
        // Both buffers are built *before* the clipboard is opened. The
        // clipboard is a global system resource — every other process's
        // access blocks while it is held — so no per-pixel work belongs
        // inside the bracket, and a 4K-sized area is a 33 MB conversion.
        let publish = Instant::now();
        let result =
            dibv5_bytes(&bitmap).and_then(|dib| publish_clipboard(overlay_hwnd(app)?, &dib, &png));
        split.publish_ms = publish.elapsed().as_millis();
        result
    });
    // Acknowledge the action on screen. Only on success: a flash after a
    // failed Copy would be a lie the user has no way to check.
    if outcome.is_ok() {
        crate::overlay::emit_flash(app, area);
    }
    report("copy", started, &split, outcome);
}

/// Writes an area's image to `Pictures\UP-TAKE\`, creating the directory on first
/// use. A separate, explicit action (PRODUCT-VISION §8) — does not also touch the
/// clipboard.
pub(crate) fn save_to_file(app: &AppHandle, area: AreaId, bounds: Rect) {
    let started = Instant::now();
    let mut split = Split::default();
    let outcome = export_source(app, area, bounds, &mut split).and_then(|(_bitmap, png)| {
        let publish = Instant::now();
        let result = write_file(app, &png);
        split.publish_ms = publish.elapsed().as_millis();
        result
    });
    if outcome.is_ok() {
        crate::overlay::emit_flash(app, area);
    }
    report("save", started, &split, outcome);
}

/// Where the selection→clipboard time actually goes, per stage (ms).
///
/// **Owed to task 1.9c by [ADR-0022].** Task 1.9's rig pass timed only the
/// total, and the ADR's premise — that capture is ~65–70 % of it — was derived
/// by *subtracting* the Copy and Save figures from each other rather than
/// measured. 1.9c is a capture-side fix and would be worthless if the encode or
/// the clipboard publish turned out to dominate instead, so the split is
/// recorded rather than inferred.
///
/// [ADR-0022]: the private planning repo's
/// `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`
#[derive(Default)]
struct Split {
    /// `uptake_capture::capture_region` — the stage 1.9c removes from the
    /// measured interval.
    capture_ms: u128,
    /// PNG encode.
    encode_ms: u128,
    /// Whatever the action does with the bytes: the DIB conversion plus the
    /// clipboard bracket for Copy, the file write for Save.
    publish_ms: u128,
    /// Where the pixels came from. Reported on every line, because
    /// `capture 0 ms` is ambiguous on its own — it is what both the fast path
    /// and a pinned export produce — and "which path ran" is the first question
    /// asked of any 1.9c latency number.
    source: Source,
}

/// Which of the routes to a Screenshot's pixels actually ran.
#[derive(Default, Clone, Copy)]
enum Source {
    /// A live `capture_region` with no fast path attempted: the pinned export,
    /// and every action that is not a create.
    #[default]
    Live,
    /// Cropped out of the frame held since mouse-down (task 1.9c).
    Held,
    /// Cropped out of the frozen still the user was looking at (task 1.9d).
    ///
    /// Distinct from [`Self::Held`] in the log even though both are crops of a
    /// full-monitor frame, because they answer different questions: `Held` may
    /// legitimately decline on staleness and `Frozen` never can, so a run where
    /// the two are conflated cannot be read.
    Frozen,
    /// The fast path was attempted and declined; the reason is why.
    Fell(crate::precapture::Fallback),
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live => write!(f, "live capture"),
            Self::Held => write!(f, "held frame"),
            Self::Frozen => write!(f, "frozen still"),
            Self::Fell(reason) => write!(f, "live capture — fell back: {reason}"),
        }
    }
}

/// Captures `bounds` and encodes it as PNG, returning both the raw bitmap
/// (needed for the DIB clipboard format, which is not decoded back out of the
/// PNG) and the encoded bytes, and recording each stage's cost in `split`.
fn capture(bounds: Rect, split: &mut Split) -> Result<(RgbaBitmap, Vec<u8>), String> {
    let grab = Instant::now();
    let captured = uptake_capture::capture_region(bounds).map_err(|error| error.to_string())?;
    split.capture_ms = grab.elapsed().as_millis();
    let encode = Instant::now();
    let png = encode_png(&captured.bitmap)?;
    split.encode_ms = encode.elapsed().as_millis();
    Ok((captured.bitmap, png))
}

/// Task 1.9c's fast path: crop the frame held since mouse-down, or capture.
///
/// **The fallback is the same `capture` call, unchanged.** That is deliberate
/// and is what keeps 1B's exit-gate row — "the paths that can produce a
/// Screenshot's pixels produce identical results" — something the code enforces
/// rather than something a test has to chase: there is one capture path and one
/// crop, and the crop is a byte-for-byte copy of a sub-rectangle
/// (`RgbaBitmap::crop`, which refuses anything that does not fit rather than
/// clamping). The two differ only in *when* the pixels were taken, which is the
/// semantic ADR-0022 §3 bounded at 200 ms and nothing else.
///
/// Every decline is logged with its reason. A silent fall back to the slow path
/// would show up as nothing worse than "1.9c did not help much", which is the
/// hardest kind of regression to notice.
fn capture_or_crop(bounds: Rect, split: &mut Split) -> Result<(RgbaBitmap, Vec<u8>), String> {
    // The frozen still wins when the screen is frozen, and this ordering is
    // ADR-0026 decision 6: **what you see at release is what you get.** The
    // frame source is resolved here, at mouse-up, rather than at mouse-down —
    // so a user who freezes part-way through a drag gets the pixels they are
    // now looking at. Resolving it earlier would hand back the pre-capture's
    // live pixels while the screen showed a still, which is precisely the
    // divergence 1B's exit-gate row 2 exists to catch.
    //
    // There is no staleness test on this branch and that is not an oversight:
    // the user is selecting *on* the displayed image, so it cannot disagree
    // with what they see however long they take (ADR-0022 §5).
    if let Some(bitmap) = crate::freeze::crop(bounds) {
        split.source = Source::Frozen;
        let encode = Instant::now();
        let png = encode_png(&bitmap)?;
        split.encode_ms = encode.elapsed().as_millis();
        return Ok((bitmap, png));
    }
    match crate::precapture::take(bounds) {
        Ok(bitmap) => {
            // Capture stays at 0 ms, and that is the honest reading rather than
            // a flattering one: the §1 row measures selection release → the
            // clipboard, and on this path no capture happens inside it. The
            // pre-capture's own cost is real but was paid during the drag,
            // before the interval starts.
            split.source = Source::Held;
            let encode = Instant::now();
            let png = encode_png(&bitmap)?;
            split.encode_ms = encode.elapsed().as_millis();
            Ok((bitmap, png))
        }
        Err(reason) => {
            split.source = Source::Fell(reason);
            capture(bounds, split)
        }
    }
}

/// Captures `bounds` for a newly created area: publishes to the clipboard
/// (PRODUCT-VISION §8 — clipboard only) **and** pins the PNG for the area to
/// render (ADR-0014 §6, the Snipaste pin).
///
/// # The seam task 1.9c changed
///
/// [ADR-0022] settles that §1's selection→clipboard budget is met by *holding a
/// full-monitor frame and cropping it*, not by making capture faster. Task 1.9b
/// confined frame acquisition to [`capture`] so that 1.9c would be a seam change
/// and not surgery, and it was: this function's only change was calling
/// [`capture_or_crop`] instead, and nothing below ever assumed the pixels were
/// captured *now*. The pre-capture itself is `crate::precapture`.
///
/// # Threading
///
/// Spawns, and the spawn is not optional. The only caller is
/// `placement::finish_gesture`, which runs inside the `WH_MOUSE_LL` callback,
/// and a capture is 100–300 ms even warm — far past `LowLevelHooksTimeout`, the
/// failure class F-33 found the hard way.
///
/// [ADR-0022]: the private planning repo's
/// `DECISIONS/ADR-0022-hold-a-frame-and-crop.md`
pub(crate) fn capture_into_area(app: &AppHandle, id: AreaId, bounds: Rect) {
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut split = Split::default();
        let outcome = capture_or_crop(bounds, &mut split).and_then(|(bitmap, png)| {
            let publish = Instant::now();
            // The pin is stored *and announced* before the clipboard is touched:
            // it is the thing the user can see, and a clipboard failure should
            // still leave a visible capture on screen rather than an empty area.
            //
            // **The announcement used to come after the clipboard work, which
            // silently voided that promise.** `dibv5_bytes(&bitmap)?` and
            // `overlay_hwnd(&app)?` both return early from this closure, so
            // either failure stored the bytes and never told the frontend they
            // existed — an area left permanently blank, with the pixels sitting
            // in the store, and a comment below asserting the opposite. Ordering
            // the emit first makes the promise structural instead of a claim.
            let version = {
                let store = app.state::<Mutex<CaptureStore>>();
                let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
                guard.insert(id, bitmap.clone(), png.clone())
            };
            if let Err(error) = crate::overlay::emit_pin(&app, id, version) {
                eprintln!("output: pinned the capture but could not announce it: {error}");
            }
            // Recorded before the `?`s below, so a failure *inside* publishing is
            // not reported as "publish 0 ms".
            let published = dibv5_bytes(&bitmap)
                .and_then(|dib| publish_clipboard(overlay_hwnd(&app)?, &dib, &png));
            split.publish_ms = publish.elapsed().as_millis();
            published
        });
        report("capture", started, &split, outcome);
    });
}

/// Encodes RGBA8 pixels as PNG via the same WIC-backed encoder
/// `uptake-capture`'s own hardware-verification driver uses
/// (`examples/grab.rs`) — reused rather than adding a second PNG codec to vet.
pub(crate) fn encode_png(bitmap: &RgbaBitmap) -> Result<Vec<u8>, String> {
    encode_as(bitmap, ImageFormat::Png, "PNG")
}

/// Encodes for the **freeze display path**, in whatever format
/// [`crate::freeze::display_format`] selected.
///
/// Separate from [`encode_png`] on purpose. Copy and Save keep PNG
/// unconditionally, because there the bytes *are* the product and a lossy
/// export would be a defect; here the bytes are only what the WebView paints
/// while the user selects, and the crop comes from the lossless bitmap. **One
/// function serving both would make that distinction a comment instead of a
/// type.**
pub(crate) fn encode_for_display(bitmap: &RgbaBitmap) -> Result<Vec<u8>, String> {
    let (format, _, name) = crate::freeze::display_format();
    encode_as(bitmap, format, name)
}

fn encode_as(bitmap: &RgbaBitmap, format: ImageFormat, name: &str) -> Result<Vec<u8>, String> {
    ImageEncoder::new(format, ImageEncoderPixelFormat::Rgba8)
        .map_err(|error| format!("could not create the {name} encoder: {error}"))?
        .encode(bitmap.pixels(), bitmap.width(), bitmap.height())
        .map_err(|error| format!("could not encode {name}: {error}"))
}

/// Logs the outcome and — separately — a budget overrun against §1's
/// selection→clipboard numbers. Instrumented inline rather than asserted
/// after the fact (F-29's lesson): a capture alone measured ~190–230 ms warm,
/// leaving little room before the 300 ms target and less before the 600 ms
/// hard fail.
fn report(action: &str, started: Instant, split: &Split, outcome: Result<(), String>) {
    let elapsed = started.elapsed().as_millis();
    // The split rides along with every budget line, not just the debug one:
    // the whole point (ADR-0022) is that "it was 320 ms" is not actionable and
    // "capture 210, encode 25, publish 80" is.
    let stages = format!(
        "capture {} ms, encode {} ms, publish {} ms ({})",
        split.capture_ms, split.encode_ms, split.publish_ms, split.source
    );
    match outcome {
        Ok(()) => {
            if elapsed > BUDGET_HARD_FAIL_MS {
                eprintln!(
                    "output: {action} took {elapsed} ms — over the §1 hard-fail budget ({BUDGET_HARD_FAIL_MS} ms) — {stages}"
                );
            } else if elapsed > BUDGET_TARGET_MS {
                eprintln!(
                    "output: {action} took {elapsed} ms — over the §1 target ({BUDGET_TARGET_MS} ms), within the hard fail — {stages}"
                );
            }
            #[cfg(debug_assertions)]
            eprintln!("output: {action} finished in {elapsed} ms — {stages}");
        }
        Err(error) => eprintln!("output: {action} failed after {elapsed} ms: {error} — {stages}"),
    }
}

// ---------------------------------------------------------------------------
// Clipboard: both CF_DIBV5 and the registered "PNG" format (PRODUCT-VISION §8).
// ---------------------------------------------------------------------------

/// The overlay window's handle, to own the clipboard with.
///
/// Resolving it is a `HashMap` lookup plus a plain field read on tao's side
/// (`Window::hwnd` returns `self.window.0` — no event-loop dispatch), so this
/// is safe from the spawned thread these actions run on.
fn overlay_hwnd(app: &AppHandle) -> Result<HWND, String> {
    Ok(crate::overlay::overlay_window(app)?
        .hwnd()
        .map_err(|error| format!("could not get the overlay window handle: {error}"))?
        .0)
}

/// Opens the clipboard against `owner`, empties it, publishes both formats,
/// and closes it.
///
/// **`owner` must not be null.** `OpenClipboard(NULL)` would associate the
/// open clipboard with the current task rather than a window, which is
/// convenient on a spawned thread but out of contract: `EmptyClipboard`'s
/// documented remarks state that a NULL window handle "sets the clipboard
/// owner to NULL. Note that this causes `SetClipboardData` to fail." It does
/// not fail in practice on Windows 11 — the first cut of this module shipped
/// that way and was verified working on the rig — but relying on documented-
/// to-fail behaviour is the shape F-25 and F-33 both took, so the overlay's
/// own `HWND` is passed instead. Nothing is delay-rendered, so being the
/// clipboard owner costs the overlay nothing: it receives
/// `WM_DESTROYCLIPBOARD`, which `DefWindowProc` ignores.
fn publish_clipboard(owner: HWND, dib: &[u8], png: &[u8]) -> Result<(), String> {
    // SAFETY: `OpenClipboard`/`CloseClipboard` bracket every clipboard call
    // below; `owner` is a live top-level window handle owned by this process.
    let opened = unsafe { OpenClipboard(owner) };
    if opened == 0 {
        return Err("could not open the clipboard".to_string());
    }
    let result = (|| {
        // SAFETY: the clipboard is open, per the check above.
        if unsafe { EmptyClipboard() } == 0 {
            return Err("could not empty the clipboard".to_string());
        }
        set_clipboard_data(CF_DIBV5, dib)?;
        set_clipboard_png(png)?;
        Ok(())
    })();
    // SAFETY: matches the successful `OpenClipboard` above, on every path.
    unsafe {
        CloseClipboard();
    }
    result
}

/// Allocates a movable global block, copies `data` into it, and hands it to
/// the clipboard under `format`.
///
/// `SetClipboardData` takes ownership of the handle **only on success** — the
/// system frees it when the clipboard is next emptied or another app closes
/// it. On failure the caller still owns it and must free it, which is exactly
/// what the `Err` arm below does; a leak-on-failure clipboard helper would be
/// the kind of bug that only shows up as slowly rising handle counts.
fn set_clipboard_data(format: u32, data: &[u8]) -> Result<(), String> {
    // SAFETY: `size` is `data.len()`, a valid allocation request; the handle
    // is either handed to `SetClipboardData` (which then owns it) or freed
    // below on every other path.
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len()) };
    if handle.is_null() {
        return Err(format!(
            "could not allocate clipboard memory for format {format}"
        ));
    }
    // SAFETY: `handle` was just allocated and is not yet shared with anything
    // else; the lock is released before `SetClipboardData`, which is required
    // — the system must be able to lock the handle itself.
    let locked = unsafe { GlobalLock(handle) };
    if locked.is_null() {
        unsafe {
            GlobalFree(handle);
        }
        return Err(format!(
            "could not lock clipboard memory for format {format}"
        ));
    }
    // SAFETY: `locked` points at an allocation of exactly `data.len()` bytes,
    // just locked above.
    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), locked.cast::<u8>(), data.len());
        GlobalUnlock(handle);
    }
    // SAFETY: `handle` is valid, unlocked, and sized to `data`.
    let published = unsafe { SetClipboardData(format, handle) };
    if published.is_null() {
        // Ours again — the system never took it.
        unsafe {
            GlobalFree(handle);
        }
        return Err(format!("could not publish clipboard format {format}"));
    }
    Ok(())
}

/// Predefined clipboard format for a `BITMAPV5HEADER`-based DIB. Not exported
/// by `windows-sys`'s `DataExchange` module (the predefined `CF_*` constants
/// live with the GDI bitmap types instead, and this one specifically is
/// absent from both) — the numeric value is stable ABI, documented in
/// `winuser.h` and unchanged since Windows 2000.
const CF_DIBV5: u32 = 17;

/// Builds a `BITMAPV5HEADER` DIB with a true alpha channel, as the packed
/// bytes `CF_DIBV5` expects: header immediately followed by the pixels.
///
/// DIBs are conventionally bottom-up for clipboard compatibility (a negative,
/// top-down height is valid GDI but not every consumer handles it), so the
/// rows are flipped here; `uptake_core::bitmap::RgbaBitmap` is top-down by
/// contract. `BI_BITFIELDS` with an explicit alpha mask — rather than plain
/// `BI_RGB` — is what makes a partially transparent capture (the dead zones
/// between mismatched monitors, `uptake-capture`'s crate docs) paste with its
/// transparency intact instead of forced opaque.
fn dibv5_bytes(bitmap: &RgbaBitmap) -> Result<Vec<u8>, String> {
    let width = i32::try_from(bitmap.width()).map_err(|_| "capture width overflows a DIB")?;
    let height = i32::try_from(bitmap.height()).map_err(|_| "capture height overflows a DIB")?;
    let pixels = bottom_up_bgra(bitmap);
    // Zero is documented as acceptable only for `BI_RGB` ("This may be set to
    // zero for BI_RGB bitmaps") and this DIB is `BI_BITFIELDS`, so a consumer
    // deriving the pixel extent from the header gets the real size.
    let size_image =
        u32::try_from(pixels.len()).map_err(|_| "capture overflows a DIB's image size")?;
    let header = BITMAPV5HEADER {
        bV5Size: u32::try_from(size_of::<BITMAPV5HEADER>()).unwrap_or_default(),
        bV5Width: width,
        // Positive: bottom-up, matching the flipped pixel order below.
        bV5Height: height,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5SizeImage: size_image,
        bV5XPelsPerMeter: 0,
        bV5YPelsPerMeter: 0,
        bV5ClrUsed: 0,
        bV5ClrImportant: 0,
        bV5RedMask: 0x00FF_0000,
        bV5GreenMask: 0x0000_FF00,
        bV5BlueMask: 0x0000_00FF,
        bV5AlphaMask: 0xFF00_0000,
        bV5CSType: LCS_S_RGB,
        bV5Endpoints: unsafe { std::mem::zeroed() },
        bV5GammaRed: 0,
        bV5GammaGreen: 0,
        bV5GammaBlue: 0,
        bV5Intent: LCS_GM_IMAGES as u32,
        bV5ProfileData: 0,
        bV5ProfileSize: 0,
        bV5Reserved: 0,
    };
    // SAFETY: `BITMAPV5HEADER` is a `repr(C)` plain-old-data struct with no
    // padding at 4-byte alignment; reading its own storage as bytes for its
    // own size cannot read out of bounds.
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            (&raw const header).cast::<u8>(),
            size_of::<BITMAPV5HEADER>(),
        )
    };
    let mut dib = Vec::with_capacity(header_bytes.len() + pixels.len());
    dib.extend_from_slice(header_bytes);
    dib.extend_from_slice(&pixels);
    Ok(dib)
}

/// `bitmap`'s pixels as bottom-up `B, G, R, A` — the DIB's on-the-wire order
/// for `bV5RedMask = 0x00FF0000` and friends, and its conventional row order.
fn bottom_up_bgra(bitmap: &RgbaBitmap) -> Vec<u8> {
    let width = bitmap.width() as usize;
    let height = bitmap.height() as usize;
    let src = bitmap.pixels();
    let mut out = vec![0_u8; src.len()];
    for row in 0..height {
        let src_row = &src[row * width * 4..(row + 1) * width * 4];
        let dest_start = (height - 1 - row) * width * 4;
        let dest_row = &mut out[dest_start..dest_start + width * 4];
        for (src_px, dest_px) in src_row.chunks_exact(4).zip(dest_row.chunks_exact_mut(4)) {
            dest_px[0] = src_px[2]; // B
            dest_px[1] = src_px[1]; // G
            dest_px[2] = src_px[0]; // R
            dest_px[3] = src_px[3]; // A
        }
    }
    out
}

/// Publishes the encoded PNG bytes under the registered `"PNG"` clipboard
/// format — the name browsers, Discord and Figma look for.
fn set_clipboard_png(png: &[u8]) -> Result<(), String> {
    let name: Vec<u16> = "PNG\0".encode_utf16().collect();
    // SAFETY: `name` is a valid, NUL-terminated UTF-16 string for the
    // duration of this call.
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    if format == 0 {
        return Err("could not register the PNG clipboard format".to_string());
    }
    set_clipboard_data(format, png)
}

// ---------------------------------------------------------------------------
// Save to file: Pictures\UP-TAKE\, timestamp naming, collision suffix.
// ---------------------------------------------------------------------------

/// Writes `png` to `Pictures\UP-TAKE\UP-TAKE_YYYY-MM-DD_HH-MM-SS.png`,
/// creating the directory on first use and appending `_2`, `_3`, … on a
/// same-second collision.
fn write_file(app: &AppHandle, png: &[u8]) -> Result<(), String> {
    let pictures = app
        .path()
        .picture_dir()
        .map_err(|error| format!("could not resolve the Pictures folder: {error}"))?;
    let dir = pictures.join("UP-TAKE");
    fs::create_dir_all(&dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    let path = unique_path(&dir, &timestamp_name());
    fs::write(&path, png).map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// `UP-TAKE_YYYY-MM-DD_HH-MM-SS` from the local wall clock, via `GetLocalTime`
/// rather than a new dependency — this workspace already talks to Win32
/// directly for everything else in `src-tauri` (see `placement.rs`,
/// `overlay.rs`), and a timestamp is one more struct, not a reason to add a
/// date/time crate.
fn timestamp_name() -> String {
    // SAFETY: `GetLocalTime` fills a plain-old-data struct with no
    // preconditions.
    let time: SYSTEMTIME = unsafe {
        let mut time = std::mem::zeroed();
        GetLocalTime(&mut time);
        time
    };
    format!(
        "UP-TAKE_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

/// `dir/stem.png`, or `dir/stem_2.png`, `dir/stem_3.png`, … — the first name
/// that does not already exist. A `_2` suffix appended for a same-second
/// collision reads more naturally than `_1` on the first file redundantly
/// numbering something unique.
fn unique_path(dir: &std::path::Path, stem: &str) -> PathBuf {
    let plain = dir.join(format!("{stem}.png"));
    if !plain.exists() {
        return plain;
    }
    let mut suffix = 2_u32;
    loop {
        let candidate = dir.join(format!("{stem}_{suffix}.png"));
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]
mod tests {
    use super::*;

    #[test]
    fn unique_path_returns_the_plain_name_when_nothing_collides() {
        let dir = std::env::temp_dir().join(format!("uptake-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = unique_path(&dir, "UP-TAKE_2026-07-25_12-00-00");
        assert_eq!(path, dir.join("UP-TAKE_2026-07-25_12-00-00.png"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unique_path_appends_a_collision_suffix_starting_at_2() {
        let dir = std::env::temp_dir().join(format!("uptake-test-collide-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let stem = "UP-TAKE_2026-07-25_12-00-00";
        fs::write(dir.join(format!("{stem}.png")), b"one").unwrap();
        let path = unique_path(&dir, stem);
        assert_eq!(path, dir.join(format!("{stem}_2.png")));
        fs::write(&path, b"two").unwrap();
        let path = unique_path(&dir, stem);
        assert_eq!(path, dir.join(format!("{stem}_3.png")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_dib_is_a_v5_header_followed_by_its_pixels_with_a_real_size_image() {
        let bitmap = RgbaBitmap::from_pixels(
            uptake_core::geometry::Size::new(3, 2),
            vec![0_u8; 3 * 2 * 4],
        )
        .unwrap();
        let dib = dibv5_bytes(&bitmap).unwrap();
        assert_eq!(dib.len(), 124 + 3 * 2 * 4);
        // bV5Size is the first field and must equal the header's own length,
        // which is how a consumer picks the header version.
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 124);
        // bV5SizeImage sits at offset 20; zero is only sanctioned for BI_RGB
        // and this DIB is BI_BITFIELDS.
        assert_eq!(
            u32::from_le_bytes(dib[20..24].try_into().unwrap()),
            3 * 2 * 4
        );
    }

    /// The three defined test screens, as pixels rather than as a web page.
    ///
    /// `examples/testscreen/index.html` is what the rig displays; this is the
    /// same three contents built directly, so the claim underneath the whole
    /// bracket can be checked without a desktop. The PRNG is mulberry32, the
    /// same one the page uses, for no reason except that two figures taken by
    /// different routes should not differ for an uninteresting reason.
    fn test_screen(kind: &str, size: uptake_core::geometry::Size) -> RgbaBitmap {
        let count = (size.width as usize) * (size.height as usize);
        let mut pixels = vec![0_u8; count * 4];
        let mut state: u32 = 0x05ee_d199;
        let mut next = || {
            state = state.wrapping_add(0x6d2b_79f5);
            let mut t = state;
            t = (t ^ (t >> 15)).wrapping_mul(t | 1);
            t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
            (t ^ (t >> 14)) as u8
        };
        // The control is 8x8 blocks of one random colour each, which is
        // full-entropy noise at 1/64 the resolution and therefore lands about
        // mid-decade between the two.
        //
        // **It was smooth vertical bands first, and that was wrong.** Measured
        // at 640x400: plain 1987 bytes, bands 2011, dense 896245. The bands
        // satisfied "between the two" by 24 bytes, which is 1.2 % of PLAIN and
        // nothing a reader of a rig log could tell apart. PNG filters rows
        // before it deflates them, so hard-edged vertical bands are nearly as
        // compressible as a flat field. A control that lands on top of one of
        // the endpoints does not falsify anything.
        const BLOCK: usize = 8;
        let width = size.width as usize;
        let blocks_across = width.div_ceil(BLOCK);
        let block_colours: Vec<[u8; 3]> = (0..blocks_across
            * (size.height as usize).div_ceil(BLOCK))
            .map(|_| [next(), next(), next()])
            .collect();
        for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            let (x, y) = (index % width, index / width);
            match kind {
                "plain" => pixel.copy_from_slice(&[0x80, 0x80, 0x80, 255]),
                "dense" => pixel.copy_from_slice(&[next(), next(), next(), 255]),
                _ => {
                    let colour = block_colours[(y / BLOCK) * blocks_across + (x / BLOCK)];
                    pixel.copy_from_slice(&[colour[0], colour[1], colour[2], 255]);
                }
            }
        }
        RgbaBitmap::from_pixels(size, pixels).unwrap()
    }

    /// **The acceptance check `quality-bars.md` §1 footnote 3 demands before any
    /// 1.9g figure may be quoted**, run against the encoder the freeze path
    /// actually uses rather than a reproduction of it.
    ///
    /// The bracket is only worth having if the two screens separate. If PLAIN
    /// and DENSE encoded to similar sizes, the screen would not be controlling
    /// the variable and the pair would be decoration. An order of magnitude is
    /// the spec's own bar and is not a tuned threshold: the real separation is
    /// far wider, and the assert is deliberately loose so that a genuine change
    /// in the encoder trips it rather than ordinary noise.
    #[test]
    fn the_defined_test_screens_separate_by_an_order_of_magnitude() {
        let size = uptake_core::geometry::Size::new(640, 400);
        let plain = encode_png(&test_screen("plain", size)).unwrap().len();
        let dense = encode_png(&test_screen("dense", size)).unwrap().len();
        assert!(
            dense > plain * 10,
            "PLAIN and DENSE do not separate: plain {plain} bytes, dense {dense} bytes. \
             The bracket is decoration unless these differ by orders of magnitude."
        );
    }

    /// **The falsifier for the self-description** (`D-29(e)`): a check that
    /// cannot come out the other way is worth nothing.
    ///
    /// The byte length is what makes a mislabelled run detectable, and that
    /// claim is only true if an *unlisted* screen is visibly neither. So the
    /// control is asserted to land strictly between the two, which is what a
    /// reader of the rig log is being asked to rely on.
    #[test]
    fn an_unlisted_screen_lands_visibly_between_the_two() {
        let size = uptake_core::geometry::Size::new(640, 400);
        let plain = encode_png(&test_screen("plain", size)).unwrap().len();
        let dense = encode_png(&test_screen("dense", size)).unwrap().len();
        let blocks = encode_png(&test_screen("blocks", size)).unwrap().len();
        // An order of magnitude clear of **both** ends, not merely on the
        // correct side of them. The first version of this asserted only
        // `blocks > plain && blocks < dense`, which a 24-byte margin satisfied
        // while the control sat 1.2 % from PLAIN — a green that could not have
        // gone red for the reason the check exists.
        assert!(
            blocks > plain * 10 && dense > blocks * 10,
            "the control does not land visibly between the listed screens: \
             plain {plain}, blocks {blocks}, dense {dense}. If an unlisted screen \
             cannot be told from a listed one, the reported byte length is \
             unfalsifiable and buys nothing."
        );
    }

    /// **The same falsifier, against the format the freeze path actually
    /// ships** — and the reason it is a second test rather than an edit to the
    /// two above.
    ///
    /// Both checks above call [`encode_png`] explicitly. That was right when
    /// they were written and stopped being right in the same branch: since
    /// [ADR-0027] the display path encodes **JPEG**, so the byte lengths printed
    /// beside every rig timing are JPEG lengths, and the property those tests
    /// prove was a property of a format the rig no longer measures. A falsifier
    /// aimed at the wrong format is `UT-F-52`'s defect one level up — the check
    /// is real, it just does not guard the number anyone reads.
    ///
    /// # The bar is deliberately weaker here, and the arithmetic is why
    ///
    /// Measured at 2560×1440 through this path's own encoder:
    ///
    /// | screen | PNG | JPEG |
    /// | --- | --- | --- |
    /// | PLAIN | 17,895 | 58,225 |
    /// | BLOCKS | 316,483 | 699,076 |
    /// | DENSE | 12,608,315 | 3,304,252 |
    ///
    /// PNG spans a factor of **704**, JPEG a factor of **57**. An order of
    /// magnitude clear of *both* ends needs the span to exceed 10 × 10 = 100, so
    /// under JPEG **no control can satisfy the bar above** — it is not that this
    /// one is badly chosen. The measured control sits 12.0× above PLAIN and
    /// 4.7× below DENSE, and the best any control could do is √57 ≈ 7.5× each
    /// way.
    ///
    /// So this asserts **3× clear of both ends**: a floor reasoned from the span
    /// rather than fitted to the measurement (which clears it by 4× and 1.6×),
    /// and still far more than the 1.2 % margin that made `UT-F-52` worth
    /// recording. What it preserves is the only claim `quality-bars.md` §1
    /// footnote 3 actually makes — that an unlisted screen is *visibly* neither.
    ///
    /// ✅ **Settled by the founder 2026-08-04, and `quality-bars.md` §1
    /// footnote 3 now states the acceptance per format rather than once.** The
    /// endpoint requirement is unchanged at 10× for **the two compressed
    /// formats** — that is the bar, and JPEG passes it. Only the *control's*
    /// margin is per format, 10× in PNG and 3× in JPEG, because 10 × 10 exceeds
    /// JPEG's whole span and therefore cannot be met by any control at all.
    ///
    /// ⛔ **NOT "every format", and the correction is the point.** This comment
    /// said *every format* until independent review measured the third one.
    /// **BMP is uncompressed**, so PLAIN, BLOCKS and DENSE encode to **exactly
    /// the same length** — 1,024,054 bytes at 640×400, 14,745,654 at 2560×1440,
    /// a span of **1.000**. Under BMP the per-monitor byte length carries **zero
    /// information about the screen**, which is not a weaker bracket but the
    /// absence of one, and `UPTAKE_FREEZE_FORMAT=bmp` is a supported override
    /// the shipped binary accepts. A universal quantifier written without
    /// checking the third case is `F-30`'s shape in one sentence. **Never quote
    /// a 1.9g figure from a BMP run; the self-description is void there.**
    ///
    /// **Why that is not a bar rewritten to fit what was built** — the thing
    /// being measured, can a reader tell PLAIN from DENSE, is untouched at full
    /// strength; what moved is an auxiliary control's margin, and it moved for
    /// an arithmetic reason. **If a future format widens the span past 100, this
    /// goes back to 10× without discussion** — and that is now *asserted* below
    /// rather than promised here, because a conditional nobody re-reads is not a
    /// mechanism.
    ///
    /// # The figures, and which size they were taken at
    ///
    /// The table above is **2560×1440**; this test asserts at **640×400**, where
    /// the span is **49.7×** and the control's margins are **3.6×** and
    /// **1.6×**. The smaller buffer is the *pessimistic* corner — the span grows
    /// with resolution (49.7 → 57.1 from 640×400 to 4K) — so the assertion is
    /// stricter than the cited table implies rather than looser. Said out loud
    /// because this is the file whose whole subject is numbers whose
    /// preconditions nobody stated (`UT-F-46`, `UT-F-47`).
    ///
    /// ⚠️ **3× is comfortable at the PLAIN end and thin at the DENSE end**, and
    /// a reader should know which. The measured margin is 4.66×, but the *floor
    /// this assertion permits* would put a control at ~1.1 MB against DENSE's
    /// 3.3 MB — same order, same digit count, differing in the leading digit,
    /// which an operator scanning a four-monitor log does not reliably see as
    /// different. Raising the floor re-opens the arithmetic this settled, so it
    /// is recorded rather than changed.
    ///
    /// [ADR-0027]: the private planning repo's
    /// `DECISIONS/ADR-0027-jpeg-for-the-freeze-display-path.md`
    #[test]
    fn the_bracket_still_separates_in_the_format_that_ships() {
        let size = uptake_core::geometry::Size::new(640, 400);
        let encode = |kind| encode_for_display(&test_screen(kind, size)).unwrap().len();
        let (plain, blocks, dense) = (encode("plain"), encode("blocks"), encode("dense"));
        let format = crate::freeze::display_format().2;
        // The endpoints are the claim §1 makes in its own words, and it survives
        // the format change: JPEG still separates PLAIN from DENSE by 57×.
        assert!(
            dense > plain * 10,
            "PLAIN and DENSE do not separate in {format}, the shipped display \
             format: plain {plain} bytes, dense {dense} bytes. The per-monitor \
             byte length cannot describe a run whose endpoints it cannot tell \
             apart."
        );
        assert!(
            blocks > plain * 3 && dense > blocks * 3,
            "the control does not land visibly between the listed screens in \
             {format}: plain {plain}, blocks {blocks}, dense {dense}. See this \
             test's own note before loosening the factor — 3× is already \
             reasoned from the span rather than fitted to it."
        );
        // **The promise above, enforced instead of remembered.** The 3× control
        // margin exists only because 10 × 10 = 100 exceeds this format's span;
        // the moment a format's span clears 100, that reason evaporates and the
        // margin must go back to 10×. Written as an assertion because a
        // conditional in a doc comment is precisely the obligation nobody
        // re-reads when the condition finally falls due.
        //
        // Not flaky: the span is 49.7× here and 57.1× at 4K, both far from 100.
        // Deliberately NOT auto-derived as `sqrt(span)` — at this size that
        // yields 7.05 against a measured `dense/blocks` of 4.66, so a clever
        // derivation turns the suite red today for no defect.
        assert!(
            dense < plain * 100,
            "{format}'s span is now {:.0}×, past 100 — so a control CAN sit an \
             order of magnitude clear of both ends, and the 3× margin above has \
             lost the arithmetic that justified it. Restore 10× and update \
             quality-bars.md §1 footnote 3. plain {plain}, dense {dense}.",
            dense as f64 / plain as f64
        );
    }

    /// **Task 1.9g's cheap falsifier, and it needs no rig.**
    ///
    /// The measured freeze is `encode 218 ms of a 224 ms freeze`, and
    /// `289.6 − 218 = 71.6 ms` is under §1's 100 ms target — so if the encode
    /// goes near-free the row passes and the stronger rewrite (hand the WebView
    /// raw frames, deleting the encode *and* the decode from the display path)
    /// is unnecessary. This measures whether it can.
    ///
    /// **The roadmap's proposed first move does not exist.** Both the 1.9g row
    /// and `STATUS.md`'s next action say to drop *the `image` crate's* PNG
    /// compression to its fastest setting. This project does not depend on the
    /// `image` crate: `cargo tree -p up-take --target x86_64-pc-windows-msvc
    /// --edges normal` is 774 entries with zero matches for it. [`encode_png`]
    /// goes through `windows-capture`'s `ImageEncoder`, which wraps the WinRT
    /// `BitmapEncoder`, and that wrapper calls `CreateAsync` with **no encoding
    /// options at all** — there is no compression knob on the path we use.
    ///
    /// So the question becomes a *format* question, which costs nothing to ask
    /// because `Bmp` and `Jpeg` are already in the enum. Printed rather than
    /// asserted: this is a measurement that informs a design decision, and a
    /// threshold invented here would be a bar describing whatever was built.
    ///
    /// # What the numbers said, and the objection they remove
    ///
    /// PNG is 68-294 ms and strongly content-dependent. BMP is a flat 25 ms and
    /// is not, but costs 14.7 MB per monitor. **JPEG is the interesting one**:
    /// 26-37 ms, within a rounding error of BMP's speed, at 58 KB (PLAIN) to
    /// 3.3 MB (DENSE) — so it removes BMP's ~59 MB-per-freeze objection against
    /// §1's 80 MB idle-RAM row while keeping almost all of the speed.
    ///
    /// **Lossy is defensible here for one specific reason, and only that one.**
    /// This encode feeds the *display* path alone: [`crate::freeze::crop`] cuts
    /// the user's actual screenshot from `Still::bitmap`, the lossless RGBA, and
    /// never from these bytes. So artifacts would reach what the user *looks at*
    /// while selecting, and never what they get.
    ///
    /// **That is not the same as saying JPEG is fine, and the remaining risk is
    /// the one a table cannot settle.** Screen content is JPEG's worst case, not
    /// its best: ringing around high-contrast glyph edges and chroma
    /// subsampling smearing coloured text are exactly what a desktop full of
    /// small text produces. It is also unknown what quality level the WinRT
    /// encoder defaults to, because — as above — this wrapper passes it no
    /// options. **Judge it by eye on the rig against a text-heavy screen before
    /// adopting it**; that is a question for a person looking at a monitor, and
    /// nothing here answers it.
    ///
    /// Run with:
    /// `cargo test -p up-take --lib encode_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "a measurement, not a check: encodes four full-monitor buffers"]
    fn encode_cost_by_format_and_content() {
        // The rig's primary, so the figures are comparable with the freeze
        // lines rather than being a small-buffer proxy.
        let size = uptake_core::geometry::Size::new(2560, 1440);
        println!(
            "\n{:>7}  {:>7}  {:>12}  {:>10}",
            "screen", "format", "bytes", "ms"
        );
        for screen in ["plain", "blocks", "dense"] {
            let bitmap = test_screen(screen, size);
            for (label, format) in [
                ("png", ImageFormat::Png),
                ("bmp", ImageFormat::Bmp),
                ("jpeg", ImageFormat::Jpeg),
            ] {
                let started = std::time::Instant::now();
                let encoded = ImageEncoder::new(format, ImageEncoderPixelFormat::Rgba8)
                    .unwrap()
                    .encode(bitmap.pixels(), bitmap.width(), bitmap.height())
                    .unwrap();
                println!(
                    "{screen:>7}  {label:>7}  {:>12}  {:>10}",
                    encoded.len(),
                    started.elapsed().as_millis()
                );
            }
        }
        println!();
    }

    #[test]
    fn bottom_up_bgra_flips_rows_and_swaps_red_and_blue() {
        // A 1x2 bitmap: top row red, bottom row green (top-down RGBA in).
        let bitmap = RgbaBitmap::from_pixels(
            uptake_core::geometry::Size::new(1, 2),
            vec![255, 0, 0, 255, 0, 255, 0, 128],
        )
        .unwrap();
        let out = bottom_up_bgra(&bitmap);
        // Bottom-up: the former bottom row (green) comes first now.
        assert_eq!(out, vec![0, 255, 0, 128, 0, 0, 255, 255]);
    }
}
