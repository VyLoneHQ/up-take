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

    def judge(self) -> None:
        """What a person does after reading each new entry and finding no
        secret: `"is_secret": false` on every entry not judged yet."""
        path = self.root / ".secrets.baseline"
        data = json.loads(path.read_text(encoding="utf-8"))
        for entries in data["results"].values():
            for entry in entries:
                if entry.get("is_secret") is None:
                    entry["is_secret"] = False
        self.write(".secrets.baseline", json.dumps(data, indent=2) + "\n")

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
        self.repo.judge()
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
        self.repo.judge()
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
        self.repo.judge()
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

    def test_a_judgement_outlives_its_line_and_an_unjudged_entry_still_refuses(self) -> None:
        # The commit that added a judged line is still read after a later one
        # removes it, so the judgement has to stay, or the branch that removed
        # the line turns red on its own history. --write-baseline reads the
        # history, finds the line in that commit and keeps its judgement. The
        # unjudged one is listed too, from the history, and passes nothing.
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("src/pins.py", LEAK)
        other = 'password = "Pq8Lm2Xv7Rt4Ws9Kd3Nb"\n'  # pragma: allowlist secret
        self.repo.write("src/other.py", other)
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
        rewritten = json.loads((self.repo.root / ".secrets.baseline").read_text(encoding="utf-8"))[
            "results"
        ]
        self.assertTrue(
            all(e.get("is_secret") is False for e in rewritten["src/pins.py"]), "judged: kept"
        )
        self.assertTrue(
            all(e.get("is_secret") is None for e in rewritten["src/other.py"]),
            "unjudged: listed, not judged",
        )
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
        lockfile = (
            "lockfileVersion: '9.0'\n\npackages:\n\n  'a@1.0.0':\n" + CREDENTIAL_URL.split("  #")[0]
        )
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
        self.repo.judge()
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
        env = self._event(
            "pull_request", {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}
        )
        code, out = self.repo.scan("--github", env=env)
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} src/leak.py:1", out)

    def test_github_push_to_a_branch_reads_since_main_whatever_before_says(self) -> None:
        # A push GitHub ran nothing for ([skip ci]) leaves the next run's
        # `before` past its commits (second review of #140, finding 2). The
        # range does not start at `before`, so they are still read. Also a new
        # branch, and a force push whose old head is gone.
        self.repo.git("update-ref", MAIN_REF, "HEAD")
        _, added = self._leak_and_delete()
        skipped = self.repo.git("rev-parse", "HEAD")
        self.repo.write("notes.md", "later\n")
        after = self.repo.commit("later")
        for before in (skipped, "0" * 40, "1" * 40):
            env = self._event(
                "push", {"ref": "refs/heads/feature", "before": before, "after": after}
            )
            code, out = self.repo.scan("--github", env=env)
            self.assertEqual(code, 1, f"before={before[:4]}: {out}")
            self.assertIn(f"{added[:12]} src/leak.py:1", out)
            self.assertIn("commits not on main", out)

    def test_github_push_to_main_reads_its_whole_history(self) -> None:
        _, added = self._leak_and_delete()
        self.repo.write("notes.md", "later\n")
        after = self.repo.commit("later")
        env = self._event(
            "push", {"ref": "refs/heads/main", "before": after + "~1", "after": after}
        )
        code, out = self.repo.scan("--github", env=env)
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} src/leak.py:1", out)
        self.assertIn("main's whole history", out)

    def test_github_tag_push_reads_since_main(self) -> None:
        self.repo.git("update-ref", MAIN_REF, "HEAD")
        _, added = self._leak_and_delete()
        self.repo.git("tag", "v1")
        after = self.repo.git("rev-parse", "HEAD")
        env = self._event("push", {"ref": "refs/tags/v1", "before": "0" * 40, "after": after})
        code, out = self.repo.scan("--github", env=env)
        self.assertEqual(code, 1, out)
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
        self.assertEqual(
            self.repo.scan("--github", env={"GITHUB_EVENT_NAME": "", "GITHUB_EVENT_PATH": ""})[0], 2
        )
        self.assertEqual(self.repo.scan("--github", env=self._event("push", "{not json"))[0], 2)
        self.assertEqual(
            self.repo.scan("--github", env=self._event("pull_request", {"pull_request": {}}))[0], 2
        )
        self.assertEqual(
            self.repo.scan("--github", env=self._event("push", {"before": "1" * 40}))[0], 2
        )
        self.assertEqual(
            self.repo.scan("--github", env=self._event("push", {"ref": "refs/heads/x"}))[0], 2
        )

    # What neither read could see (second review of #140, finding 1), and
    # entries nobody judged.

    def test_a_secret_in_cargo_lock_is_refused_and_its_checksums_pass(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        # A real crate checksum from this repository's Cargo.lock: public.
        checksum = 'checksum = "320119579fcad9c21884f5c4861d16174d0e06250625266f50fe6898340abefa"\n'  # pragma: allowlist secret
        self.repo.write("Cargo.lock", '[[package]]\nname = "a"\n' + checksum)
        self.repo.commit("a lockfile")
        self.assertEqual(self.repo.scan("--range", f"{base}..HEAD")[0], 0, "checksums pass")
        self.repo.write("Cargo.lock", '[[package]]\nname = "a"\n' + checksum + LEAK)
        added = self.repo.commit("a key in the lockfile")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn("\n  Cargo.lock:4", out, "the tree")
        self.assertIn(f"{added[:12]} Cargo.lock:4", out, "the commit")

    def test_a_secret_in_an_svg_marked_binary_is_refused_in_the_tree_and_the_range(self) -> None:
        self.repo.write(".gitattributes", "*.svg binary\n")
        self.repo.commit("svg is binary to git")
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("logo.svg", "<svg>\n<!-- " + LEAK.strip() + " -->\n</svg>\n")
        added = self.repo.commit("a key in an svg")
        self.assertEqual(self.repo.scan()[0], 1, "the tree")
        self.repo.git("rm", "-q", "logo.svg")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} logo.svg:2", out)

    def _utf16(self, name: str, bom: bool) -> None:
        text = "# notes\r\n" + LEAK.replace("\n", "\r\n")
        data = text.encode("utf-16") if bom else text.encode("utf-16-le")
        path = self.repo.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def test_a_secret_in_a_utf16_file_is_refused_in_tree_range_and_staged(self) -> None:
        # What Windows PowerShell 5.1 writes by default. With and without the
        # byte-order mark.
        for bom in (True, False):
            with self.subTest(bom=bom):
                base = self.repo.git("rev-parse", "HEAD")
                name = f"notes-{bom}.txt"
                self._utf16(name, bom)
                self.repo.git("add", name)
                self.assertEqual(self.repo.scan("--staged")[0], 1, "staged")
                added = self.repo.commit("utf-16 notes")
                self.assertEqual(self.repo.scan()[0], 1, "the tree")
                self.repo.git("rm", "-q", name)
                self.repo.commit("delete it")
                code, out = self.repo.scan("--range", f"{base}..HEAD")
                self.assertEqual(code, 1, out)
                self.assertIn(f"{added[:12]} {name}:2", out)

    def test_a_binary_file_is_read_as_bytes_and_counted(self) -> None:
        (self.repo.root / "icon.png").write_bytes(
            b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR" + bytes(range(256))
        )
        self.repo.commit("an image")
        code, out = self.repo.scan()
        self.assertEqual(code, 0, out)
        self.assertIn("(1 of them binary, read as bytes)", out)

    def test_an_entry_nobody_judged_passes_nothing(self) -> None:
        # Otherwise one run of --write-baseline makes a real secret pass.
        self.repo.write("src/config.py", LEAK)
        self.repo.commit("add a key")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("in the baseline, not judged yet", out)
        self.repo.judge()
        self.assertEqual(self.repo.scan()[0], 0)

    def test_an_entry_judged_a_secret_passes_nothing(self) -> None:
        self.repo.write("src/config.py", LEAK)
        self.repo.commit("add a key")
        self.repo.scan("--write-baseline")
        data = json.loads((self.repo.root / ".secrets.baseline").read_text(encoding="utf-8"))
        data["results"]["src/config.py"][0]["is_secret"] = True
        self.repo.write(".secrets.baseline", json.dumps(data, indent=2) + "\n")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("judged a secret", out)

    def test_write_baseline_lists_what_only_the_history_holds(self) -> None:
        # A push to main reads all of main's history, so a line that is gone
        # has to be listable.
        self.repo.write("src/old.py", LEAK)
        added = self.repo.commit("a value that is not a secret")
        self.repo.git("rm", "-q", "src/old.py")
        self.repo.commit("gone")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        self.repo.judge()
        self.repo.commit("listed and judged")
        env = self._event(
            "push", {"ref": "refs/heads/main", "after": self.repo.git("rev-parse", "HEAD")}
        )
        code, out = self.repo.scan("--github", env=env)
        self.assertEqual(code, 0, out)
        self.assertNotIn(added[:12], out)

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


