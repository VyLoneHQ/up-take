// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

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
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
}
