//! Every word the Rust side of UP-TAKE shows, in the language the user reads
//! (roadmap 1.38).
//!
//! The words live in `locales/strings.json` at the repository root, one entry
//! per string with its languages side by side, and the page reads the same file
//! through `src/lib/strings.ts`. One file for both sides, so the menu Rust
//! builds and the tour the page draws cannot drift into two vocabularies.
//!
//! # How a stale translation is caught
//!
//! Each German entry records a fingerprint of the English it was translated
//! from (`de_from`). `src/lib/strings.test.ts` recomputes it, so changing an
//! English string fails the suite until someone updates the German or confirms
//! it still fits. Checking only that both languages have every key would stay
//! green while the German kept saying the old thing.
//!
//! # Which language
//!
//! Windows' display language, read once at startup: German for any German
//! locale, English for everything else. Roadmap 1.14's Language setting is the
//! override a user will get; until then a debug build honours
//! `UPTAKE_DEV_LANGUAGE=de` or `=en` so the German can be looked at without
//! changing Windows. Tests always read English, so an assertion on a label does
//! not depend on the machine running it.
//!
//! # Never fails
//!
//! The catalogue is compiled in and a test proves it parses. If it somehow did
//! not, or a key were missing, a lookup returns the key itself: a visible
//! `menu.copy` is a bug report, a panic in a menu is a crash.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The catalogue, compiled into the binary.
const CATALOGUE: &str = include_str!("../../locales/strings.json");

/// The debug-only override, for looking at a language without changing Windows.
#[cfg(any(test, debug_assertions))]
const DEV_VAR: &str = "UPTAKE_DEV_LANGUAGE";

/// A language UP-TAKE ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// English, the source language and the fallback for every other locale.
    English,
    /// German, for every German locale Windows reports.
    German,
}

impl Language {
    /// Every language the catalogue must carry.
    #[cfg(any(test, debug_assertions))]
    pub const ALL: [Self; 2] = [Self::English, Self::German];

    /// The code the catalogue and the page use.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::German => "de",
        }
    }

    /// The language a code names, if UP-TAKE ships it.
    #[cfg(any(test, debug_assertions))]
    fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|language| language.code() == code)
    }

    /// The language for a Windows `LANGID`. The low ten bits are the primary
    /// language, and `0x07` is German whether the sublanguage is Germany,
    /// Austria, Switzerland, Luxembourg or Liechtenstein.
    #[must_use]
    pub const fn from_langid(id: u16) -> Self {
        if id & 0x03ff == 0x07 {
            Self::German
        } else {
            Self::English
        }
    }
}

macro_rules! texts {
    ($($(#[$doc:meta])* $variant:ident => $key:literal,)*) => {
        /// A string the Rust side shows. Its key is the entry in
        /// `locales/strings.json`, and the macro keeps [`Text::ALL`] complete.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Text {
            $($(#[$doc])* $variant,)*
        }

        impl Text {
            /// Every variant, for the tests that check the catalogue has them all.
            #[cfg(test)]
            pub const ALL: &[Self] = &[$(Self::$variant,)*];

            /// The catalogue key.
            #[must_use]
            pub const fn key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key,)*
                }
            }
        }
    };
}

