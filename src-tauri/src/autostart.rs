//! Starting with Windows, and telling a launch that did from a launch by hand
//! (roadmap 1.14, `ADR-0044` decision 2, backlog `I-391`).
//!
//! # Why the registry and not a plugin
//!
//! `tauri-plugin-autostart` does exactly this on Windows and adds a
//! dependency, a permission entry and a second way to be wrong about the
//! command line. The whole of the Windows implementation is one string value
//! under one well-known key, the `windows-sys` crate this app already depends
//! on is not even needed for it, and the argument in that string is the part
//! `ADR-0044` cares about. A dependency that saves thirty lines and hides the
//! one detail the decision record is about is a bad trade.
//!
//! # `HKCU`, never `HKLM`
//!
//! The per-user key needs no elevation, uninstalls with the user's profile,
//! and cannot start UP-TAKE for somebody who did not ask for it. The
//! machine-wide key would need the installer and an administrator, for a
//! setting whose default is off.
//!
//! # The argument is the whole point
//!
//! `ADR-0044` decision 2: a launch with Windows is recognised **only** by an
//! argument the registration itself passes, never inferred from the
//! environment. So the value written here ends in [`FLAG`], and
//! [`launched_by_windows`] looks for nothing else.
//!
//! Inferring was the alternative and it is worse in both directions: the
//! session's own start time, the parent process, `STARTUPINFO`'s flags and the
//! absence of a console are each true of things that are not an autostart (a
//! relaunch by the installer, a shortcut in the Startup folder, a debugger)
//! and false of things that are (a user who copies the registry value into a
//! shortcut). An argument we wrote ourselves is the only signal that means
//! what it says.
//!
//! # What a user who edits it by hand gets
//!
//! Exactly what they asked for. If they remove the flag from the registry
//! value, UP-TAKE starts with Windows **and** lands in Placement, because by
//! this module's own rule that is now a hand launch. That is a coherent thing
//! to want, and it is the reason the flag is not also cross-checked against
//! whether the registration exists: two sources would let them disagree.

/// The argument the registration passes, and the only signal that a launch
/// came from Windows rather than from a person.
pub const FLAG: &str = "--autostart";

/// The value name under the Run key. Also what a user sees in Task Manager's
/// Startup tab, so it is the product name rather than the executable's.
#[cfg(windows)]
const VALUE_NAME: &str = "UP-TAKE";

/// The per-user Run key.
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Whether this process was started by Windows rather than by a person.
///
/// Takes the arguments rather than reading them, so the rule is testable
/// without a process to launch. **Compared exactly**, not by prefix or by
/// `contains`: a path that happens to hold the text would otherwise read as
/// the flag, and this decides whether the user sees their overlay at login.
pub fn launched_by_windows<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == FLAG)
}

/// The command line the Run value holds: this executable, quoted, plus
/// [`FLAG`].
///
/// **Quoted always, not only when the path has a space.** `C:\Program
/// Files\...` is the normal install location, and an unquoted value there
/// makes Windows try `C:\Program` first. Quoting a path that did not need it
/// costs nothing.
#[cfg(windows)]
fn command_line_for(executable: &std::path::Path) -> String {
    format!("\"{}\" {FLAG}", executable.display())
}

/// Makes the registration match `enabled`.
///
/// # Errors
///
/// When the executable's own path cannot be read, or the registry key cannot
/// be opened or written.
///
/// Turning it **off** succeeds when the value is already absent: the user
/// asked for UP-TAKE not to start with Windows, and it will not, which is the
/// outcome they wanted whoever removed the value.
#[cfg(windows)]
pub fn apply(enabled: bool) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not find UP-TAKE's own path: {error}"))?;
    if enabled {
        write_value(&command_line_for(&executable))
    } else {
        delete_value()
    }
}

#[cfg(not(windows))]
pub fn apply(_enabled: bool) -> Result<(), String> {
    Err("starting with Windows is a Windows-only setting".to_string())
}

/// Whether the registration is present. Used by the settings window to report
/// what is actually on the machine rather than what was last stored.
#[cfg(windows)]
#[must_use]
pub fn is_registered() -> bool {
    read_value().is_some()
}

#[cfg(not(windows))]
#[must_use]
pub const fn is_registered() -> bool {
    false
}

#[cfg(windows)]
mod registry {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
        RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    use super::{RUN_KEY, VALUE_NAME};

