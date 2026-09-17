//! The settings window: opening it, and the four commands it speaks to Rust
//! with (roadmap 1.14, `UI-UX.md` section 3.2).
//!
//! # Why a second window and not a panel on the overlay
//!
//! The overlay is transparent, undecorated, click-through where an area is not,
//! and excluded from capture. A settings pane drawn inside it would inherit all
//! four, and the last one means a user could not screenshot their own settings
//! to ask a question about them. It is also the one surface that must survive
//! `Esc`, which the overlay's state machine uses to hand the screen back.
//!
//! # It is an ordinary window on purpose
//!
//! Resizable, in the taskbar, focusable, and **not** always-on-top. The overlay
//! is the special one; this is the product's ordinary face, and a settings
//! window that floated above every other application while you read a manual in
//! a browser would be a bug report. It is created hidden and shown once the
//! page reports itself ready, so nobody sees an unpainted frame.
//!
//! # What crosses the boundary, and what does not
//!
//! [`crate::settings`] knows nothing about Tauri, and this module knows nothing
//! about what a setting means. The effects a change has on a live app -- the
//! capture affinity, the Run key, the overlay repainting itself -- are applied
//! here in [`settings_write`], because they need an `AppHandle` and because
//! keeping them out of the store is what lets the store be tested without one.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::settings::{FILTER_RANGE, OPACITY_RANGE, Settings};

/// The settings window's Tauri label.
pub const WINDOW_LABEL: &str = "settings";

/// The event the overlay listens on so a saved appearance change is visible
/// without a restart.
const CHANGED_EVENT: &str = "settings://changed";

/// The window's size, from `UI-UX.md` section 3.2.
const SIZE: (f64, f64) = (900.0, 620.0);

/// The smallest the window may be dragged to.
///
/// Not the same as [`SIZE`], and not absent: the sidebar is a fixed 196 px and
/// a row is a name, a description and a control on one line, so below roughly
/// this the description wraps under the control and the window stops being the
/// thing section 3.2 approved. Above it, more width is only more breathing
/// room, which is why there is no maximum.
const MINIMUM_SIZE: (f64, f64) = (720.0, 480.0);

/// Facts the window shows but cannot change.
///
/// Separate from [`Settings`] deliberately. A window that received these in the
/// same payload would invite a round trip that wrote them back, and two of them
/// -- the hotkey labels -- are exactly the rows this build shows read-only.
#[derive(Debug, Clone, Serialize)]
pub struct Facts {
    /// The summon shortcut, as the user reads it.
    pub summon_hotkey: String,
    /// The copy-this-monitor shortcut, as the user reads it.
    pub grab_hotkey: String,
    /// Where Save writes when no folder has been chosen, for the placeholder.
    pub default_save_directory: String,
    /// Whether the Run-key registration is actually present on this machine.
    ///
    /// Read from the registry rather than from the stored setting, so a user
    /// who removed the entry by hand, or whose profile was copied to another
    /// machine, sees what is true rather than what was last asked for.
    pub autostart_registered: bool,
    /// The lowest and highest an area's opacity may be set to.
    pub opacity_range: (u8, u8),
    /// The lowest and highest a Filter area's strength may be set to.
    pub filter_range: (u8, u8),
    /// What the Language setting's **Windows' language** choice resolves to.
    ///
    /// Sent so the window can re-render itself the moment the row is used,
    /// including when the choice is *Windows' language* -- which it cannot work
    /// out for itself and which is not the same as the language this process is
    /// running in once somebody has changed the setting.
    pub system_language: String,
}

/// Opens the settings window, or brings it to the front if it is already open.
///
/// Never returns an error to its caller: the tray menu and the overlay both
/// call this, and neither has anywhere useful to put one. A failure is logged
/// and reported through the same channel a failed tray is.
pub fn open(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        // Already open. `show` before `set_focus` because a minimised window
        // takes focus without becoming visible, which reads as the menu item
        // doing nothing.
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }
    if let Err(error) = build(app) {
        crate::diagnostics::trouble("settings: could not open the settings window", &error);
    }
}

