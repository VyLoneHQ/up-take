#!/usr/bin/env python3
"""Tests for `scripts/soak-record.py` (backlog `I-489`).

1. **The trend is arithmetic that can be checked by hand.** The slope, the
   first and last hour and the CPU figure are tested on synthetic records
   whose answers are known exactly.
2. **A limit can fail.** A record that leaks fails the limit it is given and
   the exit code is 1; a flat one passes with 0. Without a limit nothing is
   judged (`M-20` names no number yet).
3. **What cannot be read is refused**, exit 2, never a quiet pass: one sample,
   a file that is not a soak record, a missing file.
4. **The recorder sees a real leak** (Windows only, since it reads Windows'
   own counters): a child process that allocates memory on purpose shows a
   steep rise in private bytes, and an idle child next to none. The children
   run without a console window.

Off Windows, (4) is skipped and says so; the rest run anywhere.

Run: `python scripts/test_soak_record.py`
"""

from __future__ import annotations

import csv
import importlib.util
import io
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("soak_record", HERE / "soak-record.py")
soak = importlib.util.module_from_spec(spec)
assert spec.loader is not None
sys.modules["soak_record"] = soak
spec.loader.exec_module(soak)

MB = 1024 * 1024


def samples(hours: float, every: float, private_per_hour: float, cpu_percent: float, cpu_rise: float = 0.0):
    """A synthetic record: private bytes growing linearly, CPU at a steady rate."""
    out, cpu = [], 0.0
    steps = int(hours * 3600 / every)
    for i in range(steps + 1):
        t = i * every
        if i:
            rate = cpu_percent + cpu_rise * (t / (hours * 3600))
            cpu += rate / 100 * every
        out.append(
            soak.Sample(
                seconds=t,
                private_bytes=int(100 * MB + private_per_hour * MB * t / 3600),
                working_set=150 * MB,
                handles=400,
                gdi=60,
                user=40,
                cpu_seconds=cpu,
            )
        )
    return out


def write(path: Path, rows) -> None:
    with path.open("w", newline="", encoding="utf-8") as file:
        writer = csv.writer(file)
        writer.writerow(soak.FIELDS)
        for s in rows:
            writer.writerow([getattr(s, f) for f in soak.FIELDS])


def run(*argv: str) -> tuple[int, str, str]:
    out, err = io.StringIO(), io.StringIO()
    with redirect_stdout(out), redirect_stderr(err):
        code = soak.main(list(argv))
    return code, out.getvalue(), err.getvalue()


class Trend(unittest.TestCase):
    def test_slope_is_exact_on_a_line(self):
        points = [(t, 5.0 + 2.0 * t / 3600) for t in range(0, 7200, 60)]
        self.assertAlmostEqual(soak.slope_per_hour(points), 2.0, places=9)

    def test_flat_record_has_no_slope(self):
        lines, passed = soak.summarise(samples(3, 60, 0.0, 1.0), {})
        self.assertTrue(passed)
        self.assertIn("slope +0.000MB/h", lines[1])

    def test_cpu_percent_of_one_core(self):
        lines, _ = soak.summarise(samples(3, 60, 0.0, 2.5), {})
        self.assertIn("first hour      2.50%", lines[-1])
        self.assertIn("rise +0.00 points", lines[-1])

    def test_windows_are_the_first_and_last_hour(self):
        record = samples(8, 30, 4.0, 1.0)
        first, last = soak.window(record, True), soak.window(record, False)
        self.assertEqual(first[-1].seconds, 3600)
        self.assertEqual(last[0].seconds, 8 * 3600 - 3600)


class Limits(unittest.TestCase):
    def test_a_leak_fails_its_limit_and_exits_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "leak.csv"
            write(path, samples(8, 60, 5.0, 1.0))
            code, out, _ = run("summarise", str(path), "--max-private-mb-per-hour", "1")
        self.assertEqual(code, 1, out)
        self.assertIn("FAIL (limit +1MB/h)", out)

    def test_a_flat_record_passes_and_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "flat.csv"
            write(path, samples(8, 60, 0.0, 1.0))
            code, out, _ = run("summarise", str(path), "--max-private-mb-per-hour", "1", "--max-cpu-rise", "0.5")
        self.assertEqual(code, 0, out)
        self.assertEqual(out.count("PASS"), 2, out)

    def test_creeping_cpu_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "cpu.csv"
            write(path, samples(8, 60, 0.0, 1.0, cpu_rise=3.0))
            code, out, _ = run("summarise", str(path), "--max-cpu-rise", "0.5")
        self.assertEqual(code, 1, out)
        self.assertIn("FAIL (limit +0.5 points)", out)

    def test_no_limit_judges_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "leak.csv"
            write(path, samples(8, 60, 50.0, 1.0, cpu_rise=10.0))
            code, out, _ = run("summarise", str(path))
        self.assertEqual(code, 0, out)
        self.assertNotIn("PASS", out)
        self.assertNotIn("FAIL", out)


