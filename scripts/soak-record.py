#!/usr/bin/env python3
"""Record a process's memory, handles and CPU over a long run, and report the trend.

Backlog `I-489`, for gate item `G1-5` and `SPECS/quality-bars.md` row `M-20`:
*"Areas left running for 8 hours: no leak, no drift, no creeping CPU."* This is
the recording half. It does not place areas or start UP-TAKE: the person
running the soak sets the areas up and leaves UP-TAKE running, and this script
watches the process from outside.

    python scripts/soak-record.py record --name up-take.exe --every 30 --hours 8 --out soak.csv
    python scripts/soak-record.py summarise soak.csv
    python scripts/soak-record.py summarise soak.csv --max-private-mb-per-hour 1 --max-cpu-rise 0.5

`record` writes one CSV row per sample: seconds since the start, private bytes,
working set, kernel handles, GDI objects, USER objects and CPU seconds (user
plus kernel). It stops at `--hours`, or when the process exits, and says which.
A row is flushed as it is written, so a run cut short keeps what it read.

`summarise` prints, per measure, the mean of the first and of the last hour and
the least-squares slope per hour over the whole run, and the CPU use (percent
of one core) of the first and the last hour. **It judges nothing unless given a
limit**: what counts as a leak or a drift is not written anywhere yet (`M-20`
names none), so the limits are options the founder sets. With a limit given,
each measure prints `PASS` or `FAIL` and the exit code is 1 on any `FAIL`.
Proposed, not decided: private bytes and working set rising under 1 MB an hour,
handles and GDI and USER objects under 1 an hour, and the last hour's CPU no
more than 0.5 points of one core above the first hour's.

Windows only for `record` (the counters are Windows' own, read through
`ctypes`, so nothing is installed). `summarise` runs anywhere. Every child
process this script starts runs without a console window: on 2026-11-25 a
background job's children took the founder's keyboard focus every second.

Exit codes: 0 done (or every limit passed); 1 a limit failed; 2 it could not
run (no such process, not Windows, an unreadable file, too few samples).
"""

from __future__ import annotations

import argparse
import csv
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

FIELDS = ("seconds", "private_bytes", "working_set", "handles", "gdi", "user", "cpu_seconds")
HOUR = 3600.0
MB = 1024 * 1024


class CannotRun(Exception):
    """The run could not start or the file could not be read (exit 2)."""


@dataclass(frozen=True)
class Sample:
    seconds: float
    private_bytes: int
    working_set: int
    handles: int
    gdi: int
    user: int
    cpu_seconds: float


# --- reading a process (Windows) ------------------------------------------------


def _windows():
    """The Win32 calls `record` needs, or `CannotRun` off Windows."""
    if sys.platform != "win32":
        raise CannotRun("record reads Windows' own process counters; run it on Windows")
    import ctypes
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        # PROCESS_MEMORY_COUNTERS_EX: PrivateUsage is the private bytes.
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
            ("PrivateUsage", ctypes.c_size_t),
        ]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.K32GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    kernel32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    kernel32.GetProcessHandleCount.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    user32.GetGuiResources.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    user32.GetGuiResources.restype = wintypes.DWORD
    return ctypes, wintypes, kernel32, user32, Counters


QUERY_LIMITED = 0x1000  # PROCESS_QUERY_LIMITED_INFORMATION
SYNCHRONIZE = 0x00100000
STILL_ACTIVE = 259


