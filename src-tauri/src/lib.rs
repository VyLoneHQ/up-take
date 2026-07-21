mod click_through;
#[cfg(debug_assertions)]
mod dev_harness;
#[cfg(windows)]
mod display_watch;
mod hotkey;
mod overlay;
mod tray;

use tauri::{Manager, WindowEvent};

/// Builds and runs the Tauri application.
///
/// Returns the startup error rather than handling it here, so the caller
/// decides how to exit. This matters: `std::process::exit` terminates without
/// unwinding, so no `Drop` implementation runs. The single-instance guard
/// (task 1.5) is exactly why this still matters: on the surviving instance its
/// OS mutex is released by an `on_event(RunEvent::Exit)` hook, which only fires
/// if the event loop gets to shut down normally — `.run()` below is what drives
/// that, so nothing in this function may short-circuit past it. A *second*
/// instance is a different story: the guard's own setup hook calls
/// `std::process::exit(0)` itself, deliberately, before this crate's `setup`
/// closure — and therefore before `ClickThrough`, the hotkey or the tray exist
/// — so there is nothing of ours left unreleased at that point.
///
/// Not `.expect()` either: architecture.md §5 forbids unwrap/expect outside
/// tests, and the workspace lints enforce it. A panic in an always-on tray app
/// is a lost session.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> tauri::Result<()> {
    let mut builder = tauri::Builder::default();

    // Registered before every other plugin, deliberately: plugins initialize
    // in registration order and the guard's setup hook can call
    // `std::process::exit(0)` synchronously the moment it finds another
    // instance's mutex, so this ordering is what keeps a doomed second
    // process from spending any time on global-shortcut, dialog, or this
    // crate's own `setup` closure first.
    //
    // Debug-only escape hatch: this is also the only way M-9 (another app
    // already holding the hotkey) was tested — a second UP-TAKE instance
    // standing in for the other app. A guarded process never reaches
    // `hotkey::install`, so `UPTAKE_DEV_ALLOW_MULTIPLE` skips registering the
    // guard to keep that test possible. See `dev_harness`.
    #[cfg(debug_assertions)]
    let single_instance_disabled = dev_harness::single_instance_disabled();
    #[cfg(not(debug_assertions))]
    let single_instance_disabled = false;

    if !single_instance_disabled {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A relaunch is the same signal as the hotkey — the user wants the
            // overlay, not a second process — so it gets the same response.
            //
            // Printed unconditionally in debug builds (not gated behind
            // `UPTAKE_DEV_RESHOW` like `dev_harness::log_summon`): this is the
            // only external signal that the guard's callback fired at all, and
            // the second process exits before it can log anything of its own.
            #[cfg(debug_assertions)]
            eprintln!("single-instance: relaunch detected, summoning the overlay");
            if let Err(error) = overlay::show(app) {
                eprintln!("single-instance: could not show the overlay: {error}");
            }
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        // Used only from Rust, and only to report a failed hotkey registration
        // (architecture §4). No frontend capability grants it, so the WebView
        // cannot open dialogs.
        .plugin(tauri_plugin_dialog::init())
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
            // Recorded here because `setup` runs on the event-loop thread, so
            // this is the identity every later summon is compared against.
            #[cfg(debug_assertions)]
            dev_harness::record_main_thread();
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
            // Registered before the tray: architecture §4's mitigation is
            // telling the user a failed registration, and that still holds
            // even if the tray itself fails to build right after — the two
            // failures are independent and neither should hide the other.
            // Never fatal — see `hotkey::install`.
            hotkey::install(app.handle());
            // The tray is now the only quit path (PRODUCT-VISION §4.3) as well
            // as a second way to summon the overlay. Never fatal, same class
            // as display-watch above: a missing icon is a bundling problem,
            // not a reason to refuse to start with a working hotkey.
            if let Err(error) = tray::install(app.handle()) {
                eprintln!("tray: could not create the tray icon: {error}");
            }
            // Dev builds still show the overlay at startup so `pnpm tauri dev`
            // demonstrates something without a keypress, and because CI never
            // exercises the dev path (friction F-7). Esc hides it; the hotkey
            // and the tray both bring it back.
            #[cfg(debug_assertions)]
            overlay::show(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
}
