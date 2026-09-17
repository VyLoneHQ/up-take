//! The settings file, `%APPDATA%\VyLone\UP-TAKE\config.toml`
//! (`architecture.md` section 6).
//!
//! **It holds two things: whether the first-run tour has been completed**
//! (roadmap 1.18) **and every setting the user can change** (roadmap 1.14).
//! The setting fields themselves live in [`crate::settings`], not here: this
//! module owns the file, that one owns the values and who reads them.
//!
//! ⚠️ This said *"one fact today"*, and that 1.14 *"will grow this file"*,
//! until 1.14 grew it. What that sentence decided ahead of time -- the
//! location, the versioning, and what happens to a file this build cannot read
//! -- all held, and none of it had to be undone.
//!
//! # Section 6's three rules, and how each is kept
//!
//! - **Versioned with a `schema_version` field.** Written on every save.
//! - **Never fail to start because of an old config file.** Nothing here
//!   returns an error to startup. A file that will not read is treated as
//!   absent, so the worst a broken file costs is the tour showing again.
//! - **Migrate or reset with a backup.** There is nothing to migrate from yet.
//!   An unreadable file is moved aside rather than overwritten, and only at the
//!   moment something has to be written in its place, so a file this build
//!   merely fails to read is left exactly as it was found.
//!
//! # A file from a NEWER build is read and never written
//!
//! A `schema_version` above [`SCHEMA_VERSION`] means a later UP-TAKE wrote the
//! file. The fields this build knows are read (serde ignores the rest) and a
//! save refuses: re-serialising would silently drop every field this build does
//! not know, which downgrades the user's settings on behalf of a version of the
//! app they may have run once.
//!
//! ⚠️ **The same loss applies to an unknown field in a file of THIS version**,
//! and is accepted rather than guarded, because no build has written one. When
//! 1.14 adds a setting it adds the field to [`Config`], which is what makes it
//! known.
//!
//! Deliberately NOT Tauri's `app_config_dir()`, for the reason `uptake_log`
//! gives about `app_local_data_dir()`: it resolves from the bundle identifier,
//! which is a different directory from the one the spec names.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

/// The schema this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// Everything stored in the settings file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Which schema wrote the file. Absent reads as [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The first-run tour's stored state.
    pub first_run: FirstRun,
    /// Everything the user can change (roadmap 1.14).
    pub settings: Settings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            first_run: FirstRun::default(),
            settings: Settings::default(),
        }
    }
}

/// The first-run tour's stored state (roadmap 1.18).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FirstRun {
    /// Whether the user has finished or skipped the tour.
    pub completed: bool,
}

/// What reading the settings file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// No file: a first launch, or one whose file was removed.
    Missing,
    /// A file this build can read and write.
    Read(Config),
    /// A file written by a newer schema. Read, and never written.
    Newer(Config),
    /// A file that exists and could not be read or parsed, with why.
    Unreadable(String),
}

impl Loaded {
    /// Whether the first-run tour is recorded as done. A file that could not be
    /// read records nothing, so the tour runs.
    #[must_use]
    pub const fn first_run_completed(&self) -> bool {
        match self {
            Self::Read(config) | Self::Newer(config) => config.first_run.completed,
            Self::Missing | Self::Unreadable(_) => false,
        }
    }

    /// The settings this file holds.
    ///
    /// A file that could not be read holds nothing, so the shipped defaults
    /// stand -- the same rule [`Self::first_run_completed`] follows, and for
    /// the same reason. **A file from a newer build contributes the fields
    /// this build knows**, because serde has already dropped the rest; what
    /// that case must not do is write, and [`write_settings_at`] is where that
    /// is refused.
    #[must_use]
    pub fn settings(&self) -> Settings {
        match self {
            Self::Read(config) | Self::Newer(config) => config.settings.clone(),
            Self::Missing | Self::Unreadable(_) => Settings::default(),
        }
    }
}

/// Parses a settings file's text.
#[must_use]
pub fn parse(text: &str) -> Loaded {
    match toml::from_str::<Config>(text) {
        Ok(config) if config.schema_version > SCHEMA_VERSION => Loaded::Newer(config),
        Ok(config) => Loaded::Read(config),
        Err(error) => {
            Loaded::Unreadable(format!("not a settings file this build can read: {error}"))
        }
    }
}

/// `<app_data>\VyLone\UP-TAKE\config.toml`, or `None` when there is no
/// application-data directory to put it in.
///
/// Split from [`path`] so the layout is testable without touching the real
/// environment.
#[must_use]
pub fn path_in(app_data: Option<&Path>) -> Option<PathBuf> {
    app_data.map(|root| root.join("VyLone").join("UP-TAKE").join("config.toml"))
}

