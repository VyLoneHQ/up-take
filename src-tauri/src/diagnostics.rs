//! Task 1.15: structured logging, and the one place a failure reaches the user.
//!
//! This module implements `SPECS/architecture.md` section 5's error contract.
//! It did not invent that contract: the three error classes, the `thiserror` /
//! `anyhow` split and the never-lose-the-capture rule were written down long
//! before this file existed. What was missing was a destination.
//!
//! # What this replaces, and why it was not merely untidy
//!
//! Every failure path in `output.rs` ended at `eprintln!`, which in a release
//! build reaches nobody: the app is launched from Explorer or the tray, and
//! there is no console attached to read. That is UP-TAKE finding `F-35`,
//! accepted-not-fixed since 2026-07, and its sharp edge is that **Copy is a
//! primary user-initiated action**. A user who presses Copy and gets silence
//! cannot tell "it worked" from "the clipboard is unchanged and you have lost
//! the thing you grabbed".
//!
//! `hotkey.rs` said the quiet part in a doc comment: its startup failure is
//! *"shown as a dialog rather than logged because there is still nowhere else
//! for"* it to go. There is now.
//!
//! # ⚠️ THE PRIVACY RULE, WHICH IS NOT NEGOTIABLE AND IS ENFORCED
//!
//! **Nothing that came off the user's screen may be logged.** Not recognised
//! text, not pixels, not a file name a capture was saved under, not a window
//! title. `architecture.md` section 4's threat table puts "captured screen
//! content" first and its mitigation is "zero telemetry"; a log file on disk
//! is not telemetry, but it is the same asset sitting somewhere the user did
//! not ask for it to sit, readable by any process running as them.
//!
//! This is the one rule in this file that a well-meaning future change is
//! likely to break, because logging the recognised text is *obviously useful*
//! when debugging OCR. So it is not left to care: `tests::no_captured_content_
//! reaches_a_log_macro` reads this crate's own source and fails on a logging
//! macro whose arguments name a value carrying screen content. That test is
//! the rule; this paragraph is only its explanation.
//!
//! # Where the file goes
//!
//! `%LOCALAPPDATA%\VyLone\UP-TAKE\logs\`, which is `architecture.md` section
//! 6's row for "Database, logs, cache", spelled exactly as that table spells
//! it.
//!
//! ⚠️ **This is deliberately NOT Tauri's `app_local_data_dir()`.** That
//! resolves from `tauri.conf.json`'s `identifier`, which is
//! `com.vylone.uptake`, so Tauri's own answer is
//! `%LOCALAPPDATA%\com.vylone.uptake`. The two are different directories and
//! the spec is the one being followed. Recorded rather than quietly resolved:
//! if the history database later uses Tauri's helper, UP-TAKE will write its
//! data to two roots and nothing will say so.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

/// Keeps the non-blocking writer's worker alive for the process's lifetime.
///
/// `tracing_appender::non_blocking` returns a guard that flushes on drop. Drop
/// it at the end of `init` and the file receives nothing, which is a failure
/// mode that looks exactly like working: the subscriber installs, every macro
/// runs, and the file stays empty. Held in a `OnceLock` rather than returned to
/// the caller because a caller can forget to hold it, and the symptom does not
/// appear until someone goes looking for a log that was never written.
static WRITER_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// How many rotated files to keep. Daily rotation, so this is a fortnight.
///
/// Chosen rather than defaulted: an always-on tray app writes every day it
/// runs, and an unbounded log directory on a user's machine is a bug that
/// takes months to show up and is embarrassing when it does.
const KEEP_FILES: usize = 14;

/// `%LOCALAPPDATA%\VyLone\UP-TAKE\logs`, or `None` if the environment has no
/// `LOCALAPPDATA` at all.
///
/// Split from [`init`] so it is testable without installing a global
/// subscriber -- a subscriber can be installed once per process, so a test
/// that went through `init` could run only once and would poison every test
/// after it.
fn log_directory(local_app_data: Option<&Path>) -> Option<PathBuf> {
    local_app_data.map(|root| root.join("VyLone").join("UP-TAKE").join("logs"))
}