class Process:
    """An open handle on one process, read without changing it."""

    def __init__(self, pid: int):
        self.ctypes, self.wintypes, self.kernel32, self.user32, self.Counters = _windows()
        self.pid = pid
        self.handle = self.kernel32.OpenProcess(QUERY_LIMITED | SYNCHRONIZE, False, pid)
        if not self.handle:
            raise CannotRun(f"cannot open process {pid} (error {self.ctypes.get_last_error()})")

    def close(self) -> None:
        if self.handle:
            self.kernel32.CloseHandle(self.handle)
            self.handle = None

    def alive(self) -> bool:
        code = self.wintypes.DWORD()
        return bool(self.kernel32.GetExitCodeProcess(self.handle, self.ctypes.byref(code))) and (
            code.value == STILL_ACTIVE
        )

    def read(self, seconds: float) -> Sample:
        ct, wt = self.ctypes, self.wintypes
        counters = self.Counters()
        counters.cb = ct.sizeof(self.Counters)
        if not self.kernel32.K32GetProcessMemoryInfo(self.handle, ct.byref(counters), counters.cb):
            raise CannotRun(f"cannot read the memory of process {self.pid} (error {ct.get_last_error()})")
        times = [wt.FILETIME() for _ in range(4)]
        if not self.kernel32.GetProcessTimes(self.handle, *[ct.byref(t) for t in times]):
            raise CannotRun(f"cannot read the times of process {self.pid} (error {ct.get_last_error()})")
        handles = wt.DWORD()
        if not self.kernel32.GetProcessHandleCount(self.handle, ct.byref(handles)):
            raise CannotRun(f"cannot count the handles of process {self.pid} (error {ct.get_last_error()})")

        def ticks(t) -> int:  # 100-nanosecond units
            return (t.dwHighDateTime << 32) | t.dwLowDateTime

        return Sample(
            seconds=seconds,
            private_bytes=int(counters.PrivateUsage),
            working_set=int(counters.WorkingSetSize),
            handles=int(handles.value),
            gdi=int(self.user32.GetGuiResources(self.handle, 0)),
            user=int(self.user32.GetGuiResources(self.handle, 1)),
            cpu_seconds=(ticks(times[2]) + ticks(times[3])) / 1e7,
        )


def find_pid(name: str) -> int:
    """The one running process with image name `name`; refuses none and several."""
    out = subprocess.run(
        ["tasklist", "/FO", "CSV", "/NH", "/FI", f"IMAGENAME eq {name}"],
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        check=False,
    )
    rows = [row for row in csv.reader(out.stdout.splitlines()) if len(row) > 1 and row[0].lower() == name.lower()]
    if not rows:
        raise CannotRun(f"no running process is named {name}")
    if len(rows) > 1:
        pids = ", ".join(row[1] for row in rows)
        raise CannotRun(f"{len(rows)} processes are named {name} ({pids}); pass --pid")
    return int(rows[0][1])


def record(pid: int, every: float, hours: float, out: Path, now=time.monotonic, sleep=time.sleep) -> str:
    """Samples `pid` every `every` seconds for `hours`; returns why it stopped."""
    process = Process(pid)
    try:
        start = now()
        with out.open("w", newline="", encoding="utf-8") as file:
            writer = csv.writer(file)
            writer.writerow(FIELDS)
            while True:
                elapsed = now() - start
                if not process.alive():
                    return f"process {pid} exited after {elapsed / HOUR:.2f} h"
                sample = process.read(elapsed)
                writer.writerow(
                    [
                        f"{getattr(sample, f):.3f}" if f in ("seconds", "cpu_seconds") else getattr(sample, f)
                        for f in FIELDS
                    ]
                )
                file.flush()
                if elapsed >= hours * HOUR:
                    return f"ran the full {hours:g} h"
                sleep(max(0.0, every - (now() - start - elapsed)))
    finally:
        process.close()


# --- the trend (anywhere) -------------------------------------------------------


def load(path: Path) -> list[Sample]:
    try:
        with path.open(newline="", encoding="utf-8") as file:
            rows = list(csv.DictReader(file))
    except OSError as e:
        raise CannotRun(f"cannot read {path}: {e}") from e
    try:
        return [
            Sample(
                seconds=float(row["seconds"]),
                private_bytes=int(row["private_bytes"]),
                working_set=int(row["working_set"]),
                handles=int(row["handles"]),
                gdi=int(row["gdi"]),
                user=int(row["user"]),
                cpu_seconds=float(row["cpu_seconds"]),
            )
            for row in rows
        ]
    except (KeyError, ValueError) as e:
        raise CannotRun(f"{path} is not a soak record: {e}") from e


def slope_per_hour(points: list[tuple[float, float]]) -> float:
    """The least-squares slope of `points` (seconds, value), per hour."""
    n = len(points)
    if n < 2:
        raise CannotRun("a trend needs at least two samples")
    mean_t = sum(t for t, _ in points) / n
    mean_v = sum(v for _, v in points) / n
    spread = sum((t - mean_t) ** 2 for t, _ in points)
    if spread == 0:
        raise CannotRun("every sample has the same time")
    return sum((t - mean_t) * (v - mean_v) for t, v in points) / spread * HOUR


def window(samples: list[Sample], first: bool, span: float = HOUR) -> list[Sample]:
    """The samples of the run's first (or last) `span` seconds; the whole run if shorter."""
    if first:
        end = samples[0].seconds + span
        return [s for s in samples if s.seconds <= end]
    start = samples[-1].seconds - span
    return [s for s in samples if s.seconds >= start]