class ThirdReview(unittest.TestCase):
    """The third review of #140: a NUL byte in UTF-8 text, and git settings
    that change the names a diff prints."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_a_utf8_file_with_a_nul_byte_is_read_in_the_range(self) -> None:
        # git calls it binary, so a commit's diff has no lines for it; NUL is
        # valid UTF-8, so its blob is read as UTF-8.
        base = self.repo.git("rev-parse", "HEAD")
        path = self.repo.root / "src" / "notes.py"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"# a stray NUL here: \x00\n" + LEAK.encode("utf-8"))
        added = self.repo.commit("a key beside a NUL")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("(0 of them binary", out, "read, so not counted as unread")
        self.repo.git("rm", "-q", "src/notes.py")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} src/notes.py:2", out)

    def test_git_diff_prefix_settings_do_not_empty_the_line_reads(self) -> None:
        # A YAML comment only the line-by-line read can see.
        for key, value in (
            ("diff.mnemonicPrefix", "true"),
            ("diff.noprefix", "true"),
            ("diff.dstPrefix", "new/"),
        ):
            with self.subTest(setting=key):
                self.repo.git("config", key, value)
                self.repo.write(".github/ci.yml", "jobs:\n  a:\n    # " + LEAK + "    runs-on: x\n")
                self.repo.git("add", ".github/ci.yml")
                code, out = self.repo.scan("--staged")
                self.assertEqual(code, 1, out)
                self.assertIn(chr(10) + "  .github/ci.yml:3", out, "under its own name")
                code, out = self.repo.scan()
                self.assertEqual(code, 1, out)
                self.assertIn(chr(10) + "  .github/ci.yml:3", out, "under its own name")
                self.repo.git("rm", "-q", "--cached", ".github/ci.yml")
                self.repo.git("config", "--unset", key)

    @unittest.skipIf(os.name == "nt", "Windows cannot name a file with a double quote")
    def test_a_name_git_quotes_is_refused_not_dropped(self) -> None:
        # git C-quotes a path holding `"`; the name is read back unquoted, and
        # the finding is refused under the file's own name.
        self.repo.write('we"ird.yml', "jobs:\n  a:\n    # " + LEAK + "    runs-on: x\n")
        self.repo.git("add", "-A")
        for args in (["--staged"], []):
            if not args:
                self.repo.commit("a quoted name")
            code, out = self.repo.scan(*args)
            # The finding itself is asserted, not only the exit code
            # (thirteenth review of #140, A3).
            self.assertEqual(code, 1, out)
            self.assertIn('we"ird.yml:3', out)
            self.assertNotIn("Traceback", out)


class FourthReview(unittest.TestCase):
    """The fourth review of #140: names that still switched the scan off, and
    a typechange to text git calls binary."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_a_path_holding_swagger_is_read(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("docs/swagger_client.py", LEAK)
        self.repo.git("add", "-A")
        self.assertEqual(self.repo.scan("--staged")[0], 1, "staged")
        added = self.repo.commit("a key in a swagger file")
        self.assertEqual(self.repo.scan()[0], 1, "the tree")
        self.repo.git("rm", "-q", "docs/swagger_client.py")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} docs/swagger_client.py:1", out)

    def test_a_secrets_baseline_in_a_folder_is_read(self) -> None:
        self.repo.write("config/.secrets.baseline", LEAK)
        self.repo.commit("a file that only shares the name")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("config/.secrets.baseline:1", out)

    def test_a_value_pasted_into_the_root_baseline_is_read_and_its_hashes_pass(self) -> None:
        # The root baseline is read like any file. Its own hash lines pass by
        # their exact shape; anything else in it is read.
        self.repo.write("src/pins.py", LEAK)
        self.repo.commit("a value that is not a secret")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        self.repo.judge()
        self.repo.commit("listed and judged")
        self.assertEqual(self.repo.scan()[0], 0, "its hash lines pass")
        path = self.repo.root / ".secrets.baseline"
        data = json.loads(path.read_text(encoding="utf-8"))
        data["note"] = {"api_key": KEYWORD_VALUE}
        self.repo.write(".secrets.baseline", json.dumps(data, indent=2) + "\n")
        self.repo.commit("a value pasted into the baseline")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn(".secrets.baseline:", out)

    def _blob(self, data: bytes) -> str:
        return (
            subprocess.run(
                ["git", "hash-object", "-w", "--stdin"],
                cwd=self.repo.root,
                input=data,
                capture_output=True,
                check=True,
            )
            .stdout.decode()
            .strip()
        )

    def _index(self, mode: str, blob: str, name: str) -> None:
        self.repo.git("update-index", "--add", "--cacheinfo", f"{mode},{blob},{name}")

    def test_a_typechange_to_utf16_text_is_read_in_a_commit_and_staged(self) -> None:
        # A symlink replaced by a regular file is status T, not A or M. The
        # modes are set in the index, since not every machine makes symlinks.
        utf16 = ("# notes\r\n" + LEAK.replace("\n", "\r\n")).encode("utf-16")
        base = self.repo.git("rev-parse", "HEAD")
        self._index("120000", self._blob(b"somewhere"), "notes.txt")
        self.repo.git("commit", "-q", "-m", "a link")
        self._index("100644", self._blob(utf16), "notes.txt")
        code, out = self.repo.scan("--staged")
        self.assertEqual(code, 1, "staged: " + out)
        self.repo.git("commit", "-q", "-m", "the link becomes a file")
        changed = self.repo.git("rev-parse", "HEAD")
        self.repo.git("update-index", "--force-remove", "notes.txt")
        self.repo.git("commit", "-q", "-m", "delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{changed[:12]} notes.txt:2", out)