/// Installs the process-wide subscriber. Call once, early, from `setup`.
///
/// # Failure here is not fatal, and that is a decision
///
/// If the log directory cannot be created -- a read-only profile, a full disk,
/// a policy-locked `%LOCALAPPDATA%` -- this returns `Err` and the caller
/// carries on without a log file. An always-on capture tool that refuses to
/// start because it could not open its own diagnostics has turned a
/// diagnostic convenience into an outage, and `architecture.md` section 5
/// class 2 says transient failures degrade gracefully.
///
/// The stderr layer is installed regardless, so a developer running from a
/// console still sees everything even when the file half failed.
pub(crate) fn init() -> Result<PathBuf, String> {
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
    // application, and a user with `RUST_LOG=debug` set for some unrelated
    // Rust tool should not silently start writing UP-TAKE debug logs to their
    // disk. Defaults to `info`.
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

    // Ignore the result: a second `init` would have failed at `try_init`
    // above, so reaching here with the cell already full is not possible.
    let _ = WRITER_GUARD.set(guard);

    Ok(directory)
}

/// Logs a failure and puts it in front of the user, without blocking.
///
/// # What this unifies, and what it deliberately does NOT
///
/// `tray.rs` and `hotkey.rs` each had their own `report_failure`: both logged
/// with `eprintln!` and both opened a non-blocking warning dialog. That
/// mechanical half is one rule with two implementations, which is the shape
/// `PR #88` rounds 5 and 8 both found, and a third copy for the output
/// pipeline would have made it three.
///
/// **Their MESSAGES are not unified and must not be.** The tray's text
/// explains that the app is still usable and how to quit it from Task Manager;
/// the hotkey's distinguishes "another application holds this combination"
/// (manual scenario `M-9`) from "Windows refused". Those are the valuable part
/// and they are specific to their callers. This function takes the finished
/// `detail` and is not in the business of composing it.
///
/// # Non-blocking, always
///
/// Both original copies documented the same reason and it still holds: a
/// blocking dialog during `setup` deadlocks the startup it is reporting on,
/// because the event loop has not started. After startup it matters for a
/// different reason -- a modal dialog stealing focus from a capture tool is a
/// second failure on top of the first.
///
/// # It logs as well as shows
///
/// The dialog is transient and a user may dismiss it unread. The log line is
/// what a support conversation can refer to a week later.
pub(crate) fn report_failure(app: &AppHandle, source: &str, title: &str, detail: &str) {
    tracing::error!(target: "up-take", source, detail);

    app.dialog()
        .message(detail)
        .kind(MessageDialogKind::Warning)
        .title(title)
        .show(|_| {});
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
        // The failure that matters: joining onto an empty base would produce a
        // RELATIVE path and scatter log files into whatever the working
        // directory happens to be, which for a tray app launched from Explorer
        // is `C:\Windows\system32`.
        assert_eq!(log_directory(None), None);
    }

    /// ⚠️ THIS TEST IS THE PRIVACY RULE. The module docs only explain it.
    ///
    /// It reads this crate's own source and fails if a `tracing` macro is
    /// handed a value that carries content off the user's screen. Written
    /// because "do not log the recognised text" is exactly the instruction a
    /// future session cannot be relied on to remember (`OS-F46`), and because
    /// logging it is the obvious thing to reach for when OCR misreads
    /// something.
    ///
    /// It is deliberately a source scan and not a runtime assertion: the
    /// failure it prevents is a line of code being written, and by the time
    /// such a line runs the content is already in the file.
    #[test]
    fn no_captured_content_reaches_a_log_macro() {
        // Identifiers that hold screen content in this crate. A name added
        // here costs nothing; a name missing from here is how the rule fails
        // quietly, so the list is deliberately broad and matches on the
        // ARGUMENT TEXT rather than on types.
        const FORBIDDEN: &[&str] = &[
            "text",
            "recognised",
            "recognized",
            "pixels",
            "bitmap",
            "rgba",
            "png",
            "dib",
            "frame",
            "crop",
            "clipboard_text",
            "ocr_text",
        ];

        let mut offences = Vec::new();
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rust_files(&source_root, &mut files);
        assert!(
            files.len() > 5,
            "the scan found only {} source files, so it is not reading the crate",
            files.len()
        );

        for file in &files {
            let source = fs::read_to_string(file).expect("a readable source file");
            for (number, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                // Comments and doc comments talk ABOUT the rule constantly,
                // including in this very file.
                if trimmed.starts_with("//") {
                    continue;
                }
                let Some(rest) = logging_macro_arguments(trimmed) else {
                    continue;
                };
                let checkable = fields_and_captures(rest);
                for needle in FORBIDDEN {
                    if field_name_matches(&checkable, needle) {
                        offences.push(format!(
                            "{}:{} logs `{needle}`: {trimmed}",
                            file.display(),
                            number + 1
                        ));
                    }
                }
            }
        }

        assert!(
            offences.is_empty(),
            "captured screen content must never be logged \
             (architecture.md section 4). Offending lines:\n{}",
            offences.join("\n")
        );
    }

    /// The argument text of a `tracing` macro call, if the line is one.
    fn logging_macro_arguments(line: &str) -> Option<&str> {
        for macro_name in [
            "tracing::error!",
            "tracing::warn!",
            "tracing::info!",
            "tracing::debug!",
            "tracing::trace!",
            "error!(",
            "warn!(",
            "info!(",
            "debug!(",
            "trace!(",
        ] {
            if let Some(index) = line.find(macro_name) {
                return Some(&line[index + macro_name.len()..]);
            }
        }
        None
    }

    /// The part of a macro call that can carry DATA, with prose removed.
    ///
    /// # Why this is not just the raw argument text
    ///
    /// The first version of this control matched the whole argument string and
    /// immediately fired on
    /// `tracing::warn!(%error, "output: dropped the pinned pixels ...")` --
    /// the word `pixels` in an English sentence, not a field carrying a pixel
    /// buffer. That is a false positive on the project's own correct code, and
    /// a control that cries wolf is one somebody deletes. So string literals
    /// are stripped.
    ///
    /// # ...but NOT their captures
    ///
    /// `tracing::info!("{line}")` interpolates the variable `line`. Dropping
    /// the literal whole would blind the control to exactly the case where a
    /// message is built from captured content. So `{...}` spans inside a
    /// literal are KEPT while the prose around them is discarded.
    fn fields_and_captures(arguments: &str) -> String {
        let mut out = String::new();
        let mut inside_literal = false;
        let mut chars = arguments.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if inside_literal => {
                    // Skip the escaped character so an escaped quote does not
                    // read as the end of the literal.
                    let _ = chars.next();
                }
                '"' => inside_literal = !inside_literal,
                '{' if inside_literal => {
                    out.push(' ');
                    for inner in chars.by_ref() {
                        if inner == '}' {
                            break;
                        }
                        out.push(inner);
                    }
                    out.push(' ');
                }
                _ if inside_literal => {}
                _ => out.push(c),
            }
        }
        out
    }

    /// Whether `needle` appears in `arguments` as a whole identifier.
    ///
    /// Substring matching would fire on `context`, which contains `text`, and
    /// a control that cries wolf is one somebody deletes.
    fn field_name_matches(arguments: &str, needle: &str) -> bool {
        let bytes = arguments.as_bytes();
        let mut from = 0;
        while let Some(offset) = arguments[from..].find(needle) {
            let start = from + offset;
            let end = start + needle.len();
            let before_ok = start == 0 || !is_identifier_byte(bytes[start - 1]);
            let after_ok = end == bytes.len() || !is_identifier_byte(bytes[end]);
            if before_ok && after_ok {
                return true;
            }
            from = end;
        }
        false
    }

    const fn is_identifier_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }

    fn collect_rust_files(directory: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn the_privacy_scan_can_actually_fail() {
        // `no_captured_content_reaches_a_log_macro` passing means nothing
        // unless it can fail. Round 12 of PR #88 is this project's record of a
        // guard that was documented as drilled and drilled by nothing.
        assert!(field_name_matches(
            r#"target: "up-take", text = %recognised"#,
            "text"
        ));
        assert!(field_name_matches("pixels = ?buffer", "pixels"));
        // ...and that it does NOT fire on a word that merely contains one.
        assert!(!field_name_matches("context = %detail", "text"));
        assert!(!field_name_matches("subtext_length = 4", "text"));
        // Prose is not data: the real line that made the first version of this
        // control fire on correct code.
        assert!(!field_name_matches(
            &fields_and_captures(
                r#"target: "up-take", %error, "output: dropped the pinned pixels but could not announce it""#
            ),
            "pixels"
        ));
        // ...but a capture INSIDE a message still counts, which is the case
        // stripping literals could have blinded.
        assert!(field_name_matches(
            &fields_and_captures(r#"target: "up-take", "read {text} from the area""#),
            "text"
        ));
        // And a field beside prose is still seen.
        assert!(field_name_matches(
            &fields_and_captures(r#"pixels = ?buffer, "a harmless message""#),
            "pixels"
        ));
        // The macro detector, likewise, in both directions.
        assert!(logging_macro_arguments(r#"tracing::info!(a = 1);"#).is_some());
        assert!(logging_macro_arguments("let text = ocr()?;").is_none());
    }
}
