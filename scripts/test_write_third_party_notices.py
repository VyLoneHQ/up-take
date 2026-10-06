#!/usr/bin/env python3
"""Tests for the third-party notices (I-443).

What they defend is the part of `write-third-party-notices.py` that can be wrong
quietly: which JavaScript packages a build is found to contain, whether a
package's licence is accepted, and which of its files are its licence. The two
generators it calls, `cargo about` and Vite, are other projects' and are run by
the Windows build in CI, where `verify-bundle.py` checks the result reached the
installer.

Run: `python3 scripts/test_write_third_party_notices.py`
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import sys
import tempfile
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
RELEASE_CONF = ROOT / "src-tauri" / "tauri.release.conf.json"


def load_module():
    spec = importlib.util.spec_from_file_location(
        "write_third_party_notices", HERE / "write-third-party-notices.py"
    )
    if spec is None or spec.loader is None:
        raise SystemExit("could not load write-third-party-notices.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_a_pnpm_source_names_the_real_package_not_the_store_entry(module) -> None:
    """pnpm nests the real package under `.pnpm/<name>@<version>/node_modules/`,
    and the package is the part after the LAST `node_modules/`."""
    root = os.path.join("C:", os.sep, "repo")
    source = "../../../node_modules/.pnpm/svelte@5.57.0/node_modules/svelte/src/internal/client/index.js"
    expected = os.path.normpath(
        os.path.join(root, "node_modules/.pnpm/svelte@5.57.0/node_modules/svelte")
    )
    assert module.package_directory(source, root) == expected


def test_a_scoped_package_keeps_both_parts(module) -> None:
    root = os.path.join("C:", os.sep, "repo")
    source = "node_modules/.pnpm/@sveltejs+kit@2.70.3_x/node_modules/@sveltejs/kit/src/runtime/client.js"
    found = module.package_directory(source, root)
    assert found is not None
    assert found.replace("\\", "/").endswith("node_modules/@sveltejs/kit"), found


def test_the_package_is_anchored_on_the_repository_not_the_maps_folder(module) -> None:
    """The first run's defect: SvelteKit's maps name sources relative to another
    folder, and resolving them from the map's own folder climbed out of the
    repository. However many `../` a source carries, the package is under
    `root`."""
    root = os.path.join("C:", os.sep, "repo")
    for climb in ("", "../", "../../../../../"):
        found = module.package_directory(climb + "node_modules/clsx/dist/clsx.mjs", root)
        assert found == os.path.normpath(os.path.join(root, "node_modules/clsx")), (climb, found)


def test_uptakes_own_code_is_not_a_package(module) -> None:
    root = os.path.join("C:", os.sep, "repo")
    for source in ("../../../src/routes/+page.svelte", ".svelte-kit/generated/root.js", "node_modules/"):
        assert module.package_directory(source, root) is None, source


def test_an_or_licence_needs_one_accepted_side_and_an_and_needs_both(module) -> None:
    accepted = module.licence_accepted
    assert accepted("MIT")
    assert accepted("Apache-2.0 OR MIT")
    assert accepted("(MIT OR GPL-2.0)"), "the user may take MIT"
    assert accepted("MIT AND ISC")
    assert not accepted("MIT AND GPL-2.0"), "both apply, and one is not accepted"
    assert not accepted("GPL-2.0")
    assert not accepted("SEE LICENSE IN LICENSE.txt")
    assert not accepted(None), "a package that declares no licence is refused"
    assert not accepted({"type": "MIT"}), "the old object form is refused, not guessed"
    assert not accepted("(MIT OR (Apache-2.0 AND GPL-2.0))"), "nested groups are refused"


def make_package(root: Path, name: str, licence: object, files: dict[str, str]) -> Path:
    package = root / name
    package.mkdir(parents=True)
    (package / "package.json").write_text(
        json.dumps({"name": name, "version": "1.2.3", "license": licence}), encoding="utf-8"
    )
    for file_name, text in files.items():
        (package / file_name).write_text(text, encoding="utf-8")
    return package


def refused(call) -> bool:
    try:
        call()
    except SystemExit:
        return True
    return False


def test_every_licence_file_travels_and_other_files_do_not(module) -> None:
    """A dual-licensed package ships one file per licence (`@tauri-apps/api` has
    `LICENSE_MIT` and `LICENSE_APACHE-2.0`); both must be in the notice."""
    scratch = Path(tempfile.mkdtemp(prefix="notices-test-"))
    try:
        package = make_package(
            scratch,
            "dual",
            "Apache-2.0 OR MIT",
            {
                "LICENSE_MIT": "MIT TEXT",
                "LICENSE_APACHE-2.0": "APACHE TEXT",
                "README.md": "NOT A LICENCE",
                "licensing-notes.js": "NOT A LICENCE EITHER",
            },
        )
        heading, text = module.npm_notice(package)
        assert heading == "dual 1.2.3 (Apache-2.0 OR MIT)", heading
        assert "MIT TEXT" in text and "APACHE TEXT" in text
        assert "NOT A LICENCE" not in text
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def test_a_package_without_a_licence_file_or_with_another_licence_stops_the_build(module) -> None:
    scratch = Path(tempfile.mkdtemp(prefix="notices-test-"))
    try:
        bare = make_package(scratch, "bare", "MIT", {"README.md": "no licence here"})
        assert refused(lambda: module.npm_notice(bare)), "no licence file must refuse"
        copyleft = make_package(scratch, "copyleft", "GPL-2.0", {"LICENSE": "GPL TEXT"})
        assert refused(lambda: module.npm_notice(copyleft)), "an unaccepted licence must refuse"
        fine = make_package(scratch, "fine", "ISC", {"license.md": "ISC TEXT"})
        assert module.npm_notice(fine)[1] == "ISC TEXT", "a lower-case licence.md is found"
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def test_a_notice_file_travels_with_a_package_and_with_a_crate(module) -> None:
    """Apache-2.0 section 4(d): a NOTICE file is separate from the licence and
    must travel too. Round 3 of #130's review: the first version kept only the
    licence files, so a dependency that added a NOTICE would lose it silently."""
    scratch = Path(tempfile.mkdtemp(prefix="notices-test-"))
    try:
        package = make_package(
            scratch, "apache", "Apache-2.0", {"LICENSE": "APACHE TEXT", "NOTICE": "ATTRIBUTION TEXT"}
        )
        assert "ATTRIBUTION TEXT" in module.npm_notice(package)[1]

        with_notice = scratch / "with-notice-1.0.0"
        with_notice.mkdir()
        (with_notice / "NOTICE.txt").write_text("CRATE ATTRIBUTION", encoding="utf-8")
        without = scratch / "plain-2.0.0"
        without.mkdir()
        (without / "LICENSE-MIT").write_text("MIT", encoding="utf-8")
        directories = {("with-notice", "1.0.0"): with_notice, ("plain", "2.0.0"): without}
        block = module.crate_notices({("with-notice", "1.0.0"), ("plain", "2.0.0")}, directories)
        assert "NOTICE of with-notice 1.0.0" in block and "CRATE ATTRIBUTION" in block
        assert "plain" not in block, "a crate with no NOTICE adds nothing"
        assert module.crate_notices({("plain", "2.0.0")}, directories).startswith("None of these")
        # A listed crate with no source folder cannot be checked, so it refuses.
        assert refused(lambda: module.crate_notices({("ghost", "0.1.0")}, directories))
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def test_the_crate_count_reads_every_used_by_block_once(module) -> None:
    text = "\n".join(
        [
            "=" * 80,
            "MIT License (MIT)",
            "",
            "Used by:",
            "  serde 1.0.0",
            "  windows 0.61.3",
            "",
            "  indented licence text that is not a crate",
            "=" * 80,
            "Apache License 2.0 (Apache-2.0)",
            "",
            "Used by:",
            "  serde 1.0.0",
            "",
            "text",
        ]
    )
    assert module.crates_listed(text) == {("serde", "1.0.0"), ("windows", "0.61.3")}


def test_the_installer_carries_both_files(module) -> None:
    """The script writes two files; both must be in the release resources, or
    they are generated and never reach a user."""
    resources = json.loads(RELEASE_CONF.read_text(encoding="utf-8"))["bundle"]["resources"]
    assert resources.get("assets/THIRD-PARTY-NOTICES.txt") == "THIRD-PARTY-NOTICES.txt"
    assert resources.get("assets/LICENSE.txt") == "LICENSE.txt"


def main() -> int:
    module = load_module()
    tests = [value for name, value in globals().items() if name.startswith("test_")]
    failures = 0
    for test in tests:
        try:
            test(module)
            print("ok    " + test.__name__)
        # BaseException, not Exception: a refusal in the code under test raises
        # SystemExit, which would otherwise abort the run with no FAIL line.
        except BaseException:  # noqa: BLE001, B036 - a test runner reports everything
            failures += 1
            print("FAIL  " + test.__name__)
            traceback.print_exc()
    print("")
    print(str(len(tests) - failures) + "/" + str(len(tests)) + " passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
