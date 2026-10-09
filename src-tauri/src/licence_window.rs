//! The licence windows: UP-TAKE's own licence and the third-party notices,
//! each in a window of its own that shows the text and cannot change it.
//!
//! # Why not the file in the user's editor
//!
//! Help opened the file the installer placed beside the executable with
//! whatever Windows associates with `.txt` (`I-443`). That is an editor, and the
//! installed copy sits in the user's own `AppData` folder, so a stray keystroke
//! and a save changed the licence UP-TAKE ships with. **The founder, on his rig
//! pass of `#130`, 2026-10-09:** *"I would prefere it if they would be opened in
//! dedicated read only popups/windows, so it is not possible to modify the
//! contents by accident."* The text can still be selected and copied.
//!
//! # What crosses the boundary
//!
//! The page names which file it wants, `"licence"` or `"notices"`, and Rust
//! decides the path. Anything else is refused, so a page cannot read an
//! arbitrary file through [`licence_text`].

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// The window's size: wide enough for the notices' 80-column text at the page's
/// font, tall enough to read a screenful.
const SIZE: (f64, f64) = (760.0, 680.0);

/// The smallest the window may be dragged to.
const MINIMUM_SIZE: (f64, f64) = (420.0, 320.0);

/// The licence file `which` names, as the installer places it beside the
/// executable (`I-443`): `"licence"` is UP-TAKE's own GPL text and `"notices"`
/// the third-party notices. `None` for anything else, so the page cannot read
/// an arbitrary path through this module.
pub fn file_name(which: &str) -> Option<&'static str> {
    match which {
        "licence" => Some("LICENSE.txt"),
        "notices" => Some("THIRD-PARTY-NOTICES.txt"),
        _ => None,
    }
}

/// The window label for `which`: one window per file, so opening the licence
/// twice brings the first window forward and opening the notices beside it is
/// a second window. Every label starts with `licence-`, which is the pattern
/// `capabilities/licence.json` grants.
fn window_label(which: &str) -> Option<String> {
    file_name(which).map(|_| format!("licence-{which}"))
}

/// The window's title, in UP-TAKE's language.
fn window_title(which: &str) -> &'static str {
    use crate::strings::{Text, text};
    if which == "notices" {
        text(Text::LicenceNoticesTitle)
    } else {
        text(Text::LicenceOwnTitle)
    }
}

/// Where `which` lives in an installed UP-TAKE: beside the executable, where
/// the installer puts every resource and where `ocr.rs` looks for the runtime.
fn installed_path(which: &str) -> Result<PathBuf, String> {
    let name = file_name(which).ok_or_else(|| format!("no licence file is called {which}"))?;
    let executable =
        std::env::current_exe().map_err(|error| format!("could not locate UP-TAKE: {error}"))?;
    Ok(executable
        .parent()
        .ok_or_else(|| "UP-TAKE's executable has no folder".to_owned())?
        .join(name))
}

/// Reads `which` from `folder`. Split from [`installed_path`] so a test can
/// point it at a folder of its own.
fn read_from(folder: &Path, which: &str) -> Result<String, String> {
    let name = file_name(which).ok_or_else(|| format!("no licence file is called {which}"))?;
    let path = folder.join(name);
    std::fs::read_to_string(&path).map_err(|error| format!("could not read {name}: {error}"))
}

/// Opens the window for `which`, or brings it forward if it is already open.
///
/// **A development build has neither file**, because
/// `scripts/write-third-party-notices.py` writes them for the release bundle
/// only, so this checks the file is there before opening anything and answers
/// an error the Help pane shows in words.
///
/// # Errors
///
/// When `which` names no licence file, when the file is not beside the
/// executable, or when the window cannot be created.
pub fn open(app: &AppHandle, which: &str) -> Result<(), String> {
    let label = window_label(which).ok_or_else(|| format!("no licence file is called {which}"))?;
    let path = installed_path(which)?;
    if !path.is_file() {
        let name = file_name(which).unwrap_or(which);
        return Err(format!("{name} is not in this build"));
    }
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("licence/?which={which}").into()),
    )
    .title(window_title(which))
    .inner_size(SIZE.0, SIZE.1)
    .min_inner_size(MINIMUM_SIZE.0, MINIMUM_SIZE.1)
    .resizable(true)
    .center()
    // Shown by the page once it has painted, as the settings window is: a
    // window created visible shows one white frame before the dark page lands.
    .visible(false)
    .build()
    .map(|_| ())
    .map_err(|error| format!("could not open the {which} window: {error}"))
}

