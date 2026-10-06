#!/usr/bin/env python3
"""Writes `THIRD-PARTY-NOTICES.txt` and `LICENSE.txt` for the installer (I-443).

# Why this exists

MIT, Apache-2.0, BSD and the other permissive licences UP-TAKE's dependencies
use require their copyright and permission notices to travel with every copy,
binaries included. Until this, the installer carried the notices for ONNX
Runtime and the OCR models only, nothing for the 283 crates compiled into
`up-take.exe` or the JavaScript compiled into its interface, and not the GPL's
own text either. No binary had been published, so nothing was breached, and the
first public one, free or paid, would have been.

# What it writes, and from what

* `THIRD-PARTY-NOTICES.txt`, in two parts.
  * **Rust**: `cargo about generate` over `Cargo.lock`, configured by
    `about.toml` (the Windows target only, as `deny.toml`'s graph is) and laid
    out by `about.hbs`.
  * **JavaScript**: the packages the front end's BUILD actually contains, read
    from a sourcemap build rather than from `package.json`. Measured on
    2026-10-06: eight packages are compiled in, and only one of them,
    `@tauri-apps/api`, is a runtime dependency in `package.json`; Svelte's and
    SvelteKit's runtimes arrive as dev dependencies and ship all the same. A
    list taken from the manifest would have missed seven of eight.
* `LICENSE.txt`, the repository's own `LICENSE`, so the GPL's text ships with
  the program it covers.

**Generated at build time, never by hand**, so a Dependabot bump cannot leave
the list stale: CI runs this before `tauri build`, and `verify-bundle.py` checks
both files reached the installer.

**It refuses rather than writes a partial file**: a JavaScript package with no
licence file, or one whose licence is not on the accepted list, stops the build.
`cargo about` refuses the same way for a crate, through `about.toml`'s list.

# Usage

    python3 scripts/write-third-party-notices.py --out src-tauri/assets

Needs `cargo-about` on the PATH and the front end's dependencies installed
(`pnpm install`).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: The licences a JavaScript package compiled into the interface may carry:
#: the permissive half of `deny.toml`'s allow list, which is the gate on what
#: may be compiled in at all.
ACCEPTED_NPM_LICENCES = {
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "0BSD",
    "CC0-1.0",
    "Unlicense",
}

#: Where the front end's build leaves its sourcemaps: the static site, and
#: SvelteKit's intermediate output it is assembled from.
MAP_DIRECTORIES = ("build", ".svelte-kit/output")

SEPARATOR = "=" * 80

HEADER = """UP-TAKE: third-party notices

UP-TAKE is free software under the GNU General Public License, version 3. Its
text is in LICENSE.txt, beside this file.

UP-TAKE is built with software written by others, listed below with the licence
each is distributed under. ONNX Runtime and the OCR models carry their own
notices beside this file: LICENSE-onnxruntime.txt,
ThirdPartyNotices-onnxruntime.txt and models/NOTICE-models.txt.

This list was generated when this copy of UP-TAKE was built, from the exact
versions it was built with.
"""


def package_directory(source: str, root: str) -> str | None:
    """The `node_modules` package directory a sourcemap source belongs to, under
    `root`; `None` for UP-TAKE's own code.

    **Anchored on the text from the FIRST `node_modules/` onward, not on the map's
    own folder.** SvelteKit's maps name their sources relative to a different
    folder from the one the map is in, and resolving them from the map's folder
    climbed out of the repository (measured on the first run). Every package is
    under the repository's own `node_modules`, so the path from there is the one
    fact that does not depend on where a map was written.

    The package is the part after the LAST `node_modules/`, which is how pnpm
    nests the real package under `.pnpm/<name>@<version>/node_modules/`. Scoped
    packages (`@scope/name`) keep both parts. Text and `os.path` only, never
    `Path.resolve`, which returns on-disk casing on Windows and would make one
    package look like two.
    """
    path = source.replace("\\", "/")
    first = path.find("node_modules/")
    if first < 0:
        return None
    path = path[first:]
    marker = "node_modules/"
    last = path.rfind(marker)
    rest = path[last + len(marker):].split("/")
    take = 2 if rest[0].startswith("@") else 1
    if len(rest) <= take:
        return None
    relative = path[: last + len(marker)] + "/".join(rest[:take])
    return os.path.normpath(os.path.join(root, relative))


def bundled_packages(root: Path) -> list[str]:
    """Every package directory the front end's build compiled in, sorted."""
    found: set[str] = set()
    maps = 0
    for directory in MAP_DIRECTORIES:
        for map_path in (root / directory).rglob("*.map"):
            maps += 1
            with map_path.open(encoding="utf-8") as handle:
                sources = json.load(handle).get("sources", [])
            for source in sources:
                package = package_directory(source, str(root))
                if package is not None:
                    found.add(package)
    if maps == 0:
        raise SystemExit(
            "no sourcemaps under " + " or ".join(MAP_DIRECTORIES) + ".\nThe front end was not"
            " built with --sourcemap, so this cannot say which packages it contains."
        )
    if not found:
        raise SystemExit(
            "the front end's sourcemaps name no package under node_modules.\nThat"
            " cannot be true of a SvelteKit build; refusing to write a notice that"
            " lists no JavaScript at all."
        )
    return sorted(found)


def licence_files(package: Path) -> list[Path]:
    """Every licence file the package ships: any file whose name begins with
    LICENSE, LICENCE or COPYING, in any case.

    All of them, not the first. A dual-licensed package ships one per licence
    (`@tauri-apps/api` has `LICENSE_MIT` and `LICENSE_APACHE-2.0`), and the
    user may take either, so both texts travel.
    """
    return sorted(
        entry
        for entry in package.iterdir()
        if entry.is_file() and entry.name.lower().startswith(("license", "licence", "copying"))
    )


