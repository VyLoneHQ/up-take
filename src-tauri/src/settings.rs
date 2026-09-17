//! Every setting the user can change, and the one place the rest of the app
//! asks what they chose (roadmap 1.14).
//!
//! The values live in `config.toml` beside the first-run tour's state
//! ([`crate::config`]); this module holds them in memory, hands them to the
//! settings window over IPC, and pushes the ones with a fast-path reader into
//! the atomics those readers already use.
//!
//! # The inventory is a document, not a guess
//!
//! `UI-UX.md` section 4 is the check-list this module is built against: one row
//! per setting, each with the decision that put it there. It is deliberately
//! the authority rather than the roadmap's prose, because that prose has
//! already been wrong about its own count once -- it said three settings while
//! the code and `ADR-0026`'s third amendment both said four.
//!
//! **Two of section 4's thirteen rows are not here, and their absence is
//! recorded rather than silent:** the two hotkey rows. A rebindable shortcut
//! needs a control that captures a keystroke, and section 3.2 allows three
//! control shapes -- a toggle, a segmented choice, a slider -- and says that
//! needing a fourth is a sign the setting does not belong in this window. Those
//! two rows are shown read-only here and the question is the founder's.
//!
//! # Why the settings are a snapshot rather than a lock the readers hold
//!
//! [`current`] clones. The alternative is handing out a guard, and the readers
//! are the click-through poll, the placement hook and the freeze path -- three
//! places where holding a lock across a capture is how a frame gets missed.
//! The values are a dozen bytes; the copy is cheaper than the contention.
//!
//! # A setting that a hot path reads does not read it from here
//!
//! [`crate::freeze`] keeps its own atomics for the freeze scope and the display
//! format, because they are consulted per frame and per keypress. This module
//! **pushes** into them on load and on every change, so there is still one
//! place the value is decided and one place it is stored. That is the same
//! arrangement those atomics already had with their environment variables, with
//! the source swapped -- which is what each of their doc comments said 1.14
//! would do.
//!
//! # The environment variables still win, and that is deliberate
//!
//! `UPTAKE_FREEZE_FORMAT` and `UPTAKE_FREEZE_ALL_MONITORS` override the stored
//! setting when they are set to a value those readers accept. They are how the
//! rig measures a named format without clicking through a window, `output.rs`
//! documents the first as a supported override, and a measurement run that
//! silently took a stored setting instead would be `UT-F-46` again. The
//! override announces itself on startup, as it always has.

use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// Where an area's opacity may sit, as a percentage.
///
/// The floor is not zero: an area at 0 % is invisible, has no border to grab
/// and no chrome to right-click, so it cannot be recovered except from
/// Placement. `ADR-0015` makes the chrome the area's handle, and a slider that
/// can strand an area contradicts it.
pub const OPACITY_RANGE: (u8, u8) = (10, 100);

/// Where a Filter area's strength may sit, as a percentage.
///
/// The ceiling is not 100 % for the reason the type exists: a Filter area is
/// something the user goes on working underneath (`ADR-0038`), and a wash dense
/// enough to hide the content is a Default area with extra steps.
pub const FILTER_RANGE: (u8, u8) = (5, 60);

/// Which state a launch by hand lands in (`ADR-0044` decision 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HandLaunchState {
    /// The overlay comes up in Placement. The shipped default, roadmap 1.34.
    #[default]
    Placing,
    /// The overlay stays hidden, as a launch with Windows does.
    Hidden,
}

/// Which monitors a freeze covers (`ADR-0026`'s third amendment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FreezeCovers {
    /// The monitor the cursor is on. The default, and the narrower of the two.
    #[default]
    ThisMonitor,
    /// Every monitor.
    EveryMonitor,
}

