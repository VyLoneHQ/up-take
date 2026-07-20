// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Not `.expect()`: architecture.md §5 forbids unwrap/expect outside tests,
    // and the workspace lints enforce it. A panic here would surface to the user
    // as a silent disappearance with a stack trace behind it.
    //
    // The message currently goes to stderr, which is invisible in a release
    // build because of `windows_subsystem = "windows"` in main.rs. Roadmap task
    // 1.15 (structured logging + user-facing errors) replaces this with
    // something a user can actually act on.
    if let Err(error) = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
    {
        eprintln!("UP-TAKE failed to start: {error}");
        std::process::exit(1);
    }
}
