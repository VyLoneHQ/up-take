// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(error) = up_take_lib::run() {
        // Returning ExitCode rather than calling process::exit keeps the normal
        // unwind path intact, so destructors still run. See the note on
        // up_take_lib::run.
        //
        // stderr is invisible in a release build because of the
        // windows_subsystem attribute above, so this is currently a silent
        // failure from the user's point of view — no worse than the panic it
        // replaced, but no better either. Roadmap task 1.15 (structured logging
        // + user-facing errors) is what actually fixes that.
        eprintln!("UP-TAKE failed to start: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
