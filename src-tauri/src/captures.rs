//! The pinned captures a `Screenshot` area renders, and the URI scheme that
//! carries them to the WebView (roadmap task 1.9b).
//!
//! # Why a custom URI scheme and not the IPC bridge
//!
//! This is a **pre-decided** transport (ADR-0018's fallout, recorded in the
//! roadmap): a ~500×400 capture is ~270 KB once base64-encoded, and Tauri's
//! `invoke`/`emit` bridge is JSON — every byte would be escaped, parsed, and
//! held twice. A custom scheme hands the WebView a URL it fetches through its
//! own loader, so the bytes travel as bytes and the browser caches and decodes
//! them the way it does any image. **This is not to be re-opened in profiling.**
//!
//! # Lifetime
//!
//! A capture lives exactly as long as its area. [`remove`] is called from the
//! dismiss path, so nothing here outlives the thing that displays it — a
//! `HashMap` that only ever grows would leak a PNG **and** a raw bitmap per
//! dismissed area (see [`Pinned`] for why both are held), which the 8-hour soak
//! (M-20) would find and nothing else would.
//!
//! # A trap for whoever adds a CSP
//!
//! `tauri.conf.json` currently sets `"csp": null`, so nothing restricts
//! `img-src` and a pin loads. **The first CSP added to this app must include
//! this scheme in `img-src`**, or every pin silently becomes a blank area with
//! only a console message to say why.
//!
//! # Cache-busting
//!
//! The URL carries a **version** as well as an id (`<id>-<version>.png`).
//! WebView2 caches by URL, so a re-captured area re-using `<id>.png` would show
//! its first capture forever. The version is the store's own counter, bumped on
//! every [`insert`], which makes each capture a distinct URL without the caller
//! having to invent one.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use tauri::http::{Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeContext};
use uptake_core::area::AreaId;
use uptake_core::bitmap::RgbaBitmap;

/// The scheme name, as registered in `lib.rs`.
///
/// **Not the URL the WebView uses** — on Windows that is
/// `http://uptake-area.localhost/…`. See [`pin_url`], which is the only thing
/// that should ever build one.
pub const SCHEME: &str = "uptake-area";

/// One area's pinned capture: the version its URL carries, the PNG the WebView
/// renders, and the raw bitmap its clipboard DIB needs.
///
/// # Why both representations are kept
///
/// The two consumers want different bytes and neither can cheaply produce the
/// other's. The WebView needs PNG — that is what an `<img>` loads. `CF_DIBV5`
/// needs raw RGBA, and recovering it from the PNG would mean adding a WIC decoder
/// this crate has no other use for. Both are already computed at capture time, so
/// keeping both costs memory rather than work: `w × h × 4` bytes on top of the
/// PNG, roughly 1 MB for a 500×400 area and 33 MB for one the size of a 4K screen.
///
/// That is the number worth watching under the 8-hour soak (M-20), and it is
/// accepted deliberately. The alternative is that Copy on a pinned area cannot
/// export the picture the area is showing — the defect this replaced.
struct Pinned {
    version: u64,
    png: Vec<u8>,
    bitmap: RgbaBitmap,
}

/// Every area's pinned capture, keyed by area id.
#[derive(Default)]
pub struct CaptureStore {
    /// Keyed on the id's raw number rather than on [`AreaId`] itself, because
    /// `AreaId` deliberately has **no public constructor** — uniqueness is the
    /// store's to guarantee, and the URL round-trip here would need one. The
    /// number is what crosses the wire anyway (`AreaId::get`).
    pinned: HashMap<u64, Pinned>,
    /// Monotonic, never reset — a version is only ever compared for equality
    /// with the one in a URL, so uniqueness is the only property needed.
    next_version: u64,
}

impl CaptureStore {
    /// Stores a capture for `id`, returning the version its URL must use.
    /// Replaces any previous capture for that area (a re-capture is a new
    /// version, not a second entry).
    pub fn insert(&mut self, id: AreaId, bitmap: RgbaBitmap, png: Vec<u8>) -> u64 {
        let version = self.next_version;
        self.next_version = self.next_version.wrapping_add(1);
        self.pinned.insert(
            id.get(),
            Pinned {
                version,
                png,
                bitmap,
            },
        );
        version
    }

