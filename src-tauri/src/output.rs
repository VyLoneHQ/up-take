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

/// Captures `bounds` and publishes it to the clipboard alone.
///
/// PRODUCT-VISION §8: an area's source is still on screen, so re-copying is
/// one gesture — the justification ShareX/Snipping Tool have for also
/// writing a file (a screenshot is transient) does not transfer here, and the
/// cost (an area copied forty times is forty stray PNGs) stays. Nothing is
/// written to disk by this path.
pub(crate) fn copy_to_clipboard(app: &AppHandle, area: AreaId, bounds: Rect) {
    let started = Instant::now();
    let mut split = Split::default();
    let outcome = capture(bounds, &mut split).and_then(|(bitmap, png)| {
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

/// Captures `bounds` and writes it to `Pictures\UP-TAKE\`, creating the
/// directory on first use. A separate, explicit action (PRODUCT-VISION §8) —
/// does not also touch the clipboard.
pub(crate) fn save_to_file(app: &AppHandle, area: AreaId, bounds: Rect) {
    let started = Instant::now();
    let mut split = Split::default();
    let outcome = capture(bounds, &mut split).and_then(|(_bitmap, png)| {
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

/// Captures `bounds` for a newly created area: publishes to the clipboard
/// (PRODUCT-VISION §8 — clipboard only) **and** pins the PNG for the area to
/// render (ADR-0014 §6, the Snipaste pin).
///
/// # The seam task 1.9c changes
///
/// [ADR-0022] settles that §1's selection→clipboard budget is met by *holding a
/// full-monitor frame and cropping it*, not by making capture faster — and 1.9c
/// builds that. **The frame acquisition is deliberately confined to
/// [`capture`]**, so 1.9c replaces one function's insides (or gives it an
/// already-captured frame to crop) rather than restructuring this one. Nothing
/// below assumes the pixels were captured *now*.
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
        let outcome = capture(bounds, &mut split).and_then(|(bitmap, png)| {
            let publish = Instant::now();
            // The pin is stored *before* the clipboard is touched: it is the
            // thing the user can see, and a clipboard failure should still
            // leave a visible capture on screen rather than an empty area.
            let version = {
                let store = app.state::<Mutex<CaptureStore>>();
                let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
                guard.insert(id, png.clone())
            };
            let dib = dibv5_bytes(&bitmap)?;
            let published = publish_clipboard(overlay_hwnd(&app)?, &dib, &png);
            split.publish_ms = publish.elapsed().as_millis();
            // Announced whether or not the clipboard worked, for the same
            // reason: the pin exists either way.
            if let Err(error) = crate::overlay::emit_pin(&app, id, version) {
                eprintln!("output: pinned the capture but could not announce it: {error}");
            }
            published
        });
        report("capture", started, &split, outcome);
    });
}

/// Encodes RGBA8 pixels as PNG via the same WIC-backed encoder
/// `uptake-capture`'s own hardware-verification driver uses
/// (`examples/grab.rs`) — reused rather than adding a second PNG codec to vet.
fn encode_png(bitmap: &RgbaBitmap) -> Result<Vec<u8>, String> {
    ImageEncoder::new(ImageFormat::Png, ImageEncoderPixelFormat::Rgba8)
        .map_err(|error| format!("could not create the PNG encoder: {error}"))?
        .encode(bitmap.pixels(), bitmap.width(), bitmap.height())
        .map_err(|error| format!("could not encode PNG: {error}"))
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
        "capture {} ms, encode {} ms, publish {} ms",
        split.capture_ms, split.encode_ms, split.publish_ms
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
