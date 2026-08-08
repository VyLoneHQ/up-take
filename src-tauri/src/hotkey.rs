//! Global hotkey registration (roadmap tasks 1.4 and 1.9e).
//!
//! Two combinations, and they are deliberately different in kind. `Win+Shift+U`
//! changes **state**: it toggles input focus between UP-TAKE and the real
//! screen. `Win+Shift+G` changes **nothing** and produces an artifact: it copies
//! the monitor under the cursor to the clipboard and leaves every state exactly
//! as it found it (see [`crate::output::grab_monitor`]).
//!
//! `Win+Shift+U` toggles input focus between UP-TAKE and the real screen
//! (ADR-0012) — the first press summons the overlay into Placement. Task 1.5's
//! tray can summon it too, so this is no longer the only way in — but a failed
//! registration is still
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

/// How the instant-grab shortcut is written for the user (roadmap task 1.9e).
pub const GRAB_LABEL: &str = "Win+Shift+G";

/// The combination that summons the overlay.
///
/// `Modifiers::SUPER` is the Windows key. Hard-coded until task 1.14 makes it
/// configurable; that task should keep this as the default rather than invent
/// a new one, since it is the combination the README and any early docs name.
pub fn summon_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyU)
}

/// The combination that grabs the monitor under the cursor (roadmap task 1.9e,
/// [ADR-0014] section 4: "a separate hotkey does an instant whole-monitor grab
/// of the monitor under the cursor").
///
/// # Why this combination, which is a guess and is labelled as one
///
/// `G` for grab, on the same `Win+Shift` prefix as the summon so the two read as
/// one application's keys rather than two unrelated ones. Windows itself assigns
/// `Win+G` (Game Bar) and `Win+Shift+S` (Snip and Sketch) but not `Win+Shift+G`,
/// checked against Microsoft's published shortcut list rather than recalled.
///
/// **What that check cannot cover is other applications**, which is the whole of
/// scenario M-9 and the reason [`install`] reports a failed registration to the
/// user instead of logging it. If this combination turns out to be commonly
/// taken, changing it is one line here plus the README, and task 1.14 makes it
/// the user's choice rather than ours.
///
/// [ADR-0014]: the private planning repo's
/// `DECISIONS/ADR-0014-capture-and-render-over-live-content.md`
pub fn grab_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyG)
}

/// Registers both global hotkeys, reporting each failure to the user.
///
/// Never returns an error: a hotkey that could not be registered is
/// architecture §5 class 1 (user-fixable) — the app keeps running and says what
/// is wrong — not a reason to refuse to start. Refusing would be worse for the
/// exact user it affects, since the app they cannot summon is also the app they
/// then cannot reconfigure.
///
/// # The two registrations are independent, deliberately
///
/// A failure of one does not skip the other, and each reports under its own
/// label. The alternative, one report naming "the hotkeys", tells the user to go
/// looking for a conflict without saying which combination to look for, and the
/// fix is per combination. It also means a taken `Win+Shift+G` costs the user
/// the grab and not the summon, which is the difference between a degraded
/// application and an unreachable one.
pub fn install(app: &AppHandle) {
    let summon_app = app.clone();
    let summon =
        app.global_shortcut()
            .on_shortcut(summon_shortcut(), move |_app, _shortcut, event| {
                // `Pressed` only. Acting on both would summon the overlay twice per
                // press, and the `Released` half arrives on a different thread (see
                // the module docs).
                if event.state != ShortcutState::Pressed {
                    return;
                }
                #[cfg(debug_assertions)]
                crate::dev_harness::log_summon("hotkey", overlay::current_origin(&summon_app));
                // The hotkey toggles input focus between UP-TAKE and the real
                // screen (ADR-0012), rather than only ever showing.
                overlay::toggle(&summon_app);
            });
    if let Err(error) = summon {
        report_failure(app, SUMMON_LABEL, &error.to_string());
    }

    let grab_app = app.clone();
    let grab = app
        .global_shortcut()
        .on_shortcut(grab_shortcut(), move |_app, _shortcut, event| {
            if event.state != ShortcutState::Pressed {
                return;
            }
            // **Does not summon, show, or change state.** Task 1.9e is the path
            // with no placement gesture at all, so a grab that first brought the
            // overlay up would be the gesture it exists to avoid, and it would
            // freeze nothing and select nothing on the way past.
            //
            // The cursor is read inside `grab_monitor` on this thread, before it
            // spawns, for the reason `overlay::toggle_freeze` reads it here too:
            // it is the cursor at the moment the key was pressed, and by the time
            // a worker runs the pointer may be on another monitor.
            crate::output::grab_monitor(&grab_app);
        });
    if let Err(error) = grab {
        report_failure(app, GRAB_LABEL, &error.to_string());
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
fn report_failure(app: &AppHandle, label: &str, error: &str) {
    let detail = if is_already_registered(error) {
        format!(
            "Another application is already using {label}.\n\n\
             Close that application, or change its shortcut, then restart UP-TAKE."
        )
    } else {
        format!(
            "Windows refused to register {label}.\n\n\
             Restarting UP-TAKE usually clears this. If it persists, please report it \
             with the details below.\n\n{error}"
        )
    };
    eprintln!("hotkey: {label} could not be registered: {error}");
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
    fn the_grab_shortcut_is_win_shift_g() {
        // Pinned for the same reason the summon is: this string goes in the
        // README, and roadmap 1.14 is meant to inherit it as the default rather
        // than invent one.
        let shortcut = grab_shortcut();
        assert_eq!(shortcut.key, Code::KeyG);
        assert!(shortcut.mods.contains(Modifiers::SUPER));
        assert!(shortcut.mods.contains(Modifiers::SHIFT));
        assert!(!shortcut.mods.contains(Modifiers::ALT));
        assert!(!shortcut.mods.contains(Modifiers::CONTROL));
    }

    #[test]
    fn the_two_shortcuts_are_not_the_same_combination() {
        // `install` registers both against one manager. Two identical
        // combinations would not be a compile error and would not be a visible
        // defect either: the second registration fails with "already
        // registered", which `report_failure` presents to the user as *another
        // application* holding the key. So the failure mode of getting this
        // wrong is a dialog blaming a program that does not exist.
        let (summon, grab) = (summon_shortcut(), grab_shortcut());
        assert!(summon.key != grab.key || summon.mods != grab.mods);
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