    /// Drops `id`'s capture. Called when the area is dismissed.
    pub fn remove(&mut self, id: AreaId) {
        self.pinned.remove(&id.get());
    }

    /// The bytes for `id`, if the stored version matches the one requested.
    ///
    /// A version mismatch resolves to `None` rather than to the current bytes:
    /// the only way to ask for a stale version is to hold a stale URL, and
    /// answering it with fresh pixels would make a caching bug invisible.
    fn get(&self, id: u64, version: u64) -> Option<&[u8]> {
        self.pinned
            .get(&id)
            .filter(|pinned| pinned.version == version)
            .map(|pinned| pinned.png.as_slice())
    }

    /// Whether `id`'s stored capture is still exactly `version`.
    ///
    /// **False for both ways a version stops being the one on screen**, which is
    /// the distinction `I-61` turns on and the one a magnify generation cannot
    /// draw: the entry was *forgotten* (a dismissal, or a scroll back to natural
    /// size), or it was *replaced* by a newer capture, since [`insert`] is keyed
    /// on the id alone.
    ///
    /// [`insert`]: CaptureStore::insert
    fn holds(&self, id: AreaId, version: u64) -> bool {
        self.pinned
            .get(&id.get())
            .is_some_and(|pinned| pinned.version == version)
    }
}

/// Proof that the pixels a pin would name are still the ones the store holds.
///
/// # Why announcing a pin takes a value and not a checked condition
///
/// `I-61` was `overlay::emit_pin` announcing a capture that [`forget`] had
/// already dropped, leaving the frontend holding a URL that resolves to nothing.
/// The first fix was a re-check immediately before the emit, and an independent
/// review made two points about that shape which this replaces.
///
/// **It could not be drilled.** Deleting the re-check left the entire suite
/// green, so nothing could tell a reviewer the guard had gone. `emit_pin` takes
/// one of these instead of an id and a version, and [`still_holds`] is the only
/// thing that can make one, so deleting the question stops the program
/// compiling. That is the only form of this that survives a refactor.
///
/// **It reached one of the two call sites.** `emit_pin`'s other caller,
/// `output::capture_into_area`, consulted nothing at all: the re-check
/// enumerated the entry points of the *magnify path* rather than the callers of
/// `emit_pin`, which are not the same set. Making the proof an argument closes
/// that by construction, and closes it for a call site nobody has written yet.
///
/// **It deliberately carries no lifetime and promises nothing about the
/// future.** The store can be emptied between this being made and the emit that
/// consumes it. That race is narrower than the one it replaces and it is real;
/// it is recorded as a backlog row rather than implied away by the type.
///
/// **Single-use, and neither `Copy` nor `Clone` on purpose.**
/// [`crate::overlay::emit_pin`] takes it by value, so one answer from
/// [`still_holds`] backs exactly one announcement. A shared reference would let
/// a single question answer for every later emit, including emits after the
/// pixels had gone -- which is the defect this type exists for, with one more
/// step in front of it.
///
/// ⚠️ **That paragraph used to end "adding either derive re-opens that, so
/// neither is here", and an independent review measured what that sentence was
/// worth: nothing.** It drilled the revert three ways -- restoring `&FreshPin`
/// at both call sites, adding `#[derive(Clone)]`, and cloning to announce twice
/// -- and all 296 tests stayed green each time. The `use of moved value` error
/// the commit message cited is a property of today's source, not a control on
/// it: it catches an accident and not an edit. **That is the same shape as the
/// finding this type was built to answer** -- a guard nothing can drill -- which
/// is how it got past its author twice.
///
/// [`SingleUse`] is the answer to half of it. It implements neither trait, so
/// `#[derive(Clone)]` or `#[derive(Copy)]` here is now a **compile error** with
/// the field named in it, rather than a comment asking to be obeyed.
///
/// **It blocks the DERIVE spelling and nothing else, and this paragraph said
/// otherwise until 2026-08-21.** A hand-written impl of either trait for this
/// type compiles, lets one proof back two announcements, and leaves the whole
/// suite green -- drilled by an independent review, which is the third time
/// this property has been asserted at one spelling and left open at another.
/// That spelling is now held by `no_hand_written_escape_from_the_single_use_property`
/// below, so the claim and its control finally cover the same ground.
///
/// The other half -- that `emit_pin` takes this by value and not by reference
/// -- has no type-level expression in Rust either, and is held by
/// `the_pin_proof_is_taken_by_value` in the tests below.
pub(crate) struct FreshPin {
    id: AreaId,
    version: u64,
    /// Present to make the absence of `Clone` and `Copy` mechanical. Never read.
    _single_use: SingleUse,
}