class FourthReviewStaged(unittest.TestCase):
    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_a_staged_typechange_gets_the_parsed_read(self) -> None:
        # A link replaced by a YAML file on disk: the value sits in a block
        # scalar, which only the parsed read joins, and the parsed read takes
        # its names from the staged list, so T has to be in it.
        blob = (
            subprocess.run(
                ["git", "hash-object", "-w", "--stdin"],
                cwd=self.repo.root,
                input=b"somewhere",
                capture_output=True,
                check=True,
            )
            .stdout.decode()
            .strip()
        )
        self.repo.git("update-index", "--add", "--cacheinfo", f"120000,{blob},deploy.yml")
        self.repo.git("commit", "-q", "-m", "a link")
        self.repo.write("deploy.yml", "deploy:\n  api_key: >-\n    " + KEYWORD_VALUE + "\n")
        # Not `git add`: with core.symlinks off (Windows) it keeps the link mode.
        file_blob = self.repo.git("hash-object", "-w", "deploy.yml")
        self.repo.git("update-index", "--cacheinfo", f"100644,{file_blob},deploy.yml")
        status = self.repo.git("diff", "--cached", "--name-status")
        self.assertTrue(status.startswith("T"), status)
        code, out = self.repo.scan("--staged")
        self.assertEqual(code, 1, out)


