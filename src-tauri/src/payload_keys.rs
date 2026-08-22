//! Test-only machinery for pinning the keys every IPC payload reaches the
//! frontend under (`I-67`).
//!
//! # The defect this exists for
//!
//! `UT-F-72`: `HoverPayload`'s Rust field `chrome_only` arrives as `chromeOnly`
//! solely because of `#[serde(rename_all = "camelCase")]`, and when that was
//! found, removing the attribute left `cargo test`, `clippy`, `vitest`, `biome`
//! and `svelte-check` **all green** while the frontend read `undefined`.
//!
//! ⚠️ **That was true on 2026-08-14 and is NOT true now, and this module was
//! argued from the obsolete half of its own backlog row.** `#56` merged a
//! covering test, `the_hover_payload_reaches_the_frontend_as_camel_case`, so
//! `cargo test` goes **RED** on that one struct today. Measured 2026-08-22 by
//! deleting the attribute at this branch's head: **two** tests fail, `#56`'s and
//! this module's own. `I-67`'s row already said so, in the sentence directly
//! after the one quoted here: *"`#56` covered that one instance with a
//! serialization test; the CLASS is still open"*.
//!
//! **The class argument is untouched by the correction, which is why this module
//! stands.** One instance is covered by one hand-written test naming one struct.
//! Twelve payload types exist and the other eleven have nothing; a rename added
//! to any of them, or removed from one that gains one, is still green. What
//! `#56` bought is a single guard rail on the single struct that has already
//! failed, and generalising from it is exactly what this module does instead.
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
//! real defect every time rather than a candidate for one.
//!
//! ⚠️ **This said it "cannot produce the false positives `OS-F74` is about,
//! because it makes no judgement", and that was false as written.** Making no
//! judgement is not the same as reporting no non-defects: a `Serialize` fixture
//! declared inside a scanned file's own `#[cfg(test)]` module is not a payload
//! and was reported as an uncovered one. The review round that found it wrote
//! that fixture and watched the control demand a reason for it. Test code is now
//! skipped, so the claim holds of what the code does rather than of what its
//! author meant -- and the honest form of it is narrower: **within production
//! code** it reports one condition and makes no judgement.
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
    let test_modules = source
        .lines()
        .filter(|line| line.trim_start() == "#[cfg(test)]")
        .count();
    assert!(
        test_modules <= 1,
        "{what}: {test_modules} `#[cfg(test)]` blocks. `serialize_structs` skips to \
         end-of-file at the first one, so a second means it scanned less of this \
         file than this list claims to cover. Split the file, or teach the \
         extractor to track nesting."
    );
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

/// Every `struct` or `enum` name in `source` whose `derive` mentions
/// `Serialize`, excluding anything inside the file's `#[cfg(test)]` module.
///
/// Deliberately blind to what sits between the `derive` and the type, so a
/// `#[serde(...)]` attribute in between does not hide it. It matches `derive`
/// lines rather than parsing Rust, which is why the caller asserts the result is
/// non-empty.
///
/// # Three things it got wrong, all found by the same review round
///
/// **Visibility defeated it.** The match was `strip_prefix("struct ")` on a
/// trimmed line, so `pub struct` and `pub(crate) struct` were skipped in
/// silence -- one keyword, and the completeness control that the whole *this is
/// the class* claim rests on reported nothing. Every payload in this repository
/// happens to be private today, which is why the suite was green; a single `pub`
/// on a new one would have made this control blind at the moment it was most
/// needed.
///
/// **Enums were invisible.** A `#[derive(Serialize)] enum` crosses the wire with
/// the same consequences and was matched by nothing here.
///
/// **`#[cfg(test)]` produced a false positive**, which matters because this
/// module's own doc claimed it could not produce any. A fixture struct declared
/// inside a scanned file's test module is not a payload, never reaches the
/// frontend, and was reported as an uncovered one -- a demand to document a
/// non-defect, which is how a check gets muted. Test code is skipped rather than
/// exempted one name at a time.
///
/// # Why skipping test code is safe here, and is checked rather than assumed
///
/// Skipping to end-of-file at `#[cfg(test)]` is right only while the attribute
/// appears once and last, which is true of every file in this crate and is not a
/// property of Rust. So [`assert_payload_coverage`] refuses a source carrying
/// more than one, rather than silently scanning less of the file than the caller
/// believes. Under-scanning is the failure this control cannot survive: a short
/// scan agrees with any list, which is the empty-extraction case one step short.
fn serialize_structs(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut derives_serialize = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed == "#[cfg(test)]" {
            break;
        }
        if trimmed.starts_with("#[derive(") {
            derives_serialize = trimmed.contains("Serialize");
            continue;
        }
        if derives_serialize && trimmed.starts_with("#[") {
            // A `#[serde(...)]` or a `#[allow(...)]` between the derive and the
            // type. Keep looking rather than losing it.
            continue;
        }
        if derives_serialize {
            if let Some(name) = type_name(trimmed) {
                names.push(name);
            }
            derives_serialize = false;
        }
    }
    names
}

