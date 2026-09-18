//! System tray icon, menu, and the app's only quit path (roadmap task 1.5).
//!
//! PRODUCT-VISION.md §4.3 is explicit: `Esc` **never quits**, only hides —
//! quitting is a tray action, full stop. This module is therefore the only
//! place in the app that calls [`AppHandle::exit`].
//!
//! Which is exactly why a failure here is reported to the user, and since
//! task 1.15, logged as well. ⚠️ This said "rather than logged" until then;
//! corrected by round 1 of `PR #94`'s review, which found the sentence still
//! standing in a file that same change had edited. The overlay window is `visible: false`, `skipTaskbar: true` and
//! `decorations: false`. Since roadmap 1.34 a launch does show it, in
//! Placement, but the first `Esc` hands the screen back, and with no areas
//! that is Hidden. So a release build whose tray did not come up is, one
//! keypress after launch, a process with no tray, no taskbar entry, no window
//! and no quit command. ⚠️ This said the startup summon was debug-only, which
//! was true until 1.34 and made the same conclusion sound unconditional; it
//! now holds from the first `Esc` rather than from launch.
//! `eprintln!` reaches nobody there (`main.rs`
//! sets `windows_subsystem = "windows"`), which would leave that user with a
//! process they cannot close and no idea why. Same reasoning as
//! [`crate::hotkey::install`], and stronger: a hotkey conflict is
//! user-fixable, a missing tray is not.
//!
//! Since task 1.15 part 2 this module writes through
//! [`crate::diagnostics`] rather than to a console nobody is reading, so the
//! menu actions leave a trace in a release build too. The dialog on a failed
//! tray, above, is unchanged and is still the part a user sees.

use tauri::AppHandle;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::{hotkey, overlay};

// Namespaced deliberately. `TrayIcon::register` pushes the `on_menu_event`
// handler into `AppManager`'s *global* menu-event listener list, so this
// closure is invoked for menu events from every menu in the app and matches on
// the raw id string alone. A bare `"quit"` added by a later menu would land in
// the `QUIT_ID` arm and exit the app; the `_ => {}` fallback cannot prevent
// that, because the collision is a match, not a miss.
//
// ✅ The candidate this named -- 1.14's settings UI -- has arrived, and it
// carries no native menu at all: the settings window draws its own controls in
// the WebView, so there is no second `MenuItem` anywhere in the app and the
// only ids in this list are the three below. The prediction was right about
// the hazard and wrong about where it would come from, so the rule stands and
// the example is struck: **namespace every id**, and the next native menu is
// still the one to watch.
const SHOW_ID: &str = "tray:show";
const SETTINGS_ID: &str = "tray:settings";
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
    use crate::strings::{self, Text};
    let detail = strings::fill(
        Text::TrayUnavailableDetail,
        &[("hotkey", hotkey::SUMMON_LABEL), ("error", error)],
    );
    // The tailored message above is this module's; the log-and-show mechanics
    // are shared with `hotkey` through `diagnostics` (task 1.15), which is the
    // half that was duplicated.
    crate::diagnostics::report_failure(
        app,
        "tray: could not create the tray icon",
        strings::text(Text::TrayUnavailableTitle),
        &detail,
    );
}

/// Builds the tray icon and its menu.
fn build(app: &AppHandle) -> Result<(), String> {
    use crate::strings::{self, Text};
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
        strings::fill(Text::TrayShow, &[("hotkey", hotkey::SUMMON_LABEL)]),
        true,
        None::<&str>,
    )
    .map_err(|e| format!("Could not build the Show menu item: {e}"))?;
    // The tray is where Settings opens from (`UI-UX.md` section 3.3). It is
    // the only always-available surface: the overlay is hidden most of the
    // time, and when it is up it is a canvas rather than a chrome.
    let settings = MenuItem::with_id(
        app,
        SETTINGS_ID,
        strings::text(Text::TraySettings),
        true,
        None::<&str>,
    )
    .map_err(|e| format!("Could not build the Settings menu item: {e}"))?;
    let quit = MenuItem::with_id(
        app,
        QUIT_ID,
        strings::text(Text::TrayQuit),
        true,
        None::<&str>,
    )
    .map_err(|e| format!("Could not build the Quit menu item: {e}"))?;
    // Settings between Show and Quit, so the destructive item stays last.
    let menu = Menu::with_items(app, &[&show, &settings, &quit])
        .map_err(|e| format!("Could not build the tray menu: {e}"))?;

    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("UP-TAKE")
        .menu(&menu)
        // Left click is freed for the show action below; right click (or the
        // platform's menu gesture) still opens the menu regardless of this
        // setting — it only governs the left button.
        .show_menu_on_left_click(false)
        // Each arm announces itself. None of these actions left a trace on
        // success before, which made a clean exit indistinguishable from any
        // other way the process could end, and a verification run was lost to
        // exactly that ambiguity. The Show arms are separated for the same
        // reason: the menu item and a left click reach the same
        // `overlay::summon` through different tauri callbacks, so one line is
        // what tells you which one fired.
        //
        // **These were `#[cfg(debug_assertions)] eprintln!` until task 1.15
        // part 2**, which is to say they answered that question for whoever
        // built the app and for nobody who runs it. The ambiguity is at its
        // worst in a release build on someone else's machine, where "did they
        // quit, or did it die?" is exactly what a log has to answer. Every
        // message here is a literal, so `uptake-log`'s privacy rule holds by
        // type rather than by care.
        .on_menu_event(|app, event| match event.id.as_ref() {
            SHOW_ID => {
                crate::diagnostics::note("tray: Show chosen from the menu");
                // Summon into Placement (ADR-0012), the same as a relaunch.
                overlay::summon(app);
            }
            SETTINGS_ID => {
                crate::diagnostics::note("tray: Settings chosen from the menu");
                crate::settings_window::open(app);
            }
            QUIT_ID => {
                // The last line the app logs. `app.exit(0)` unwinds through
                // `RunEvent::Exit`, so anything logged after this would be a
                // lie about the order.
                crate::diagnostics::note("tray: Quit chosen, exiting");
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
                crate::diagnostics::note("tray: left click on the icon");
                overlay::summon(app);
            }
        })
        .build(app)
        .map_err(|e| format!("Could not create the tray icon: {e}"))?;

    Ok(())
}