/// A field type with no `Clone` and no `Copy`, so `FreshPin` cannot gain either
/// by derive.
///
/// Zero-sized, so it costs nothing at runtime. The alternative the reviewer
/// named is a `trybuild`/`compile_fail` harness, which is a dev-dependency on a
/// public GPL-3.0 binary for one assertion and has no existing home in this
/// repository; a `compile_fail` doctest cannot reach `pub(crate)` items, so that
/// route is closed too. This needs neither.
struct SingleUse;

impl FreshPin {
    pub(crate) const fn id(&self) -> AreaId {
        self.id
    }

    pub(crate) const fn version(&self) -> u64 {
        self.version
    }
}

/// Proof that `id` still holds `version`, or `None` when it does not.
///
/// **Asks the store rather than the magnify generation, and that is a fix
/// rather than a tidier spelling.** `Magnify::is_current` goes false for two
/// events that want opposite answers: a *cancel*, where the pixels are gone and
/// the pin must be withheld, and a *supersede* by the next scroll notch, where
/// the pixels are still there and withholding the pin would leave the store
/// holding a capture the view was never told about. The store knows which
/// happened. The generation cannot.
pub(crate) fn still_holds(app: &AppHandle, id: AreaId, version: u64) -> Option<FreshPin> {
    let store = app.state::<Mutex<CaptureStore>>();
    let guard = store.lock().unwrap_or_else(PoisonError::into_inner);
    guard.holds(id, version).then_some(FreshPin {
        id,
        version,
        _single_use: SingleUse,
    })
}

/// The URL a pinned capture is served at, for the frontend to request.
///
/// # The Windows form is not the obvious one
///
/// **On Windows (and Android) a Tauri custom protocol is reached at
/// `http://<scheme>.localhost/…`, not at `<scheme>://localhost/…`.** The first
/// cut of this function used the obvious form; it produced a URL WebView2
/// cannot resolve, and the only symptom was a broken-image icon in the corner
/// of every pin — indistinguishable from "the capture returned nothing".
/// Confirmed against `tauri-2.11.5`'s own `protocol::isolation`, which builds
/// `format!("{https}://{schema}.localhost")` for exactly this reason; `https`
/// there is the opt-in `app.windows.useHttpsScheme`, which this app leaves at
/// its `false` default.
///
/// The non-Windows arm is the platform-correct form for macOS and Linux, where
/// the scheme *is* registered as a real scheme. Neither platform is built yet
/// (the crate is Windows-only today) — it is written out rather than
/// `todo!()`ed because getting it wrong is invisible until someone looks at a
/// blank pin, which is precisely what just happened here.
#[must_use]
pub fn pin_url(id: AreaId, version: u64) -> String {
    let path = format!("{}-{version}.png", id.get());
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/{path}")
    } else {
        format!("{SCHEME}://localhost/{path}")
    }
}

/// Parses `<id>-<version>.png` out of a request path.
///
/// Deliberately strict — a path that is not exactly this shape is a 404 rather
/// than a best-effort guess, because every URL this scheme ever serves is one
/// [`pin_url`] generated.
pub(crate) fn parse_path(path: &str) -> Option<(u64, u64)> {
    let stem = path.trim_start_matches('/').strip_suffix(".png")?;
    let (id, version) = stem.split_once('-')?;
    Some((id.parse().ok()?, version.parse().ok()?))
}

