"""Asserts that a bundle actually carries the runtime, the models and every notice.

Run after `tauri build`. Exits non-zero, loudly, if anything the release config
names is missing from what the bundler produced.

Why this exists
---------------

Round 3 of `I-337`'s independent review found the gap it closes, and the gap was
introduced by the fix for round 2's CI failure rather than by the original work.

The history in three steps, because the shape matters more than the bug:

1. `bundle.resources` lived in `tauri.conf.json`. Every build packaged the
   assets, structurally, because that file is read unconditionally. But
   `tauri-build` validates resource paths at COMPILE time, so every `cargo
   check` on a machine without the gitignored 31 MB staging directory failed.
2. The fix moved the resources into `tauri.release.conf.json`, merged in with
   `--config` only when an installer is built. That fixed the compile problem.
3. **It also made packaging depend on remembering a flag.** Exactly one line in
   `.github/workflows/ci.yml` supplies it. Drop it, typo it, or add a second
   release workflow that forgets it, and `tauri build` exits 0 and produces an
   installer with no runtime, no models and none of the three notices, with
   `cargo test`, `cargo clippy` and the whole CI run still green.

Step 3 is strictly worse than step 1's problem: a compile error is loud and
immediate, while shipping a GPL-3.0 installer that is missing an MIT licence it
is required to carry is silent and reaches users.

So this checks the ARTIFACT rather than the invocation. It does not care how the
build was run, which flags were passed, or which config was merged. It asks what
came out. That is the only form of the check that a future release workflow
nobody has written yet cannot route around.

What it checks
--------------

For every destination named in `tauri.release.conf.json`:

* the file is staged beside the built executable, where `ocr.rs` will look for
  it at run time; and
* **every installer this build produced** carries a directive for it, so it
  travels to a user rather than merely existing in the build tree.

The second check is the load-bearing one. A stale file from an earlier build
satisfies the first on its own, which is precisely the false green this is
supposed to refuse.

**"Every installer" is plural on purpose.** `bundle.targets` is `"all"`, so a
Windows build emits an NSIS setup and an MSI and CI uploads both. Round 4 of
this change's review found the first version of this file reading the NSIS
script alone and reporting success over an MSI that carried nothing.

Usage
-----

    python scripts/verify-bundle.py
    python scripts/verify-bundle.py --target target/release
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

#: The release-only config whose `bundle.resources` map is the authority on what
#: an installer must carry. Read rather than restated, so this cannot drift from
#: the file the bundler is given.
RELEASE_CONFIG = REPO / "src-tauri" / "tauri.release.conf.json"


def expected_destinations(config_path: Path) -> list[str]:
    """The installed paths the release config promises, relative to the exe.

    Raises `SystemExit` rather than returning an empty list if the map is
    missing or empty: a checker that silently finds nothing to check is the
    green that cannot be earned, which is the defect this whole file is about.
    """
    if not config_path.is_file():
        raise SystemExit("cannot find the release config at " + str(config_path))
    config = json.loads(config_path.read_text(encoding="utf-8"))
    resources = config.get("bundle", {}).get("resources")
    if not resources:
        raise SystemExit(
            str(config_path)
            + " names no bundle.resources.\nThere is nothing for an installer to"
            " carry, which means the runtime, the models and every notice would"
            " ship missing. Refusing to report success."
        )
    return sorted(str(destination) for destination in resources.values())


def installer_manifests(target: Path) -> tuple[list[tuple[str, Path]], list[str]]:
    """Every installer script this build produced, and any reason to refuse.

    **Both of them, and that is the whole of round 4's finding.**
    `tauri.conf.json` sets `bundle.targets` to `"all"`, so a Windows build emits
    an NSIS setup **and** an MSI, and `ci.yml` uploads both as release
    artifacts. The first version of this file read the NSIS script only and
    reported "the bundle carries every resource" while the MSI beside it carried
    none: drilled by the reviewer, who stripped the resource components out of a
    real generated `main.wxs` and watched this script exit 0.

    A checker that says "the bundle" while inspecting one of two shipped
    bundles is the false green this file exists to refuse, one level up.

    Returns the manifests found, plus refusal reasons for anything ambiguous. A
    duplicate architecture directory is a refusal rather than a guess: the
    previous version took `sorted(...)[0]`, which silently prefers whichever
    name sorts first rather than whichever build is current.
    """
    found: list[tuple[str, Path]] = []
    problems: list[str] = []
    for label, pattern in (("NSIS", "nsis/*/installer.nsi"), ("MSI", "wix/*/main.wxs")):
        matches = sorted(target.glob(pattern))
        if len(matches) > 1:
            problems.append(
                label
                + " has more than one generated script ("
                + ", ".join(str(m) for m in matches)
                + "), so there is no single artifact to check. Clean the target"
                " directory and rebuild rather than letting this guess."
            )
            continue
        if matches:
            found.append((label, matches[0]))
    return found, problems


def embeds(label: str, manifest: str, destination: str) -> bool:
    r"""Whether `manifest` carries a directive that installs `destination`.

    The two generators say it differently and neither is a substring of the
    other, so this is a per-format question rather than one shared search:

    * NSIS writes the INSTALLED path, as `/oname=models\ppocr_keys_v1.txt`.
    * WiX writes the SOURCE path, as
      `<File ... Source="...\src-tauri\assets\models\ppocr_keys_v1.txt" />`.

    Matching the source path for WiX is what makes the check meaningful there:
    the staging layout under `src-tauri/assets` mirrors the installed layout, so
    a resource that is present under one name and absent under the other cannot
    satisfy both this and the staging check.
    """
    windows = destination.replace("/", "\\")
    if label == "NSIS":
        return "/oname=" + windows in manifest
    return "assets\\" + windows in manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        type=Path,
        default=REPO / "target" / "release",
        help="the build output directory (default: target/release)",
    )
    arguments = parser.parse_args()
    target: Path = arguments.target

    destinations = expected_destinations(RELEASE_CONFIG)
    print("Checking " + str(len(destinations)) + " bundled resource(s) in " + str(target))

    if not target.is_dir():
        raise SystemExit(
            str(target) + " does not exist. Build first, then run this."
        )

    problems: list[str] = []

    # 1. Staged beside the executable, which is where `ocr.rs` resolves them.
    for destination in destinations:
        staged = target / destination
        if not staged.is_file():
            problems.append(
                destination
                + " is NOT staged beside the executable (looked for "
                + str(staged)
                + ")"
            )

    # 2. Actually referenced by the installer the bundler generated. This is the
    #    check that catches a build run without the release config: the staging
    #    directory can be populated by a previous build while the installer that
    #    was just produced carries nothing.
    manifests, ambiguities = installer_manifests(target)
    problems.extend(ambiguities)
    if not manifests:
        problems.append(
            "no generated installer script found under "
            + str(target)
            + " (looked for nsis/*/installer.nsi and wix/*/main.wxs); either no"
            " bundle was produced or the target is wrong"
        )
    for label, script in manifests:
        directives = script.read_text(encoding="utf-8", errors="replace")
        for destination in destinations:
            if not embeds(label, directives, destination):
                problems.append(
                    destination
                    + " is NOT in the "
                    + label
                    + " installer ("
                    + str(script)
                    + " has no directive for it)"
                )

    if problems:
        print("")
        print("REFUSED. The bundle does not carry what the release config promises:")
        for problem in problems:
            print("  - " + problem)
        print("")
        print(
            "The usual cause is a build run without"
            " `--config src-tauri/tauri.release.conf.json`, which exits 0 and"
            " produces an installer with no runtime, no models and no notices."
        )
        print(
            "Shipping that installer would violate ONNX Runtime's MIT notice"
            " requirement and PaddleOCR's Apache-2.0 one, and `cargo deny` cannot"
            " see either, because it walks the crate graph."
        )
        return 1

    for destination in destinations:
        print("  ok  " + destination)
    print("")
    print(
        "Every resource the release config names is staged and carried by: "
        + ", ".join(label for label, _ in manifests)
        + "."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