class NoNetwork(unittest.TestCase):
    """The fifth review of #140: detect-secrets' network verification sent
    every match of a verifiable type to its provider, and dropped it when the
    answer was not "valid", which a rate limit also gives. Run in this process
    with the network replaced, so a call is seen and answered 429."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))
        self._cwd = os.getcwd()

    def tearDown(self) -> None:
        os.chdir(self._cwd)
        self._dir.cleanup()

    def test_a_telegram_shaped_token_is_refused_and_nothing_is_sent(self) -> None:
        import importlib.util
        from unittest import mock

        import requests

        # A made-up value in the shape of a Telegram bot token, unquoted, so
        # the Telegram detector is the one that sees it.
        middle = "x7Qp2Lm9Vt4Rw8Ks3Nd6Fb1Hc5Gj0Zy"  # pragma: allowlist secret
        token = "123456789:" + "AAH" + middle + "TeK"
        self.repo.write("bot.env", "TELEGRAM=" + token + "\n")
        self.repo.commit("a token-shaped value")

        calls = []

        class RateLimited:
            status_code = 429
            text = ""

            @staticmethod
            def json():
                return {"ok": False}

        def fake(*args, **kwargs):
            calls.append(args[0] if args else kwargs.get("url"))
            return RateLimited()

        spec = importlib.util.spec_from_file_location("secret_scan_inproc", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        os.chdir(self.repo.root)
        with mock.patch.object(requests, "get", fake), mock.patch.object(requests, "post", fake):
            code = module.check(Path(".secrets.baseline"), None)
        self.assertEqual(calls, [], "nothing is sent to a provider")
        self.assertEqual(code, 1, "the value is refused, whatever a provider would say")

    def test_a_latin1_file_is_read_and_not_counted_as_unread(self) -> None:
        # Git shows it as text, so the line-by-line read reads it.
        (self.repo.root / "notes.txt").write_bytes("# café\n".encode("latin-1") + LEAK.encode())
        self.repo.commit("latin-1")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("(0 of them binary", out)

    def test_a_binary_marked_file_that_decodes_as_text_is_not_counted(self) -> None:
        # The count is of files read as bytes only, not of every file marked
        # binary (sixteenth review of #140, a note).
        self.repo.write(".gitattributes", "*.svg binary\n")
        self.repo.write("logo.svg", "<svg><title>logo</title></svg>\n")
        self.repo.commit("an SVG in UTF-8, marked binary")
        code, out = self.repo.scan()
        self.assertEqual(code, 0, out)
        self.assertIn("(0 of them binary, read as bytes)", out)

    def test_the_real_baseline_does_not_turn_verification_on(self) -> None:
        names = [
            f["path"] for f in json.loads(REAL_BASELINE.read_text(encoding="utf-8"))["filters_used"]
        ]
        self.assertNotIn(
            "detect_secrets.filters.common.is_ignored_due_to_verification_policies", names
        )


class SixthReview(unittest.TestCase):
    """The sixth review of #140: a heuristic filter dropped a token inside a
    call, for every detector. Only two filters may drop a finding now."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))
        self._cwd = os.getcwd()

    def tearDown(self) -> None:
        os.chdir(self._cwd)
        self._dir.cleanup()

    def test_a_token_inside_a_rust_call_is_refused_everywhere(self) -> None:
        token = "ghp_" + "Zq3x9KfP0wLm7Tn2Rv8sJ4dHc5Gj0Yb1Wa6E"  # pragma: allowlist secret
        line = f'    let c = Octocrab::builder().personal_token("{token}");\n'
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("src/client.rs", "fn main() {\n" + line + "}\n")
        self.repo.git("add", "-A")
        self.assertEqual(self.repo.scan("--staged")[0], 1, "staged")
        added = self.repo.commit("a token inside a call")
        self.assertEqual(self.repo.scan()[0], 1, "the tree")
        self.repo.git("rm", "-q", "src/client.rs")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} src/client.rs:2", out)

    def test_only_the_two_kept_filters_are_active(self) -> None:
        import importlib.util

        spec = importlib.util.spec_from_file_location("secret_scan_filters", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        os.chdir(self.repo.root)
        module.load_baseline(Path(".secrets.baseline"))
        self.assertEqual(set(module.get_settings().filters), set(module.KEEP_FILTERS))


class SeventhReview(unittest.TestCase):
    """The seventh review of #140: a nextline pragma anywhere in a commit
    silenced another line in detect-secrets' diff reader, and a branch with no
    history in common with main could not be read."""

    NEXTLINE = "# pragma: allowlist nextline secret\n"

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    @staticmethod
    def _edit(lines: list[str], at: dict[int, str]) -> str:
        """The file with the given 1-based lines replaced."""
        return "".join(at.get(n, line) for n, line in enumerate(lines, 1))

    def _misplaced(self, name: str, lines: list[str], key: str, pragma: str) -> str:
        """An existing file in which one commit replaces line 3 with a key and
        line 40 with a nextline pragma: the commit's added lines are then
        [3, 40], and detect-secrets' diff reader took line 40 for line 3's
        neighbour (the shape the seventh review drilled). Returns the commit."""
        self.repo.write(name, self._edit(lines, {3: key, 40: pragma}))
        return self.repo.commit("a key and a pragma elsewhere")

    def test_a_misplaced_nextline_pragma_hides_nothing_in_a_commit(self) -> None:
        lines = [f"x{n} = {n}\n" for n in range(1, 51)]
        self.repo.write("app.py", "".join(lines))
        base = self.repo.commit("a file")
        added = self._misplaced("app.py", lines, LEAK, self.NEXTLINE)
        self.repo.write("app.py", "".join(lines))
        self.repo.commit("clean it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} app.py:3", out)

    def test_a_misplaced_nextline_pragma_hides_nothing_staged_or_unpushed(self) -> None:
        remote = self.repo.root.parent / (self.repo.root.name + "-remote.git")
        subprocess.run(["git", "init", "-q", "--bare", str(remote)], check=True)
        self.addCleanup(lambda: __import__("shutil").rmtree(remote, ignore_errors=True))
        self.repo.git("remote", "add", "origin", str(remote))
        lines = ["jobs:\n"] + [f"  k{n}: {n}\n" for n in range(2, 51)]
        self.repo.write("ci.yml", "".join(lines))
        self.repo.commit("a yaml file")
        self.repo.git("push", "-q", "origin", "main")
        # A YAML comment: only the line-by-line read sees it, so the parsed
        # read cannot cover for a misread pragma.
        edited = self._edit(lines, {3: "  # " + LEAK, 40: "  " + self.NEXTLINE})
        self.repo.write("ci.yml", edited)
        self.repo.git("add", "ci.yml")
        self.assertEqual(self.repo.scan("--staged")[0], 1, "staged")
        self.repo.commit("a key in a comment and a pragma elsewhere")
        self.assertEqual(self.repo.scan("--unpushed")[0], 1, "unpushed")

    def test_a_nextline_pragma_in_its_place_still_passes_its_line(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("app.py", "import os\n" + self.NEXTLINE + LEAK)
        self.repo.commit("a judged line")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 0, out)

    def test_an_unusual_line_break_does_not_shift_the_line_numbers(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("notes.py", "# a b and a lone \r carriage return\n\n" + LEAK)
        added = self.repo.commit("odd breaks")
        self.repo.git("rm", "-q", "notes.py")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} notes.py:3", out)

    def test_a_branch_with_no_history_in_common_with_main_is_read(self) -> None:
        self.repo.git("update-ref", MAIN_REF, "HEAD")
        self.repo.git("checkout", "-q", "--orphan", "gh-pages")
        self.repo.git("rm", "-q", "-r", "--cached", ".")
        self.repo.write("index.html", LEAK)
        self.repo.git("add", "index.html", ".secrets.baseline")
        added = self.repo.commit("a page")
        self.repo.git("rm", "-q", "index.html")
        after = self.repo.commit("delete it")
        path = self.repo.root.parent / (self.repo.root.name + "-event.json")
        path.write_text(
            json.dumps({"ref": "refs/heads/gh-pages", "after": after}), encoding="utf-8"
        )
        self.addCleanup(lambda: path.unlink(missing_ok=True))
        env = {"GITHUB_EVENT_NAME": "push", "GITHUB_EVENT_PATH": str(path)}
        code, out = self.repo.scan("--github", env=env)
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} index.html:1", out)


