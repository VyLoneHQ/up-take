mod click_through;
#[cfg(windows)]
mod display_watch;
mod overlay;

use tauri::{Manager, WindowEvent};

/// Builds and runs the Tauri application.
///
/// Returns the startup error rather than handling it here, so the caller
/// decides how to exit. This matters: `std::process::exit` terminates without
/// unwinding, so no `Drop` implementation runs. Once roadmap task 1.5 adds the
/// single-instance guard, that guard will own a lock whose release lives in a
/// destructor — exiting from inside this function would leave a stale lock
/// behind and block the next launch, a failure that only reproduces after an
/// already-failed start.
///
/// Not `.expect()` either: architecture.md §5 forbids unwrap/expect outside
/// tests, and the workspace lints enforce it. A panic in an always-on tray app
/// is a lost session.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            overlay::overlay_show,
            overlay::overlay_hide,
            click_through::overlay_set_interactive_regions
        ])
        .on_window_event(|window, event| {
            if window.label() != overlay::WINDOW_LABEL {
                return;
            }
            // Each of these can invalidate the CSS→physical region conversion
            // (the window's origin or scale factor changed) or the overlay's
            // fit itself — tao's WM_DPICHANGED handler rescales the window's
            // physical size, which sync_bounds must undo. sync_bounds is
            // self-converging, so the events raised by its own corrections
            // come back here and find nothing to do.
            if matches!(
                event,
                WindowEvent::Moved(_)
                    | WindowEvent::Resized(_)
                    | WindowEvent::ScaleFactorChanged { .. }
            ) && let Err(error) = overlay::sync_bounds(window.app_handle())
            {
                eprintln!("overlay: could not re-sync after a window event: {error}");
            }
        })
        .setup(|app| {
            // State must be managed and the poll thread parked before the
            // first `overlay::show`, which activates the poll.
            app.manage(click_through::ClickThrough::new());
            click_through::spawn_poll_thread(app.handle().clone());
            // Display-configuration changes reach a *visible* overlay only
            // through WM_DISPLAYCHANGE, which tao does not surface — the
            // native hook is the M-6 subscription (task 1.3).
            //
            // Logged rather than propagated: `?` here would refuse to start the
            // whole app over a degraded subscription. What is actually lost is
            // narrow — `show` still re-fits the overlay, and the Moved/Resized/
            // ScaleFactorChanged hook above still drives `sync_bounds`; only a
            // display change arriving while the overlay is already visible goes
            // unnoticed. Architecture §5 class 3: log with context, keep the app
            // alive.
            #[cfg(windows)]
            if let Err(error) = display_watch::install(app.handle()) {
                eprintln!(
                    "display-watch: display changes while the overlay is visible will not be tracked: {error}"
                );
            }
            // Until the global hotkey lands (task 1.4) a release build has no
            // way to summon the overlay; in dev it shows at startup so
            // `pnpm tauri dev` demonstrates it. Esc hides it again.
            #[cfg(debug_assertions)]
            overlay::show(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
}
