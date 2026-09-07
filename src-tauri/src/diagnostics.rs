//! Task 1.15: the one place a failure reaches the user.
//!
//! The LOGGING half lives in `uptake-log`, which is the only crate permitted to
//! call the `tracing` macros -- `clippy.toml` bans them workspace-wide and that
//! crate's `Cargo.toml` allows them for itself alone. Read its crate docs for
//! the privacy rule and how that boundary holds it.
//!
//! This module is the part that needs `tauri`: putting a failure in front of
//! the user. It is here rather than in `uptake-log` so that crate stays
//! testable with no window and no Tauri dependency.
//!
//! # What this replaces, and why it was not merely untidy
//!
//! Every failure path in `output.rs` ended at `eprintln!`, which in a release
//! build reaches nobody: the app launches from Explorer or the tray with no
//! console attached. That is UP-TAKE finding `F-35`, accepted-not-fixed since
//! 2026-07, and its sharp edge is that **Copy is a primary user-initiated
//! action**. The OCR path additionally suppresses its success flash on failure
//! -- correctly, since the flash acknowledges something that did not happen --
//! so a user who pressed the key got nothing at all, and could not tell that
//! their clipboard was unchanged.
//!
//! `hotkey.rs` had said the quiet part in a doc comment: its startup failure is
//! shown as a dialog *"because there is still nowhere else for"* it to go.
//! There is now.

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use uptake_core::area::AreaId;

pub(crate) use uptake_log::{init, measurement, note, trouble};

/// [`uptake_log::trouble_for`], with the id typed rather than a bare `u64`.
///
/// The log crate takes a `u64` so it needs no dependency on the domain types;
/// this keeps the call sites honest about what they are passing.
pub(crate) fn trouble_for_area(message: &'static str, area: AreaId) {
    uptake_log::trouble_for(message, area.get());
}

/// Logs a failure and puts it in front of the user, without blocking.
///
/// # What is logged and what is not, which is the privacy design in one line
///
/// **`source` is logged. `title` and `detail` are not.** `source` is
/// `&'static str` by type, so it cannot carry something read at runtime. The
/// dialog may say whatever the user needs -- it is read by the person whose
/// screen it is -- while the file on their disk gets only a literal.
///
/// # What this unifies, and what it deliberately does NOT
///
/// `tray.rs` and `hotkey.rs` each had their own `report_failure`, both logging
/// and both opening a non-blocking warning dialog: one rule with two
/// implementations, and a third for the output pipeline would have made it
/// three. **Their MESSAGES are not unified and must not be** -- the tray's
/// explains the app is still usable and how to quit from Task Manager, the
/// hotkey's distinguishes "another application holds this combination" (`M-9`)
/// from "Windows refused". Those are the valuable part and belong to their
/// callers.
///
/// # Non-blocking, always
///
/// Both original copies documented the same reason: a blocking dialog during
/// `setup` deadlocks the startup it is reporting on, because the event loop has
/// not started. After startup it matters differently -- a modal stealing focus
/// from a capture tool is a second failure on top of the first.
pub(crate) fn report_failure(app: &AppHandle, source: &'static str, title: &str, detail: &str) {
    uptake_log::failure(source);

    app.dialog()
        .message(detail)
        .kind(MessageDialogKind::Warning)
        .title(title)
        .show(|_| {});
}
