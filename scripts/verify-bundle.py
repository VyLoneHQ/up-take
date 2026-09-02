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
* the generated NSIS installer script carries a `File` directive for it, so it
  travels into the installer rather than merely existing in the build tree.

The second check is the load-bearing one. A stale file from an earlier build
satisfies the first on its own, which is precisely the false green this is
supposed to refuse.

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


def nsis_script(target: Path) -> Path | None:
    """The generated NSIS installer script, if this build produced one."""
    candidates = sorted(target.glob("nsis/*/installer.nsi"))
    return candidates[0] if candidates else None


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
    script = nsis_script(target)
    if script is None:
        problems.append(
            "no generated NSIS installer script found under "
            + str(target / "nsis")
            + "; either no bundle was produced or the target is wrong"
        )
    else:
        directives = script.read_text(encoding="utf-8", errors="replace")
        for destination in destinations:
            # The generator writes Windows separators in `/oname=`.
            oname = "/oname=" + destination.replace("/", "\\")
            if oname not in directives:
                problems.append(
                    destination
                    + " is NOT in the installer ("
                    + str(script)
                    + " has no File directive for it)"
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
    print("The bundle carries every resource the release config names.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
