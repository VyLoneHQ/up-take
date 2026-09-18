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
        // windows_subsystem attribute above, so this is a silent failure from
        // the user's point of view: no worse than the panic it replaced, but no
        // better either.
        //
        // ⚠️ **This said roadmap task 1.15 "is what actually fixes that", and
        // part 2 has now run WITHOUT fixing it.** Said plainly rather than left
        // as a promise that quietly expired. Part 2 routed 45 call sites through
        // the log and could not route this one: if `run` returned an error,
        // nothing establishes that `diagnostics::init` ever succeeded, so a log
        // call here may vanish, which is the same silence in a different file.
        // What this wants is a DIALOG, and at this point there is no Tauri app
        // to raise one from. That is a design question rather than a conversion,
        // and it is carried as its own backlog row.
        #[allow(
            clippy::print_stderr,
            reason = "this can run before the log exists, so it is one of the two sinks task 1.15 part 2 deliberately leaves on stderr"
        )]
        {
            eprintln!("UP-TAKE failed to start: {error}");
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