/// The text of `which`, for the licence window's page to show.
///
/// # Errors
///
/// When `which` names no licence file, or the file cannot be read.
#[tauri::command]
pub fn licence_text(which: &str) -> Result<String, String> {
    let path = installed_path(which)?;
    let folder = path
        .parent()
        .ok_or_else(|| "UP-TAKE's executable has no folder".to_owned())?;
    read_from(folder, which)
}

#[cfg(test)]
mod tests {
    use super::{file_name, read_from, window_label};

    /// `I-443`: Help shows exactly the two files the installer carries, under
    /// the names it carries them, and nothing else.
    #[test]
    #[allow(clippy::expect_used, reason = "a failed expect is a failed test")]
    fn help_shows_the_licence_files_the_installer_carries() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.release.conf.json"))
                .expect("tauri.release.conf.json must be valid JSON");
        let destinations: Vec<&str> = conf["bundle"]["resources"]
            .as_object()
            .expect("the release config names its resources")
            .values()
            .filter_map(serde_json::Value::as_str)
            .collect();
        for which in ["licence", "notices"] {
            let name = file_name(which).expect("a known licence file");
            assert!(
                destinations.contains(&name),
                "{name} is shown by Help and not packaged, so an installed UP-TAKE has no {which}"
            );
        }
        // Anything else is refused, so the page cannot read an arbitrary path.
        for other in ["", "LICENSE.txt", "../secret", "notice"] {
            assert_eq!(file_name(other), None, "{other:?}");
            assert_eq!(window_label(other), None, "{other:?}");
        }
    }

    /// One window per file, and every label is one `capabilities/licence.json`
    /// grants. A label outside its pattern is a window that cannot show itself,
    /// which looks like a button that does nothing (`UT-F-121`).
    #[test]
    #[allow(clippy::expect_used, reason = "a failed expect is a failed test")]
    fn each_file_has_its_own_window_and_the_capability_covers_it() {
        let licence = window_label("licence").expect("a label");
        let notices = window_label("notices").expect("a label");
        assert_ne!(licence, notices);
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/licence.json"))
                .expect("capabilities/licence.json must be valid JSON");
        let patterns: Vec<&str> = capability["windows"]
            .as_array()
            .expect("the capability lists its windows")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        for label in [&licence, &notices] {
            assert!(
                patterns.iter().any(|pattern| pattern
                    .strip_suffix('*')
                    .map_or(*pattern == label.as_str(), |prefix| label
                        .starts_with(prefix))),
                "{label} is not covered by {patterns:?}"
            );
            assert_ne!(label.as_str(), crate::overlay::WINDOW_LABEL);
            assert_ne!(label.as_str(), crate::settings_window::WINDOW_LABEL);
        }
    }

    #[test]
    #[allow(clippy::expect_used, reason = "a failed expect is a failed test")]
    fn the_text_is_read_from_the_folder_and_only_for_a_known_file() {
        let folder = std::env::temp_dir().join(format!("uptake-licence-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("a temporary folder");
        std::fs::write(folder.join("LICENSE.txt"), "GNU GENERAL PUBLIC LICENSE\n")
            .expect("a licence file");
        assert_eq!(
            read_from(&folder, "licence").as_deref(),
            Ok("GNU GENERAL PUBLIC LICENSE\n")
        );
        // Not there: an error naming the file, not a panic and not empty text.
        let missing = read_from(&folder, "notices").expect_err("no notices file");
        assert!(missing.contains("THIRD-PARTY-NOTICES.txt"), "{missing}");
        // A name that is not a licence file is refused before any path is built.
        assert!(read_from(&folder, "../LICENSE").is_err());
        let _ = std::fs::remove_dir_all(&folder);
    }
}
