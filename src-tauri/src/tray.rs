//! System tray icon, menu, and the app's only quit path (roadmap task 1.5).
//!
//! PRODUCT-VISION.md §4.3 is explicit: `Esc` **never quits**, only hides —
//! quitting is a tray action, full stop. This module is therefore the only
//! place in the app that calls [`AppHandle::exit`].
//!
//! Which is exactly why a failure here is reported to the user rather than
//! logged. The overlay window is `visible: false`, `skipTaskbar: true` and
//! `decorations: false`, and the startup `overlay::show` is debug-only — so a
//! release build whose tray did not come up has no tray, no taskbar entry, no
//! window and no quit command. `eprintln!` reaches nobody there (`main.rs`
//! sets `windows_subsystem = "windows"`), which would leave that user with a
//! process they cannot close and no idea why. Same reasoning as
//! [`crate::hotkey::install`], and stronger: a hotkey conflict is
//! user-fixable, a missing tray is not.

use tauri::AppHandle;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

use crate::{hotkey, overlay};

// Namespaced deliberately. `TrayIcon::register` pushes the `on_menu_event`
// handler into `AppManager`'s *global* menu-event listener list, so this
// closure is invoked for menu events from every menu in the app and matches on
// the raw id string alone. A bare `"quit"` added by a later menu (task 1.14's
// settings UI is the obvious candidate) would land in the `QUIT_ID` arm and
// exit the app; the `_ => {}` fallback cannot prevent that, because the
// collision is a match, not a miss.
const SHOW_ID: &str = "tray:show";
const QUIT_ID: &str = "tray:quit";

/// Builds the tray icon and its menu, telling the user if it could not.
///
/// Never returns an error, and never fatal (architecture §5 class 3):
/// everything here is a bundling or OS-resource concern rather than a reason
/// to refuse to start with a working hotkey. See the module docs for why the
/// failure is surfaced rather than logged.
pub fn install(app: &AppHandle) {
    if let Err(error) = build(app) {
        report_failure(app, &error);
    }
}

/// Tells the user the tray is unavailable, and how to close the app without it.
///
/// Non-blocking, for the same reason as [`crate::hotkey`]'s dialog: this runs
/// during `setup`, before the event loop starts, so a blocking dialog would
/// deadlock the startup it is reporting on.
fn report_failure(app: &AppHandle, error: &str) {
    eprintln!("tray: could not create the tray icon: {error}");
    let detail = format!(
        "UP-TAKE has no tray icon, so it has no menu and no Quit command.\n\n\
         {} still summons the overlay and Esc still hides it, so the app is usable. \
         To close it, end `up-take.exe` from Task Manager.\n\n\
         Restarting UP-TAKE usually clears this. If it persists, please report it \
         with the details below.\n\n{error}",
        hotkey::SUMMON_LABEL
    );
    app.dialog()
        .message(detail)
        .kind(MessageDialogKind::Warning)
        .title("UP-TAKE — tray unavailable")
        .show(|_| {});
}

/// Builds the tray icon and its menu.
fn build(app: &AppHandle) -> Result<(), String> {
    // Sourced from `bundle.icon` in tauri.conf.json — the same icon already
    // embedded for the window and the installer, so there is nothing new to
    // ship. `None` here means the config path is broken, which a menu item
    // can't fix either.
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| "no default window icon is configured".to_string())?;

    let show = MenuItem::with_id(
        app,
        SHOW_ID,
        format!("Show UP-TAKE ({})", hotkey::SUMMON_LABEL),
        true,
        None::<&str>,
    )
    .map_err(|e| format!("Could not build the Show menu item: {e}"))?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Quit", true, None::<&str>)
        .map_err(|e| format!("Could not build the Quit menu item: {e}"))?;
    let menu = Menu::with_items(app, &[&show, &quit])
        .map_err(|e| format!("Could not build the tray menu: {e}"))?;

    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("UP-TAKE")
        .menu(&menu)
        // Left click is freed for the show action below; right click (or the
        // platform's menu gesture) still opens the menu regardless of this
        // setting — it only governs the left button.
        .show_menu_on_left_click(false)
        // Each arm announces itself in debug builds. None of these actions
        // left a trace on success before, which made a clean exit
        // indistinguishable from any other way the process could end — a
        // verification run was lost to exactly that ambiguity. The Show
        // arms are separated for the same reason: the menu item and a left
        // click reach the same `overlay::show` through different tauri
        // callbacks, so one line is what tells you which one fired.
        .on_menu_event(|app, event| match event.id.as_ref() {
            SHOW_ID => {
                #[cfg(debug_assertions)]
                eprintln!("tray: Show chosen from the menu");
                if let Err(error) = overlay::show(app) {
                    eprintln!("tray: could not show the overlay: {error}");
                }
            }
            QUIT_ID => {
                // The last line the app prints. `app.exit(0)` unwinds through
                // `RunEvent::Exit`, so anything logged after this would be a
                // lie about the order.
                #[cfg(debug_assertions)]
                eprintln!("tray: Quit chosen — exiting");
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // `Up` is the click completing (button released over the icon),
            // the conventional trigger for a tray icon's primary action.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                #[cfg(debug_assertions)]
                eprintln!("tray: left click on the icon");
                if let Err(error) = overlay::show(app) {
                    eprintln!("tray: could not show the overlay: {error}");
                }
            }
        })
        .build(app)
        .map_err(|e| format!("Could not create the tray icon: {e}"))?;

    Ok(())
}