/// Parses `frozen-<index>-<version>.png` — a frozen still rather than an area's
/// pin (task 1.9d).
///
/// The two namespaces cannot collide: an area's path begins with its id, which
/// is a number, so nothing an area produces starts with `frozen-`. Checked
/// **before** [`parse_path`] in [`serve`], because `frozen-0-3` would otherwise
/// reach it and fail on `"frozen".parse::<u64>()` — a 404 that looks like a
/// missing capture rather than a namespace that was never routed.
pub(crate) fn parse_frozen_path(path: &str) -> Option<(usize, u64)> {
    let stem = path.trim_start_matches('/').strip_suffix(".png")?;
    let rest = stem.strip_prefix("frozen-")?;
    let (index, version) = rest.split_once('-')?;
    Some((index.parse().ok()?, version.parse().ok()?))
}

/// Serves a pinned capture, or a 404 with an empty body.
///
/// Runs on the WebView2 UI thread — see the registration in `lib.rs` for why the
/// async handler does not change that, and why it is acceptable here. The lock is
/// held only for the clone, which is PNG bytes (a few hundred KB for a typical
/// area), **not** the 33 MB an earlier version of this comment claimed; that
/// figure is the raw BGRA size in `output.rs`'s DIB path.
pub fn serve(
    ctx: UriSchemeContext<'_, tauri::Wry>,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let not_found = || {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Vec::new())
            .unwrap_or_else(|_| Response::new(Vec::new()))
    };
    let path = request.uri().path();
    // Frozen stills first — see `parse_frozen_path` for why the order matters.
    //
    // The URL keeps its `.png` suffix whatever the still is encoded as: it is an
    // opaque, versioned identifier the WebView never treats as a filename, and
    // the header below is what actually decides how the bytes are read. Changing
    // the suffix would mean changing both parsers for no gain.
    //
    // The still carries its own content type, taken when it was encoded — never
    // re-derived from the current setting here. `freeze::Still::content_type`
    // has the reason; the short version is that 1.14 makes the setting
    // changeable while a freeze is on screen, and bytes outlive the setting that
    // produced them.
    let found = if let Some((index, version)) = parse_frozen_path(path) {
        crate::freeze::still_bytes(index, version)
    } else {
        let Some((id, version)) = parse_path(path) else {
            return not_found();
        };
        let state = ctx.app_handle().state::<Mutex<CaptureStore>>();
        let guard = state.lock().unwrap_or_else(PoisonError::into_inner);
        // An area's pin is always PNG: those bytes are the product, and only the
        // freeze *display* path is switchable.
        guard
            .get(id, version)
            .map(|bytes| (bytes.to_vec(), "image/png"))
    };
    let Some((bytes, content_type)) = found else {
        return not_found();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        // The URL is versioned, so the bytes behind it never change and the
        // WebView may keep them as long as it likes. This is what makes a pin
        // cost one fetch rather than one per repaint — and what makes a
        // re-freeze show new pixels, since it mints a new version.
        .header("Cache-Control", "public, max-age=31536000, immutable")
        .body(bytes)
        .unwrap_or_else(|_| not_found())
}

/// The capture `id` is currently displaying, as `(bitmap, png)`.
///
/// # This is what makes Copy and Save export the picture, not the screen
///
/// Task 1.9 wired Copy/Save to capture the region **live**, every time. That was
/// correct by coincidence while an area could not move: a passive `Screenshot`
/// pins the instant it is created, so re-capturing its rectangle produced the
/// same pixels. Task 1.17(a) added move and resize in LIVING and broke the
/// coincidence — reported from the rig 2026-07-27, Copy on a moved Screenshot
/// area returned **the desktop underneath its new position**, cropped to the
/// area's size.
///
/// The failure is quiet, which is what makes it serious: the result is a
/// plausible image of exactly the right dimensions, so nothing looks wrong until
/// you examine what you pasted. `open_menu` had predicted it in a comment and
/// deferred it — a prediction in a comment is not a fix.
///
/// Returns `None` for an area with no capture (a `Default` area), where capturing
/// live is the only thing Copy could mean.
pub(crate) fn pinned_capture(app: &AppHandle, id: AreaId) -> Option<(RgbaBitmap, Vec<u8>)> {
    let state = app.state::<Mutex<CaptureStore>>();
    let guard = state.lock().unwrap_or_else(PoisonError::into_inner);
    guard
        .pinned
        .get(&id.get())
        .map(|pinned| (pinned.bitmap.clone(), pinned.png.clone()))
}

