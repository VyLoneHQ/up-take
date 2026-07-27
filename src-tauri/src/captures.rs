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
//! `HashMap` that only ever grows would leak a PNG per dismissed area, which the
//! 8-hour soak (M-20) would find and nothing else would.
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

/// The scheme name, as registered in `lib.rs`.
///
/// **Not the URL the WebView uses** — on Windows that is
/// `http://uptake-area.localhost/…`. See [`pin_url`], which is the only thing
/// that should ever build one.
pub const SCHEME: &str = "uptake-area";

/// One area's pinned capture: the PNG bytes and the version its URL carries.
struct Pinned {
    version: u64,
    png: Vec<u8>,
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
    /// Stores `png` as `id`'s capture, returning the version its URL must use.
    /// Replaces any previous capture for that area (a re-capture is a new
    /// version, not a second entry).
    pub fn insert(&mut self, id: AreaId, png: Vec<u8>) -> u64 {
        let version = self.next_version;
        self.next_version = self.next_version.wrapping_add(1);
        self.pinned.insert(id.get(), Pinned { version, png });
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
fn parse_path(path: &str) -> Option<(u64, u64)> {
    let stem = path.trim_start_matches('/').strip_suffix(".png")?;
    let (id, version) = stem.split_once('-')?;
    Some((id.parse().ok()?, version.parse().ok()?))
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
    let Some((id, version)) = parse_path(request.uri().path()) else {
        return not_found();
    };
    let state = ctx.app_handle().state::<Mutex<CaptureStore>>();
    let bytes = {
        let guard = state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.get(id, version).map(<[u8]>::to_vec)
    };
    let Some(bytes) = bytes else {
        return not_found();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "image/png")
        // The URL is versioned, so the bytes behind it never change and the
        // WebView may keep them as long as it likes. This is what makes a pin
        // cost one fetch rather than one per repaint.
        .header("Cache-Control", "public, max-age=31536000, immutable")
        .body(bytes)
        .unwrap_or_else(|_| not_found())
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
    use uptake_core::geometry::Rect;

    use super::*;

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

    #[test]
    fn a_stored_capture_is_served_at_its_own_version_only() {
        let mut store = CaptureStore::default();
        let areas = ids(2);
        let version = store.insert(areas[0], vec![1, 2, 3]);

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
        let first = store.insert(area, vec![0xAA]);
        let second = store.insert(area, vec![0xBB]);

        assert_ne!(first, second, "a re-capture needs a distinct URL");
        assert_eq!(store.get(area.get(), second), Some(&[0xBB][..]));
        assert_eq!(store.get(area.get(), first), None, "the old URL is gone");
    }

    #[test]
    fn dismissing_an_area_drops_its_capture() {
        // The leak M-20 would eventually find: one 33 MB pin per dismissed area.
        let mut store = CaptureStore::default();
        let area = ids(1)[0];
        let version = store.insert(area, vec![9]);
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
