//! The only crate permitted to call the `tracing` macros.
//!
//! Task 1.15. `clippy.toml` bans those macros across the workspace and this
//! crate's `Cargo.toml` sets `disallowed_macros = "allow"` for itself alone, so
//! **every log line UP-TAKE writes is in this file** and the compiler is what
//! keeps it that way.
//!
//! # ⚠️ THE PRIVACY RULE, AND HOW IT IS HELD
//!
//! **Nothing that came off the user's screen may be logged.** Not recognised
//! text, not pixels, not a saved file's name, not a window title.
//! `architecture.md` section 4's threat table (private planning repository)
//! puts "captured screen content" first. A log file is not telemetry, but it is
//! that same asset sitting on a user's disk where they did not ask for it,
//! readable by anything running as them.
//!
//! Two structural facts hold it, neither of them a checker:
//!
//! 1. **A crate boundary the compiler enforces.** Logging cannot be scattered,
//!    because nowhere else may call the macros.
//! 2. **The messages are `&'static str` by type.** A literal cannot contain
//!    what was on a screen at runtime. There is exactly one exception,
//!    [`measurement`], and it is named for what it is.
//!
//! # What this replaced
//!
//! Rounds 1 to 3 of `PR #94` reviewed a test that scanned the crate's source
//! for forbidden identifiers reaching a logging macro. It was walked past
//! **eight times** -- laundering through `format!` into the module's own
//! wrapper, a multi-line call, a raw-string quote desync, a brace-less
//! `#[cfg(test)] mod x;`, a crate with no `src/`, `eprintln!` missing from the
//! sink list, a sink chosen by array order rather than by position, and a
//! sibling-crate leak -- and it went red once on correct code. Every fix grew
//! the parser and the next round found more.
//!
//! **Ten BEHAVIOUR findings across three rounds, every one in that scanner and
//! none in the feature it guarded.** A hand-rolled Rust parser inside a unit
//! test is a lint with no compiler. This is the same rule with a compiler.
//!
//! # What is honestly NOT held
//!
//! - **`eprintln!` is not banned yet.** 68 remain in `src-tauri`; part 2 of
//!   `1.15` is where they go and the ban widens with them. Adding it today
//!   would need 68 exceptions, which is worse than the gap.
//! - **A crate-root `#![allow(clippy::disallowed_macros)]` waives the ban,
//!   and no manifest changes.** Round 4 of `PR #94` drilled it: one line at
//!   another crate's root and clippy goes green on a live leak. I had
//!   written that the only escape was a `Cargo.toml` change; that was
//!   false. clippy cannot prevent it -- a crate sets its own lint levels --
//!   so `no_other_crate_waives_the_ban` looks for the string instead. That
//!   is a text check, and it is deliberately the dumbest kind there is.
//! - **[`trouble`] takes a `&dyn Display` cause.** A caller who builds a string
//!   out of screen content and passes it defeats this. `&dyn Error` would fit
//!   better but this codebase's errors are `String` throughout. It is one
//!   signature to review rather than seventeen files to scan, and it is
//!   UP-TAKE `I-381` (qualified: AGENTIC-OS has an unrelated row of that
//!   number).

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The target every event carries, so a log reader can filter to this app.
const TARGET: &str = "up-take";

/// Keeps the non-blocking writer's worker alive for the process's lifetime.
///
/// `tracing_appender::non_blocking` returns a guard that flushes on drop. Drop
/// it at the end of [`init`] and the file receives nothing -- a failure that
/// looks exactly like working: the subscriber installs, every macro runs, and
/// the file stays empty. Held here rather than returned because a caller can
/// forget to hold it, and the symptom does not appear until someone goes
/// looking for a log that was never written.
static WRITER_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// How many rotated files to keep. Daily rotation, so a fortnight.
///
/// Chosen rather than defaulted: an always-on tray app writes every day it
/// runs, and an unbounded log directory on a user's machine is a bug that takes
/// months to appear and is embarrassing when it does.
const KEEP_FILES: usize = 14;

/// `%LOCALAPPDATA%\VyLone\UP-TAKE\logs`, or `None` if there is no
/// `LOCALAPPDATA` at all.
///
/// ⚠️ **Deliberately NOT Tauri's `app_local_data_dir()`**, which resolves from
/// `tauri.conf.json`'s `identifier` (`com.vylone.uptake`) and gives a different
/// directory. `architecture.md` section 6's row is `%LOCALAPPDATA%\VyLone\
/// UP-TAKE\` and the spec is what is followed. Recorded because if the history
/// database later uses Tauri's helper, UP-TAKE writes to two roots and nothing
/// says so.
///
/// Split from [`init`] so it is testable without installing a global
/// subscriber, which can only happen once per process.
fn log_directory(local_app_data: Option<&Path>) -> Option<PathBuf> {
    local_app_data.map(|root| root.join("VyLone").join("UP-TAKE").join("logs"))
}