/// Drops the capture belonging to a dismissed area.
pub(crate) fn forget(app: &AppHandle, id: AreaId) {
    let state = app.state::<Mutex<CaptureStore>>();
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(id);
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "architecture §5 bans unwrap outside tests; inside them a failed \
              setup should abort the test loudly"
)]
mod tests {
    use uptake_core::area::{AreaStore, AreaType};
    use uptake_core::geometry::{Rect, Size};

    use super::*;

    #[test]
    fn the_pin_proof_is_taken_by_value() {
        // The half of `FreshPin`'s single-use property that Rust cannot express
        // in the type system, and the half an independent review drilled green:
        // restoring `&FreshPin` at both call sites left all 296 tests passing,
        // because every one of them tested the property HOLDING rather than
        // anything NOTICING it go.
        //
        // A reference re-opens the defect the type exists for. One call to
        // `still_holds` would answer for every later announcement, including
        // announcements made after `forget` had dropped the pixels -- `I-61`
        // with one more step in front of it.
        //
        // Read from the source because there is nowhere else to read it from:
        // the signature is the whole property, and `trybuild` is a
        // dev-dependency on a public binary for one assertion.
        let source = include_str!("overlay.rs");
        let Some(line) = source
            .lines()
            .find(|line| line.trim_start().starts_with("pub(crate) fn emit_pin("))
        else {
            panic!(
                "no `pub(crate) fn emit_pin(` in overlay.rs -- renamed, or its \
                 visibility changed? This test cannot answer for a function it \
                 cannot find, and an unfound signature must not read as a pass."
            )
        };
        assert!(
            line.contains("pin: crate::captures::FreshPin"),
            "`emit_pin` must take the freshness proof BY VALUE, so one proof backs \
             one announcement. Found: {line}"
        );
        assert!(
            !line.contains("&crate::captures::FreshPin"),
            "`emit_pin` takes the freshness proof by reference, which lets a single \
             call to `still_holds` answer for any number of later emits. Found: {line}"
        );
    }