class EighthReview(unittest.TestCase):
    """The eighth review of #140: UTF-8 padded with NULs was read as UTF-16
    garbage, a branch whose last push started no run was never read, and a
    staged file named `0:x` was read as `x`."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_utf8_padded_with_nuls_is_read_at_odd_and_even_length(self) -> None:
        remote = self.repo.root.parent / (self.repo.root.name + "-remote.git")
        subprocess.run(["git", "init", "-q", "--bare", str(remote)], check=True)
        self.addCleanup(lambda: __import__("shutil").rmtree(remote, ignore_errors=True))
        self.repo.git("remote", "add", "origin", str(remote))
        self.repo.git("push", "-q", "origin", "main")
        base = self.repo.git("rev-parse", "HEAD")
        added = []
        for pad in (600, 601):
            name = f"pad{pad}.txt"
            data = LEAK.encode("utf-8") + b"\x00" * pad
            (self.repo.root / name).write_bytes(data)
            self.repo.git("add", name)
            self.assertEqual(self.repo.scan("--staged")[0], 1, f"staged, {pad}")
            added.append(self.repo.commit(f"a key and {pad} NULs"))
            self.repo.git("rm", "-q", name)
            self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        for sha, pad in zip(added, (600, 601)):
            self.assertIn(f"{sha[:12]} pad{pad}.txt:1", out)
        code, out = self.repo.scan("--unpushed")
        self.assertEqual(code, 1, out)
        for sha in added:
            self.assertIn(sha[:12], out)

    @unittest.skipIf(os.name == "nt", "Git for Windows refuses a path with a colon")
    def test_a_staged_file_named_like_a_stage_is_read_as_itself(self) -> None:
        self.repo.write("x.txt", "nothing here\n")
        self.repo.commit("a clean x")
        path = self.repo.root / "0:x.txt"
        path.write_bytes(b"\x00" + LEAK.encode("utf-8"))
        self.repo.git("add", "0:x.txt")
        self.assertEqual(self.repo.scan("--staged")[0], 1)


class NinthReview(unittest.TestCase):
    """The ninth review of #140: one judged finding switched off detect-
    secrets' second parse of a file, and a submodule pointer stopped the
    commit read. (Its finding about the daily run left with the daily run.)"""

    # The SHA-256 of "test", unquoted: only the eager parse of `key = value`
    # quotes it, and only then can the hex detector see it.
    HEX = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"  # pragma: allowlist secret

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_a_judged_value_does_not_pass_an_unquoted_one_beside_it(self) -> None:
        self.repo.write("deploy.env", LEAK)
        self.repo.commit("a value judged not a secret")
        self.assertEqual(self.repo.scan("--write-baseline")[0], 0)
        self.repo.judge()
        base = self.repo.commit("judged")
        self.repo.write("deploy.env", LEAK + "colour = " + self.HEX + "\n")
        self.repo.git("add", "-A")
        self.assertEqual(self.repo.scan("--staged")[0], 1, "staged")
        added = self.repo.commit("an unquoted value beside it")
        self.assertEqual(self.repo.scan()[0], 1, "the tree")
        self.repo.write("deploy.env", LEAK)
        self.repo.commit("remove it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} deploy.env:2", out)

    def test_a_parsed_only_value_is_read_in_a_commit_range(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("deploy.env", "colour = " + self.HEX + "\n")
        env = self.repo.commit("an unquoted value")
        self.repo.write("deploy.yml", "deploy:\n  api_key: >-\n    " + KEYWORD_VALUE + "\n")
        yml = self.repo.commit("a block scalar")
        self.repo.git("rm", "-q", "deploy.env", "deploy.yml")
        self.repo.commit("delete both")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{env[:12]} deploy.env:1", out)
        self.assertIn(f"{yml[:12]} deploy.yml:", out)

    def test_a_submodule_pointer_does_not_stop_the_scan(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        sha = "1" * 40
        self.repo.git("update-index", "--add", "--cacheinfo", f"160000,{sha},vendor/lib")
        self.assertEqual(self.repo.scan("--staged")[0], 0, "staged")
        self.repo.git("commit", "-q", "-m", "a submodule pointer")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 0, out)


class TenthReview(unittest.TestCase):
    """The tenth review of #140: a file name that is not UTF-8 stopped every
    run that read its commit, a pathspec glob hid a staged file behind a
    submodule pointer, and a YAML file marked binary lost its parsed read."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def _git_bytes(self, *args: str, stdin: bytes | None = None) -> bytes:
        return subprocess.run(
            ["git", *args], cwd=self.repo.root, input=stdin, capture_output=True, check=True
        ).stdout

    def _commit_with(self, name: bytes, data: bytes, message: str) -> str:
        """A commit on main that adds `name` beside HEAD's files, built with
        `mktree`, so the name never has to exist on this machine's disk."""
        blob = self._git_bytes("hash-object", "-w", "--stdin", stdin=data).strip()
        listing = self._git_bytes("ls-tree", "-z", "HEAD")
        tree = self._git_bytes(
            "mktree", "-z", stdin=listing + b"100644 blob " + blob + b"\t" + name + b"\0"
        ).strip()
        commit = self._git_bytes(
            "commit-tree", tree.decode(), "-p", "HEAD", "-m", message
        ).strip().decode()
        self.repo.git("update-ref", "refs/heads/main", commit)
        self.repo.git("reset", "-q", "--mixed", commit)
        return commit

    def test_a_name_that_is_not_utf8_is_read_and_does_not_stop_the_run(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        self._commit_with(b"caf\xe9.txt", b"nothing secret here\n", "a Latin-1 name")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 0, "a clean file under that name: " + out)
        added = self._commit_with(b"cl\xe9.txt", LEAK.encode(), "a key under a Latin-1 name")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} cl\\xe9.txt:1", out)

    def test_a_staged_name_that_globs_a_submodule_pointer_is_read(self) -> None:
        # `*x.yml` as a pathspec also matches `!x.yml`, a submodule pointer,
        # which git lists first. Index entries only: Windows has no `*` in names,
        # and git there refuses one in the index unless told not to.
        sha = "1" * 40
        self.repo.git("update-index", "--add", "--cacheinfo", f"160000,{sha},!x.yml")
        blob = self._git_bytes("hash-object", "-w", "--stdin", stdin=LEAK.encode()).strip()
        self.repo.git(
            "-c", "core.protectNTFS=false",
            "update-index", "--add", "--cacheinfo", f"100644,{blob.decode()},*x.yml",
        )
        code, out = self.repo.scan("--staged")
        self.assertEqual(code, 1, out)
        self.assertIn("*x.yml:1", out)

    def test_a_yaml_file_marked_binary_gets_the_parsed_read_in_a_commit(self) -> None:
        self.repo.write(".gitattributes", "*.yml binary\n")
        base = self.repo.commit("YAML marked binary")
        self.repo.write("deploy.yml", "deploy:\n  api_key: >-\n    " + KEYWORD_VALUE + "\n")
        added = self.repo.commit("a block scalar in a binary-marked file")
        self.repo.git("rm", "-q", "deploy.yml")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} deploy.yml:", out)