/// How a held picture is encoded on the display path (`ADR-0027`).
///
/// Named for what the user is choosing rather than for the file format, which
/// is `UI-UX.md` section 3.2's rule: the ordinary Windows user is the first
/// user, and JPEG against PNG is not the choice they are making. `ADR-0027` is
/// satisfied either way, because PNG stays reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HeldPictureQuality {
    /// JPEG. Measured at 26-37 ms against PNG's 68-294 ms.
    #[default]
    Fast,
    /// PNG. Lossless, and slower on a dense screen.
    Exact,
}

/// Which language the interface is shown in (roadmap 1.38).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Whatever Windows' display language resolves to. The default.
    #[default]
    System,
    /// English.
    English,
    /// German.
    German,
}

impl Language {
    /// The catalogue code this choice forces, or `None` to let Windows decide.
    #[must_use]
    pub const fn code(self) -> Option<&'static str> {
        match self {
            Self::System => None,
            Self::English => Some("en"),
            Self::German => Some("de"),
        }
    }
}

/// Everything the user can change.
///
/// `#[serde(default)]` on the struct and on every field is what makes an older
/// `config.toml` readable: a file written before a setting existed simply has
/// no key for it, and reads back as that setting's default. It is also why
/// adding a row here does not need a schema bump -- see [`crate::config`]'s
/// module docs, which say so from the other side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // ---- General -------------------------------------------------------
    /// Whether UP-TAKE starts with Windows. Off by default: a tool that reads
    /// the screen asks rather than assumes (`OPEN-QUESTIONS.md` `Q-14`).
    pub start_with_windows: bool,
    /// Which state a launch by hand lands in.
    pub hand_launch_state: HandLaunchState,

    // ---- Capture -------------------------------------------------------
    /// Where Save writes. `None` means `Pictures\UP-TAKE`, resolved at the
    /// moment of the save rather than stored, so a user whose Pictures folder
    /// moves is not left pointing at the old one.
    pub save_directory: Option<PathBuf>,
    /// Whether taking a screenshot leaves Placement (`ADR-0023` section 2).
    pub leave_placing_after_screenshot: bool,
    /// Whether a Screenshot area writes its picture to disk as it is drawn.
    ///
    /// **Off by default, and the default is the decision.** `output.rs`'s own
    /// docs say Save is "a separate, explicit action (PRODUCT-VISION section 8)
    /// -- does not also touch the clipboard", and a screen-reading tool that
    /// writes files unasked is the behaviour that sentence exists to refuse.
    /// On, it is the user saying they want the other thing, which is what a
    /// setting is for. Asked for by the founder on the rig, 2026-09-17.
    pub auto_save_screenshots: bool,
    /// Which monitors a freeze covers.
    pub freeze_covers: FreezeCovers,
    /// How a held picture is encoded on the display path.
    pub held_picture_quality: HeldPictureQuality,
    /// Whether the overlay appears in screen recordings (`ADR-0019`).
    pub show_in_screen_recordings: bool,

    // ---- Appearance ----------------------------------------------------
    /// How solid an area looks, as a percentage.
    pub area_opacity_percent: u8,
    /// How strong a Filter area's wash is, as a percentage.
    pub filter_strength_percent: u8,
    /// Which language the interface is shown in.
    pub language: Language,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            hand_launch_state: HandLaunchState::Placing,
            save_directory: None,
            leave_placing_after_screenshot: false,
            auto_save_screenshots: false,
            freeze_covers: FreezeCovers::ThisMonitor,
            held_picture_quality: HeldPictureQuality::Fast,
            show_in_screen_recordings: false,
            area_opacity_percent: 40,
            filter_strength_percent: 16,
            language: Language::System,
        }
    }
}

impl Settings {
    /// Brings every value inside the range this build accepts.
    ///
    /// Applied on read as well as on write, because the file is a text file a
    /// user may edit by hand and a slider position of 250 is not a reason to
    /// refuse to start. Nothing here can fail: an out-of-range number is
    /// clamped to the nearer end rather than reset to the default, so a
    /// hand-edited 200 becomes the maximum rather than jumping back to 40.
    fn clamped(mut self) -> Self {
        self.area_opacity_percent = self
            .area_opacity_percent
            .clamp(OPACITY_RANGE.0, OPACITY_RANGE.1);
        self.filter_strength_percent = self
            .filter_strength_percent
            .clamp(FILTER_RANGE.0, FILTER_RANGE.1);
        self
    }
}