fn build(app: &AppHandle) -> Result<(), String> {
    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("settings/".into()))
        .title(crate::strings::text(crate::strings::Text::SettingsTitle))
        .inner_size(SIZE.0, SIZE.1)
        .min_inner_size(MINIMUM_SIZE.0, MINIMUM_SIZE.1)
        // A slim title bar of our own (section 3.2), so the window belongs to
        // the same product as the overlay rather than to Windows.
        .decorations(false)
        // Transparent so the panel's own rounded corners are what the user
        // sees, rather than a dark rectangle behind them.
        .transparent(true)
        .resizable(true)
        .center()
        // Shown by the page once it has painted. A window created visible
        // shows one white frame before the stylesheet lands, which on a dark
        // panel is a flash bright enough to be the first thing anyone reports.
        .visible(false)
        .build()
        .map(|_| ())
        .map_err(|error| format!("could not create the settings window: {error}"))
}

/// The settings as they stand.
#[tauri::command]
pub fn settings_read() -> Settings {
    crate::settings::current()
}

/// The facts the window shows and cannot change.
#[tauri::command]
pub fn settings_facts(app: AppHandle) -> Facts {
    Facts {
        summon_hotkey: crate::hotkey::SUMMON_LABEL.to_string(),
        grab_hotkey: crate::hotkey::GRAB_LABEL.to_string(),
        default_save_directory: crate::output::save_directory(&app)
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        autostart_registered: crate::autostart::is_registered(),
        opacity_range: OPACITY_RANGE,
        filter_range: FILTER_RANGE,
        system_language: crate::strings::system_language().to_string(),
    }
}

/// Stores `settings`, persists them, and applies the ones a running app can
/// change without a restart.
///
/// # The order is the contract
///
/// The store is written **first**, so that every effect below reads the same
/// values the window just sent and a failure part-way through cannot leave the
/// app running on a mixture of old and new.
///
/// # What a partial failure does
///
/// Reports the first thing that went wrong and applies the rest anyway. The
/// alternative -- stopping at the first error -- would let a failed registry
/// write mean the user's opacity change silently did not happen either. Each
/// of these is independent, and the honest outcome is *this one did not work*
/// rather than *nothing worked and you are not sure which*.
///
/// # Errors
///
/// When the settings file could not be written, the Run key could not be
/// changed, or the capture affinity could not be applied. The message names
/// which, and the setting is in force for this run regardless.
#[tauri::command]
pub fn settings_write(app: AppHandle, settings: Settings) -> Result<(), String> {
    let previous = crate::settings::current();
    let mut first_failure: Option<String> = None;
    let mut fail = |error: String| {
        crate::diagnostics::trouble("settings", &error);
        if first_failure.is_none() {
            first_failure = Some(error);
        }
    };

    if let Err(error) = crate::settings::save(settings.clone()) {
        fail(error);
    }

    // Only when it changed. Writing the Run key on every save would mean a
    // user who never touches that switch still has their registry written
    // every time they move a slider.
    if settings.start_with_windows != previous.start_with_windows
        && let Err(error) = crate::autostart::apply(settings.start_with_windows)
    {
        fail(error);
    }

    #[cfg(windows)]
    if settings.show_in_screen_recordings != previous.show_in_screen_recordings
        && let Err(error) =
            crate::overlay::apply_capture_exclusion(&app, settings.show_in_screen_recordings)
    {
        fail(error);
    }

    // The overlay repaints on this: opacity and the Filter wash are CSS
    // custom properties on its page, and the page owns its own rendering
    // (ADR-0011). Emitted on every save rather than only on an appearance
    // change -- it is one small payload, and a rule about which fields matter
    // is a rule that goes stale the first time a field is added.
    let _ = app.emit(CHANGED_EVENT, &settings);

    first_failure.map_or(Ok(()), Err)
}

