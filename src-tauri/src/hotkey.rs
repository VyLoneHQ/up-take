//! Global hotkey registration (roadmap task 1.4).
//!
//! `Win+Shift+U` summons the overlay. Task 1.5's tray can summon it too, so
//! this is no longer the only way in — but a failed registration is still
//! surfaced to the user rather than logged: architecture §4 lists "shadowing
//! another app's hotkey" as a threat whose mitigation is "detect registration
//! failure and tell the user rather than silently doing nothing". The tray
//! does not discharge that. A user who presses the combination and sees
//! nothing has no way to tell a shadowed hotkey from a broken app, and the
//! fix — closing or rebinding the other application — is one only they can
//! make and only if told.
//!
//! ## Which thread the handler runs on
//!
//! The event-loop (main) thread, and that is a property of the dependency
//! chain rather than something we chose — verified in the sources rather than
//! assumed, because [`overlay::show`] behaves differently off that thread:
//!
//! 1. `tauri-plugin-global-shortcut` constructs its `GlobalHotKeyManager`
//!    inside the plugin's `setup` hook, so the message-only window that
//!    receives `WM_HOTKEY` is owned by the main thread.
//! 2. `WM_HOTKEY` is therefore dispatched by tao's own event loop into
//!    `global_hotkey_proc`.
//! 3. That wndproc calls `GlobalHotKeyEvent::send`, which invokes the
//!    registered handler **inline** when one is set — no channel, no hop.
//!
//! Only [`ShortcutState::Released`] escapes this: `global_hotkey` detects the
//! key-up by spawning a thread that polls `GetAsyncKeyState`, so a `Released`
//! handler runs on that worker instead. We act on `Pressed` and ignore
//! `Released`, which keeps the summon path on the event-loop thread.
//!
//! **Why it matters.** tao buffers events raised inside a handler until the
//! handler returns, so a `Moved` event raised by `show`'s own reposition is
//! dispatched *after* `show` finishes and incidentally refreshes the
//! click-through regions. Called off the event-loop thread, that event arrives
//! while the window is still invisible, `sync_bounds` returns early, and
//! nothing refreshes them — which is exactly why `show` calls
//! `reconvert_regions` itself. Being on the event-loop thread means this path
//! does not *depend* on that call; it does not make the call redundant, since
//! `dev_harness` and any future off-thread caller do. See `dev_harness.rs`.

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::overlay;

/// How the summon shortcut is written for the user. Windows' own spelling —
/// the key is labelled `Win` on the hardware, not `Super` or `Meta`.
pub const SUMMON_LABEL: &str = "Win+Shift+U";

/// The combination that summons the overlay.
///
/// `Modifiers::SUPER` is the Windows key. Hard-coded until task 1.14 makes it
/// configurable; that task should keep this as the default rather than invent
/// a new one, since it is the combination the README and any early docs name.
pub fn summon_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyU)
}

/// Registers the summon hotkey, reporting a failure to the user.
///
/// Never returns an error: a hotkey that could not be registered is
/// architecture §5 class 1 (user-fixable) — the app keeps running and says what
/// is wrong — not a reason to refuse to start. Refusing would be worse for the
/// exact user it affects, since the app they cannot summon is also the app they
/// then cannot reconfigure.
pub fn install(app: &AppHandle) {
    let shortcut = summon_shortcut();
    let handler_app = app.clone();
    let outcome = app
        .global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            // `Pressed` only. Acting on both would summon the overlay twice per
            // press, and the `Released` half arrives on a different thread (see
            // the module docs).
            if event.state != ShortcutState::Pressed {
                return;
            }
            #[cfg(debug_assertions)]
            crate::dev_harness::log_summon("hotkey", overlay::current_origin(&handler_app));
            if let Err(error) = overlay::show(&handler_app) {
                eprintln!("hotkey: could not show the overlay: {error}");
            }
        });

    if let Err(error) = outcome {
        report_failure(app, &error.to_string());
    }
}

/// Tells the user the hotkey is unavailable, and what to do about it.
///
/// Shown as a dialog rather than logged because there is still nowhere else for
/// it to go. Task 1.5's tray is *not* that place: it can summon the overlay,
/// but it cannot tell the user that a combination they are already pressing
/// belongs to another application — a tray icon says nothing until it is
/// clicked, and the user with a shadowed hotkey has no reason to click it.
/// Until the settings window lands (task 1.14) there is no surface that could
/// hold this, and stderr is invisible in an installed build. Non-blocking —
/// during `setup` the event
/// loop has not started, so a blocking dialog would deadlock the startup it is
/// reporting on.
fn report_failure(app: &AppHandle, error: &str) {
    let detail = if is_already_registered(error) {
        format!(
            "Another application is already using {SUMMON_LABEL}.\n\n\
             Close that application, or change its shortcut, then restart UP-TAKE."
        )
    } else {
        format!(
            "Windows refused to register {SUMMON_LABEL}.\n\n\
             Restarting UP-TAKE usually clears this. If it persists, please report it \
             with the details below.\n\n{error}"
        )
    };
    eprintln!("hotkey: {SUMMON_LABEL} could not be registered: {error}");
    app.dialog()
        .message(detail)
        .kind(MessageDialogKind::Warning)
        .title("UP-TAKE — hotkey unavailable")
        .show(|_| {});
}

/// Whether a registration error is the "someone else holds this combination"
/// case (manual scenario M-9).
///
/// **Matched on the message text, of necessity.** Windows reports this
/// distinctly as `ERROR_HOTKEY_ALREADY_REGISTERED` and `global_hotkey` does
/// model it as its own `Error::AlreadyRegistered` variant — but
/// `tauri-plugin-global-shortcut` flattens every cause into
/// `Error::GlobalHotkey(String)` on the way out, so the variant is gone by the
/// time we see it.
///
/// This is brittle by construction: it depends on a dependency's `Display`
/// output. It is written so that breaking is harmless — a missed match falls
/// back to the generic message, which is still true, still actionable, and
/// still includes the original error text. Nothing silently disappears.
fn is_already_registered(error: &str) -> bool {
    error.contains("already registered")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summon_shortcut_is_win_shift_u() {
        // Pins the combination against an accidental edit: this string is in
        // the README and will be in the first release notes.
        let shortcut = summon_shortcut();
        assert_eq!(shortcut.key, Code::KeyU);
        assert!(shortcut.mods.contains(Modifiers::SUPER));
        assert!(shortcut.mods.contains(Modifiers::SHIFT));
        assert!(!shortcut.mods.contains(Modifiers::ALT));
        assert!(!shortcut.mods.contains(Modifiers::CONTROL));
    }

    #[test]
    fn the_conflict_case_is_recognised() {
        // Copied verbatim from a real conflict on the dev rig — a second
        // UP-TAKE instance started while the first held the shortcut. Task
        // 1.5's single-instance guard now exits that second instance before it
        // reaches registration, so reproducing it again needs
        // `UPTAKE_DEV_ALLOW_MULTIPLE=1` (see `dev_harness`). Pinning
        // the *observed* string rather than a plausible one is the point: a
        // dependency bump that reworded it fails here, instead of silently
        // downgrading every conflict to the generic message.
        assert!(is_already_registered(
            "HotKey already registered: HotKey { mods: Modifiers(SHIFT | SUPER), key: KeyU, id: 570425383 }"
        ));
    }

    #[test]
    fn other_failures_are_not_mistaken_for_a_conflict() {
        for error in [
            "Unable to register hotkey: something else went wrong",
            "Failed to watch media key event",
            "",
        ] {
            assert!(!is_already_registered(error));
        }
    }
}