/// The name declared by `struct X` or `enum X`, with any visibility in front.
///
/// Returns `None` for a line that declares neither, which is how a derive
/// followed by something else stops being tracked.
fn type_name(trimmed: &str) -> Option<String> {
    let rest = trimmed
        .strip_prefix("pub ")
        .or_else(|| {
            trimmed
                .strip_prefix("pub(")
                .and_then(|after| after.split_once(") "))
                .map(|(_, after)| after)
        })
        .unwrap_or(trimmed);
    let rest = rest
        .strip_prefix("struct ")
        .or_else(|| rest.strip_prefix("enum "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
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
    fn the_extractor_sees_a_public_struct() {
        // `F1` of round 2, and the sharpest of the round: the match was
        // `strip_prefix("struct ")`, so one keyword in front of it defeated the
        // control that the whole "this is the class" claim rests on. Both
        // spellings, because `pub(crate)` is the one this crate actually uses
        // for anything shared.
        let public = "#[derive(Serialize)]\npub struct Exported {\n    id: u64,\n}\n";
        assert_eq!(serialize_structs(public), vec!["Exported"]);
        let restricted = "#[derive(Serialize)]\npub(crate) struct Shared {\n    id: u64,\n}\n";
        assert_eq!(serialize_structs(restricted), vec!["Shared"]);
    }

    #[test]
    fn the_extractor_sees_a_serializable_enum() {
        // `F2`. An enum crosses the wire with the same consequences and was
        // matched by nothing, so the class was closed over structs alone.
        let source = "#[derive(Serialize)]\nenum Verdict {\n    Ok,\n}\n";
        assert_eq!(serialize_structs(source), vec!["Verdict"]);
        let public = "#[derive(Serialize)]\npub enum Outcome {\n    Ok,\n}\n";
        assert_eq!(serialize_structs(public), vec!["Outcome"]);
    }

    #[test]
    fn the_extractor_skips_a_fixture_in_the_test_module() {
        // `F3`. A fixture inside a scanned file's own `#[cfg(test)]` module is
        // not a payload, and reporting one is a demand to document a non-defect
        // -- which this module's doc claimed it could not produce.
        let source = "#[derive(Serialize)]\nstruct Real {\n    a: bool,\n}\n\
                      #[cfg(test)]\nmod tests {\n    #[derive(Serialize)]\n\
                      struct Fixture {\n        b: bool,\n    }\n}\n";
        assert_eq!(serialize_structs(source), vec!["Real"]);
    }

    #[test]
    #[should_panic(expected = "`#[cfg(test)]` blocks")]
    fn the_coverage_control_refuses_a_file_with_two_test_modules() {
        // The cost of skipping to end-of-file: it is only sound while the
        // attribute appears once. A second one means the scan stopped early, and
        // a short scan agrees with any list -- the empty-extraction failure one
        // step short of empty, and therefore one step short of the arm that
        // already catches it.
        assert_payload_coverage(
            "a fixture",
            "#[derive(Serialize)]\nstruct Real {\n    a: bool,\n}\n\
             #[cfg(test)]\nmod a {}\n#[cfg(test)]\nmod b {}\n",
            &["Real"],
            &[],
        );
    }

    #[test]
    fn every_module_that_emits_a_payload_is_covered_by_a_test_that_names_it() {
        // `F4`/`F5`: the class was closed per FILE. Two call sites pass
        // `include_str!` for `overlay.rs` and `placement.rs`, so a payload
        // declared in a THIRD module reached the frontend with nothing to notice
        // -- the completeness control had itself no completeness control. This
        // reads the source directory instead of a list of files someone
        // remembered to extend.
        //
        // It cannot use `include_str!`, which needs a literal path, so it reads
        // from `CARGO_MANIFEST_DIR`. That is resolved at compile time and the
        // sources are beside the test, so it works in CI exactly as it does
        // here; a missing directory panics rather than passing empty.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            panic!(
                "the crate source directory is readable at {}",
                dir.display()
            )
        };
        let mut emitting: Vec<String> = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                panic!("the crate source directory is readable")
            };
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                panic!("{} is readable", path.display())
            };
            if serialize_structs(&source).is_empty() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                panic!("{} has a name", path.display())
            };
            emitting.push(name.to_owned());
        }
        emitting.sort();
        assert!(
            !emitting.is_empty(),
            "no module in {} declares a Serialize type. The extractor is broken, \
             not the crate.",
            dir.display()
        );
        // The two modules whose tests call `assert_payload_coverage`. A third
        // name here is a module whose payloads nothing pins: give it a coverage
        // test of its own, and add it here in the same change.
        assert_eq!(
            emitting,
            vec!["overlay.rs", "placement.rs"],
            "a module declares a Serialize type and no `assert_payload_coverage` \
             call names it. `I-67`: a payload key is reachable by no other guard \
             in this repository."
        );
    }

    #[test]
    fn the_extractor_finds_an_indented_struct() {
        // Every payload here is at column 0 today. A nested one would otherwise
        // be invisible, which is the silent half of this control.
        let source = "    #[derive(Serialize)]\n    struct Inner {\n        id: u64,\n    }\n";
        assert_eq!(serialize_structs(source), vec!["Inner"]);
    }
}