texts! {
    /// Area menu: copy a Screenshot area's capture.
    MenuCopy => "menu.copy",
    /// Area menu: save a Screenshot area's capture to a file.
    MenuSaveImage => "menu.save_image",
    /// Area menu: the row that opens the type list.
    MenuAreaType => "menu.area_type",
    /// Type list: convert to a Default area.
    MenuTypeDefault => "menu.type.default",
    /// Type list: convert to a Screenshot area.
    MenuTypeScreenshot => "menu.type.screenshot",
    /// Type list: convert to a Filter area.
    MenuTypeFilter => "menu.type.filter",
    /// Type list: convert to an Upscale area.
    MenuTypeUpscale => "menu.type.upscale",
    /// Type list: convert to an OCR area.
    MenuTypeOcr => "menu.type.ocr",
    /// Area menu: the row that opens the depth list.
    MenuDepth => "menu.depth",
    /// Depth list: always above other areas.
    MenuDepthFront => "menu.depth.front",
    /// Depth list: stacked automatically.
    MenuDepthAuto => "menu.depth.auto",
    /// Depth list: always below other areas.
    MenuDepthBack => "menu.depth.back",
    /// Area menu: let clicks fall through the area.
    MenuClickThrough => "menu.click_through",
    /// Area menu: remove the area.
    MenuDismiss => "menu.dismiss",
    /// Tray menu: show the overlay. `{hotkey}` is the summon shortcut.
    TrayShow => "tray.show",
    /// Tray menu: quit UP-TAKE.
    TrayQuit => "tray.quit",
    /// Dialog title when the tray icon could not be created.
    TrayUnavailableTitle => "tray.unavailable.title",
    /// Dialog text when the tray icon could not be created. `{hotkey}`, `{error}`.
    TrayUnavailableDetail => "tray.unavailable.detail",
    /// Dialog title when a global shortcut could not be registered.
    HotkeyUnavailableTitle => "hotkey.unavailable.title",
    /// Another application holds the shortcut. `{label}`.
    HotkeyTaken => "hotkey.taken",
    /// Windows refused the shortcut for another reason. `{label}`, `{error}`.
    HotkeyRefused => "hotkey.refused",
    /// Dialog title: a Copy or a monitor grab failed.
    OutputCopyTitle => "output.copy.title",
    /// Dialog title: a Save failed.
    OutputSaveTitle => "output.save.title",
    /// Dialog title: an area did not receive its capture.
    OutputCaptureTitle => "output.capture.title",
    /// What survived a failed Copy or grab: the clipboard.
    OutputClipboardUnchanged => "output.clipboard_unchanged",
    /// What survived a failed Save: no file was touched.
    OutputNothingWritten => "output.nothing_written",
    /// What survived a failed capture: the area as it was.
    OutputAreaUnchanged => "output.area_unchanged",
    /// Dialog text for a failed action. `{reassurance}`, `{reason}`.
    OutputFailed => "output.failed",
    /// Dialog title: OCR read the area and the text did not reach the clipboard.
    OcrCopyTitle => "output.ocr_copy.title",
    /// Dialog text for the same. `{reason}`.
    OcrCopyDetail => "output.ocr_copy.detail",
}

type Table = HashMap<String, HashMap<String, String>>;

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| parse(CATALOGUE))
}

