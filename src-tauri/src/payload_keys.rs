//! Test-only machinery for pinning the keys every IPC payload reaches the
//! frontend under (`I-67`).
//!
//! # The defect this exists for
//!
//! `UT-F-72`: `HoverPayload`'s Rust field `chrome_only` arrives as `chromeOnly`
//! solely because of `#[serde(rename_all = "camelCase")]`. Remove that attribute
//! and `cargo test`, `clippy`, `vitest`, `biome` and `svelte-check` are **all
//! green** while the frontend reads `undefined`, its default fires, and the
//! defect the branch removed comes back.
//!
//! `src/lib/area-kinds.test.ts` is the guard for wire names written twice, and
//! its own closing sentence says it "cannot see a name that reaches the frontend
//! by any route other than these four functions". A payload key is such a route,
//! so that guard is blind here by construction.
//!
//! # What the enumeration found, which is worse than one struct with one attribute
//!
//! Measured across `overlay.rs` and `placement.rs` rather than assumed. **Twelve
//! payload types, and exactly one of them carries a rename.** `HoverPayload` is
//! camelCase; the other eleven are snake_case verbatim, and one of those eleven
//! has a multi-word field of its own: `StatePayload.freeze_probe`, which the
//! frontend correctly reads as `freeze_probe` at `+page.svelte`.
//!
//! **Both are right today and nothing says so.** The wire is half camelCase and
//! half snake_case, the single `rename_all` in `src-tauri` reads as a stray to
//! anyone tidying, and adding one to `StatePayload` would be just as green while
//! breaking the freeze probe. A lone exception to a convention is a deletion
//! candidate; a lone exception that is *load-bearing* and untested is `UT-F-72`
//! waiting to happen again in the other direction.
//!
//! # Why a hand-maintained table and not a generator
//!
//! The backlog row asked whether `ts-rs` or `specta` is the smaller answer.
//! Neither, at this size: both need a build step, a checked-in artefact and a CI
//! check that the artefact is current, which is three moving parts for twelve
//! structs. This is one test file and the suite that already runs.
//!
//! # Why the coverage control is a regex, when the row warns against one
//!
//! `I-67`'s constraint says enumerating `#[derive(Serialize)]` structs by regex
//! "is the kind of check that reports non-defects and gets muted (`OS-F74`)".
//! That is true of a regex used as **the check**, and this is not that. The
//! regex here is a **completeness control over a hand-maintained list**: it
//! reports exactly one condition, a payload type that no test names, which is a
//! real defect every time rather than a candidate for one. It cannot produce the
//! false positives `OS-F74` is about, because it makes no judgement.
//!
//! It still has a failure mode, and it is the opposite one: a reshaped attribute
//! or a renamed `struct` keyword makes it match nothing, and a silent empty set
//! would agree with any list at all. So [`assert_payload_coverage`] **refuses an
//! empty extraction**, the same way `area-kinds.test.ts` throws rather than
//! comparing two empty sets.

use serde::Serialize;

/// The keys `value` actually serializes to, sorted.
///
/// Reads the serialized form rather than the Rust field names, which is the
/// whole point: a `rename` or a `rename_all` is invisible to anything that reads
/// the struct definition, and it is the only thing that decides what the
/// frontend can see.
///
/// # Panics
///
/// If `value` does not serialize to a JSON object. Every payload here is a
/// struct, so that is a broken test rather than a condition to handle.
pub(crate) fn keys_of<T: Serialize>(value: &T) -> Vec<String> {
    let Ok(serde_json::Value::Object(map)) = serde_json::to_value(value) else {
        panic!("a payload must serialize to a JSON object")
    };
    let mut keys: Vec<String> = map.into_iter().map(|(key, _)| key).collect();
    keys.sort();
    keys
}

/// Asserts that `keys_of(value)` is exactly `expected`.
///
/// Takes the expected set as `&str` so a test reads as the wire contract rather
/// than as Rust: these are the strings the frontend indexes with.
pub(crate) fn assert_keys<T: Serialize>(what: &str, value: &T, expected: &[&str]) {
    let mut want: Vec<String> = expected.iter().map(|key| (*key).to_owned()).collect();
    want.sort();
    assert_eq!(
        keys_of(value),
        want,
        "{what} does not serialize to the keys the frontend reads"
    );
}

/// Asserts that every `Serialize` struct in `source` is named in `covered`.
///
/// `exempt` takes `(name, reason)` pairs for a `Serialize` struct that is not an
/// IPC payload. There are none today; the parameter exists so that the first one
/// is added with a reason beside it rather than by deleting a row from a test.
///
/// # Panics
///
/// If the extraction finds nothing. A reshaped attribute or a renamed keyword
/// would otherwise produce an empty set that agrees with any list, which is this
/// control passing for the exact reason it exists.
pub(crate) fn assert_payload_coverage(
    what: &str,
    source: &str,
    covered: &[&str],
    exempt: &[(&str, &str)],
) {
    let found = serialize_structs(source);
    assert!(
        !found.is_empty(),
        "{what}: extracted no Serialize structs. Has the attribute or the struct \
         keyword been reshaped? An empty set agrees with any list."
    );
    for name in &found {
        let known = covered.contains(&name.as_str())
            || exempt.iter().any(|(exempt_name, _)| exempt_name == name);
        assert!(
            known,
            "{what}: `{name}` derives Serialize and no test pins its keys. Add it \
             to the key table, or to the exempt list with a reason. This is `I-67`: \
             a payload key is reachable by no other guard in this repository."
        );
    }
}

