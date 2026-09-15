//! Every word the Rust side of UP-TAKE shows, in the language the user reads
//! (roadmap 1.38).
//!
//! The words live in `locales/strings.json` at the repository root, one entry
//! per string with its languages side by side, and the page reads the same file
//! through `src/lib/strings.ts`. One file for both sides, so the menu Rust
//! builds and the tour the page draws cannot drift into two vocabularies.
//!
//! # A new language is a change to that file alone
//!
//! The list of languages is the catalogue's own `languages` field, and nothing
//! here names a language except English, the fallback. Adding one means adding
//! its code to that list and its text to every entry; the suite then refuses
//! the catalogue until every entry has it.
//!
//! # How a stale translation is caught
//!
//! Each translation records a fingerprint of the English it was made from
//! (`de_from` for German). `src/lib/strings.test.ts` recomputes it, so changing
//! an English string fails the suite until someone updates the translation or
//! confirms it still fits. Checking only that every language has every key
//! would stay green while a translation kept saying the old thing.
//!
//! # Which language
//!
//! Windows' display language, read once at startup, matched against the
//! catalogue: its locale name exactly (`pt-BR`), then its primary language
//! (`de` for `de-AT`), then English. Roadmap 1.14's Language setting is the
//! override a user will get; until then a debug build honours
//! `UPTAKE_DEV_LANGUAGE=de` so a translation can be looked at without changing
//! Windows. Tests always read English, so an assertion on a label does not
//! depend on the machine running it.
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

/// The language every lookup falls back to, and the one the catalogue lists
/// first.
const FALLBACK: &str = "en";

/// The debug-only override, for looking at a language without changing Windows.
#[cfg(any(test, debug_assertions))]
const DEV_VAR: &str = "UPTAKE_DEV_LANGUAGE";

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

/// The catalogue as parsed: its language codes, in order, and its strings by
/// key, then language.
struct Catalogue {
    languages: Vec<String>,
    strings: Table,
}

fn catalogue() -> &'static Catalogue {
    static PARSED: OnceLock<Catalogue> = OnceLock::new();
    PARSED.get_or_init(|| parse(CATALOGUE))
}

/// Reads the catalogue. Anything that is not the expected shape is skipped
/// rather than refused, so the worst a broken file can do is show keys.
fn parse(json: &str) -> Catalogue {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Catalogue {
            languages: Vec::new(),
            strings: Table::new(),
        };
    };
    let languages = value
        .get("languages")
        .and_then(serde_json::Value::as_array)
        .map(|codes| {
            codes
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let strings = value
        .get("strings")
        .and_then(serde_json::Value::as_object)
        .map(|entries| {
            entries
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
        })
        .unwrap_or_default();
    Catalogue { languages, strings }
}

/// The catalogue language for a locale name such as `de-AT`: the name itself if
/// the catalogue carries it, then its primary language, then English. Compared
/// without regard to case, and `_` is accepted where Windows would write `-`.
fn pick<'a>(locale: &str, available: &'a [String]) -> &'a str {
    let wanted = locale.trim();
    let primary = wanted.split(['-', '_']).next().unwrap_or_default();
    available
        .iter()
        .find(|code| code.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            available
                .iter()
                .find(|code| !primary.is_empty() && code.eq_ignore_ascii_case(primary))
        })
        .map_or(FALLBACK, String::as_str)
}

/// The language UP-TAKE shows, as a catalogue code, decided once.
#[must_use]
pub fn language() -> &'static str {
    static LANGUAGE: OnceLock<&'static str> = OnceLock::new();
    LANGUAGE.get_or_init(detect)
}

#[cfg(test)]
const fn detect() -> &'static str {
    FALLBACK
}

#[cfg(not(test))]
fn detect() -> &'static str {
    let available = &catalogue().languages;
    #[cfg(debug_assertions)]
    if let Ok(forced) = std::env::var(DEV_VAR) {
        return pick(&forced, available);
    }
    system_locale().map_or(FALLBACK, |locale| pick(&locale, available))
}