class Refusals(unittest.TestCase):
    def test_one_sample_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "one.csv"
            write(path, samples(0, 60, 0.0, 1.0))
            code, _, err = run("summarise", str(path))
        self.assertEqual(code, 2)
        self.assertIn("at least two samples", err)

    def test_not_a_record_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "other.csv"
            path.write_text("a,b\n1,2\n", encoding="utf-8")
            code, _, err = run("summarise", str(path))
        self.assertEqual(code, 2)
        self.assertIn("not a soak record", err)

    def test_a_header_only_record_is_refused(self):
        # Review of `#188`, round 1: it raised IndexError, exit 1, which reads
        # as a failed limit.
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "empty.csv"
            write(path, [])
            code, _, err = run("summarise", str(path), "--max-cpu-rise", "1")
        self.assertEqual(code, 2)
        self.assertIn("at least two samples", err)

    def test_a_short_row_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "short.csv"
            path.write_text(",".join(soak.FIELDS) + "\n0,1,2\n", encoding="utf-8")
            code, _, err = run("summarise", str(path))
        self.assertEqual(code, 2)
        self.assertIn("not a soak record", err)

    def test_a_record_that_is_not_utf8_is_refused(self):
        # Review of `#188`, round 5: UnicodeDecodeError, a traceback, exit 1.
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "binary.csv"
            path.write_bytes(",".join(soak.FIELDS).encode() + bytes([10, 0xFF, 0xFE, 44, 49, 10]))
            code, _, err = run("summarise", str(path), "--max-cpu-rise", "1")
        self.assertEqual(code, 2, err)
        self.assertIn("not a soak record", err)

    def test_a_malformed_field_is_refused(self):
        # Review of `#188`, round 6: `"x"junk` and a surplus field were
        # accepted and summarised, exit 0.
        for extra in ['"x"junk', "7"]:
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "bad.csv"
                write(path, samples(2, 60, 0.0, 1.0))
                lines = path.read_text(encoding="utf-8").splitlines()
                lines[1] = lines[1] + "," + extra
                path.write_text("\n".join(lines) + "\n", encoding="utf-8")
                code, _, err = run("summarise", str(path))
            self.assertEqual(code, 2, (extra, err))
            self.assertIn("not a soak record", err)

    def test_an_unterminated_quote_is_refused(self):
        # Only strict parsing catches this one: without it, `"0.5` at the end
        # of the file reads as the number 0.5 and the record is summarised.
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "quote.csv"
            write(path, samples(2, 60, 0.0, 1.0))
            text = path.read_text(encoding="utf-8").rstrip("\n")
            head, last = text.rsplit(",", 1)
            path.write_text(head + ',"' + last, encoding="utf-8")
            code, _, err = run("summarise", str(path))
        self.assertEqual(code, 2, err)
        self.assertIn("not a soak record", err)

    def test_a_nan_timestamp_is_refused(self):
        # Review of `#188`, round 4: it reached a ZeroDivisionError, exit 1.
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "nan.csv"
            write(path, samples(2, 60, 0.0, 1.0))
            lines = path.read_text(encoding="utf-8").splitlines()
            lines[1] = "nan," + lines[1].split(",", 1)[1]
            path.write_text("\n".join(lines) + "\n", encoding="utf-8")
            code, _, err = run("summarise", str(path), "--max-cpu-rise", "1")
        self.assertEqual(code, 2, err)
        self.assertIn("not a soak record", err)

    def test_non_finite_numbers_on_the_command_line_are_refused(self):
        # Review of `#188`, round 4: `--every nan` passed `<= 0` and sampled
        # in a tight loop.
        with tempfile.TemporaryDirectory() as tmp:
            out = str(Path(tmp) / "x.csv")
            for flag, value in [("--every", "nan"), ("--hours", "inf"), ("--every", "-inf")]:
                code, _, err = run("record", "--pid", "1", f"{flag}={value}", "--out", out)
                self.assertEqual(code, 2, (flag, value, err))
                self.assertIn("finite and above zero", err)
            path = Path(tmp) / "ok.csv"
            write(path, samples(2, 60, 0.0, 1.0))
            code, _, err = run("summarise", str(path), "--max-cpu-rise", "nan")
        self.assertEqual(code, 2, err)
        self.assertIn("finite number: cpu_rise", err)

    def test_an_existing_record_is_never_written_over(self):
        # Review of `#188`, round 2: a wrong --pid emptied an earlier record.
        if sys.platform != "win32":
            self.skipTest("record reaches the file only on Windows")
        proc = subprocess.Popen(
            [sys.executable, "-I", "-c", IDLE],
            stdin=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        self.addCleanup(proc.wait)
        self.addCleanup(proc.kill)
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "earlier.csv"
            out.write_text("earlier evidence\n", encoding="utf-8")
            for pid in (proc.pid, 4_294_967_293):
                code, _, err = run("record", "--pid", str(pid), "--hours", "0.001", "--out", str(out))
                self.assertEqual(code, 2, err)
            self.assertEqual(out.read_text(encoding="utf-8"), "earlier evidence\n")

    def test_an_output_folder_that_is_not_there_is_refused(self):
        pid = 1
        if sys.platform == "win32":
            # A process it can open, so that the refusal is the folder's.
            proc = subprocess.Popen(
                [sys.executable, "-I", "-c", IDLE],
                stdin=subprocess.DEVNULL,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )
            self.addCleanup(proc.wait)
            self.addCleanup(proc.kill)
            pid = proc.pid
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "no-such-folder" / "x.csv"
            code, _, err = run("record", "--pid", str(pid), "--hours", "0.001", "--out", str(out))
        self.assertEqual(code, 2)
        self.assertIn("cannot write" if sys.platform == "win32" else "run it on Windows", err)

    def test_a_missing_file_is_refused(self):
        code, _, err = run("summarise", "no-such-record.csv")
        self.assertEqual(code, 2)
        self.assertIn("cannot read", err)

    def test_record_off_windows_is_refused(self):
        if sys.platform == "win32":
            self.skipTest("this is the refusal off Windows")
        with tempfile.TemporaryDirectory() as tmp:
            code, _, err = run("record", "--pid", "1", "--hours", "0.001", "--out", str(Path(tmp) / "x.csv"))
        self.assertEqual(code, 2)
        self.assertIn("run it on Windows", err)


LEAKER = """
import time
held = []
for _ in range(60):
    held.append(bytearray(4 * 1024 * 1024))
    time.sleep(0.1)
time.sleep(30)
"""

IDLE = "import time\ntime.sleep(30)\n"


@unittest.skipUnless(sys.platform == "win32", "the recorder reads Windows' own counters")
class RealProcess(unittest.TestCase):
    def child(self, code: str) -> subprocess.Popen:
        proc = subprocess.Popen(
            [sys.executable, "-I", "-c", code],
            stdin=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        # Cleanups run last first: killed, then waited for, so none is left.
        self.addCleanup(proc.wait)
        self.addCleanup(proc.kill)
        time.sleep(0.5)
        return proc

    def slope(self, code: str) -> float:
        proc = self.child(code)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "real.csv"
            reason = soak.record(proc.pid, 0.2, 4 / 3600, path)
            rows = soak.load(path)
        self.assertIn("ran the full", reason)
        self.assertGreater(len(rows), 10)
        return soak.slope_per_hour([(s.seconds, s.private_bytes / MB) for s in rows])

    def test_a_real_leak_rises_and_an_idle_process_does_not(self):
        leak = self.slope(LEAKER)
        idle = self.slope(IDLE)
        # 4 MB every 0.1 s is about 144,000 MB an hour while it runs.
        self.assertGreater(leak, 10_000, f"leaking child: {leak:+.0f} MB/h")
        self.assertLess(abs(idle), leak / 100, f"idle child: {idle:+.0f} MB/h against {leak:+.0f}")

    def test_a_long_interval_never_runs_past_the_deadline(self):
        # Review of `#188`, round 6: `--every 2 --hours 0.0001` asked for
        # 0.36 s and ran 2.13 s. On a fake clock, an interval of 100 s and a
        # run of 36 s: the last wait ends at 36 s, and that sample is the last.
        proc = self.child(IDLE)
        clock = [0.0]

        def now():
            return clock[0]

        def sleep(seconds):
            clock[0] += seconds

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "x.csv"
            soak.record(proc.pid, 100.0, 0.01, out, now=now, sleep=sleep)
            seconds = [s.seconds for s in soak.load(out)]
        self.assertEqual(seconds, [0.0, 36.0])
        self.assertEqual(clock[0], 36.0)

    def test_it_stops_when_the_process_exits(self):
        proc = self.child("import time\ntime.sleep(1)\n")
        with tempfile.TemporaryDirectory() as tmp:
            # Five seconds at most: without the exit check this reads
            # "ran the full" instead of hanging the suite for an hour.
            reason = soak.record(proc.pid, 0.2, 5 / 3600, Path(tmp) / "exit.csv")
        self.assertIn("exited", reason)

    def test_a_failed_lookup_is_not_no_such_process(self):
        # Review of `#188`, round 2: tasklist failing read as "no process".
        failed = subprocess.CompletedProcess(["tasklist"], 1, "", "ERROR: access denied")
        real = soak.subprocess.run
        soak.subprocess.run = lambda *a, **k: failed
        try:
            with self.assertRaises(soak.CannotRun) as caught:
                soak.find_pid("up-take.exe")
        finally:
            soak.subprocess.run = real
        self.assertIn("tasklist failed", str(caught.exception))

    def test_a_failed_liveness_query_is_not_an_exit(self):
        # Review of `#188`, round 3: GetExitCodeProcess failing read as
        # "the process exited", a normal end, exit 0.
        proc = self.child(IDLE)
        real = soak._windows

        def failing():
            ctypes, wintypes, kernel32, user32, counters = real()

            class Kernel:
                def __getattr__(self, name):
                    return getattr(kernel32, name)

                def GetExitCodeProcess(self, *args):  # noqa: N802, the Win32 name
                    return 0

            return ctypes, wintypes, Kernel(), user32, counters

        soak._windows = failing
        try:
            with tempfile.TemporaryDirectory() as tmp:
                code, _, err = run("record", "--pid", str(proc.pid), "--hours", "0.001", "--out", str(Path(tmp) / "x.csv"))
        finally:
            soak._windows = real
        self.assertEqual(code, 2, err)
        self.assertIn("cannot ask whether", err)

    def test_a_failed_gui_count_is_not_zero_objects(self):
        # Review of `#188`, round 4: GetGuiResources answers 0 both for no
        # objects and for a failure, and a failure was recorded as no leak.
        proc = self.child(IDLE)
        real = soak._windows

        def failing():
            ctypes, wintypes, kernel32, user32, counters = real()

            class User:
                def GetGuiResources(self, *args):  # noqa: N802, the Win32 name
                    ctypes.set_last_error(5)
                    return 0

            return ctypes, wintypes, kernel32, User(), counters

        soak._windows = failing
        try:
            process = soak.Process(proc.pid)
            try:
                with self.assertRaises(soak.CannotRun) as caught:
                    process.read(0.0)
            finally:
                process.close()
        finally:
            soak._windows = real
        self.assertIn("cannot count the GDI objects", str(caught.exception))

    def test_every_failed_counter_query_is_refused(self):
        # Review of `#188`, round 7: the GDI case stopped before USER, and no
        # test made the memory, times or handle-count query fail, so ignoring
        # any of those four failures left the suite green. Each fails alone
        # here, every other query real.
        proc = self.child(IDLE)
        real = soak._windows
        cases = [
            ("K32GetProcessMemoryInfo", None, "cannot read the memory"),
            ("GetProcessTimes", None, "cannot read the times"),
            ("GetProcessHandleCount", None, "cannot count the handles"),
            ("GetGuiResources", 1, "cannot count the USER objects"),
        ]
        for name, kind, message in cases:

            def failing(name=name, kind=kind):
                ctypes, wintypes, kernel32, user32, counters = real()

                class Wrap:
                    def __init__(self, inner):
                        self.inner = inner

                    def __getattr__(self, attr):
                        target = getattr(self.inner, attr)
                        if attr != name:
                            return target

                        def fail(*args):
                            if kind is not None and args[1] != kind:
                                return target(*args)
                            ctypes.set_last_error(5)
                            return 0

                        return fail

                return ctypes, wintypes, Wrap(kernel32), Wrap(user32), counters

            soak._windows = failing
            try:
                process = soak.Process(proc.pid)
                try:
                    with self.assertRaises(soak.CannotRun, msg=name) as caught:
                        process.read(0.0)
                finally:
                    process.close()
            finally:
                soak._windows = real
            self.assertIn(message, str(caught.exception), name)

    def test_a_real_zero_gui_count_is_kept(self):
        # The other side: a process with no GUI objects really has 0, and no
        # last error, so it is recorded, not refused.
        proc = self.child(IDLE)
        process = soak.Process(proc.pid)
        try:
            sample = process.read(0.0)
        finally:
            process.close()
        # A windowless Python child owns no GDI or USER objects: exactly 0,
        # so a guard that turned a real 0 into anything else fails here
        # (review of `#188`, round 6: `>= 0` could not tell).
        self.assertEqual((sample.gdi, sample.user), (0, 0))

    def test_a_name_nothing_runs_under_is_refused(self):
        with self.assertRaises(soak.CannotRun) as caught:
            soak.find_pid("no-such-soak-process.exe")
        self.assertIn("no running process", str(caught.exception))

    def test_a_name_several_run_under_is_refused(self):
        first, second = self.child(IDLE), self.child(IDLE)
        name = Path(sys.executable).name
        with self.assertRaises(soak.CannotRun) as caught:
            soak.find_pid(name)
        self.assertIn("pass --pid", str(caught.exception))
        self.assertTrue(first.pid and second.pid)

    def test_no_such_process_is_refused(self):
        with self.assertRaises(soak.CannotRun):
            soak.Process(4_000_000_000 % 2**32 - 3)


if __name__ == "__main__":
    unittest.main(verbosity=2)
