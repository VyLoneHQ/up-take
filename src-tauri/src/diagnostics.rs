//! Task 1.15: structured logging, and the one place a failure reaches the user.
//!
//! This module implements section 5 of `SPECS/architecture.md` **in the
//! private planning repository** -- there is no `SPECS/` directory here, so a
//! bare path reads as resolvable and is not. **This qualifier appears once, at
//! this first mention; the citations below are bare, which is what the rest of
//! the codebase does.**
//!
//! ⚠️ **THE SAME WRONG ENUMERATION, THREE TIMES, AND THIS IS THE THIRD.**
//! Round 2 asked for a qualifier here and I wrote that `output.rs:5`,
//! `freeze.rs` and `hotkey.rs` "all carry" one. Round 3 checked: they do not.
//! I then "fixed" those three files -- and the enumeration behind THAT was
//! wrong too. Measured properly:
//!
//! ```text
//! grep -rn "architecture §\|architecture\.md" --include=*.rs --include=*.toml .
//! ```
//!
//! **47 sites cite it bare and exactly one qualifies it**
//! (`uptake-ocr/src/engine.rs:20`). Bare is the convention; qualifying is the
//! exception. So the three edits are reverted -- they made two files
//! inconsistent with forty-five others to satisfy a rule nobody follows -- and
//! the claim is replaced by the count and the command that produced it.
//!
//! Its error contract:
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
//! when debugging OCR. So it is not left to care:
//! `tests::no_captured_content_reaches_a_log_macro` reads this crate's own
//! source and fails when a value carrying screen content reaches a sink. That
//! test is the rule; this paragraph is only its explanation.
//!
//! ⚠️ **AND ITS LIMIT IS STATED, because the first version of this paragraph
//! overclaimed and round 1 of `PR #94` proved it.** That review laundered
//! recognised text past the guard three ways: a call split across lines, a raw
//! string whose embedded quotes desynchronised the literal tracker, and -- the
//! sharp one -- `format!` on one line and [`report_failure`] on the next, so
//! the only macro the scan could see was this module's own
//! `tracing::error!(source, detail)`, two generic parameter names. **The
//! mechanism this module introduced was invisible to its own guard.** All
//! three are closed. What is NOT closed, and what no source scan can close, is
//! content bound to a name the list does not carry: `let payload = ocr_result`
//! passes. That residual is UP-TAKE `I-381` -- qualified because AGENTIC-OS has an
//! unrelated row of the same number -- and its remedy is type-level.
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

    /// Names that hold content off the user's screen in this crate.
    ///
    /// A name added here costs nothing. A name MISSING from here is how the
    /// rule fails quietly, so the list is deliberately broad and matches on
    /// argument text rather than on types.
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

    /// Everything that can put a value somewhere it outlives the moment.
    ///
    /// # Why this is not just the `tracing` macros
    ///
    /// Round 1 of `PR #94`'s review drilled the first version of this control
    /// and laundered recognised text straight past it: build the string with
    /// `format!` on one line, hand the resulting local to
    /// [`report_failure`] on the next. The macro scan saw
    /// `tracing::error!(target: "up-take", source, detail)` inside
    /// `report_failure` -- two generic parameter names, neither forbidden --
    /// and passed. **The abstraction this module introduced as its central
    /// mechanism was invisible to its own guard.**
    ///
    /// A source scan cannot follow a value through a binding, so it does not
    /// try. It forbids BUILDING the string instead: `format!` is on this list,
    /// and interpolating screen content into one has no legitimate use in this
    /// crate. Measured rather than assumed before choosing that rule -- the
    /// whole of `src-tauri/src` contains no such `format!` today.
    const SINKS: &[&str] = &[
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
        "report_failure(",
        "format!(",
        "format_args!(",
        // ⚠️ THE ORIGINAL SINK, ABSENT UNTIL ROUND 3 FOUND IT. This whole
        // feature exists because failures ended at `eprintln!`, 68 of them
        // still remain in this crate, and the control guarding against leaks
        // did not treat the very macro it is replacing as a place a leak could
        // go. Drilled: `eprintln!("leak drill: {ocr_text}")` passed.
        "eprintln!(",
        "println!(",
        "write!(",
        "writeln!(",
        "panic!(",
    ];

    /// ⚠️ THIS TEST IS THE PRIVACY RULE. The module docs only explain it.
    ///
    /// # What it can see, stated precisely because the first version overclaimed
    ///
    /// It reads this crate's source, joins each call into one logical unit,
    /// discards prose, and fails if a [`SINKS`] entry is handed an identifier
    /// from [`FORBIDDEN`].
    ///
    /// **What it still cannot see**, and no source scan can: content bound to
    /// a name that is not on `FORBIDDEN`. `let payload = ocr_result; info!(%payload)`
    /// passes. The list is the boundary of the guarantee, which is why the
    /// remedy for that residual is a type-level one and is filed as UP-TAKE `I-381`
    /// rather than pretended away here. ⚠️ Qualified with the project name
    /// deliberately: AGENTIC-OS `I-381` is a different, unrelated row about
    /// `SHARED_TIMEOUT_S`, and round 3 resolved the bare id to it.
    #[test]
    fn no_captured_content_reaches_a_log_macro() {
        // ⚠️ EVERY `.rs` IN THE WORKSPACE, discovered by walking rather than
        // by assuming a layout.
        //
        // Round 2 made this walk `crates/*/src`, which fixed reading one crate
        // and left two holes round 3 drilled: a crate whose `Cargo.toml` sets
        // `[lib] path = "custom/lib.rs"` has no `src/` at all and contributed
        // no root, and the `roots.len() >= 4` floor had a whole crate of slack
        // so it noticed neither. Guessing at directory layout was the mistake;
        // this walks the tree instead.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest.parent().expect("src-tauri has a parent");
        let mut files = Vec::new();
        collect_rust_files(workspace, &mut files);

        // `examples/`, `tests/` and `benches/` are EXCLUDED, and it is a stated
        // boundary rather than an accident of the walk. None of the three ships
        // in an installer, so none can put a user's screen content on that
        // user's disk -- which is the asset `architecture.md` section 4 names.
        // It is the same reason `#[cfg(test)]` blocks are skipped, applied to
        // the files that are test code in their entirety rather than in part.
        //
        // Concretely: `ocr_smoke.rs` PRINTS what OCR read, which is its whole
        // purpose as a console tool, and `uptake-ocr/tests/threading.rs`
        // formats a frame's dimensions. Both are correct, and a control that
        // fired on them would be one somebody deletes.
        //
        // ⚠️ THIS IS A HOLE AND IT IS NAMED: a leak written into a `tests/`
        // file is invisible here. Acceptable where the production case is not,
        // for the reason above -- but acceptable is not the same as absent.
        files.retain(|p| {
            !p.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some("examples" | "tests" | "benches")
                )
            })
        });

        // Asserted PER CRATE, not in total. A total stays healthy while one
        // crate silently stops being read, which is the failure round 2's
        // `roots.len() >= 4` floor was meant to catch and did not.
        for crate_name in [
            "uptake-core",
            "uptake-capture",
            "uptake-ocr",
            "uptake-assets",
        ] {
            assert!(
                files
                    .iter()
                    .any(|p| p.components().any(|c| c.as_os_str() == crate_name)),
                "no source files found for {crate_name}, so the workspace walk is broken"
            );
        }
        assert!(
            files
                .iter()
                .any(|p| p.components().any(|c| c.as_os_str() == "src-tauri")),
            "no source files found for src-tauri, so the workspace walk is broken"
        );
        assert!(
            files.len() > 20,
            "the scan found only {} source files, so it is not reading the workspace",
            files.len()
        );

        let mut offences = Vec::new();
        for file in &files {
            let source = fs::read_to_string(file).expect("a readable source file");
            for (line_number, unit) in logical_calls(&source) {
                for needle in FORBIDDEN {
                    if field_name_matches(&unit, needle) {
                        offences.push(format!(
                            "{}:{line_number} puts `{needle}` into a sink: {}",
                            file.display(),
                            unit.trim()
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

    /// Every sink call in `source`, as `(line number, checkable argument text)`.
    ///
    /// Joins continuation lines until the call's parentheses balance, because
    /// round 1's drill split a macro across three lines and walked straight
    /// through a scan that read one physical line at a time.
    /// How many continuation lines a call may span before the scan gives up.
    ///
    /// Not decoration. The first version of the accumulator had no bound, and
    /// when it met a line it could not balance -- the `SINKS` array literal in
    /// this very file, whose entries are the sink NAMES -- it swallowed the
    /// rest of the file into one unit and reported every forbidden word in it.
    /// A runaway parse must stop, and it must stop loudly rather than by
    /// silently dropping the call.
    const MAX_CONTINUATION_LINES: usize = 80;

    fn logical_calls(source: &str) -> Vec<(usize, String)> {
        let lines: Vec<&str> = source.lines().collect();
        let mut found = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let trimmed = lines[index].trim_start();

            // ⚠️ TEST CODE IS SKIPPED, and that is a decision with a cost.
            //
            // A test module legitimately contains the very lines this control
            // forbids: `the_privacy_scan_can_actually_fail` is nothing BUT
            // deliberate leak examples, and this module's own `SINKS` list
            // spells out every sink name. Scanning them makes the control fire
            // on itself, which is what the first version did.
            //
            // The cost: a leak written inside a `#[cfg(test)]` block is
            // invisible here. That is acceptable in a way the production case
            // is not -- test code does not run in a user's installed build and
            // has no user's screen to read -- but it is a hole and is named
            // rather than left for a reviewer to find.
            if trimmed.starts_with("#[cfg(test)]") {
                index = skip_test_item(&lines, index);
                continue;
            }
            if trimmed.starts_with("//") {
                index += 1;
                continue;
            }

            // ⚠️ THE EARLIEST SINK IN THE LINE, NOT THE FIRST ONE IN THE ARRAY.
            //
            // `find_map` returned on whichever SINKS entry matched first in
            // ARRAY order, wherever it sat in the text. Round 3 drilled it:
            // `tracing::info!(text = %ocr_text, note = "e.g. tracing::error!(x)")`
            // anchored on the `tracing::error!` inside the string literal --
            // index 0 of the array, late in the line -- so the unit began after
            // the real leak and the scan passed a canonical single-line leak.
            let Some(sink) = SINKS
                .iter()
                .filter_map(|s| trimmed.find(s).map(|i| (i, i + s.len())))
                .min_by_key(|(start, _)| *start)
                .map(|(_, end)| end)
            else {
                index += 1;
                continue;
            };

            // SINKS is not uniform: the macro entries stop at `!` and the
            // function entries include their `(`. Normalise, or `tracing::error!`
            // is handed a remainder that still holds its own opening paren and
            // can never balance -- which is what the bound above caught.
            let head = strip_line_comment(&trimmed[sink..]);
            let rest = head.strip_prefix('(').unwrap_or(&head);
            let mut unit = String::from(rest);
            let mut cursor = index;
            let mut spanned = 0;
            while depth(&unit) > 0 && cursor + 1 < lines.len() {
                if spanned >= MAX_CONTINUATION_LINES {
                    // Loud, not silent. An unbalanced call is a parse this
                    // scan does not understand, and a control that quietly
                    // skips what it cannot read reports green forever.
                    panic!(
                        "PRIVACY SCAN LIMIT, NOT A LEAK: could not balance the \
                         call starting at line {} within {MAX_CONTINUATION_LINES} \
                         lines, so it was not checked. Shorten the call or raise \
                         the bound. Line: {}",
                        index + 1,
                        lines[index].trim()
                    );
                }
                cursor += 1;
                spanned += 1;
                // ⚠️ A TRAILING comment counts too, not just a whole-line one.
                // Round 3 drilled it: adding `// not text, just a plain
                // trailing note` to a legitimate multi-line `tracing::warn!`
                // in `lib.rs` turned this control RED on code that logs an
                // error and nothing else. A false positive is as damaging as a
                // miss -- it is how a control gets worked around.
                let next = strip_line_comment(lines[cursor].trim_start());
                if next.trim().is_empty() {
                    continue;
                }
                unit.push(' ');
                unit.push_str(&next);
            }
            found.push((index + 1, fields_and_captures(&unit)));
            index += 1;
        }
        found
    }

    /// `line` with any trailing `//` comment removed, ignoring `//` inside a
    /// string literal so a URL is not mistaken for a comment.
    fn strip_line_comment(line: &str) -> String {
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut inside = false;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if inside => i += 1,
                b'"' => inside = !inside,
                b'/' if !inside && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                    return line[..i].to_string();
                }
                _ => {}
            }
            i += 1;
        }
        line.to_string()
    }

    /// The index just past the attribute beginning at `from`, which may span
    /// several lines.
    fn skip_attribute(lines: &[&str], from: usize) -> usize {
        let mut index = from;
        let mut depth = 0i32;
        let mut opened = false;
        while index < lines.len() {
            for c in strip_literals(lines[index]).chars() {
                match c {
                    '[' | '(' => {
                        depth += 1;
                        opened = true;
                    }
                    ']' | ')' => depth -= 1,
                    _ => {}
                }
            }
            index += 1;
            if opened && depth <= 0 {
                return index;
            }
        }
        index
    }

    /// The index just past the `#[cfg(test)]` item beginning at `from`.
    ///
    /// # ⚠️ THIS DECIDES WHAT KIND OF ITEM IT IS BEFORE SKIPPING ANYTHING, AND
    /// THE REASON IS THAT THIS CRATE HAS ALREADY SHIPPED THE OTHER VERSION
    ///
    /// `payload_keys.rs:348-372` records it in full: a first fix there began
    /// skipping immediately and stopped at the next closing brace, "which
    /// assumes every `#[cfg(test)]` item HAS a closing brace. `lib.rs:15` is
    /// `#[cfg(test)]` followed by `mod payload_keys;`, a declaration with no
    /// braces at all... **281 of 296 lines unscanned**."
    ///
    /// Round 2 of `PR #94`'s review found this file reproducing that
    /// superseded version, and drilled it: splitting `use tauri::{Manager,
    /// RunEvent, WindowEvent};` into three plain `use` lines -- a change
    /// `cargo fmt` or an organise-imports action could make, with no semantic
    /// effect -- removed the only brace pair the runaway skip happened to
    /// anchor on, and a live leak planted in `pub fn run()` went GREEN.
    /// Today's safety was an accident of one grouped import.
    ///
    /// So: `mod x;`, `use ...;`, `const ...;` and anything else ending in `;`
    /// before an opening brace start no skip at all.
    fn skip_test_item(lines: &[&str], from: usize) -> usize {
        // The attribute itself is consumed either way.
        let mut index = from + 1;

        // Find the item the attribute applies to, stepping over further
        // attributes and comments.
        //
        // ⚠️ An attribute is not necessarily ONE LINE. This module's own test
        // block is `#[cfg(test)]` followed by a four-line
        // `#[allow(clippy::unwrap_used, ...)]`, and a version of this loop that
        // skipped only lines BEGINNING with `#` stopped on
        // `clippy::unwrap_used,`, took that for the item, and resumed scanning
        // inside the test module -- which then tripped over the `SINKS` array's
        // own entries. So attributes are balanced, not counted.
        while index < lines.len() {
            let trimmed = lines[index].trim();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                index += 1;
                continue;
            }
            if trimmed.starts_with('#') {
                index = skip_attribute(lines, index);
                continue;
            }
            break;
        }
        if index >= lines.len() {
            return index;
        }

        // A declaration terminated before any brace opens: skip nothing beyond
        // the declaration itself. This is the case that swallowed a file.
        let first = lines[index];
        let stripped = strip_literals(first);
        let brace_at = stripped.find('{');
        let semi_at = stripped.find(';');
        if brace_at.is_none() || semi_at.is_some_and(|s| brace_at.is_none_or(|b| s < b)) {
            return index + 1;
        }

        // A braced item: balance it.
        let mut depth = 0i32;
        while index < lines.len() {
            for c in strip_literals(lines[index]).chars() {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            index += 1;
            if depth <= 0 {
                return index;
            }
        }
        index
    }

    /// Unbalanced open parentheses in `text`, ignoring string literals.
    fn depth(text: &str) -> i32 {
        let mut depth = 1; // the sink's own `(` was consumed by the caller
        for c in strip_literals(text).chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        }
        depth
    }

    /// `text` with every string literal removed entirely.
    fn strip_literals(text: &str) -> String {
        scan_literals(text, false)
    }

    /// The part of a call that can carry DATA, with prose removed.
    ///
    /// # Why this is not just the raw argument text
    ///
    /// The first version matched everything and immediately fired on
    /// `tracing::warn!(%error, "output: dropped the pinned pixels ...")` --
    /// the word `pixels` in an English sentence, not a field carrying a pixel
    /// buffer. A control that cries wolf is one somebody deletes.
    ///
    /// # ...but NOT their captures
    ///
    /// `tracing::info!("{line}")` interpolates the variable `line`. Dropping
    /// the literal whole would blind the control to exactly the case where a
    /// message is BUILT from captured content, which is round 1's laundering
    /// drill. So `{...}` spans inside a literal are kept.
    fn fields_and_captures(text: &str) -> String {
        scan_literals(text, true)
    }

    /// Shared literal-aware walk. Handles `"..."`, escapes, and raw strings
    /// (`r"..."`, `r#"..."#`), which round 1 desynchronised with an odd number
    /// of embedded quotes to walk a live field past the scanner.
    fn scan_literals(text: &str, keep_captures: bool) -> String {
        let bytes = text.as_bytes();
        let mut out = String::new();
        let mut i = 0;
        while i < bytes.len() {
            // A raw string opener: `r` then zero or more `#` then `"`.
            if bytes[i] == b'r' && !preceded_by_identifier(bytes, i) {
                let mut hashes = 0;
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] == b'#' {
                    hashes += 1;
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'"' {
                    let closer = String::from("\"") + &"#".repeat(hashes);
                    let rest = &text[j + 1..];
                    let end = rest.find(&closer).map_or(text.len(), |k| j + 1 + k);
                    if keep_captures {
                        push_captures(&text[j + 1..end], &mut out);
                    }
                    i = end + closer.len();
                    continue;
                }
            }
            // A CHAR literal. Skipped because `push_captures` contains `'{'`
            // and `'}'`, and counting those as braces desynchronised
            // `skip_test_item` badly enough that the scan walked back into
            // the test module it had just skipped and tripped over its own
            // fixtures. Lifetimes (`&'a str`) look similar and must NOT be
            // consumed, so a closing quote within three bytes is required.
            if bytes[i] == b'\''
                && let Some(end) = char_literal_end(bytes, i)
            {
                i = end + 1;
                continue;
            }
            if bytes[i] == b'"' {
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b'"' {
                        break;
                    }
                    j += 1;
                }
                let end = j.min(bytes.len());
                if keep_captures {
                    push_captures(&text[i + 1..end], &mut out);
                }
                i = end + 1;
                continue;
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    /// The index of a char literal's closing quote, if `i` opens one.
    ///
    /// `None` for a lifetime, which shares the opening byte and has no closer.
    fn char_literal_end(bytes: &[u8], i: usize) -> Option<usize> {
        // `'x'`
        if i + 2 < bytes.len() && bytes[i + 1] != b'\\' && bytes[i + 2] == b'\'' {
            return Some(i + 2);
        }
        // `'\n'`, `'\''`, `'\\'`
        if i + 3 < bytes.len() && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
            return Some(i + 3);
        }
        None
    }

    /// Whether the byte before `i` could continue an identifier, so `r` in
    /// `char_reader` is not read as a raw-string opener.
    fn preceded_by_identifier(bytes: &[u8], i: usize) -> bool {
        i > 0 && is_identifier_byte(bytes[i - 1])
    }

    /// Appends every `{...}` capture found in a literal's body.
    fn push_captures(body: &str, out: &mut String) {
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '{' {
                continue;
            }
            // `{{` is an escaped brace, not a capture.
            if chars.peek() == Some(&'{') {
                let _ = chars.next();
                continue;
            }
            out.push(' ');
            for inner in chars.by_ref() {
                if inner == '}' {
                    break;
                }
                out.push(inner);
            }
            out.push(' ');
        }
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
                // Build output and vendored trees are not this project's source
                // and walking them is minutes, not milliseconds.
                let name = entry.file_name();
                if matches!(
                    name.to_str(),
                    Some("target" | ".git" | "node_modules" | "dist" | ".svelte-kit")
                ) {
                    continue;
                }
                collect_rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// The control above passing means nothing unless it can fail, and round 1
    /// of `PR #94`'s review found three ways it could not. Each is asserted
    /// here by name so a later simplification cannot quietly reopen one.
    #[test]
    fn the_privacy_scan_can_actually_fail() {
        // The plain case.
        let calls = logical_calls("tracing::info!(text = %recognised);");
        assert!(calls.iter().any(|(_, u)| field_name_matches(u, "text")));

        // BYPASS 1, round 1: a call split across lines. The first version read
        // one physical line and saw nothing.
        let multi = "tracing::info!(\n    text = %text\n);";
        assert!(
            logical_calls(multi)
                .iter()
                .any(|(_, u)| field_name_matches(u, "text")),
            "a multi-line macro call must be joined before scanning"
        );

        // BYPASS 2, round 1: a raw string with an odd number of embedded
        // quotes desynchronised the literal tracker, hiding the live field
        // that followed it.
        let raw = "tracing::info!(a = r#\"she said \" odd quote\"#, text = %text);";
        assert!(
            logical_calls(raw)
                .iter()
                .any(|(_, u)| field_name_matches(u, "text")),
            "a raw string must not desynchronise the literal tracker"
        );

        // BYPASS 3, round 1: laundering through a wrapper. Closed by refusing
        // to let the string be BUILT.
        let laundered = "let leaked = format!(\"recognised text was: {text}\");";
        assert!(
            logical_calls(laundered)
                .iter()
                .any(|(_, u)| field_name_matches(u, "text")),
            "format! must be a sink, or content can be laundered into one"
        );

        // ...and prose is still not data: the real line that made the first
        // version fire on this project's own correct code.
        let prose = r#"tracing::warn!(%error, "output: dropped the pinned pixels but could not announce it");"#;
        assert!(
            !logical_calls(prose)
                .iter()
                .any(|(_, u)| field_name_matches(u, "pixels")),
            "an English message is not a field"
        );

        // A word that merely contains a forbidden one is not one.
        assert!(!field_name_matches("context = %detail", "text"));
        assert!(!field_name_matches("subtext_length = 4", "text"));

        // `{{` is an escaped brace, not a capture.
        let escaped = "tracing::info!(\"a literal {{text}} brace\");";
        assert!(
            !logical_calls(escaped)
                .iter()
                .any(|(_, u)| field_name_matches(u, "text")),
            "`{{{{` is an escaped brace and carries no value"
        );

        // An identifier ending in `r` is not a raw-string opener.
        assert!(strip_literals("let char_reader = 1;").contains("char_reader"));
    }
}
