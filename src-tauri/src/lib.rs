mod click_through;
#[cfg(debug_assertions)]
mod dev_harness;
mod hotkey;
mod overlay;
mod overlay_state;
#[cfg(windows)]
mod overlay_wndproc;
mod placement;
mod tray;

use std::sync::Mutex;

use tauri::{Manager, RunEvent, WindowEvent};
use uptake_core::area::AreaStore;

/// Builds and runs the Tauri application.
///
/// Returns the startup error rather than handling it here, so the caller
/// decides how to exit. This matters: `std::process::exit` terminates without
/// unwinding, so no `Drop` implementation runs — and this app owns things whose
/// cleanup lives in destructors and in `RunEvent::Exit` hooks, reached only by
/// letting `.run()` below shut the event loop down normally.
///
/// **What this is *not* protecting against, so nobody re-derives it wrongly:**
/// the single-instance guard's OS mutex is safe either way. It is a named
/// kernel object, the system closes every handle when a process terminates, and
/// the object dies with its last handle — so the next launch's `CreateMutexW`
/// cannot see `ERROR_ALREADY_EXISTS` no matter how this one ended. The guard's
/// `on_event(RunEvent::Exit)` release is tidiness, not a correctness
/// requirement, and a stale lock blocking a relaunch is not a reachable state.
/// Do not write recovery code for it.
///
/// A *second* instance never gets here anyway: the guard's own setup hook calls
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
            overlay::summon(app);
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
            overlay::overlay_escape,
            overlay::overlay_dismiss_focused,
            overlay::overlay_request_state,
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
            // The overlay's interaction state (ADR-0012), managed before the
            // first summon so `drive` always has it to read.
            app.manage(Mutex::new(overlay_state::OverlayState::Hidden));
            // The area store (ADR-0009, task 1.6b), managed before the first
            // summon: `has_areas` reads it to decide whether Living is real, and
            // the placement hook writes it when a drag creates one.
            app.manage(Mutex::new(AreaStore::new()));
            // Chain a system-cursor restore onto the panic hook before anything
            // can override the cursor, so a panic while placing does not leave
            // every app showing the crosshair. See `placement`.
            placement::install_panic_guard();
            // Clear any cursor override a *previous* run left behind. The system
            // cursor is global and survives a hard kill (ADR-0014 accepts that),
            // so without this the user keeps a crosshair everywhere until they
            // reload their cursor scheme — and, worse, this process would take
            // that crosshair for the genuine cursor when it snapshots the set it
            // restores from. Cheap, and it makes a crashed run self-repairing.
            placement::clear_cursor_residue();
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
            if let Err(error) = overlay_wndproc::install(app.handle()) {
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
            // as a second way to summon the overlay. Never fatal — but not
            // silent either: losing the tray in a release build leaves a
            // process with no window, no taskbar entry and no way to quit, so
            // `tray::install` reports its own failure to the user the way
            // `hotkey::install` does rather than logging into a void. See the
            // `tray` module docs.
            tray::install(app.handle());
            // Dev builds still summon the overlay at startup so `pnpm tauri dev`
            // demonstrates something without a keypress, and because CI never
            // exercises the dev path (friction F-7). This lands in Placement;
            // Esc/the hotkey hand control back, the tray and hotkey bring it up.
            #[cfg(debug_assertions)]
            overlay::summon(app.handle());
            Ok(())
        })
        // `build` + `run` rather than `run(context)` alone, to reach
        // `RunEvent::Exit`: a graceful shutdown (the tray Quit) must restore the
        // system cursors the placement layer may have overridden. A *hard* kill
        // runs none of this — see the `placement` module docs.
        .build(tauri::generate_context!())?
        .run(|_app, event| {
            if let RunEvent::Exit = event {
                placement::teardown();
            }
        });
    Ok(())
}
