//! System tray icon, menu, and the app's only quit path (roadmap task 1.5).
//!
//! PRODUCT-VISION.md §4.3 is explicit: `Esc` **never quits**, only hides —
//! quitting is a tray action, full stop. This module is therefore the only
//! place in the app that calls [`AppHandle::exit`].

use tauri::AppHandle;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::{hotkey, overlay};

const SHOW_ID: &str = "show";
const QUIT_ID: &str = "quit";

/// Builds the tray icon and its menu.
///
/// Never fatal (architecture §5 class 3): everything here is a bundling/OS
/// resource concern, not a user-fixable one, and refusing to start over a
/// missing icon would strand the user worse than a hotkey with no tray to
/// back it up. The caller logs the error and keeps going.
pub fn install(app: &AppHandle) -> Result<(), String> {
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
        .on_menu_event(|app, event| match event.id.as_ref() {
            SHOW_ID => {
                if let Err(error) = overlay::show(app) {
                    eprintln!("tray: could not show the overlay: {error}");
                }
            }
            QUIT_ID => app.exit(0),
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
                if let Err(error) = overlay::show(app) {
                    eprintln!("tray: could not show the overlay: {error}");
                }
            }
        })
        .build(app)
        .map_err(|e| format!("Could not create the tray icon: {e}"))?;

    Ok(())
}