/// Installs the process-wide subscriber. Call once, early.
///
/// # Failure here is not fatal, and that is a decision
///
/// If the log directory cannot be created -- a read-only profile, a full disk,
/// a policy-locked `%LOCALAPPDATA%` -- this returns `Err` and the caller
/// carries on without a log file. An always-on capture tool that refuses to
/// start because it could not open its own diagnostics has turned a
/// convenience into an outage, and `architecture.md` section 5 class 2 says
/// transient failures degrade gracefully.
///
/// # Errors
///
/// When `LOCALAPPDATA` is unset, the directory cannot be created, the log file
/// cannot be opened, or a subscriber is already installed.
pub fn init() -> Result<PathBuf, String> {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let directory = log_directory(local.as_deref())
        .ok_or_else(|| "LOCALAPPDATA is not set, so there is nowhere to put a log".to_string())?;

    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;

    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("up-take")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(&directory)
        .map_err(|error| {
            format!(
                "could not open a log file in {}: {error}",
                directory.display()
            )
        })?;

    let (writer, guard) = tracing_appender::non_blocking(appender);

    // `UPTAKE_LOG` rather than `RUST_LOG`: this is a shipped desktop
    // application, and a user with `RUST_LOG=debug` set for an unrelated Rust
    // tool should not silently start writing UP-TAKE logs to their disk.
    let filter = tracing_subscriber::EnvFilter::try_from_env("UPTAKE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(true),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(true),
        )
        .try_init()
        .map_err(|error| format!("a tracing subscriber was already installed: {error}"))?;

    // A second `init` would have failed at `try_init`, so the cell cannot
    // already be full here.
    let _ = WRITER_GUARD.set(guard);

    // Logged here rather than by the caller: the path is the one fact worth
    // having at the top of every log, and a caller that had to remember to
    // write it is the obligation-an-agent-must-remember shape.
    tracing::info!(target: TARGET, logs = %directory.display(), "UP-TAKE starting");

    Ok(directory)
}

/// Records something that happened. The message is a literal, by type.
pub fn note(message: &'static str) {
    tracing::info!(target: TARGET, "{message}");
}

/// Records something that went wrong but did not stop the app.
///
/// See the crate docs for why `cause` is `&dyn Display` and what that leaves
/// open.
pub fn trouble(message: &'static str, cause: &dyn fmt::Display) {
    tracing::warn!(target: TARGET, %cause, "{message}");
}

/// Records something that went wrong, against an identifier that is safe by
/// construction.
///
/// Separate from [`trouble`] because a `u64` newtype cannot carry screen
/// content, and a typed parameter says so in a way `&dyn Display` cannot.
pub fn trouble_for(message: &'static str, id: u64) {
    tracing::warn!(target: TARGET, id, "{message}");
}

/// Records a failure. The message is a literal, by type.
///
/// Callers that also need to tell the USER use `diagnostics::report_failure`
/// in `src-tauri`, which shows a dialog and calls this. The dialog text is
/// deliberately not passed here: the dialog is read by the person whose screen
/// it is, the log file on their disk is not.
pub fn failure(message: &'static str) {
    tracing::error!(target: TARGET, "{message}");
}

