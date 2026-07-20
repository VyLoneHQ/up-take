mod overlay;

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
            overlay::overlay_hide
        ])
        .setup(|_app| {
            // Until the global hotkey lands (task 1.4) a release build has no
            // way to summon the overlay; in dev it shows at startup so
            // `pnpm tauri dev` demonstrates it. Esc hides it again.
            #[cfg(debug_assertions)]
            overlay::show(_app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
}