/// Windows' display language as a locale name, such as `de-AT`.
#[cfg(all(not(test), windows))]
fn system_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::{GetUserDefaultUILanguage, LCIDToLocaleName};

    // LOCALE_NAME_MAX_LENGTH, the terminator included.
    let mut name = [0u16; 85];
    let capacity = i32::try_from(name.len()).ok()?;
    // SAFETY: takes no arguments and only reads the user's display language
    // setting; it has no failure return.
    let id = unsafe { GetUserDefaultUILanguage() };
    // SAFETY: `name` is writable for `capacity` UTF-16 units, and the call
    // writes at most that many, the terminator included. A LANGID is a valid
    // LCID with the default sort.
    let written = unsafe { LCIDToLocaleName(u32::from(id), name.as_mut_ptr(), capacity, 0) };
    // The count includes the terminator, and 0 means the call failed.
    let length = usize::try_from(written).ok()?.checked_sub(1)?;
    String::from_utf16(name.get(..length)?).ok()
}

#[cfg(all(not(test), not(windows)))]
const fn system_locale() -> Option<String> {
    None
}

/// A string in the current language.
#[must_use]
pub fn text(text: Text) -> &'static str {
    text_in(language(), text)
}

/// A string in a given language, falling back to English, then to the key.
#[must_use]
pub fn text_in(language: &str, text: Text) -> &'static str {
    let key = text.key();
    let Some(entry) = catalogue().strings.get(key) else {
        return key;
    };
    entry
        .get(language)
        .or_else(|| entry.get(FALLBACK))
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
pub fn overlay_language() -> &'static str {
    language()
}

#[cfg(test)]
mod tests {
    use super::{
        CATALOGUE, DEV_VAR, FALLBACK, Text, catalogue, parse, pick, substitute, text, text_in,
    };

    #[test]
    fn the_catalogue_parses_and_lists_english_first() {
        let parsed = parse(CATALOGUE);
        assert!(
            parsed.strings.len() >= Text::ALL.len(),
            "the catalogue holds {} strings and Rust alone names {}",
            parsed.strings.len(),
            Text::ALL.len()
        );
        assert_eq!(parsed.languages.first().map(String::as_str), Some(FALLBACK));
    }

    /// Every string Rust shows exists in every language the catalogue lists,
    /// and a lookup never falls back to the key. A missing translation would
    /// otherwise show English silently.
    #[test]
    fn every_rust_string_exists_in_every_catalogue_language() {
        for &item in Text::ALL {
            let entry = catalogue()
                .strings
                .get(item.key())
                .unwrap_or_else(|| panic!("{} is not in locales/strings.json", item.key()));
            for language in &catalogue().languages {
                let found = entry.get(language).map(String::as_str);
                assert!(
                    found.is_some_and(|text| !text.is_empty()),
                    "{} has no {language} text",
                    item.key()
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
    fn tests_read_english_and_an_unknown_language_falls_back_to_it() {
        assert_eq!(text(Text::MenuDismiss), "Dismiss");
        assert_eq!(text_in("de", Text::MenuDismiss), "Entfernen");
        assert_eq!(text_in("xx", Text::MenuDismiss), "Dismiss");
    }

    /// The pass condition of roadmap 1.38: a third language is a change to the
    /// catalogue and not to this code. A catalogue carrying French is parsed,
    /// listed, picked for a French locale and looked up, with nothing here
    /// knowing French exists.
    #[test]
    fn a_third_language_needs_only_the_catalogue() {
        let parsed = parse(
            r#"{"languages": ["en", "de", "fr"],
                "strings": {"menu.dismiss": {"en": "Dismiss", "de": "Entfernen", "fr": "Fermer"}}}"#,
        );
        assert_eq!(parsed.languages, ["en", "de", "fr"]);
        assert_eq!(pick("fr-CA", &parsed.languages), "fr");
        assert_eq!(
            parsed
                .strings
                .get("menu.dismiss")
                .and_then(|entry| entry.get("fr")),
            Some(&"Fermer".to_owned())
        );
    }

    #[test]
    fn a_locale_picks_its_exact_name_then_its_primary_language_then_english() {
        let available: Vec<String> = ["en", "de", "pt-BR", "pt"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        for (locale, expected) in [
            ("de-AT", "de"),
            ("de-CH", "de"),
            ("de_DE", "de"),
            ("DE", "de"),
            ("pt-BR", "pt-BR"),
            ("pt-PT", "pt"),
            ("en-GB", "en"),
            ("fr-FR", FALLBACK),
            ("", FALLBACK),
            ("-", FALLBACK),
        ] {
            assert_eq!(pick(locale, &available), expected, "{locale:?}");
        }
        assert_eq!(pick("de-AT", &[]), FALLBACK);
    }

    #[test]
    fn the_dev_override_is_named_as_a_dev_switch() {
        assert!(DEV_VAR.starts_with("UPTAKE_DEV_"));
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