/// The one exception, for `quality-bars.md` section 1 budget lines.
///
/// # Why this exists at all, when everything else takes a literal
///
/// `output::report_lines` and `ocr_report_lines` build their text from an
/// action name, an elapsed time and a stage split -- numbers and fixed strings,
/// never anything read off the screen. They are pure functions with their own
/// tests, which is why those lines are worth having in the log.
///
/// It is deliberately ONE function whose name says what it is for, so "which
/// call sites can put a runtime `String` in the log?" is answered by
/// `git grep measurement` and by nothing else.
pub fn measurement(line: &str) {
    tracing::info!(target: TARGET, "{line}");
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed unwrap is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn the_log_directory_is_the_one_architecture_section_6_names() {
        let got = log_directory(Some(Path::new(r"C:\Users\someone\AppData\Local")))
            .expect("a directory when LOCALAPPDATA is set");
        assert_eq!(
            got,
            PathBuf::from(r"C:\Users\someone\AppData\Local\VyLone\UP-TAKE\logs"),
            "the spec's row is VyLone\\UP-TAKE, not the Tauri identifier"
        );
    }

    #[test]
    fn no_localappdata_means_no_directory_rather_than_a_relative_one() {
        // The failure that matters: joining onto an empty base produces a
        // RELATIVE path and scatters log files into whatever the working
        // directory happens to be, which for a tray app launched from Explorer
        // is `C:\Windows\system32`.
        assert_eq!(log_directory(None), None);
    }

    /// The public surface is a FIXED LIST, so adding any helper fails until a
    /// human edits this list and thinks about what it logs.
    ///
    /// # Why a name set and not a signature scan
    ///
    /// The first version filtered lines starting with `pub fn ` and looked for
    /// `: &str` on the same line. Round 4 of `PR #94` defeated it with an
    /// ordinary wrapped signature -- `rustfmt`'s own canonical output for a
    /// long one -- where the opening line carries no parameter and the
    /// parameter line does not start with `pub fn `. It passed `cargo fmt
    /// --check`, passed this test, and forwarded a runtime `&str` straight
    /// into `tracing::info!`.
    ///
    /// That is the same class as the eight bypasses of the scanner this crate
    /// replaced, in miniature: a check that must UNDERSTAND Rust to work. A
    /// name set does not. `pub fn <name>` puts the name on the opening line
    /// however the arguments wrap, so extracting names is robust where
    /// extracting types is not -- and the thing worth gating is not the
    /// signature's shape but whether a person decided this helper should
    /// exist.
    #[test]
    fn the_public_surface_is_exactly_what_was_reviewed() {
        // Every entry was read and its logging argued. `measurement` is the
        // only one taking a runtime string, and the crate docs say why.
        const REVIEWED: &[&str] = &[
            "init",
            "note",
            "trouble",
            "trouble_for",
            "failure",
            "measurement",
        ];

        let mut found: Vec<String> = include_str!("lib.rs")
            .lines()
            .map(str::trim)
            .filter_map(|line| line.strip_prefix("pub fn "))
            .filter_map(|rest| rest.split(['(', '<']).next())
            .map(str::to_string)
            .collect();
        found.sort_unstable();

        let mut expected: Vec<String> = REVIEWED.iter().map(|n| (*n).to_string()).collect();
        expected.sort_unstable();

        assert_eq!(
            found, expected,
            "the public surface of the only crate allowed to log has changed. \
             Every entry here writes to a user's disk: read what the new one \
             logs, satisfy yourself it cannot carry screen content, then add it \
             to REVIEWED."
        );
    }

    /// Nothing outside this crate may waive the ban.
    ///
    /// # Round 4's sharpest finding, and it broke my own argument
    ///
    /// I had written that "the only escape is a `Cargo.toml` change, which
    /// review sees". That was FALSE. A single `#![allow(clippy::
    /// disallowed_macros)]` at another crate's ROOT waives the ban entirely,
    /// touches no manifest, and reads like any ordinary lint suppression.
    /// Drilled by the reviewer: one line in `src-tauri/src/lib.rs` and clippy
    /// went green on a live leak.
    ///
    /// clippy cannot stop that -- a crate may set its own lint levels. So this
    /// is the one thing here that IS a text check, and it is deliberately the
    /// simplest possible kind: does a string appear in a file. No parsing, no
    /// literal tracking, no structure assumed, which is precisely what the
    /// eight bypasses of the old scanner all exploited.
    #[test]
    fn no_other_crate_waives_the_ban() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/uptake-log has a workspace root two levels up");

        let mut offences = Vec::new();
        let mut scanned = 0usize;
        let mut stack = vec![workspace.to_path_buf()];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                if path.is_dir() {
                    if !matches!(
                        name.to_str(),
                        Some("target" | ".git" | "node_modules" | "dist" | ".svelte-kit")
                    ) {
                        stack.push(path);
                    }
                    continue;
                }
                let is_source = path.extension().is_some_and(|e| e == "rs" || e == "toml");
                // This crate is the one that may.
                let is_this_crate = path.components().any(|c| c.as_os_str() == "uptake-log");
                if !is_source || is_this_crate {
                    continue;
                }
                scanned += 1;
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                if text.contains("disallowed_macros") {
                    offences.push(path.display().to_string());
                }
            }
        }

        assert!(
            scanned > 20,
            "only {scanned} files scanned, so this check is not reading the workspace"
        );
        assert!(
            offences.is_empty(),
            "only crates/uptake-log may waive the tracing ban; found `disallowed_macros` \
             in:\n{}",
            offences.join("\n")
        );
    }
}