class EleventhReview(unittest.TestCase):
    """The eleventh review of #140: a value written into an existing YAML
    block scalar, a module beside the script standing in for a library, and
    the tree's line read skipping a file marked binary."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_a_value_written_into_an_existing_block_scalar_is_read(self) -> None:
        # detect-secrets numbers a block scalar's finding at its key's line,
        # and this commit adds only the value's line.
        self.repo.write("config.yml", "db:\n  password: |\n    placeholder\n")
        self.repo.commit("a placeholder")
        self.repo.scan("--write-baseline")
        self.repo.judge()
        base = self.repo.commit("the placeholder judged")
        self.repo.write("config.yml", "db:\n  password: |\n    " + KEYWORD_VALUE + "\n")
        added = self.repo.commit("a key written into the value")
        self.repo.write("config.yml", "db:\n  password: |\n    placeholder\n")
        self.repo.commit("back to the placeholder")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} config.yml:", out)

    def test_a_module_beside_the_script_does_not_stand_in_for_a_library(self) -> None:
        # A copy of the script in the repository's own scripts/ folder, with a
        # `unidiff` there whose diff reader finds nothing.
        scripts = self.repo.root / "scripts"
        (scripts / "unidiff").mkdir(parents=True)
        (scripts / "unidiff" / "__init__.py").write_text(
            "class PatchSet(list):\n"
            "    @classmethod\n"
            "    def from_string(cls, text):\n"
            "        return cls()\n",
            encoding="utf-8",
        )
        copy = scripts / "secret-scan.py"
        copy.write_bytes(SCRIPT.read_bytes())
        # A key in a YAML comment: only the line read sees it, and the line
        # read is the one the stub would empty. A key the parsed read also
        # finds left this test green with the guard removed (fourteenth
        # review of #140, finding 2).
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("ci.yml", "name: build\n# " + LEAK)
        self.repo.git("add", "ci.yml")
        self.repo.git("commit", "-q", "-m", "a key in a comment")
        self.repo.git("rm", "-q", "ci.yml")
        self.repo.git("commit", "-q", "-m", "delete it")
        done = subprocess.run(
            [sys.executable, str(copy), "--range", f"{base}..HEAD"],
            cwd=self.repo.root,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)

    def test_the_tree_reads_the_lines_of_a_yaml_file_marked_binary(self) -> None:
        # The parsed read never sees a YAML comment; only the line read does.
        self.repo.write(".gitattributes", "*.yml binary\n")
        self.repo.write("ci.yml", "steps:\n  - run: make\n# " + LEAK)
        self.repo.commit("a key in a comment of a binary-marked YAML file")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("ci.yml:3", out)


class TwelfthReview(unittest.TestCase):
    """The twelfth review of #140: a commit that only deleted lines was never
    parsed, and the install step ran a `pip` a pull request could add."""

    HALF_A = "Zq3x9KfP0wLm7Tn2Rv"  # pragma: allowlist secret
    HALF_B = "8sJ4dHq3x9KfP0wLm7"  # pragma: allowlist secret

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_a_deletion_that_joins_a_token_is_read(self) -> None:
        # A YAML double-quoted string joins lines across a trailing backslash.
        # With the middle line it holds a space; without it, a token's shape.
        head = 'url: "ghp_' + self.HALF_A + "\\\n"
        tail = self.HALF_B + '"\n'
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("svc.yml", head + "a b\\\n" + tail)
        self.repo.commit("a string with a space in it")
        self.assertEqual(self.repo.scan("--range", f"{base}..HEAD")[0], 0, "nothing to find yet")
        self.repo.write("svc.yml", head + tail)
        joined = self.repo.commit("only a deletion")
        self.repo.write("svc.yml", head + "a b\\\n" + tail)
        self.repo.commit("the line back")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{joined[:12]} svc.yml:", out)

    def test_every_python_step_up_to_the_scan_is_isolated(self) -> None:
        # `python -m` puts the checkout first on the import path, and so does
        # running a script from it without `-I`.
        # The workflow as YAML parses it, every step's `run` in order, each of
        # its lines a command: a reading of text lines missed a `run: |`
        # block, then a `- run:` step and a quoted value (fifteenth and
        # sixteenth reviews of #140, finding 3 and finding 2). PyYAML comes
        # with detect-secrets.
        import re

        import yaml

        workflow = yaml.safe_load(
            (HERE.parent / ".github" / "workflows" / "secrets.yml").read_text(encoding="utf-8")
        )
        commands = [
            line.strip()
            for job in workflow["jobs"].values()
            for step in job.get("steps", [])
            for line in str(step.get("run", "")).splitlines()
            if line.strip()
        ]
        scan = next(i for i, command in enumerate(commands) if "secret-scan.py" in command)
        # Every python interpreter in a command, not only one that starts it:
        # `/usr/bin/python3 -m pip` passed a check on the first word
        # (seventeenth review of #140, finding 2). Each must be followed by -I.
        interpreter = re.compile(r"(?:^|[\s/=])python[0-9.]*(?=\s|$)")
        for command in commands[: scan + 1]:
            for found in interpreter.finditer(command):
                self.assertRegex(command[found.end() :], r"^\s+-I\s", command)


class ThirteenthReview(unittest.TestCase):
    """The thirteenth review of #140: file names that unidiff split in the
    wrong place crashed every read, or made a modified file unreadable."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def _refused(self, code: int, out: str, where: str) -> None:
        self.assertEqual(code, 1, out)
        self.assertIn(where, out)
        self.assertNotIn("Traceback", out)

    def test_a_name_holding_space_b_slash_is_read_in_the_tree_staged_and_a_range(self) -> None:
        name = "plan b/notes.md"
        self.repo.write(name, "nothing here\n")
        base = self.repo.commit("a name holding ' b/'")
        code, out = self.repo.scan()
        self.assertEqual(code, 0, "a clean file under that name: " + out)
        self.repo.write(name, "nothing here\n" + LEAK)
        self.repo.git("add", "-A")
        self._refused(*self.repo.scan("--staged"), f"{name}:2")
        added = self.repo.commit("a key in it")
        self._refused(*self.repo.scan(), f"{name}:2")
        self.repo.write(name, "nothing here\n")
        self.repo.commit("the key removed")
        self._refused(*self.repo.scan("--range", f"{base}..HEAD"), f"{added[:12]} {name}:2")

    def _commit_named(self, name: bytes, data: bytes) -> str:
        """A commit on main adding `name`, built with `mktree`: Windows cannot
        put a double quote or a newline in a file name."""

        def run(*args: str, stdin: bytes | None = None) -> bytes:
            return subprocess.run(
                ["git", *args], cwd=self.repo.root, input=stdin, capture_output=True, check=True
            ).stdout.strip()

        blob = run("hash-object", "-w", "--stdin", stdin=data)
        listing = subprocess.run(
            ["git", "ls-tree", "-z", "HEAD"], cwd=self.repo.root, capture_output=True, check=True
        ).stdout
        tree = run("mktree", "-z", stdin=listing + b"100644 blob " + blob + b"\t" + name + b"\0")
        commit = run("commit-tree", tree.decode(), "-p", "HEAD", "-m", "a name").decode()
        self.repo.git("update-ref", "refs/heads/main", commit)
        return commit

    def test_a_quoted_name_with_a_space_is_read_in_a_range(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        added = self._commit_named(b'say "hi" now.md', LEAK.encode())
        self._refused(
            *self.repo.scan("--range", f"{base}..HEAD"), f'{added[:12]} say "hi" now.md:1'
        )

    def test_a_name_with_a_newline_is_read_in_a_range(self) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        added = self._commit_named(b"nl\nname.md", LEAK.encode())
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self._refused(code, out, f"{added[:12]} nl")

    def test_an_added_line_that_starts_with_two_pluses_is_not_a_file_name(self) -> None:
        # In a diff it reads `+++ ...`, the shape of a header line.
        base = self.repo.git("rev-parse", "HEAD")
        self.repo.write("notes.txt", "++ b/elsewhere.txt\n" + LEAK)
        added = self.repo.commit("a line that starts with ++")
        self._refused(*self.repo.scan("--range", f"{base}..HEAD"), f"{added[:12]} notes.txt:2")

    def test_an_unexpected_error_exits_2_not_1(self) -> None:
        # `1` means a finding. A baseline that is not JSON raises in the JSON
        # reader, not as CannotRun, and its traceback exited 1.
        self.repo.write(".secrets.baseline", "{ not json\n")
        code, out = self.repo.scan()
        self.assertEqual(code, 2, out)
        self.assertIn("could not run", out)


class FourteenthReview(unittest.TestCase):
    """The fourteenth review of #140: text in neither UTF-8 nor UTF-16 that
    git calls binary was read by nothing."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def test_latin1_text_marked_binary_is_read_in_the_tree_staged_and_a_range(self) -> None:
        self.repo.write(".gitattributes", "notes.txt binary\n")
        base = self.repo.commit("notes marked binary")
        (self.repo.root / "notes.txt").write_bytes(b"Caf\xe9 notes\n" + LEAK.encode())
        self.repo.git("add", "notes.txt")
        self.assertEqual(self.repo.scan("--staged")[0], 1, "staged")
        added = self.repo.commit("a key in Latin-1 notes")
        self.assertEqual(self.repo.scan()[0], 1, "the tree")
        self.repo.git("rm", "-q", "notes.txt")
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} notes.txt:2", out)

    def test_a_key_inside_a_binary_file_is_read(self) -> None:
        # A database or a build artifact: NULs and bytes no text has, and a
        # key in plain ASCII among them.
        data = b"SQLite format 3\x00\xff\xfe\x01\n" + LEAK.encode() + bytes(range(256))
        (self.repo.root / "cache.db").write_bytes(data)
        base = self.repo.git("rev-parse", "HEAD")
        added = self.repo.commit("a database")
        self.assertEqual(self.repo.scan()[0], 1, "the tree")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} cache.db:2", out)


class FifteenthReview(unittest.TestCase):
    """The fifteenth review of #140: UTF-16 that does not decode strictly, and
    UTF-32, passed every read."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def _all_reads_refuse(self, name: str, data: bytes) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        (self.repo.root / name).write_bytes(data)
        self.repo.git("add", name)
        self.assertEqual(self.repo.scan("--staged")[0], 1, "staged")
        added = self.repo.commit(f"a key in {name}")
        self.assertEqual(self.repo.scan()[0], 1, "the tree")
        self.repo.git("rm", "-q", name)
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} {name}:", out)

    def test_utf16_cut_to_an_odd_length_is_read(self) -> None:
        self._all_reads_refuse("odd.txt", ("# notes\n" + LEAK).encode("utf-16") + b"\x41")

    def test_utf16_with_an_unpaired_surrogate_is_read(self) -> None:
        data = ("# notes\n" + LEAK).encode("utf-16-le") + b"\x00\xd8"
        self._all_reads_refuse("lone.txt", b"\xff\xfe" + data)

    def test_utf32_is_read(self) -> None:
        self._all_reads_refuse("wide.txt", ("# notes\n" + LEAK).encode("utf-32"))

    def test_a_utf16_mark_on_ascii_with_no_nul_is_read(self) -> None:
        # It decodes as UTF-16 into CJK, and holds no NUL to strip (sixteenth
        # review of #140, finding 1). Marked binary too, as `*.bin` is here.
        self.repo.write(".gitattributes", "*.dat binary\n")
        self.repo.commit("dat marked binary")
        # An even length, so the whole file decodes as UTF-16 without error:
        # an odd one fails that decode and was read as bytes already.
        data = b"\xff\xfe# x\n" + LEAK.encode()
        if len(data) % 2:
            data = data[:-1] + b" \n"
        self.assertEqual(len(data) % 2, 0)
        self._all_reads_refuse("mark.dat", data)

    def test_mostly_cjk_utf16_without_a_bom_is_read(self) -> None:
        text = "\u6f22\u5b57" * 40 + "\n" + LEAK
        self._all_reads_refuse("cjk.txt", text.encode("utf-16-le"))

    @unittest.skipIf(os.name == "nt", "NTFS cannot hold a name that is not UTF-8")
    def test_the_tree_reads_a_file_whose_name_is_not_utf8(self) -> None:
        # Finding 2: the name came back with U+FFFD, named no file on disk,
        # and the tree read skipped it in silence.
        name = os.fsencode(str(self.repo.root)) + b"/cl\xe9.txt"
        with open(name, "wb") as handle:
            handle.write(LEAK.encode())
        self.repo.commit("a key under a Latin-1 name")
        code, out = self.repo.scan()
        self.assertEqual(code, 1, out)
        self.assertIn("cl\\xe9.txt:1", out)