def cpu_percent(samples: list[Sample]) -> float:
    """CPU use over `samples`, in percent of one core."""
    wall = samples[-1].seconds - samples[0].seconds
    if wall <= 0:
        raise CannotRun("a CPU figure needs two samples at different times")
    return (samples[-1].cpu_seconds - samples[0].cpu_seconds) / wall * 100


MEASURES = (
    ("private_bytes", "private bytes", MB, "MB"),
    ("working_set", "working set", MB, "MB"),
    ("handles", "kernel handles", 1, ""),
    ("gdi", "GDI objects", 1, ""),
    ("user", "USER objects", 1, ""),
)


def summarise(samples: list[Sample], limits: dict[str, float]) -> tuple[list[str], bool]:
    """The report's lines, and whether every limit given passed."""
    # Fewer than two samples is refused by `slope_per_hour`, below.
    span = samples[-1].seconds - samples[0].seconds if samples else 0.0
    lines = [f"{len(samples)} samples over {span / HOUR:.2f} h"]
    passed = True
    first, last = window(samples, True), window(samples, False)
    for field, label, unit, suffix in MEASURES:
        slope = slope_per_hour([(s.seconds, getattr(s, field)) for s in samples]) / unit
        start = sum(getattr(s, field) for s in first) / len(first) / unit
        end = sum(getattr(s, field) for s in last) / len(last) / unit
        line = f"{label:15} first hour {start:10.2f}{suffix}  last hour {end:10.2f}{suffix}  slope {slope:+.3f}{suffix}/h"
        limit = limits.get(field)
        if limit is not None:
            ok = slope <= limit
            passed &= ok
            line += f"  {'PASS' if ok else 'FAIL'} (limit {limit:+g}{suffix}/h)"
        lines.append(line)
    cpu_first, cpu_last = cpu_percent(first), cpu_percent(last)
    line = f"{'CPU':15} first hour {cpu_first:9.2f}%  last hour {cpu_last:9.2f}%  rise {cpu_last - cpu_first:+.2f} points"
    limit = limits.get("cpu_rise")
    if limit is not None:
        ok = cpu_last - cpu_first <= limit
        passed &= ok
        line += f"  {'PASS' if ok else 'FAIL'} (limit {limit:+g} points)"
    lines.append(line)
    return lines, passed


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    rec = sub.add_parser("record", help="sample a process into a CSV file")
    who = rec.add_mutually_exclusive_group(required=True)
    who.add_argument("--pid", type=int)
    who.add_argument("--name", help="image name, e.g. up-take.exe; refused unless exactly one runs")
    rec.add_argument("--every", type=float, default=30.0, help="seconds between samples (default 30)")
    rec.add_argument("--hours", type=float, default=8.0, help="how long to record (default 8)")
    rec.add_argument("--out", type=Path, required=True)
    summ = sub.add_parser("summarise", help="report the trend of a record")
    summ.add_argument("record", type=Path)
    summ.add_argument("--max-private-mb-per-hour", type=float)
    summ.add_argument("--max-working-set-mb-per-hour", type=float)
    summ.add_argument("--max-handles-per-hour", type=float)
    summ.add_argument("--max-gdi-per-hour", type=float)
    summ.add_argument("--max-user-per-hour", type=float)
    summ.add_argument("--max-cpu-rise", type=float, help="points of one core, last hour over first")
    args = parser.parse_args(argv)
    try:
        if args.command == "record":
            if args.every <= 0 or args.hours <= 0:
                raise CannotRun("--every and --hours must be above zero")
            pid = args.pid if args.pid is not None else find_pid(args.name)
            print(f"soak-record: recording process {pid} every {args.every:g} s for {args.hours:g} h into {args.out}")
            print(f"soak-record: {record(pid, args.every, args.hours, args.out)}")
            return 0
        limits = {
            "private_bytes": args.max_private_mb_per_hour,
            "working_set": args.max_working_set_mb_per_hour,
            "handles": args.max_handles_per_hour,
            "gdi": args.max_gdi_per_hour,
            "user": args.max_user_per_hour,
            "cpu_rise": args.max_cpu_rise,
        }
        lines, passed = summarise(load(args.record), {k: v for k, v in limits.items() if v is not None})
        print("\n".join(lines))
        return 0 if passed else 1
    except CannotRun as e:
        print(f"soak-record: could not run: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
