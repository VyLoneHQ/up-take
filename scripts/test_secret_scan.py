#!/usr/bin/env python3
"""Tests for the secret scan: each one builds a throwaway git repository.

A secret check that has never been seen to go red is not evidence of anything,
so most of these plant a secret and assert the refusal. Each repository gets
this repository's real `.secrets.baseline` settings (the same detectors and the
same lockfile exclusion) with its findings emptied, so the tests exercise the
configuration CI runs and not a default.

The planted values are made up, never were credentials, and carry
`pragma: allowlist secret` so this file passes the scan it tests.

Run: `python3 scripts/test_secret_scan.py` (needs detect-secrets 1.5.0 and
unidiff 0.7.5, as CI installs them).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "secret-scan.py"
REAL_BASELINE = HERE.parent / ".secrets.baseline"
MAIN_REF = "refs/remotes/origin/main"

KEYWORD_LINE = 'api_key = "Zq3x9KfP0wLm7Tn2Rv8sJ4dH"  # pragma: allowlist secret\n'
KEYWORD_VALUE = "Zq3x9KfP0wLm7Tn2Rv8sJ4dH"  # pragma: allowlist secret
# The same line without its pragma, which is what a real leak looks like.
LEAK = KEYWORD_LINE.split("  #")[0] + "\n"
INTEGRITY = (
    "  '@asamuzakjp/css-color@7.1.2':\n"
    "    resolution: {integrity: sha512-99DHAnXDB5z6EEK+9GMpVI7Mw4oxj97dY5bpOzMnjADQWxI8rN6TvTduuFLUhUMlS7/CfVZ06tcZsus6cltnNw==}\n"  # pragma: allowlist secret
)
CREDENTIAL_URL = "    resolution: {tarball: https://builder:Pa55w0rdQx7Lm@registry.example.com/a.tgz}\n"  # pragma: allowlist secret


class Repo:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "test")
        self.git("config", "commit.gpgsign", "false")
        settings = json.loads(REAL_BASELINE.read_text(encoding="utf-8"))
        settings["results"] = {}
        self.write(".secrets.baseline", json.dumps(settings, indent=2) + "\n")
        self.write("README.md", "A repository with nothing in it.\n")
        self.commit("start")

    def git(self, *args: str) -> str:
        done = subprocess.run(
            ["git", *args], cwd=self.root, capture_output=True, text=True, check=True
        )
        return done.stdout.strip()

    def write(self, name: str, text: str) -> None:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8", newline="\n")

    def commit(self, message: str) -> str:
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def scan(self, *args: str, env: dict[str, str] | None = None) -> tuple[int, str]:
        done = subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=self.root,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
            env=None if env is None else {**os.environ, **env},
        )
        return done.returncode, done.stdout + done.stderr


class SecretScan(unittest.TestCase):
    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_a_clean_tree_passes(self) -> None:
        code, out = self.repo.scan()
        self.assertEqual(code, 0, out)
        self.assertIn("nothing new", out)

    def test_a_secret_in_the_tree_is_refused_and_its_value_is_not_printed(self) -> None:
        self.repo.write("src/config.py", "# settings\n" + LEAK)
        self.repo.commit("add a key")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("src/config.py:2", out)
        self.assertIn("Secret Keyword", out)
        self.assertNotIn(KEYWORD_VALUE, out)

    def test_a_staged_secret_is_read_before_it_is_committed(self) -> None:
        self.repo.write("config.py", LEAK)
        self.repo.git("add", "config.py")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)

    def test_the_pragma_passes_a_line(self) -> None:
        self.repo.write("config.py", KEYWORD_LINE)
        self.repo.commit("a key that is not one")
        base = self.repo.git("rev-list", "--max-parents=0", "HEAD")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 0, out)

    def test_a_listed_finding_passes_after_its_line_moves_and_keeps_its_judgement(self) -> None:
        # In a folder on purpose: detect-secrets loads `src/config.py` as
        # `src\config.py` on Windows, and a name with no slash cannot show it.
        name = "src/config.py"
        self.repo.write(name, LEAK)
        self.repo.commit("add a key")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        written = json.loads((self.repo.root / ".secrets.baseline").read_text(encoding="utf-8"))
        written["results"][name][0]["is_secret"] = False
        self.repo.write(".secrets.baseline", json.dumps(written, indent=2) + "\n")
        self.repo.commit("judged")

        self.repo.write(name, "# moved down\n\n" + LEAK)
        self.repo.commit("move it")
        code, out = self.repo.scan()
        self.assertEqual(code, 0, out)

        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        rewritten = json.loads((self.repo.root / ".secrets.baseline").read_text(encoding="utf-8"))
        entry = rewritten["results"][name][0]
        self.assertEqual(entry["line_number"], 3)
        self.assertIs(entry["is_secret"], False, "the rewrite kept the judgement")

    def test_a_secret_added_and_removed_inside_the_range_is_refused(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("config.py", LEAK)
        added = self.repo.commit("add a key")
        self.repo.write("config.py", "# gone\n")
        self.repo.commit("remove it")

        code, out = self.repo.scan()
        self.assertEqual(code, 0, "the tree is clean: " + out)

        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} config.py:1", out)
        self.assertNotIn(KEYWORD_VALUE, out)

    def test_a_finding_the_baseline_lists_passes_in_the_range_too(self) -> None:
        # A branch that adds a false positive and lists it in the same pull
        # request: the commit that added it is in the range, and passes.
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("src/pins.py", LEAK)
        self.repo.commit("a value that is not a secret")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        self.repo.commit("listed")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 0, out)

    def test_a_secret_in_a_yaml_comment_is_refused_in_the_tree(self) -> None:
        # detect-secrets parses YAML and never reads its comments, so the
        # tree is also read line by line as written.
        self.repo.write("ci.yml", "jobs:\n  a:\n    # " + LEAK + "    runs-on: ubuntu-latest\n")
        self.repo.commit("a key in a comment")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("ci.yml:3", out)

    def test_a_yaml_value_only_the_parsed_read_joins_is_refused_in_the_tree(self) -> None:
        # The other half of reading the tree both ways: a block scalar puts
        # the key and the value on different lines.
        self.repo.write("deploy.yml", "deploy:\n  api_key: >-\n    " + KEYWORD_VALUE + "\n")
        self.repo.commit("a key in a block scalar")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("deploy.yml", out)

    def test_a_finding_only_the_written_line_shows_can_be_listed_for_the_range(self) -> None:
        # The case found on this check's own pull request: a YAML comment the
        # keyword pattern matches. Listed from the tree, it passes in the
        # commit that added it.
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write(".github/ci.yml", "jobs:\n  # Secrets: detect-secrets reads this\n  a: 1\n")
        self.repo.commit("a comment that is not a secret")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, "the comment is a finding: " + out)
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        self.repo.commit("listed")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 0, out)

    def test_a_secret_in_a_file_a_later_commit_deletes_is_refused(self) -> None:
        # The case the range exists for, and the one the first version passed:
        # detect-secrets skips a name that is not a file on disk, and the file
        # is gone by the head. Found by the red drill on GitHub.
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("src/leak.py", LEAK)
        added = self.repo.commit("add a key")
        self.repo.git("rm", "-q", "src/leak.py")
        self.repo.commit("delete the file")
        self.assertEqual(self.repo.scan()[0], 0, "the tree is clean")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} src/leak.py:1", out)

    def test_a_judged_finding_outlives_its_line_and_an_unjudged_one_does_not(self) -> None:
        # The commit that added a judged line is still read after a later one
        # removes it, so the judgement has to stay, or the branch that removed
        # the line turns red on its own history.
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("src/pins.py", LEAK)
        self.repo.write("src/other.py", 'password = "Pq8Lm2Xv7Rt4Ws9Kd3Nb"\n')  # pragma: allowlist secret
        self.repo.commit("two values")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        written = json.loads((self.repo.root / ".secrets.baseline").read_text(encoding="utf-8"))
        for entry in written["results"]["src/pins.py"]:
            entry["is_secret"] = False
        self.repo.write(".secrets.baseline", json.dumps(written, indent=2) + "\n")
        self.repo.commit("pins judged, other not")
        self.repo.write("src/pins.py", "# gone\n")
        self.repo.write("src/other.py", "# gone\n")
        self.repo.commit("both lines removed")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        rewritten = json.loads((self.repo.root / ".secrets.baseline").read_text(encoding="utf-8"))["results"]
        self.assertIn("src/pins.py", rewritten, "judged: kept")
        self.assertNotIn("src/other.py", rewritten, "never judged: dropped")
        self.repo.commit("rewritten")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertNotIn("src/pins.py", out)
        self.assertIn("src/other.py:1", out)

    def test_a_commit_before_the_range_is_not_read(self) -> None:
        self.repo.write("config.py", LEAK)
        self.repo.commit("add a key")
        self.repo.write("config.py", "# gone\n")
        base = self.repo.commit("remove it")
        self.repo.write("notes.md", "nothing here\n")
        self.repo.commit("later")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 0, out)

    def test_a_clean_merge_adds_nothing_and_its_parents_are_read(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.git("checkout", "-q", "-b", "side")
        self.repo.write("side.py", LEAK)
        added = self.repo.commit("a key on the side")
        self.repo.git("checkout", "-q", "main")
        self.repo.write("main.md", "main moves on\n")
        self.repo.commit("main")
        self.repo.git("merge", "-q", "--no-edit", "side")
        merge = self.repo.git("rev-parse", "HEAD")
        self.repo.write("side.py", "# gone\n")
        self.repo.commit("remove it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        # Two detectors may each name the one line; both name the commit
        # that added it, and the merge that carried it is not read again.
        named = [line for line in out.splitlines() if "side.py:1" in line]
        self.assertTrue(named, out)
        self.assertTrue(all(line.strip().startswith(added[:12]) for line in named), out)
        self.assertNotIn(merge[:12], out)

    def test_a_lockfile_hash_passes_in_the_tree_and_in_a_commit(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("pnpm-lock.yaml", "lockfileVersion: '9.0'\n\npackages:\n\n" + INTEGRITY)
        self.repo.commit("a lockfile")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 0, out)

    def test_a_lockfile_line_carrying_a_credential_is_refused_in_both(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        lockfile = "lockfileVersion: '9.0'\n\npackages:\n\n  'a@1.0.0':\n" + CREDENTIAL_URL.split("  #")[0]
        self.repo.write("pnpm-lock.yaml", lockfile + "\n")
        added = self.repo.commit("a lockfile with a password in a url")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn("Basic Auth Credentials", out)
        self.assertIn(f"{added[:12]} pnpm-lock.yaml", out)
        self.assertIn("Secrets in the tracked files", out)

    def test_a_utf8_file_with_non_ascii_text_is_read(self) -> None:
        # On Windows detect-secrets opens files as cp1252 and skips one it
        # cannot decode as binary. "Á" is C3 81 in UTF-8, and cp1252 has no
        # character at 81. Most non-ASCII text decodes as cp1252 without an
        # error, which is why the test needs this letter.
        #
        # What this does NOT pin: the script's UTF-8 re-run. The tree is also
        # read line by line through git, which hands over UTF-8 whatever the
        # locale, so this file is refused even without the re-run (a drill
        # showed it). The re-run keeps the parsed read on Windows equal to
        # Linux's, so a baseline written on Windows lists what CI finds; no
        # test here can tell the two machines apart.
        self.repo.write("notes.py", "# Ángel\n" + LEAK)
        self.repo.commit("non-ascii")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("notes.py:2", out)

    def test_the_baseline_itself_is_not_read_as_a_secret(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("config.py", LEAK)
        self.repo.commit("add a key")
        self.repo.scan("--write-baseline")
        self.repo.write("config.py", KEYWORD_LINE)
        self.repo.commit("judged, and the baseline with it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertNotIn(".secrets.baseline", out)

    # The local hooks (.pre-commit-config.yaml): --staged before a commit,
    # --unpushed before a push.

    def test_staged_refuses_what_the_commit_would_add(self) -> None:
        self.repo.write("src/config.py", LEAK)
        self.repo.git("add", "src/config.py")
        code, out = self.repo.scan("--staged")
        self.assertEqual(code, 1, out)
        self.assertIn("src/config.py:1", out)
        self.assertIn("Nothing is public yet", out)
        self.assertNotIn(KEYWORD_VALUE, out)

    def test_staged_reads_a_yaml_comment(self) -> None:
        # Real content around it: a YAML file that parses to nothing is read
        # as plain lines by detect-secrets anyway, and would not tell the two
        # reads apart. This one only the line-by-line read can see.
        self.repo.write("ci.yml", "jobs:\n  a:\n    # " + LEAK + "    runs-on: ubuntu-latest\n")
        self.repo.git("add", "ci.yml")
        code, out = self.repo.scan("--staged")
        self.assertEqual(code, 1, out)
        self.assertIn("ci.yml:3", out)

    def test_staged_reads_a_yaml_value_as_parsed(self) -> None:
        # A block scalar puts the key and the value on different lines, so no
        # single line as written carries both. Only the parsed read joins them.
        value = KEYWORD_VALUE
        self.repo.write("deploy.yml", "deploy:\n  api_key: >-\n    " + value + "\n")
        self.repo.git("add", "deploy.yml")
        code, out = self.repo.scan("--staged")
        self.assertEqual(code, 1, out)
        self.assertIn("deploy.yml", out)

    def test_staged_passes_a_clean_change_and_an_empty_index(self) -> None:
        self.assertEqual(self.repo.scan("--staged")[0], 0)
        self.repo.write("notes.md", "nothing here\n")
        self.repo.git("add", "notes.md")
        code, out = self.repo.scan("--staged")
        self.assertEqual(code, 0, out)
        self.assertIn("1 staged files", out)

    def test_staged_passes_a_listed_finding(self) -> None:
        self.repo.write("src/pins.py", LEAK)
        self.repo.commit("a value that is not a secret")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        self.repo.commit("listed")
        self.repo.write("src/pins.py", "# moved\n" + LEAK)
        self.repo.git("add", "src/pins.py")
        code, out = self.repo.scan("--staged")
        self.assertEqual(code, 0, out)

    def _remote(self) -> None:
        remote = self.repo.root.parent / (self.repo.root.name + "-remote.git")
        subprocess.run(["git", "init", "-q", "--bare", str(remote)], check=True)
        self.addCleanup(lambda: __import__("shutil").rmtree(remote, ignore_errors=True))
        self.repo.git("remote", "add", "origin", str(remote))
        self.repo.git("push", "-q", "origin", "main")

    def test_unpushed_reads_each_commit_no_remote_has(self) -> None:
        self._remote()
        self.repo.write("src/leak.py", LEAK)
        added = self.repo.commit("add a key")
        self.repo.git("rm", "-q", "src/leak.py")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--unpushed")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} src/leak.py:1", out)
        self.assertIn("2 commits no remote has", out)

    def test_unpushed_does_not_read_what_a_remote_already_has(self) -> None:
        self._remote()
        self.repo.write("src/leak.py", LEAK)
        self.repo.commit("add a key")
        self.repo.git("push", "-q", "origin", "main")
        self.repo.write("notes.md", "later\n")
        self.repo.commit("later")
        code, out = self.repo.scan("--unpushed")
        self.assertEqual(code, 0, out)
        self.assertIn("1 commits no remote has", out)

    def test_unpushed_reads_every_local_branch_not_only_head(self) -> None:
        # `git push origin aaa bbb`: pre-commit names only the first branch
        # to its hook, so a secret on the second passed (review of #140,
        # finding 2). Every local branch no remote has is read.
        self._remote()
        self.repo.git("checkout", "-q", "-b", "bbb")
        self.repo.write("src/leak.py", LEAK)
        self.repo.commit("a key on bbb")
        self.repo.git("rm", "-q", "src/leak.py")
        self.repo.commit("delete it")
        self.repo.git("checkout", "-q", "main")
        self.repo.write("notes.md", "clean\n")
        main_head = self.repo.commit("aaa is clean")
        code, out = self.repo.scan("--unpushed", env={"PRE_COMMIT_TO_REF": main_head})
        self.assertEqual(code, 1, out)
        self.assertIn("src/leak.py:1", out)

    def test_unpushed_reads_the_commit_pre_commit_names_on_no_branch(self) -> None:
        self._remote()
        self.repo.write("src/leak.py", LEAK)
        loose = self.repo.commit("a key")
        self.repo.git("reset", "-q", "--hard", "HEAD~1")
        self.assertEqual(self.repo.scan("--unpushed")[0], 0, "no branch holds it")
        code, out = self.repo.scan("--unpushed", env={"PRE_COMMIT_TO_REF": loose})
        self.assertEqual(code, 1, out)

    # Merge commits (review of #140, finding 1).

    def test_a_secret_written_into_a_merge_is_refused(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.git("checkout", "-q", "-b", "side")
        self.repo.write("side.md", "side\n")
        self.repo.commit("side")
        self.repo.git("checkout", "-q", "main")
        self.repo.write("main.md", "main\n")
        self.repo.commit("main")
        self.repo.git("merge", "-q", "--no-commit", "side")
        self.repo.write("src/evil.py", LEAK)
        self.repo.git("add", "src/evil.py")
        self.repo.git("commit", "-q", "--no-edit")
        merge = self.repo.git("rev-parse", "HEAD")
        self.repo.git("rm", "-q", "src/evil.py")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{merge[:12]} src/evil.py:1", out)

    def test_a_secret_in_a_conflict_resolution_is_refused(self) -> None:
        self.repo.write("c.py", "x = 1\n")
        base = self.repo.commit("base")
        self.repo.git("checkout", "-q", "-b", "side")
        self.repo.write("c.py", "x = 2\n")
        self.repo.commit("side")
        self.repo.git("checkout", "-q", "main")
        self.repo.write("c.py", "x = 3\n")
        self.repo.commit("main")
        subprocess.run(["git", "merge", "-q", "side"], cwd=self.repo.root, capture_output=True)
        self.repo.write("c.py", "x = 4\n" + LEAK)
        self.repo.git("add", "c.py")
        self.repo.git("commit", "-q", "--no-edit")
        merge = self.repo.git("rev-parse", "HEAD")
        self.repo.write("c.py", "x = 4\n")
        self.repo.commit("clean it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{merge[:12]} c.py:2", out)

    def test_a_secret_written_into_an_octopus_merge_is_refused(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        for branch in ("one", "two"):
            self.repo.git("checkout", "-q", "-b", branch, base)
            self.repo.write(f"{branch}.md", branch + "\n")
            self.repo.commit(branch)
        self.repo.git("checkout", "-q", "main")
        # --no-ff, or git fast-forwards to `one` and makes a merge of two parents.
        self.repo.git("merge", "-q", "--no-ff", "--no-commit", "one", "two")
        # Into a file one side changed. git's plain diff of a merge is the
        # combined form (`@@@`), in which detect-secrets finds no added line.
        self.repo.write("one.md", "one\n" + LEAK)
        self.repo.git("add", "one.md")
        self.repo.git("commit", "-q", "-m", "octopus")
        merge = self.repo.git("rev-parse", "HEAD")
        parents = self.repo.git("rev-list", "--parents", "-n", "1", merge).split()[1:]
        self.assertEqual(len(parents), 3, "an octopus merge")
        self.repo.write("one.md", "one\n")
        self.repo.commit("clean it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{merge[:12]} one.md:2", out)

    # --github: the range from GitHub's own event file.

    def _event(self, name: str, payload: object | str) -> dict[str, str]:
        path = self.repo.root.parent / (self.repo.root.name + "-event.json")
        text = payload if isinstance(payload, str) else json.dumps(payload)
        path.write_text(text, encoding="utf-8")
        self.addCleanup(lambda: path.unlink(missing_ok=True))
        return {"GITHUB_EVENT_NAME": name, "GITHUB_EVENT_PATH": str(path)}

    def _leak_and_delete(self) -> tuple[str, str]:
        """A key added in one commit and deleted in the next. Returns (base,
        the commit that added it)."""
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("src/leak.py", LEAK)
        added = self.repo.commit("add a key")
        self.repo.git("rm", "-q", "src/leak.py")
        self.repo.commit("delete it")
        return base, added

    def test_github_pull_request_reads_base_to_head(self) -> None:
        base, added = self._leak_and_delete()
        head = self.repo.git("rev-parse", "HEAD")
        env = self._event("pull_request", {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}})
        code, out = self.repo.scan("--github", env=env)
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} src/leak.py:1", out)

    def test_github_push_reads_before_to_after_and_nothing_older(self) -> None:
        _, added = self._leak_and_delete()
        before = self.repo.git("rev-parse", "HEAD")
        self.repo.write("notes.md", "later\n")
        after = self.repo.commit("later")
        env = self._event("push", {"before": before, "after": after})
        code, out = self.repo.scan("--github", env=env)
        self.assertEqual(code, 0, out)
        env = self._event("push", {"before": added + "~1", "after": after})
        self.assertEqual(self.repo.scan("--github", env=env)[0], 1)

    def test_github_new_branch_and_lost_head_read_since_main(self) -> None:
        self.repo.git("update-ref", MAIN_REF, "HEAD")
        _, added = self._leak_and_delete()
        after = self.repo.git("rev-parse", "HEAD")
        for before in ("0" * 40, "1" * 40):  # a new branch; a force push whose old head is gone
            env = self._event("push", {"before": before, "after": after})
            code, out = self.repo.scan("--github", env=env)
            self.assertEqual(code, 1, f"before={before[:4]}: {out}")
            self.assertIn(f"{added[:12]} src/leak.py:1", out)

    def test_github_manual_run_reads_since_main_and_a_tag_cannot_stand_in(self) -> None:
        # A tag named origin/main wins over the remote branch for the bare
        # name (review of #140, finding 4); the full ref is used.
        self.repo.git("update-ref", MAIN_REF, "HEAD")
        self._leak_and_delete()
        self.repo.git("tag", "origin/main", "HEAD")
        code, out = self.repo.scan("--github", env=self._event("workflow_dispatch", {}))
        self.assertEqual(code, 1, out)

    def test_github_without_main_cannot_run(self) -> None:
        code, out = self.repo.scan("--github", env=self._event("workflow_dispatch", {}))
        self.assertEqual(code, 2, out)

    def test_github_deleted_branch_reads_the_tree_only(self) -> None:
        env = self._event("push", {"deleted": True, "before": "1" * 40, "after": "0" * 40})
        code, out = self.repo.scan("--github", env=env)
        self.assertEqual(code, 0, out)

    def test_github_event_that_cannot_be_read_cannot_run(self) -> None:
        self.assertEqual(self.repo.scan("--github", env={"GITHUB_EVENT_NAME": "", "GITHUB_EVENT_PATH": ""})[0], 2)
        self.assertEqual(self.repo.scan("--github", env=self._event("push", "{not json"))[0], 2)
        self.assertEqual(self.repo.scan("--github", env=self._event("pull_request", {"pull_request": {}}))[0], 2)
        self.assertEqual(self.repo.scan("--github", env=self._event("push", {"before": "1" * 40}))[0], 2)

    def test_the_modes_cannot_be_combined(self) -> None:
        self.assertEqual(self.repo.scan("--staged", "--unpushed")[0], 2)

    def test_no_baseline_cannot_run(self) -> None:
        (self.repo.root / ".secrets.baseline").unlink()
        code, out = self.repo.scan()
        self.assertEqual(code, 2, out)
        self.assertIn("no baseline", out)

    def test_a_range_git_cannot_read_cannot_run(self) -> None:
        code, out = self.repo.scan("--range", "nosuchref..HEAD")
        self.assertEqual(code, 2, out)

    def test_a_range_without_two_dots_cannot_run(self) -> None:
        code, out = self.repo.scan("--range", "HEAD")
        self.assertEqual(code, 2, out)

    def test_nothing_tracked_cannot_run(self) -> None:
        self.repo.git("rm", "-q", "--cached", "-r", ".")
        code, out = self.repo.scan()
        self.assertEqual(code, 2, out)


class RealBaseline(unittest.TestCase):
    def test_the_real_baseline_names_files_as_git_does(self) -> None:
        results = json.loads(REAL_BASELINE.read_text(encoding="utf-8"))["results"]
        self.assertTrue(results, "the real baseline lists the known false positives")
        for name in results:
            self.assertNotIn("\\", name, "a baseline written with Windows names matches nothing on CI")


if __name__ == "__main__":
    unittest.main(verbosity=2)