class SeventeenthReview(unittest.TestCase):
    """The seventeenth review of #140: the parsed read saw only raw bytes, so a
    YAML or INI value in a file that is not UTF-8 passed every read."""

    HEX = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"  # pragma: allowlist secret

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.repo = Repo(Path(self._dir.name))

    def tearDown(self) -> None:
        self._dir.cleanup()

    def _refused_everywhere(self, name: str, data: bytes) -> None:
        base = self.repo.git("rev-parse", "HEAD")
        (self.repo.root / name).write_bytes(data)
        self.repo.git("add", name)
        self.assertEqual(self.repo.scan("--staged")[0], 1, "staged")
        added = self.repo.commit(f"a value in {name}")
        self.assertEqual(self.repo.scan()[0], 1, "the tree")
        self.repo.git("rm", "-q", name)
        self.repo.commit("delete it")
        code, out = self.repo.scan("--range", f"{base}..HEAD")
        self.assertEqual(code, 1, out)
        self.assertIn(f"{added[:12]} {name}:", out)

    def test_a_block_scalar_in_utf16_yaml_is_read(self) -> None:
        text = "db:\n  password: |\n    " + KEYWORD_VALUE + "\n"
        self._refused_everywhere("conf.yml", text.encode("utf-16"))

    def test_an_unquoted_value_in_latin1_ini_is_read(self) -> None:
        text = "; caf\xe9\n[svc]\nblob = " + self.HEX + "\n"
        self._refused_everywhere("svc.ini", text.encode("latin-1"))