/// The settings file's path on this machine, from `%APPDATA%`.
#[must_use]
pub fn path() -> Option<PathBuf> {
    path_in(std::env::var_os("APPDATA").map(PathBuf::from).as_deref())
}

/// Reads the settings file at `path`.
#[must_use]
pub fn load_from(path: &Path) -> Loaded {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == ErrorKind::NotFound => Loaded::Missing,
        Err(error) => Loaded::Unreadable(format!("could not read {}: {error}", path.display())),
    }
}

/// Writes `config` to `path`, creating the directory if needed.
///
/// Written to a sibling file and renamed over the original, so a crash or a
/// full disk mid-write leaves the previous file whole rather than a truncated
/// one that would read as unreadable next launch.
///
/// # Errors
///
/// When the directory cannot be created, or the file cannot be written or
/// renamed into place.
pub fn save_to(path: &Path, config: &Config) -> Result<(), String> {
    let text = toml::to_string(config)
        .map_err(|error| format!("could not encode the settings: {error}"))?;
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)
            .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    }
    let staging = path.with_extension("toml.tmp");
    fs::write(&staging, text)
        .map_err(|error| format!("could not write {}: {error}", staging.display()))?;
    fs::rename(&staging, path)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

/// Records the first-run tour as completed in the file at `path`.
///
/// `now` names the backup an unreadable file is moved to, and is a parameter
/// so a test can predict it.
///
/// # Errors
///
/// When the file was written by a newer build (it is left untouched), when an
/// unreadable file cannot be moved aside, or when the save fails.
pub fn mark_first_run_completed_at(path: &Path, now: u64) -> Result<(), String> {
    set_first_run_completed_at(path, true, now)
}

/// Records the first-run tour as completed, or not, in the file at `path`.
///
/// **Clearing it is what `ADR-0043` decision 5's *show the tour again* does**,
/// and it is the same write in the other direction rather than a second path:
/// a separate "reset" that did not go through this read-modify-write would be
/// the place a later field silently got dropped.
///
/// `now` names the backup an unreadable file is moved to, and is a parameter
/// so a test can predict it.
///
/// # Errors
///
/// When the file was written by a newer build (it is left untouched), when an
/// unreadable file cannot be moved aside, or when the save fails.
pub fn set_first_run_completed_at(path: &Path, completed: bool, now: u64) -> Result<(), String> {
    let mut config = match load_from(path) {
        Loaded::Missing => Config::default(),
        Loaded::Read(config) => config,
        Loaded::Newer(_) => {
            return Err(format!(
                "{} was written by a newer UP-TAKE, so it is left untouched",
                path.display()
            ));
        }
        Loaded::Unreadable(_) => {
            let aside = path.with_extension(format!("toml.unreadable-{now}"));
            fs::rename(path, &aside).map_err(|error| {
                format!(
                    "could not move the unreadable {} aside: {error}",
                    path.display()
                )
            })?;
            Config::default()
        }
    };
    config.schema_version = SCHEMA_VERSION;
    config.first_run.completed = completed;
    save_to(path, &config)
}

/// Records the first-run tour as not yet seen, so it runs again.
///
/// # Errors
///
/// When there is no `%APPDATA%`, or for any reason
/// [`set_first_run_completed_at`] gives.
pub fn clear_first_run_completed() -> Result<(), String> {
    let path =
        path().ok_or_else(|| "APPDATA is not set, so there is nowhere to record it".to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    set_first_run_completed_at(&path, false, now)
}

/// Writes `settings` into the file at `path`, keeping everything else in it.
///
/// **Read-modify-write, never write-whole**, which is the same rule
/// [`mark_first_run_completed_at`] follows: saving the settings must not reset
/// the first-run tour, and a later build that adds a third block must not lose
/// it to a save from this one. The fields this build does not know are still
/// dropped -- serde cannot keep what it did not parse -- and that is exactly
/// why a file from a newer schema is refused rather than merged.
///
/// # Errors
///
/// When the file was written by a newer UP-TAKE (it is left untouched), when
/// an unreadable file cannot be moved aside, or when the save fails.
pub fn write_settings_at(path: &Path, settings: &Settings) -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut config = match load_from(path) {
        Loaded::Missing => Config::default(),
        Loaded::Read(config) => config,
        Loaded::Newer(_) => {
            return Err(format!(
                "{} was written by a newer UP-TAKE, so your settings were not saved to it",
                path.display()
            ));
        }
        Loaded::Unreadable(_) => {
            let aside = path.with_extension(format!("toml.unreadable-{now}"));
            fs::rename(path, &aside).map_err(|error| {
                format!(
                    "could not move the unreadable {} aside: {error}",
                    path.display()
                )
            })?;
            Config::default()
        }
    };
    config.schema_version = SCHEMA_VERSION;
    config.settings = settings.clone();
    save_to(path, &config)
}