    /// A NUL-terminated UTF-16 copy, for the `W` entry points.
    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text).encode_wide().chain(Some(0)).collect()
    }

    /// The Run key, opened with `access`, closed by the caller.
    fn open(access: u32) -> Result<HKEY, String> {
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `wide` gives a NUL-terminated UTF-16 string that outlives the
        // call, and `key` is a valid out-pointer for one HKEY.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                0,
                access,
                &raw mut key,
            )
        };
        if status == ERROR_SUCCESS {
            Ok(key)
        } else {
            Err(format!(
                "could not open HKEY_CURRENT_USER\\{RUN_KEY} (error {status})"
            ))
        }
    }

    pub(super) fn write(command_line: &str) -> Result<(), String> {
        let key = open(KEY_SET_VALUE)?;
        let value = wide(command_line);
        let bytes = std::mem::size_of_val(value.as_slice());
        let Ok(length) = u32::try_from(bytes) else {
            // SAFETY: `key` came from a successful open and is not used again.
            unsafe { RegCloseKey(key) };
            return Err("the startup command line is impossibly long".to_string());
        };
        // SAFETY: `key` is open for KEY_SET_VALUE, the name and the data are
        // NUL-terminated UTF-16 buffers that outlive the call, and `length` is
        // their size in bytes as REG_SZ requires.
        let status = unsafe {
            RegSetValueExW(
                key,
                wide(VALUE_NAME).as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast::<u8>(),
                length,
            )
        };
        // SAFETY: `key` came from a successful open and is not used again.
        unsafe { RegCloseKey(key) };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "could not write the startup entry (error {status})"
            ))
        }
    }

    pub(super) fn delete() -> Result<(), String> {
        let key = match open(KEY_SET_VALUE) {
            Ok(key) => key,
            // No Run key at all means nothing starts from it, which is the
            // state "off" is asking for.
            Err(_) => return Ok(()),
        };
        // SAFETY: `key` is open for KEY_SET_VALUE and the name is a
        // NUL-terminated UTF-16 buffer that outlives the call.
        let status = unsafe { RegDeleteValueW(key, wide(VALUE_NAME).as_ptr()) };
        // SAFETY: `key` came from a successful open and is not used again.
        unsafe { RegCloseKey(key) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!(
                "could not remove the startup entry (error {status})"
            ))
        }
    }

    pub(super) fn read() -> Option<()> {
        let key = open(KEY_QUERY_VALUE).ok()?;
        // SAFETY: `key` is open for KEY_QUERY_VALUE and the name is a
        // NUL-terminated UTF-16 buffer that outlives the call. Every out
        // parameter is null, which asks only whether the value exists.
        let status = unsafe {
            RegQueryValueExW(
                key,
                wide(VALUE_NAME).as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `key` came from a successful open and is not used again.
        unsafe { RegCloseKey(key) };
        (status == ERROR_SUCCESS).then_some(())
    }
}

#[cfg(windows)]
fn write_value(command_line: &str) -> Result<(), String> {
    registry::write(command_line)
}

#[cfg(windows)]
fn delete_value() -> Result<(), String> {
    registry::delete()
}

#[cfg(windows)]
fn read_value() -> Option<()> {
    registry::read()
}

#[cfg(test)]
mod tests {
    use super::{FLAG, launched_by_windows};

    #[test]
    fn the_flag_is_what_makes_a_launch_windows_own() {
        assert!(launched_by_windows(["up-take.exe", FLAG]));
        assert!(!launched_by_windows(["up-take.exe"]));
        assert!(!launched_by_windows(Vec::<&str>::new()));
    }

    #[test]
    fn a_path_that_merely_contains_the_text_is_not_the_flag() {
        // ADR-0044 decision 2 turns on this being an exact signal. A
        // `contains` or a prefix test would read the first of these as an
        // autostart and leave the user staring at a screen with no overlay
        // after launching UP-TAKE themselves.
        assert!(!launched_by_windows([r"C:\tools\--autostart\up-take.exe"]));
        assert!(!launched_by_windows(["--autostart-please"]));
        assert!(!launched_by_windows(["autostart"]));
        assert!(!launched_by_windows(["--Autostart"]));
    }

    #[cfg(windows)]
    #[test]
    fn the_registered_command_line_is_quoted_and_carries_the_flag() {
        use std::path::Path;

        let value =
            super::command_line_for(Path::new(r"C:\Program Files\VyLone\UP-TAKE\up-take.exe"));
        assert_eq!(
            value,
            r#""C:\Program Files\VyLone\UP-TAKE\up-take.exe" --autostart"#
        );
        // The quoting is the half that is easy to drop as unnecessary. Without
        // it Windows runs `C:\Program` and the user gets nothing at login,
        // with no error anywhere they would look.
        assert!(value.starts_with('"'), "{value}");
        // Split the way Windows splits it, and the flag must survive as its
        // own argument rather than being swallowed into the quoted path.
        assert!(value.ends_with(&format!(" {FLAG}")), "{value}");
    }

    #[cfg(windows)]
    #[test]
    fn a_registered_command_line_reads_back_as_a_windows_launch() {
        use std::path::Path;

        // The round trip the two halves of this module owe each other: what
        // `apply` writes must be what `launched_by_windows` recognises. They
        // are tested apart everywhere else, and a flag renamed in one place
        // would leave both sides green.
        let value = super::command_line_for(Path::new(r"C:\up-take.exe"));
        let Some((_, arguments)) = value.rsplit_once("\" ") else {
            panic!("the registered value has no arguments: {value}")
        };
        assert!(launched_by_windows(arguments.split(' ')));
    }
}