/// Every `struct` name in `source` whose `derive` mentions `Serialize`.
///
/// Deliberately blind to what sits between the `derive` and the `struct`, so a
/// `#[serde(...)]` attribute in between does not hide the type. It matches
/// `derive` lines rather than parsing Rust, which is why the caller asserts the
/// result is non-empty.
fn serialize_structs(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut derives_serialize = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[derive(") {
            derives_serialize = trimmed.contains("Serialize");
            continue;
        }
        if derives_serialize && trimmed.starts_with("#[") {
            // A `#[serde(...)]` or a `#[allow(...)]` between the derive and the
            // struct. Keep looking rather than losing the type.
            continue;
        }
        if derives_serialize {
            if let Some(rest) = trimmed.strip_prefix("struct ") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    names.push(name);
                }
            }
            derives_serialize = false;
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::{assert_keys, assert_payload_coverage, keys_of, serialize_structs};

    #[test]
    fn a_rename_is_visible_here_and_nowhere_else() {
        // The point of reading the serialized form. `chrome_only` and
        // `chromeOnly` are the same struct definition and different wires.
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Renamed {
            chrome_only: bool,
        }
        #[derive(serde::Serialize)]
        struct Plain {
            chrome_only: bool,
        }
        assert_eq!(keys_of(&Renamed { chrome_only: true }), vec!["chromeOnly"]);
        assert_eq!(keys_of(&Plain { chrome_only: true }), vec!["chrome_only"]);
    }

    #[test]
    fn the_extractor_sees_a_struct_behind_a_serde_attribute() {
        // `HoverPayload`'s shape: the derive, then an attribute, then the
        // struct. An extractor that required them adjacent would miss the one
        // type in this repository that actually has a rename.
        let source = "#[derive(Serialize, Clone)]\n#[serde(rename_all = \"camelCase\")]\n\
                      struct HoverPayload {\n    id: Option<u64>,\n}\n";
        assert_eq!(serialize_structs(source), vec!["HoverPayload"]);
    }

    #[test]
    fn the_extractor_ignores_a_struct_that_does_not_derive_serialize() {
        let source = "#[derive(Debug, Clone, Copy)]\nstruct Gesture {\n    id: u64,\n}\n";
        assert!(serialize_structs(source).is_empty());
    }

    // The four tests below drill the two helpers' FAILURE paths, and they exist
    // because a mutation pass found them missing. `assert_payload_coverage` was
    // mutated to `let known = true || ...`, making the control vacuous, and the
    // whole suite stayed green: every call site passes a complete list, so
    // nothing ever exercised the arm that refuses. A control whose refusal is
    // never driven is `A3`'s "check that cannot go red" with the red merely
    // deferred to a day nobody is watching, and `A10` says each guard is owed a
    // drill of its own rather than a suite that goes green for other reasons.

    #[test]
    #[should_panic(expected = "derives Serialize and no test pins its keys")]
    fn the_coverage_control_refuses_a_payload_that_no_test_names() {
        assert_payload_coverage(
            "a fixture",
            "#[derive(Serialize)]\nstruct Known {\n    a: bool,\n}\n\
             #[derive(Serialize)]\nstruct Stray {\n    b: bool,\n}\n",
            &["Known"],
            &[],
        );
    }

    #[test]
    #[should_panic(expected = "extracted no Serialize structs")]
    fn the_coverage_control_refuses_an_empty_extraction() {
        // The silent half. Without this arm a reshaped attribute produces an
        // empty set, which agrees with every list including an empty one, and
        // the control reports success for the reason it exists to prevent.
        assert_payload_coverage("a fixture", "struct NotSerialized {}\n", &["Known"], &[]);
    }

    #[test]
    fn the_coverage_control_accepts_an_exempted_name() {
        // The positive control for the escape hatch: without it the test above
        // would also pass against a version that refuses everything.
        assert_payload_coverage(
            "a fixture",
            "#[derive(Serialize)]\nstruct OnDisk {\n    a: bool,\n}\n",
            &[],
            &[("OnDisk", "written to disk, never sent over IPC")],
        );
    }

    #[test]
    #[should_panic(expected = "does not serialize to the keys the frontend reads")]
    fn the_key_assertion_refuses_a_set_that_does_not_match() {
        #[derive(serde::Serialize)]
        struct Two {
            a: bool,
            b: bool,
        }
        assert_keys("Two", &Two { a: true, b: true }, &["a"]);
    }

    #[test]
    fn the_key_assertion_does_not_care_about_declaration_order() {
        // Both sides are sorted, so a field reordered in the struct is not a
        // wire change and must not read as one. Otherwise this control would
        // report a non-defect, which is how a check gets muted (`OS-F74`).
        #[derive(serde::Serialize)]
        struct Two {
            b: bool,
            a: bool,
        }
        assert_keys("Two", &Two { b: true, a: true }, &["a", "b"]);
    }

    #[test]
    fn the_extractor_finds_an_indented_struct() {
        // Every payload here is at column 0 today. A nested one would otherwise
        // be invisible, which is the silent half of this control.
        let source = "    #[derive(Serialize)]\n    struct Inner {\n        id: u64,\n    }\n";
        assert_eq!(serialize_structs(source), vec!["Inner"]);
    }
}