/// Records the first-run tour as completed on this machine.
///
/// # Errors
///
/// When there is no `%APPDATA%`, or for any reason
/// [`mark_first_run_completed_at`] gives.
pub fn mark_first_run_completed() -> Result<(), String> {
    let path =
        path().ok_or_else(|| "APPDATA is not set, so there is nowhere to record it".to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    mark_first_run_completed_at(&path, now)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{
        Config, Loaded, SCHEMA_VERSION, Settings, load_from, mark_first_run_completed_at, parse,
        path_in, set_first_run_completed_at, write_settings_at,
    };
    use crate::payload_keys::assert_payload_coverage;

    /// A fresh directory under the system temp dir, unique to this test.
    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "uptake-config-{name}-{}-{nanos}",
            std::process::id()
        ));
        let Ok(()) = fs::create_dir_all(&dir) else {
            panic!("could not create {}", dir.display())
        };
        dir
    }

    fn read(path: &Path) -> String {
        let Ok(text) = fs::read_to_string(path) else {
            panic!("could not read {}", path.display())
        };
        text
    }

    #[test]
    fn the_file_lives_where_the_spec_says() {
        assert_eq!(path_in(None), None);
        let Some(path) = path_in(Some(Path::new("C:/Users/someone/AppData/Roaming"))) else {
            panic!("an app-data root gives a path")
        };
        assert!(
            path.ends_with("VyLone/UP-TAKE/config.toml"),
            "{}",
            path.display()
        );
    }

    #[test]
    fn no_file_means_the_tour_has_not_run() {
        let dir = scratch("missing");
        let loaded = load_from(&dir.join("config.toml"));
        assert_eq!(loaded, Loaded::Missing);
        assert!(!loaded.first_run_completed());
    }

    #[test]
    fn an_empty_file_is_the_defaults_rather_than_an_error() {
        let loaded = parse("");
        assert_eq!(loaded, Loaded::Read(Config::default()));
        assert!(!loaded.first_run_completed());
    }

    #[test]
    fn a_field_this_build_does_not_know_is_ignored_on_read() {
        let loaded =
            parse("schema_version = 1\nsomething_later = 3\n\n[first_run]\ncompleted = true\n");
        assert!(matches!(loaded, Loaded::Read(_)), "{loaded:?}");
        assert!(loaded.first_run_completed());
    }

    #[test]
    fn a_file_that_is_not_toml_is_unreadable_and_runs_the_tour() {
        let loaded = parse("this is not [ toml");
        assert!(matches!(loaded, Loaded::Unreadable(_)), "{loaded:?}");
        assert!(!loaded.first_run_completed());
    }

    #[test]
    fn completing_the_tour_is_read_back_as_completed() {
        let path = scratch("roundtrip")
            .join("VyLone")
            .join("UP-TAKE")
            .join("config.toml");
        let Ok(()) = mark_first_run_completed_at(&path, 1) else {
            panic!("could not record completion")
        };
        let loaded = load_from(&path);
        assert!(loaded.first_run_completed(), "{loaded:?}");
        assert!(read(&path).contains(&format!("schema_version = {SCHEMA_VERSION}")));
    }

    #[test]
    fn a_newer_file_is_read_and_left_byte_for_byte_alone() {
        let path = scratch("newer").join("config.toml");
        let original =
            "schema_version = 99\nfrom_the_future = true\n\n[first_run]\ncompleted = false\n";
        let Ok(()) = fs::write(&path, original) else {
            panic!("could not write the fixture")
        };
        assert!(matches!(load_from(&path), Loaded::Newer(_)));
        assert!(
            mark_first_run_completed_at(&path, 1).is_err(),
            "a newer file was overwritten"
        );
        assert_eq!(read(&path), original);
    }

    #[test]
    fn an_unreadable_file_is_moved_aside_before_it_is_replaced() {
        let dir = scratch("unreadable");
        let path = dir.join("config.toml");
        let original = "not [ toml at all";
        let Ok(()) = fs::write(&path, original) else {
            panic!("could not write the fixture")
        };
        let Ok(()) = mark_first_run_completed_at(&path, 1234) else {
            panic!("could not record completion over an unreadable file")
        };
        assert!(load_from(&path).first_run_completed());
        assert_eq!(
            read(&dir.join("config.toml.unreadable-1234")),
            original,
            "the user's unreadable file was not kept"
        );
    }

    #[test]
    fn a_file_that_merely_fails_to_read_is_not_touched_by_reading_it() {
        let dir = scratch("untouched");
        let path = dir.join("config.toml");
        let Ok(()) = fs::write(&path, "not [ toml") else {
            panic!("could not write the fixture")
        };
        let _ = load_from(&path);
        assert_eq!(read(&path), "not [ toml");
        assert!(!dir.join("config.toml.tmp").exists());
    }

    #[test]
    fn settings_written_to_the_file_are_read_back() {
        let path = scratch("settings-roundtrip").join("config.toml");
        let settings = Settings {
            start_with_windows: true,
            area_opacity_percent: 75,
            ..Settings::default()
        };
        let Ok(()) = write_settings_at(&path, &settings) else {
            panic!("could not write the settings")
        };
        assert_eq!(load_from(&path).settings(), settings);
    }

    #[test]
    fn saving_the_settings_does_not_forget_the_tour() {
        // Both blocks live in one file, so a save that wrote the whole struct
        // from defaults would show the first-run tour again to everyone who
        // changed a setting. Read-modify-write is what stops that, and this is
        // the assertion that holds it.
        let path = scratch("settings-keeps-tour").join("config.toml");
        let Ok(()) = mark_first_run_completed_at(&path, 1) else {
            panic!("could not record completion")
        };
        let Ok(()) = write_settings_at(
            &path,
            &Settings {
                area_opacity_percent: 90,
                ..Settings::default()
            },
        ) else {
            panic!("could not write the settings")
        };
        let loaded = load_from(&path);
        assert!(loaded.first_run_completed(), "the tour was forgotten");
        assert_eq!(loaded.settings().area_opacity_percent, 90);
    }

    #[test]
    fn showing_the_tour_again_does_not_forget_the_settings() {
        // The same rule in the other direction: ADR-0043 decision 5's replay
        // clears one field, not the file.
        let path = scratch("tour-keeps-settings").join("config.toml");
        let Ok(()) = write_settings_at(
            &path,
            &Settings {
                area_opacity_percent: 90,
                ..Settings::default()
            },
        ) else {
            panic!("could not write the settings")
        };
        let Ok(()) = mark_first_run_completed_at(&path, 1) else {
            panic!("could not record completion")
        };
        let Ok(()) = set_first_run_completed_at(&path, false, 2) else {
            panic!("could not clear completion")
        };
        let loaded = load_from(&path);
        assert!(!loaded.first_run_completed());
        assert_eq!(loaded.settings().area_opacity_percent, 90);
    }

    #[test]
    fn a_file_from_before_the_settings_existed_reads_as_the_defaults() {
        // Every config.toml written by 1.18 looks like this. It must read, and
        // it must read as the shipped defaults rather than as an error, which
        // is what `#[serde(default)]` on the field buys and why adding a
        // setting needs no schema bump.
        let loaded = parse("schema_version = 1\n\n[first_run]\ncompleted = true\n");
        assert!(matches!(loaded, Loaded::Read(_)), "{loaded:?}");
        assert!(loaded.first_run_completed());
        assert_eq!(loaded.settings(), Settings::default());
    }

    #[test]
    fn a_newer_file_keeps_its_settings_when_a_save_is_refused() {
        let path = scratch("newer-settings").join("config.toml");
        let original = "schema_version = 99\nfrom_the_future = true\n";
        let Ok(()) = fs::write(&path, original) else {
            panic!("could not write the fixture")
        };
        assert!(
            write_settings_at(&path, &Settings::default()).is_err(),
            "a newer file was overwritten"
        );
        assert_eq!(read(&path), original);
    }

    #[test]
    fn the_settings_types_are_not_ipc_payloads() {
        // `payload_keys`'s sweep finds every `Serialize` type in the crate and
        // demands a key table or a reason. These go to disk and nowhere else.
        assert_payload_coverage(
            "config.rs",
            include_str!("config.rs"),
            &[],
            &[
                (
                    "Config",
                    "written to the settings file, never sent over IPC",
                ),
                (
                    "FirstRun",
                    "written to the settings file, never sent over IPC",
                ),
            ],
        );
    }
}