def licence_accepted(expression: object) -> bool:
    """Whether a package's SPDX licence expression is acceptable.

    `A OR B` is accepted when either side is, because the user may take either
    (`@tauri-apps/api` is `Apache-2.0 OR MIT`); `A AND B` only when both are.
    The same reading `cargo deny` gives a crate. Parentheses around the whole
    expression are dropped; nested groups are not parsed, and an expression
    using them is refused rather than guessed at.
    """
    if not isinstance(expression, str):
        return False
    text = expression.strip()
    if text.startswith("(") and text.endswith(")"):
        text = text[1:-1].strip()
    if "(" in text or ")" in text:
        return False
    return any(
        all(part.strip() in ACCEPTED_NPM_LICENCES for part in alternative.split(" AND "))
        for alternative in text.split(" OR ")
    )


def npm_notice(package: Path) -> tuple[str, str]:
    """`(heading, text)` for one JavaScript package. Refuses a package with no
    licence file or a licence outside the accepted list."""
    manifest = json.loads((package / "package.json").read_text(encoding="utf-8"))
    name, version = manifest.get("name", package.name), manifest.get("version", "?")
    licence = manifest.get("license")
    if not licence_accepted(licence):
        raise SystemExit(
            name + " " + version + " declares licence " + repr(licence) + ", which is not"
            " on the accepted list.\nAdd it to ACCEPTED_NPM_LICENCES only if deny.toml"
            " would accept it for a crate."
        )
    found = licence_files(package)
    if not found:
        raise SystemExit(
            name + " " + version + " ships no licence file in " + str(package) + ".\nIts"
            " licence requires its notice with every copy, so the build stops here"
            " rather than ship without it."
        )
    text = "\n\n".join(
        path.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n").strip()
        for path in found
    )
    return name + " " + version + " (" + licence + ")", text


def javascript_part(packages: list[str]) -> str:
    """The JavaScript half of the file."""
    notices = sorted({npm_notice(Path(package)) for package in packages})
    blocks = [
        "PART 2: JavaScript compiled into UP-TAKE's interface ("
        + str(len(notices)) + " packages)",
    ]
    for heading, text in notices:
        blocks.append(SEPARATOR + "\n" + heading + "\n\n" + text)
    return "\n\n".join(blocks) + "\n"


def rust_part(root: Path) -> str:
    """The Rust half of the file, from `cargo about`.

    Written to a file with `-o` rather than read from stdout: on Windows
    `cargo about` refuses a redirected stdout outright, because PowerShell
    re-encodes it.
    """
    with tempfile.TemporaryDirectory(prefix="cargo-about-") as scratch:
        output = Path(scratch) / "rust.txt"
        result = subprocess.run(
            ["cargo", "about", "generate", "--locked", "-o", str(output), "about.hbs"],
            cwd=root,
            check=False,
        )
        if result.returncode != 0:
            raise SystemExit("cargo about generate failed (exit " + str(result.returncode) + ")")
        # Line endings normalised: the template's and the crates' own licence
        # files can each arrive with CRLF on Windows, and one file should not
        # mix the two.
        text = output.read_text(encoding="utf-8").replace("\r\n", "\n").strip()
    crates = crates_listed(text)
    if not crates:
        raise SystemExit("cargo about generated no crates; refusing to write an empty list")
    return (
        "PART 1: Rust crates compiled into up-take.exe ("
        + str(len(crates))
        + " crates)\n\n"
        + text
        + "\n"
    )


def crates_listed(text: str) -> set[tuple[str, str]]:
    """Every `(name, version)` under a `Used by:` line of `about.hbs`'s layout.

    A crate under two licences appears twice and counts once.
    """
    listed: set[tuple[str, str]] = set()
    in_used_by = False
    for line in text.splitlines():
        if line == "Used by:":
            in_used_by = True
        elif in_used_by and not line.strip():
            in_used_by = False
        elif in_used_by:
            name, version = line.split()
            listed.add((name, version))
    return listed


def build_front_end(root: Path) -> None:
    """Builds the front end with sourcemaps, so `bundled_packages` can read them."""
    pnpm = shutil.which("pnpm")
    if pnpm is None:
        raise SystemExit("pnpm is not on the PATH")
    subprocess.run([pnpm, "exec", "vite", "build", "--sourcemap"], cwd=root, check=True)


def remove_maps(root: Path) -> None:
    """Deletes the sourcemaps this build added, so none can reach an installer
    whatever builds next."""
    for directory in MAP_DIRECTORIES:
        for map_path in (root / directory).rglob("*.map"):
            map_path.unlink()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--out", type=Path, required=True, help="where to write both files")
    arguments = parser.parse_args()

    rust = rust_part(ROOT)
    build_front_end(ROOT)
    try:
        javascript = javascript_part(bundled_packages(ROOT))
    finally:
        remove_maps(ROOT)

    arguments.out.mkdir(parents=True, exist_ok=True)
    notices = HEADER + "\n\n" + rust + "\n\n" + javascript
    (arguments.out / "THIRD-PARTY-NOTICES.txt").write_text(notices, encoding="utf-8", newline="\n")
    shutil.copyfile(ROOT / "LICENSE", arguments.out / "LICENSE.txt")
    print("wrote " + str(arguments.out / "THIRD-PARTY-NOTICES.txt"))
    print("wrote " + str(arguments.out / "LICENSE.txt"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