    #[test]
    fn no_hand_written_escape_from_the_single_use_property() {
        // `SingleUse` makes `#[derive(Clone)]` and `#[derive(Copy)]` compile
        // errors on `FreshPin`. It does NOT reach a hand-written impl, and the
        // doc comment on the type claimed it did until an independent review
        // drilled the gap: four lines of `impl Clone`, a `.clone()` before the
        // emit, and one freshness proof backs two announcements with the whole
        // suite green and nothing red anywhere.
        //
        // That is the same shape as the finding this type was built to answer,
        // for the third time: the property was tested at the spelling its
        // author had in mind and left open at the one he did not.
        //
        // **Scanned over the whole crate rather than this file, and that is not
        // belt-and-braces.** `FreshPin` is `pub(crate)`, so Rust's orphan rule
        // lets the impl live in ANY module of `src-tauri`, and the first draft
        // of this very test read `captures.rs` alone -- which would have been
        // the same defect a fourth time, in the control written to stop it.
        //
        // The needles are assembled at run time on purpose. Written out whole
        // they would appear in this file and match themselves, and a source
        // control defeated by the text describing it reads green forever. Test
        // modules are cut for the same reason: the drill that proves this test
        // works has to be able to live in one without tripping it, and an impl
        // behind `#[cfg(test)]` cannot reach a release binary anyway.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut scanned = 0_usize;
        let mut saw_the_type = false;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let production = source
                .split_once("mod tests {")
                .map_or(source.as_str(), |(before, _)| before)
                // A same-crate impl may spell the type by any path it likes, so
                // compare on the bare name rather than on one of them.
                .replace("crate::captures::", "")
                .replace("captures::", "");
            scanned += 1;
            saw_the_type |= production.contains("struct FreshPin");
            for trait_name in ["Clone", "Copy"] {
                let needle = format!("impl {trait_name} for FreshPin");
                assert!(
                    !production.contains(&needle),
                    "{}: hand-written `{trait_name}` impl for `FreshPin`.                      `SingleUse` blocks only the derive spelling, so this                      re-opens the defect the type exists for: one call to                      `still_holds` backing more than one announcement,                      including announcements made after `forget` dropped the                      pixels.",
                    path.display()
                );
            }
        }
        // Both halves of the positive control. A sweep that reads nothing, or
        // that reads files but never meets the type, is indistinguishable from
        // a sweep that found no violation -- which is exactly the green that
        // could not have been earned.
        assert!(
            scanned >= 2,
            "scanned {scanned} file(s) under {} -- the sweep found no crate to              read, so its silence means nothing.",
            dir.display()
        );
        assert!(
            saw_the_type,
            "swept {scanned} file(s) and never saw `struct FreshPin`. Renamed,              moved out of this crate, or cut away with a test module -- either              way this test just passed over a type it never read, and an              unfound type must not read as a pass."
        );
    }

    /// A 1×1 bitmap, standing in for the raw pixels a real pin carries. Only the
    /// clipboard path reads them, and nothing here decodes one.
    fn pixels() -> RgbaBitmap {
        RgbaBitmap::transparent(Size::new(1, 1)).unwrap()
    }

    /// Real ids from a real [`AreaStore`] — `AreaId` has no public constructor
    /// on purpose, and reaching for one here would erode that rather than test
    /// against it.
    fn ids(count: usize) -> Vec<AreaId> {
        let mut store = AreaStore::new();
        (0..count)
            .map(|i| {
                let offset = i32::try_from(i).unwrap() * 200;
                store
                    .create(AreaType::Screenshot, Rect::new(offset, 0, 100, 100))
                    .unwrap()
            })
            .collect()
    }

    /// The three answers `CaptureStore::holds` has to give, which is the guard
    /// behind every pin announcement since `I-61`.
    ///
    /// **This is the seam the first version of the fix did not have.** That one
    /// asked `Magnify::is_current` inside `magnify_once`, which needs an
    /// `AppHandle` and a real capture, so deleting the guard left the whole
    /// suite green and the author had to say so in the test's own doc. The
    /// question now lives on the store, where it can be asked directly, and the
    /// answer is carried to `emit_pin` as a value that only `still_holds` can
    /// make, so deleting the guard no longer compiles.
    #[test]
    fn a_pin_is_fresh_only_while_the_store_still_holds_that_exact_version() {
        let mut store = CaptureStore::default();
        let areas = ids(2);
        let version = store.insert(areas[0], pixels(), vec![1, 2, 3]);

        // Present: the ordinary case, and the one a supersede must not break.
        assert!(store.holds(areas[0], version));

        // Forgotten. This is `I-61` itself: `captures::forget` ran between the
        // insert and the announcement, so the URL would resolve to nothing.
        store.remove(areas[0]);
        assert!(!store.holds(areas[0], version));

        // Replaced by a newer capture. `insert` is keyed on the id alone, so the
        // older version is no longer the one on screen and its worker must not
        // announce it over the newer one.
        let first = store.insert(areas[1], pixels(), vec![4]);
        let second = store.insert(areas[1], pixels(), vec![5]);
        assert!(!store.holds(areas[1], first));
        assert!(store.holds(areas[1], second));
    }

    #[test]
    fn freshness_is_per_area_and_not_a_global_version_counter() {
        // The versions come from one counter shared across areas, so a naive
        // `holds` that compared only the number would answer for the wrong area.
        let mut store = CaptureStore::default();
        let areas = ids(2);
        let first = store.insert(areas[0], pixels(), vec![1]);
        let second = store.insert(areas[1], pixels(), vec![2]);
        assert_ne!(
            first, second,
            "the counter is shared, so this is a real risk"
        );
        assert!(!store.holds(areas[0], second));
        assert!(!store.holds(areas[1], first));
    }

    #[test]
    fn an_area_with_no_capture_at_all_is_never_fresh() {
        let mut store = CaptureStore::default();
        let areas = ids(2);
        let version = store.insert(areas[0], pixels(), vec![1]);
        assert!(!store.holds(areas[1], version));
    }

    #[test]
    fn a_stored_capture_is_served_at_its_own_version_only() {
        let mut store = CaptureStore::default();
        let areas = ids(2);
        let version = store.insert(areas[0], pixels(), vec![1, 2, 3]);

        assert_eq!(store.get(areas[0].get(), version), Some(&[1, 2, 3][..]));
        // A stale URL must 404 rather than silently receive the newer pixels —
        // that is what makes a caching bug visible instead of invisible.
        assert_eq!(store.get(areas[0].get(), version.wrapping_add(1)), None);
        assert_eq!(store.get(areas[1].get(), version), None);
    }

    #[test]
    fn re_capturing_an_area_replaces_it_under_a_new_version() {
        let mut store = CaptureStore::default();
        let area = ids(1)[0];
        let first = store.insert(area, pixels(), vec![0xAA]);
        let second = store.insert(area, pixels(), vec![0xBB]);

        assert_ne!(first, second, "a re-capture needs a distinct URL");
        assert_eq!(store.get(area.get(), second), Some(&[0xBB][..]));
        assert_eq!(store.get(area.get(), first), None, "the old URL is gone");
    }

    #[test]
    fn a_pin_keeps_the_raw_bitmap_its_clipboard_format_needs() {
        // The regression this guards is the rig finding of 2026-07-27: Copy
        // re-captured the screen instead of exporting the pin, which looked
        // correct only while areas could not move. Exporting the pin needs the
        // raw pixels as well as the PNG, so losing the bitmap here would silently
        // send Copy back down the live-capture path.
        let mut store = CaptureStore::default();
        let area = ids(1)[0];
        let bitmap = RgbaBitmap::transparent(Size::new(3, 2)).unwrap();
        store.insert(area, bitmap.clone(), vec![1]);

        let held = store.pinned.get(&area.get()).unwrap();
        assert_eq!(held.bitmap, bitmap, "the raw pixels must survive the store");
        assert_eq!((held.bitmap.width(), held.bitmap.height()), (3, 2));
    }

    #[test]
    fn dismissing_an_area_drops_its_capture() {
        // The leak M-20 would eventually find: one 33 MB pin per dismissed area.
        let mut store = CaptureStore::default();
        let area = ids(1)[0];
        let version = store.insert(area, pixels(), vec![9]);
        store.remove(area);

        assert_eq!(store.get(area.get(), version), None);
    }

    #[test]
    fn only_the_exact_path_shape_resolves() {
        assert_eq!(parse_path("/12-34.png"), Some((12, 34)));
        assert_eq!(parse_path("12-34.png"), Some((12, 34)));
        for bad in [
            "/12.png",
            "/12-34.jpg",
            "/12-34",
            "/-34.png",
            "/12-.png",
            "/abc-34.png",
            "/12-abc.png",
            "",
        ] {
            assert_eq!(parse_path(bad), None, "{bad} should not resolve");
        }
    }

    #[test]
    fn the_generated_url_round_trips_through_the_parser() {
        // The one property that matters: nothing else ever constructs these
        // URLs, so `pin_url` and `parse_path` only have to agree with each other.
        let area = ids(1)[0];
        let url = pin_url(area, 99);
        // Parse the path out the way a URL parser would, rather than trimming a
        // hard-coded prefix: the previous version of this test trimmed
        // `"uptake-area://localhost"`, which passed happily while `pin_url`
        // emitted a URL WebView2 could not resolve at all. A test that only ever
        // sees its own assumption cannot catch that.
        let path = url
            .split_once("://")
            .and_then(|(_, rest)| rest.split_once('/'))
            .map(|(_, path)| format!("/{path}"));
        assert_eq!(path.as_deref().and_then(parse_path), Some((area.get(), 99)));
    }

    #[test]
    fn the_windows_url_uses_the_http_localhost_form_tauri_actually_serves() {
        // Pinned as a value, not derived from the same helper, because the bug
        // this catches was a *plausible* URL rather than a malformed one — see
        // `pin_url`'s docs. On Windows the scheme is a host, not a scheme.
        let area = ids(1)[0];
        let url = pin_url(area, 7);
        if cfg!(windows) {
            assert_eq!(
                url,
                format!("http://uptake-area.localhost/{}-7.png", area.get())
            );
        } else {
            assert_eq!(url, format!("uptake-area://localhost/{}-7.png", area.get()));
        }
    }
}