/// The live settings. Written at startup and on every save.
static CURRENT: RwLock<Option<Settings>> = RwLock::new(None);

/// The settings as they stand.
///
/// Returns the defaults before [`init`] has run and if the lock is poisoned.
/// Both are deliberate: a poisoned lock means a writer panicked, and a capture
/// tool that stops capturing because a settings write panicked is worse than
/// one that goes on with the shipped defaults.
#[must_use]
pub fn current() -> Settings {
    CURRENT
        .read()
        .ok()
        .and_then(|held| held.clone())
        .unwrap_or_default()
}

/// Loads the settings from the file and pushes them to their fast-path readers.
///
/// Never fails. A file that cannot be read is [`crate::config::Loaded`]'s
/// problem and reads as absent, which here means the shipped defaults -- the
/// same rule the first-run tour follows, and for the same reason: nothing about
/// a settings file is a reason to refuse to start.
pub fn init() {
    let loaded = crate::config::path().map(|path| crate::config::load_from(&path));
    let settings = loaded
        .as_ref()
        .map_or_else(Settings::default, crate::config::Loaded::settings)
        .clamped();
    store(&settings);
    publish(&settings);
}

/// Replaces the settings in memory.
fn store(settings: &Settings) {
    if let Ok(mut held) = CURRENT.write() {
        *held = Some(settings.clone());
    }
}

/// Pushes the settings that have a fast-path reader into it.
///
/// Called on load and after every save, so the two stores cannot disagree.
/// **The environment overrides are applied inside `freeze`'s own functions**,
/// not here, so a rig run keeps the variable's value across a settings save --
/// which is the whole point of them winning.
fn publish(settings: &Settings) {
    crate::freeze::set_freeze_scope(settings.freeze_covers);
    crate::freeze::set_display_format(settings.held_picture_quality);
}

/// Stores `settings`, writes them to the settings file, and publishes them.
///
/// # Errors
///
/// When the file cannot be written, or was written by a newer UP-TAKE. **The
/// in-memory copy is updated either way**, so a save that could not reach the
/// disk still takes effect for this run and the user is told why it will not
/// survive a restart. The alternative -- refusing the change because it cannot
/// be persisted -- makes a read-only profile directory mean no settings at all.
pub fn save(settings: Settings) -> Result<(), String> {
    let settings = settings.clamped();
    store(&settings);
    publish(&settings);
    let path = crate::config::path().ok_or_else(|| {
        "APPDATA is not set, so there is nowhere to save the settings".to_string()
    })?;
    crate::config::write_settings_at(&path, &settings)
}

#[cfg(test)]
mod tests {
    use super::{
        FILTER_RANGE, FreezeCovers, HandLaunchState, HeldPictureQuality, Language, OPACITY_RANGE,
        Settings,
    };
    use crate::payload_keys::{assert_keys, assert_payload_coverage};

    #[test]
    fn the_defaults_are_the_inventorys_defaults() {
        // `UI-UX.md` section 4, one assertion per row. The table is the
        // check-list, so a default changed in the code without a decision is a
        // red test rather than a surprise on someone's machine.
        let settings = Settings::default();
        assert!(!settings.start_with_windows, "Q-14: off");
        assert_eq!(settings.hand_launch_state, HandLaunchState::Placing);
        assert_eq!(
            settings.save_directory, None,
            "None means Pictures\\UP-TAKE"
        );
        assert!(!settings.leave_placing_after_screenshot, "ADR-0023 s2: off");
        assert!(
            !settings.auto_save_screenshots,
            "PRODUCT-VISION s8: Save is explicit, so this is off"
        );
        assert_eq!(settings.freeze_covers, FreezeCovers::ThisMonitor);
        assert_eq!(settings.held_picture_quality, HeldPictureQuality::Fast);
        assert!(!settings.show_in_screen_recordings, "ADR-0019: off");
        assert_eq!(settings.area_opacity_percent, 40);
        assert_eq!(settings.filter_strength_percent, 16);
        assert_eq!(settings.language, Language::System);
    }