class Pins(unittest.TestCase):
    def test_every_copy_of_the_versions_agrees_with_what_is_installed(self) -> None:
        # Dependabot does not bump pip pins here, so a one-sided bump would
        # make "a baseline that passes here passes there" false with no signal.
        import re
        from importlib.metadata import version

        root = HERE.parent
        files = [
            root / ".pre-commit-config.yaml",
            root / ".github" / "workflows" / "secrets.yml",
            HERE / "secret-scan.py",
            root / "CONTRIBUTING.md",
        ]
        for package in ("detect-secrets", "unidiff"):
            versions = {
                path.name: set(
                    re.findall(
                        re.escape(package) + r"==([0-9][0-9.]*)", path.read_text(encoding="utf-8")
                    )
                )
                for path in files
            }
            found = set().union(*versions.values())
            self.assertEqual(len(found), 1, f"{package}: {versions}")
            self.assertTrue(
                all(versions.values()), f"{package} is pinned in every file: {versions}"
            )
        pinned = re.search(
            r"detect-secrets==([0-9.]+)", files[0].read_text(encoding="utf-8")
        ).group(1)
        self.assertEqual(
            version("detect-secrets"), pinned, "the installed version is the pinned one"
        )


class RealBaseline(unittest.TestCase):
    def test_the_real_baseline_names_files_as_git_does(self) -> None:
        results = json.loads(REAL_BASELINE.read_text(encoding="utf-8"))["results"]
        self.assertTrue(results, "the real baseline lists the known false positives")
        for name in results:
            self.assertNotIn(
                "\\", name, "a baseline written with Windows names matches nothing on CI"
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