/// Reads the catalogue's `strings` object into key, then language, then text.
/// Anything that is not that shape is skipped rather than refused.
fn parse(json: &str) -> Table {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Table::new();
    };
    let Some(strings) = value.get("strings").and_then(serde_json::Value::as_object) else {
        return Table::new();
    };
    strings
        .iter()
        .map(|(key, entry)| {
            let texts = entry
                .as_object()
                .map(|fields| {
                    fields
                        .iter()
                        .filter_map(|(field, text)| {
                            text.as_str().map(|text| (field.clone(), text.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            (key.clone(), texts)
        })
        .collect()
}

/// The language UP-TAKE shows, decided once.
#[must_use]
pub fn language() -> Language {
    static LANGUAGE: OnceLock<Language> = OnceLock::new();
    *LANGUAGE.get_or_init(detect)
}

#[cfg(test)]
const fn detect() -> Language {
    Language::English
}

#[cfg(not(test))]
fn detect() -> Language {
    #[cfg(debug_assertions)]
    if let Some(forced) = std::env::var(DEV_VAR)
        .ok()
        .as_deref()
        .and_then(Language::from_code)
    {
        return forced;
    }
    system()
}

#[cfg(all(not(test), windows))]
fn system() -> Language {
    // SAFETY: takes no arguments and only reads the user's display language
    // setting; it has no failure return.
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    Language::from_langid(id)
}

#[cfg(all(not(test), not(windows)))]
const fn system() -> Language {
    Language::English
}

/// A string in the current language.
#[must_use]
pub fn text(text: Text) -> &'static str {
    text_in(language(), text)
}

/// A string in a given language, falling back to English, then to the key.
#[must_use]
pub fn text_in(language: Language, text: Text) -> &'static str {
    let key = text.key();
    let Some(entry) = table().get(key) else {
        return key;
    };
    entry
        .get(language.code())
        .or_else(|| entry.get(Language::English.code()))
        .map_or(key, String::as_str)
}

/// A string in the current language with its `{name}` placeholders filled.
#[must_use]
pub fn fill(text: Text, values: &[(&str, &str)]) -> String {
    substitute(self::text(text), values)
}

/// Replaces each `{name}` in one pass, so a value that itself contains braces
/// (an OS error message, say) is never expanded a second time. A placeholder
/// with no value is left as written, where a reader will see it.
fn substitute(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = &after[..close];
        match values.iter().find(|(wanted, _)| *wanted == name) {
            Some((_, value)) => out.push_str(value),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The page's language, asked for once when it mounts.
///
/// Rust decides and the page follows, so the menu Rust builds and the tour the
/// page draws are always in the same language.
#[tauri::command]
#[must_use]
pub fn overlay_language() -> &'static str {
    language().code()
}

#[cfg(test)]
mod tests {
    use super::{CATALOGUE, DEV_VAR, Language, Text, parse, substitute, table, text, text_in};

    #[test]
    fn the_catalogue_parses_and_is_not_empty() {
        let parsed = parse(CATALOGUE);
        assert!(
            parsed.len() >= Text::ALL.len(),
            "the catalogue holds {} strings and Rust alone names {}",
            parsed.len(),
            Text::ALL.len()
        );
    }

    /// Every string Rust shows exists in every language, and a lookup never
    /// falls back to the key. A missing German entry would otherwise show
    /// English silently.
    #[test]
    fn every_rust_string_exists_in_every_language() {
        for &item in Text::ALL {
            let entry = table()
                .get(item.key())
                .unwrap_or_else(|| panic!("{} is not in locales/strings.json", item.key()));
            for language in Language::ALL {
                let found = entry.get(language.code()).map(String::as_str);
                assert!(
                    found.is_some_and(|text| !text.is_empty()),
                    "{} has no {} text",
                    item.key(),
                    language.code()
                );
                assert_ne!(text_in(language, item), item.key());
            }
        }
    }

    #[test]
    fn no_two_rust_strings_share_a_key() {
        let mut keys: Vec<&str> = Text::ALL.iter().map(|item| item.key()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before);
    }

    #[test]
    fn tests_read_english() {
        assert_eq!(text(Text::MenuDismiss), "Dismiss");
        assert_eq!(text_in(Language::German, Text::MenuDismiss), "Entfernen");
    }

    #[test]
    fn every_german_locale_reads_german_and_nothing_else_does() {
        // de-DE, de-CH, de-AT, de-LU, de-LI.
        for id in [0x0407, 0x0807, 0x0c07, 0x1007, 0x1407] {
            assert_eq!(Language::from_langid(id), Language::German, "{id:#06x}");
        }
        // en-US, en-GB, fr-FR, nl-NL, and an id whose low byte is 7 while its
        // primary language (the low ten bits, 0x107) is not German.
        for id in [0x0409, 0x0809, 0x040c, 0x0413, 0x0107] {
            assert_eq!(Language::from_langid(id), Language::English, "{id:#06x}");
        }
    }

    #[test]
    fn the_dev_override_knows_exactly_the_shipped_codes() {
        assert!(DEV_VAR.starts_with("UPTAKE_DEV_"));
        assert_eq!(Language::from_code("de"), Some(Language::German));
        assert_eq!(Language::from_code("en"), Some(Language::English));
        assert_eq!(Language::from_code("fr"), None);
        assert_eq!(Language::from_code("DE"), None);
    }

    #[test]
    fn substitution_fills_names_once_and_leaves_unknown_ones() {
        assert_eq!(
            substitute("{a} and {b}", &[("a", "1"), ("b", "2")]),
            "1 and 2"
        );
        // A value containing a placeholder is not expanded again.
        assert_eq!(
            substitute(
                "{reason} / {reassurance}",
                &[("reason", "{reassurance}"), ("reassurance", "ok")]
            ),
            "{reassurance} / ok"
        );
        assert_eq!(substitute("{missing}", &[]), "{missing}");
        assert_eq!(
            substitute("no close {brace", &[("brace", "x")]),
            "no close {brace"
        );
        assert_eq!(substitute("Größe {n}", &[("n", "3")]), "Größe 3");
    }
}
