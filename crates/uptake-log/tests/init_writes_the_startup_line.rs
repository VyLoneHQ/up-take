//! `init` must put its startup line in a real file on disk.
//!
//! An INTEGRATION test rather than a unit test on purpose: `init` installs a
//! global subscriber via `try_init`, which succeeds at most once per process, so
//! it needs a test binary of its own.
//!
//! # ⚠️ READ THE CONTENT, NEVER THE DIRECTORY ENTRY
//!
//! This test exists because of a 2026-09-07 rig pass where the log file looked
//! empty and was not. **On Windows, a file with an open handle keeps a stale
//! size in its directory entry**, so both of these report `0` for a file that
//! has bytes in it:
//!
//! - PowerShell's `dir` / `Get-ChildItem`
//! - Rust's `DirEntry::metadata().len()` (from `read_dir`)
//!
//! while both of these are correct:
//!
//! - `fs::metadata(path).len()` -- a fresh query on the path
//! - `fs::read(path)` -- the bytes themselves
//!
//! Measured side by side on one file at one instant: `DirEntry` said 0,
//! `fs::metadata` said 11, `fs::read` returned 11 bytes. An earlier version of
//! this very test used `DirEntry::metadata()` and "proved" a bug that did not
//! exist, which nearly cost a 300-line rewrite of a working crate before the
//! measurement itself was checked.
//!
//! So: assert on CONTENT. A size read through `read_dir` proves nothing here.

use std::time::{Duration, Instant};

#[test]
fn init_puts_its_startup_line_in_the_log_file() {
    let scratch = std::env::temp_dir().join(format!(
        "uptake-log-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is before the epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&scratch).expect("could not create the scratch directory");

    // SAFETY: this test binary is single-threaded here and is the only reader.
    unsafe {
        std::env::set_var("LOCALAPPDATA", &scratch);
    }

    let directory = uptake_log::init().expect("init returned an error");
    uptake_log::note("a line written by the integration test");

    // Polled, not slept: the writer is asynchronous, and polling reports
    // "never" rather than "slow" if it ever stops writing at all.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut contents = String::new();
    while Instant::now() < deadline {
        contents = read_all_logs(&directory);
        if contents.contains("UP-TAKE starting") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        contents.contains("UP-TAKE starting"),
        "init's startup line never reached the log. Read {} bytes from {}",
        contents.len(),
        directory.display()
    );
    assert!(
        contents.contains("a line written by the integration test"),
        "the startup line landed but a later note did not. Contents:\n{contents}"
    );
}

/// Every log file's bytes, concatenated. Reads content, never a directory entry.
fn read_all_logs(directory: &std::path::Path) -> String {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return String::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter_map(|path| std::fs::read(&path).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect()
}