/// Asks the user for a folder, and returns it.
///
/// # Why the dialog is opened here rather than in the WebView
///
/// `lib.rs` registers `tauri-plugin-dialog` for Rust's use and notes that **no
/// frontend capability grants it**, so the WebView cannot open dialogs. That is
/// a deliberate narrowing of what a compromised page could do, and it is worth
/// one command to keep.
///
/// # Blocking, and why that is safe exactly here
///
/// `diagnostics::report_failure` documents the house rule -- dialogs never
/// block -- and the reason is the event loop: a modal opened during `setup`
/// deadlocks the startup it is reporting on. **A command does not run on the
/// event-loop thread**, it runs on Tauri's async runtime, so waiting here
/// blocks only the invocation. It must never be called from `setup` or from a
/// window event, and nothing does.
#[tauri::command]
pub fn settings_choose_folder(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;

    app.dialog()
        .file()
        .set_title(crate::strings::text(crate::strings::Text::SettingsTitle))
        .blocking_pick_folder()
        .map(|folder| folder.to_string())
}

/// Arms the first-run tour to run again (`ADR-0043` decision 5).
///
/// Does **not** start the tour now: it is taught on the overlay, and the
/// settings window is not the overlay. It runs the next time the overlay
/// opens, which is what the window says.
///
/// # Both halves, because the first one alone was a lie
///
/// ⚠️ **This cleared the stored flag and stopped**, and
/// [`crate::first_run::init`] decides once at startup and returns early when
/// the tour is recorded as done. So in a process that had already run it,
/// nothing re-read the file and the tour did not appear -- while the window
/// said it would, on the next overlay open. Found by the independent review of
/// `PR #105`.
///
/// **The file is written first and the process armed second.** A failure
/// between the two leaves the tour armed on disk, which shows it once more than
/// asked; the other order loses the request entirely on a crash. And the
/// process is armed **only** when the file was written, so the window's message
/// and the stored state cannot disagree.
///
/// # Errors
///
/// For any reason [`crate::config::clear_first_run_completed`] gives.
#[tauri::command]
pub fn settings_replay_tour() -> Result<(), String> {
    crate::config::clear_first_run_completed()?;
    crate::first_run::restart();
    Ok(())
}

/// Closes the settings window.
///
/// The custom title bar draws its own close button, so the page needs a way to
/// say so. `close` rather than `hide`: the window is cheap to rebuild, and a
/// hidden one would keep a WebView alive for a surface most people open twice
/// a year.
#[tauri::command]
pub fn settings_close(app: AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.close();
    }
}

#[cfg(test)]
mod tests {
    use super::{Facts, MINIMUM_SIZE, SIZE, WINDOW_LABEL};
    use crate::payload_keys::{assert_keys, assert_payload_coverage};

    #[test]
    fn the_window_is_not_the_overlay() {
        // Two labels, one process. `overlay::overlay_window` looks its window
        // up by label and `lib.rs`'s window-event hook returns early for any
        // other, so a collision here would send the settings window's move and
        // resize events into `sync_bounds` and have it fight the user's mouse.
        assert_ne!(WINDOW_LABEL, crate::overlay::WINDOW_LABEL);
    }

    #[test]
    fn the_window_cannot_be_dragged_smaller_than_its_layout() {
        assert!(MINIMUM_SIZE.0 < SIZE.0 && MINIMUM_SIZE.1 < SIZE.1);
        // The sidebar is a fixed 196px (UI-UX.md section 3.2); a minimum that
        // did not clear it with room for a row would let the window be resized
        // into something that cannot show a setting.
        assert!(MINIMUM_SIZE.0 > 196.0 * 2.0);
    }

    #[test]
    fn the_facts_reach_the_window_under_the_keys_it_reads() {
        assert_keys(
            "Facts",
            &Facts {
                summon_hotkey: "Win+Shift+U".to_string(),
                grab_hotkey: "Win+Shift+G".to_string(),
                default_save_directory: r"C:\Users\someone\Pictures\UP-TAKE".to_string(),
                autostart_registered: false,
                opacity_range: (10, 100),
                filter_range: (5, 60),
                system_language: "en".to_string(),
            },
            &[
                "summon_hotkey",
                "grab_hotkey",
                "default_save_directory",
                "autostart_registered",
                "opacity_range",
                "filter_range",
                "system_language",
            ],
        );
    }

    #[test]
    fn no_payload_in_this_module_escapes_the_key_table() {
        assert_payload_coverage(
            "settings_window.rs",
            include_str!("settings_window.rs"),
            &["Facts"],
            &[],
        );
    }
}
