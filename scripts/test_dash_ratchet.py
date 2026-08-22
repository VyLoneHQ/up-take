#!/usr/bin/env python3
"""Tests for the em-dash ratchet, written because its green was not evidence.

The independent review of the pull request that added `dash-ratchet.py` made a
narrow and correct point: the anti-tamper guard could not fire on that pull
request at all, because `origin/main` carried no baseline to compare against
yet. So the check went green having never run the branch it was defending, and
"CI is green" was being offered as evidence that the fix worked.

These tests are that evidence. Each one builds a throwaway git repository, puts
the script in the state under test, and asserts the exit code and the message.
Every guard **these tests cover** has been confirmed to fail when that guard is
removed; a test that cannot go red is the defect this repository keeps finding
(`UT-F-40`, `UT-F-44`, `UT-F-52`).

**That sentence used to read "every guard here", which is a claim about the
script and not about this file.** A review took it at its word, mutated
`not baseline_file.exists()` out of the script, and watched all seventeen tests
stay green: the guard was real, load-bearing to the module docstring's own
residual argument, and untested. It has a test now (`test_a_missing_baseline_is_refused`),
and the sentence is scoped to what it can actually promise. **A coverage claim is
a claim, and the honest form names the set it covers.**

Run: `python3 scripts/test_dash_ratchet.py`
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import importlib.util

# The file name has a hyphen, so a plain import statement cannot spell it.
_spec = importlib.util.spec_from_file_location(
    "dash_ratchet", Path(__file__).resolve().parent / "dash-ratchet.py"
)
ratchet = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ratchet)

# Written as escapes for the same reason the script does it: this file is `.py`,
# `.py` is in the counted suffix list, and a literal here would be counted by the
# thing it is testing.
EM = chr(0x2014)
EN = chr(0x2013)


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        check=True,
        text=True,
    ).stdout


def run_main(repo: Path, *argv: str, exempt: dict[str, str] | None = None):
    """Calls the script's `main()` inside `repo`, returning (code, out, err).

    Calls `main()` rather than spawning a subprocess so the EXEMPT dict can be
    set for a test, and so the thing under test is the function CI runs.
    """
    old_cwd = Path.cwd()
    old_argv = sys.argv
    old_exempt = dict(ratchet.EXEMPT)
    out, err = io.StringIO(), io.StringIO()
    try:
        os.chdir(repo)
        sys.argv = ["dash-ratchet.py", *argv]
        ratchet.EXEMPT.clear()
        ratchet.EXEMPT.update(exempt or {})
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                code = ratchet.main()
            except SystemExit as exit_:
                # `read_baseline` refuses by raising SystemExit with a message.
                code = 1 if exit_.code else 0
                err.write(str(exit_.code))
        return code, out.getvalue(), err.getvalue()
    finally:
        os.chdir(old_cwd)
        sys.argv = old_argv
        ratchet.EXEMPT.clear()
        ratchet.EXEMPT.update(old_exempt)


class RatchetCase(unittest.TestCase):
    """A repository with one prose file and a `main` to compare against."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self._tmp.name) / "repo"
        self.repo.mkdir()
        git(self.repo.parent, "init", "-q", "-b", "main", str(self.repo))
        git(self.repo, "config", "user.email", "t@example.invalid")
        git(self.repo, "config", "user.name", "test")
        (self.repo / "scripts").mkdir()
        self.write_prose(0)
        self.commit("base")

    def tearDown(self):
        self._tmp.cleanup()

    def write_prose(self, dashes: int, name: str = "prose.md"):
        (self.repo / name).write_text("x" + EM * dashes + "\n", encoding="utf-8")

    def write_baseline(self, total: int, **extra):
        (self.repo / "scripts" / "dash-baseline.json").write_text(
            json.dumps({"total": total, "files": 1, **extra}) + "\n", encoding="utf-8"
        )

    def commit(self, message: str):
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", message)

    # -- the guard the pull request could not exercise -----------------------

    def test_a_ceiling_above_the_reference_is_refused(self):
        """The anti-tamper guard: a hand-raised ceiling is refused."""
        self.write_prose(2)
        self.write_baseline(2)
        self.commit("baseline of 2 on main")
        git(self.repo, "branch", "reference")

        self.write_prose(9)
        self.write_baseline(9)  # raising the bound instead of fixing the prose
        self.commit("raise the ceiling by hand")

        code, _, err = run_main(self.repo, "--against", "reference")
        self.assertEqual(code, 1, "a raised ceiling must be refused")
        self.assertIn("REFUSED", err)
        self.assertIn("ratchet turns one way", err)

    def test_a_ceiling_equal_to_the_reference_passes(self):
        """The guard must not fire on an unchanged bound, or it gates everything."""
        self.write_prose(2)
        self.write_baseline(2)
        self.commit("baseline of 2")
        git(self.repo, "branch", "reference")

        code, _, _ = run_main(self.repo, "--against", "reference")
        self.assertEqual(code, 0)

    def test_a_lowered_ceiling_passes(self):
        self.write_prose(5)
        self.write_baseline(5)
        self.commit("five")
        git(self.repo, "branch", "reference")
        self.write_prose(1)
        self.write_baseline(1)
        self.commit("swept down to one")

        code, out, _ = run_main(self.repo, "--against", "reference")
        self.assertEqual(code, 0, out)

    # -- a tranche is not permanent unless the bound comes down with it -------

    def test_a_sweep_that_does_not_bank_is_refused(self):
        """Below the ceiling is a failure: unbanked ground can be spent again."""
        self.write_prose(5)
        self.write_baseline(5)
        self.commit("five")
        git(self.repo, "branch", "reference")
        self.write_prose(1)  # swept, baseline deliberately left at 5

        code, _, err = run_main(self.repo, "--against", "reference")
        self.assertEqual(code, 1, "an unbanked sweep must be refused")
        self.assertIn("REFUSED", err)
        self.assertIn("--write-baseline", err)

    def test_banked_ground_cannot_be_spent_again(self):
        """The point of banking: once the bound comes down, it binds.

        The reviewer's probe was that an UNBANKED sweep leaves headroom a later
        change spends in a brand-new file while the check says "No regression".
        With the sweep refused until it banks, that state is not reachable, so
        what this pins is the other side: after banking, handing the characters
        back in a new file is a regression and fails.
        """
        self.write_prose(5)
        self.write_baseline(5)
        self.commit("five")
        git(self.repo, "branch", "reference")

        self.write_prose(1)
        self.write_baseline(1)  # banked, as the refusal now forces
        self.commit("swept and banked")

        self.write_prose(4, name="new.md")  # total back to 5, ceiling now 1
        self.commit("spend it again in a new file")

        code, _, err = run_main(self.repo, "--against", "reference")
        self.assertEqual(code, 1, "the banked ground was handed back and passed")
        self.assertIn("FAILED", err)
        self.assertIn("new.md", err)

    def test_a_missing_baseline_is_refused(self):
        """The guard the module's residual argument rests on, and it had no test.

        Without it a change deleting the baseline reaches `main`, after which
        `reference is None` on every later run, the anti-tamper comparison is
        silently off, and any ceiling is accepted. Found by a review that
        mutated the branch out and watched the whole suite stay green.
        """
        self.write_prose(1)
        self.commit("no baseline anywhere")

        code, _, err = run_main(self.repo, "--against", "main")
        self.assertEqual(code, 1, "a missing baseline must be refused, not assumed")
        self.assertIn("REFUSED", err)
        self.assertIn("--write-baseline", err)

    def test_write_baseline_works_without_a_resolvable_reference(self):
        """Banking must not need the ref, because it never reads it (P-5)."""
        self.write_prose(3)
        self.commit("three")

        code, out, err = run_main(
            self.repo, "--write-baseline", "--against", "origin/does-not-exist"
        )
        self.assertEqual(code, 0, err)
        self.assertIn("baseline written", out)

    def test_the_written_baseline_keeps_LF_line_endings(self):
        """`write_text` without `newline=` turns every LF into CRLF on Windows.

        The damage is invisible to git, because `.gitattributes` normalizes it
        back at staging: `git diff` shows only the count line and nothing in the
        commit path can see the whole file was rewritten. UP-TAKE `I-68` is the
        workspace-level record of the hazard, and this is the guard the fix
        shipped without.

        Read as BYTES. Text mode translates the endings back on the way in, so
        the obvious version of this assertion passes against the very defect it
        exists to catch.
        """
        self.write_prose(3)
        self.commit("three")

        code, _, err = run_main(self.repo, "--write-baseline", "--against", "main")
        self.assertEqual(code, 0, err)

        written = (self.repo / "scripts" / "dash-baseline.json").read_bytes()
        self.assertNotIn(b"\r\n", written, "the baseline was written with CRLF")
        self.assertIn(b"\n", written, "the baseline has no line endings at all")

    def test_a_boolean_baseline_is_refused(self):
        """`bool` is an `int`, so a bare isinstance check accepts `true` as 1."""
        (self.repo / "scripts" / "dash-baseline.json").write_text(
            '{"total": true, "files": 1}\n', encoding="utf-8"
        )
        self.write_prose(1)
        self.commit("a boolean ceiling")

        code, _, err = run_main(self.repo, "--against", "main")
        self.assertEqual(code, 1)
        self.assertIn("is not a count", err)

    # -- fail closed on a broken checkout ------------------------------------

    def test_an_unresolvable_reference_is_refused(self):
        """A ref that does not resolve is a failed fetch, not a first run."""
        self.write_prose(1)
        self.write_baseline(1)
        self.commit("one")

        code, _, err = run_main(self.repo, "--against", "origin/does-not-exist")
        self.assertEqual(code, 1, "an unresolvable ref must fail closed")
        self.assertIn("REFUSED", err)
        self.assertIn("does not resolve", err)

    def test_a_reference_without_a_baseline_is_the_first_run(self):
        """The one legitimate case, and it is distinguished from the one above."""
        git(self.repo, "branch", "reference")  # reference predates the baseline
        self.write_prose(1)
        self.write_baseline(1)
        self.commit("introduce the baseline")

        code, _, err = run_main(self.repo, "--against", "reference")
        self.assertEqual(code, 0, err)
        self.assertIn("NOTICE", err)
        self.assertIn("first", err)

    # -- an exemption is not progress ----------------------------------------

    def test_an_exemption_does_not_lower_the_total(self):
        """Adding an EXEMPT entry must not move the number by one."""
        self.write_prose(7)
        self.write_baseline(7)
        self.commit("seven")
        git(self.repo, "branch", "reference")

        without = run_main(self.repo, "--against", "reference", "--list")
        with_exempt = run_main(
            self.repo,
            "--against",
            "reference",
            "--list",
            exempt={"prose.md": "a fixture another test compares against"},
        )
        self.assertEqual(without[0], 0, without[2])
        self.assertEqual(with_exempt[0], 0, with_exempt[2])
        self.assertIn("7 in 1 file(s)", without[1])
        self.assertIn(
            "7 in 1 file(s)",
            with_exempt[1],
            "an exemption changed the total, so it can be banked as ground gained",
        )

    def test_an_exemption_is_not_reported_as_ground_gained(self):
        """The specific defect: exempt, then be invited to bank the difference."""
        self.write_prose(7)
        self.write_baseline(7)
        self.commit("seven")
        git(self.repo, "branch", "reference")

        _, out, _ = run_main(
            self.repo,
            "--against",
            "reference",
            exempt={"prose.md": "a fixture another test compares against"},
        )
        self.assertNotIn("ground you gained", out)
        self.assertIn("floor", out)

    def test_a_blank_exemption_reason_is_refused(self):
        self.write_prose(1)
        self.write_baseline(1)
        self.commit("one")
        code, _, err = run_main(self.repo, "--against", "main", exempt={"prose.md": "   "})
        self.assertEqual(code, 1)
        self.assertIn("no reason", err)

    def test_a_stale_exemption_path_is_refused(self):
        self.write_prose(1)
        self.write_baseline(1)
        self.commit("one")
        code, _, err = run_main(
            self.repo, "--against", "main", exempt={"gone.md": "used to be load-bearing"}
        )
        self.assertEqual(code, 1)
        self.assertIn("not tracked", err)

    # -- the ordinary job ----------------------------------------------------

    def test_a_regression_above_the_ceiling_fails(self):
        self.write_prose(1)
        self.write_baseline(1)
        self.commit("one")
        git(self.repo, "branch", "reference")
        self.write_prose(4)  # four now, ceiling still one

        code, _, err = run_main(self.repo, "--against", "reference")
        self.assertEqual(code, 1)
        self.assertIn("FAILED", err)
        self.assertIn("prose.md", err)

    def test_write_baseline_refuses_to_raise(self):
        self.write_prose(1)
        self.write_baseline(1)
        self.commit("one")
        self.write_prose(5)

        code, _, err = run_main(self.repo, "--write-baseline", "--against", "main")
        self.assertEqual(code, 1)
        self.assertIn("only turns one way", err)

    def test_write_baseline_from_a_subdirectory_counts_the_whole_repo(self):
        """The cwd regression: from `scripts/` the first version counted zero."""
        self.write_prose(6)
        self.commit("six")

        old = Path.cwd()
        try:
            os.chdir(self.repo / "scripts")
            code, out, err = run_main(Path.cwd(), "--write-baseline", "--against", "main")
        finally:
            os.chdir(old)

        self.assertEqual(code, 0, err)
        self.assertIn("6 in 1 file(s)", out)
        written = json.loads(
            (self.repo / "scripts" / "dash-baseline.json").read_text(encoding="utf-8")
        )
        self.assertEqual(written["total"], 6, "a subdirectory run recorded the wrong ceiling")

    def test_an_unreadable_tracked_file_is_refused(self):
        """A file it cannot read is one it cannot vouch for."""
        self.write_prose(1)
        self.write_baseline(1)
        self.commit("one")
        # Tracked, then made invalid UTF-8 in the working tree.
        (self.repo / "prose.md").write_bytes(b"\xff\xfe\x00bad")

        code, _, err = run_main(self.repo, "--against", "main")
        self.assertEqual(code, 1)
        self.assertIn("could not read", err)

    def test_an_en_dash_counts_too(self):
        """Both characters, or the rule is half enforced."""
        (self.repo / "prose.md").write_text("a" + EN + "b\n", encoding="utf-8")
        self.write_baseline(0)
        self.commit("an en-dash")

        code, _, err = run_main(self.repo, "--against", "main")
        self.assertEqual(code, 1, "an en-dash above the ceiling must fail")
        self.assertIn("FAILED", err)


if __name__ == "__main__":
    unittest.main(verbosity=2)