    #[test]
    fn a_hand_edited_number_is_clamped_to_the_nearer_end_not_reset() {
        let clamped = Settings {
            area_opacity_percent: 250,
            filter_strength_percent: 0,
            ..Settings::default()
        }
        .clamped();
        assert_eq!(clamped.area_opacity_percent, OPACITY_RANGE.1);
        assert_eq!(clamped.filter_strength_percent, FILTER_RANGE.0);
    }

    #[test]
    fn an_opacity_of_zero_cannot_be_stored_because_it_strands_the_area() {
        // ADR-0015 makes the chrome the handle. A slider that reaches zero
        // makes an area unrecoverable outside Placement, which is why the
        // floor is a constant rather than a nicety.
        let clamped = Settings {
            area_opacity_percent: 0,
            ..Settings::default()
        }
        .clamped();
        assert_eq!(clamped.area_opacity_percent, OPACITY_RANGE.0);
        assert!(OPACITY_RANGE.0 > 0, "the floor must not be zero");
    }

    #[test]
    fn a_language_choice_forces_a_code_and_system_does_not() {
        assert_eq!(Language::System.code(), None);
        assert_eq!(Language::English.code(), Some("en"));
        assert_eq!(Language::German.code(), Some("de"));
    }

    #[test]
    fn the_settings_reach_the_window_under_the_keys_it_reads() {
        // `Settings` crosses IPC to the settings window, so its wire names are
        // pinned here (`I-67`). Snake case, verbatim: eleven of the twelve
        // payload types in this crate are, and the one that is not carries a
        // `rename_all` that is load-bearing and tested where it lives.
        assert_keys(
            "Settings",
            &Settings::default(),
            &[
                "start_with_windows",
                "hand_launch_state",
                "save_directory",
                "leave_placing_after_screenshot",
                "auto_save_screenshots",
                "freeze_covers",
                "held_picture_quality",
                "show_in_screen_recordings",
                "area_opacity_percent",
                "filter_strength_percent",
                "language",
            ],
        );
    }

    #[test]
    fn every_enum_reaches_the_window_as_the_word_the_window_matches_on() {
        // The window compares against these strings. A `rename_all` removed
        // here would send `ThisMonitor` and the segmented control would match
        // nothing -- green everywhere else, and the control silently blank.
        assert_eq!(
            serde_json::to_string(&FreezeCovers::ThisMonitor).unwrap_or_default(),
            "\"this_monitor\""
        );
        assert_eq!(
            serde_json::to_string(&HeldPictureQuality::Fast).unwrap_or_default(),
            "\"fast\""
        );
        assert_eq!(
            serde_json::to_string(&HandLaunchState::Placing).unwrap_or_default(),
            "\"placing\""
        );
        assert_eq!(
            serde_json::to_string(&Language::German).unwrap_or_default(),
            "\"german\""
        );
    }

    #[test]
    fn no_payload_in_this_module_escapes_the_key_table() {
        assert_payload_coverage(
            "settings.rs",
            include_str!("settings.rs"),
            &["Settings"],
            &[
                (
                    "HandLaunchState",
                    "a field of Settings, covered by its key table and its wire-name test",
                ),
                (
                    "FreezeCovers",
                    "a field of Settings, covered by its key table and its wire-name test",
                ),
                (
                    "HeldPictureQuality",
                    "a field of Settings, covered by its key table and its wire-name test",
                ),
                (
                    "Language",
                    "a field of Settings, covered by its key table and its wire-name test",
                ),
            ],
        );
    }
}
