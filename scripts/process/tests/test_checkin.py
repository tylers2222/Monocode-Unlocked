"""Tests for checkin.sh, run as a copy next to stub alarm scripts.

    python3 -m unittest discover -s scripts/process/tests
"""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

PROCESS = Path(__file__).parent.parent
BASH = shutil.which("bash")
CHECKS = ["ready-issues", "pr-cap", "notice-gap"]
SUMMARY = "checkin: {} (ready-issues={} pr-cap={} notice-gap={} missing=0 unmatched=0)"
STUB = """\
import sys
from pathlib import Path
me = Path(__file__)
with open(me.with_name("log"), "a") as log:
    print(me.stem, file=log)
out = me.with_suffix(".out")
print(out.read_text() if out.exists() else f"{me.stem} {sys.argv[1:]}")
print(me.stem, file=sys.stderr)
"""
# name: stub exit codes in CHECKS order, checkin exit code, summary word
CASES = {
    "all ok": ([0, 0, 0], 0, "ok"),
    "alarm": ([1, 0, 0], 1, "alarm"),
    "error before an alarm": ([2, 1, 0], 2, "error"),
    "error after an alarm": ([0, 1, 2], 2, "error"),
    "first check errors, the rest still run": ([2, 0, 0], 2, "error"),
    "unexpected code 3": ([0, 3, 0], 2, "error"),
}
# The fixed-width table of notice-gap.py render(). The ok row's log text says MISSING, and the footer's
# single-digit counts put "unmatched," at the verdict offset.
NOTICE_GAP = """\
logged       session   turn end        notice            delay  verdict    line
10-19 18:38  -         -               -                     -  unmatched  2026-10-19 18:38 AEDT Change R plan v2 done
-            30f5be8b  10-20 16:36:09  -                     -  MISSING    (no finished.log line)
-            9c41d2e7  10-20 17:02:51  -                     -  MISSING    (no finished.log line)
10-19 15:21  4d93f1f2  10-19 15:21:40  10-19 15:21:40     0.0m  ok         2026-10-19 15:21 M-9a2 #92 MISSING rows fixed, done
3 monitored turn ends (1 with a finished.log line), 1 log lines unmatched, 2 late or missing notices (more than 10 min)"""


def run(codes, notice_gap_out=None, without_python3=False, from_its_dir=False):
    """Run a copy of checkin.sh next to stubs that exit with codes; result.ran lists the stubs that ran, in order.

    The notice-gap stub prints notice_gap_out when given, else its name and arguments like the other stubs.
    bash gets the copy's absolute path from an empty cwd, or with from_its_dir `checkin.sh` from the copy's directory.
    """
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        empty = tmp / "empty"
        empty.mkdir()
        shutil.copy(PROCESS / "checkin.sh", tmp)
        for name, code in zip(CHECKS, codes):
            (tmp / f"{name}.py").write_text(f"{STUB}sys.exit({code})\n")
        if notice_gap_out is not None:
            (tmp / "notice-gap.out").write_text(notice_gap_out)
        script, cwd = ("checkin.sh", tmp) if from_its_dir else (tmp / "checkin.sh", empty)
        env = {**os.environ, "PATH": str(empty)} if without_python3 else None
        result = subprocess.run([BASH, script], cwd=cwd, env=env, capture_output=True, text=True)
        log = tmp / "log"
        result.ran = log.read_text().split() if log.exists() else []
        return result


class Checkin(unittest.TestCase):
    def assertCheckin(self, codes, status, last_line, **how):
        result = run(codes, **how)
        self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        self.assertEqual(result.stdout.splitlines()[-1], last_line)
        self.assertEqual(result.ran, CHECKS)
        return result

    def test_status_is_the_worst_check(self):
        for case, (codes, status, word) in CASES.items():
            with self.subTest(case):
                self.assertCheckin(codes, status, SUMMARY.format(word, *codes))

    def test_missing_and_unmatched_rows_are_an_alarm(self):
        for case, codes, status, last_line in [
            ("rows alone", [0, 0, 0], 1, "checkin: alarm (ready-issues=0 pr-cap=0 notice-gap=0 missing=2 unmatched=1)"),
            ("an error still wins", [0, 0, 2], 2, "checkin: error (ready-issues=0 pr-cap=0 notice-gap=2 missing=2 unmatched=1)"),
        ]:
            with self.subTest(case):
                result = self.assertCheckin(codes, status, last_line, notice_gap_out=NOTICE_GAP)
                self.assertIn(f"== notice-gap\n{NOTICE_GAP}\n", result.stdout)

    def test_runs_from_its_own_directory(self):
        self.assertCheckin([0, 0, 0], 0, SUMMARY.format("ok", 0, 0, 0), from_its_dir=True)

    def test_each_check_runs_under_its_header_with_its_flags(self):
        result = run([0, 0, 0])
        self.assertEqual(result.stdout, """\
== ready-issues
ready-issues ['--instance', '2']
== pr-cap
pr-cap ['--instance', '2']
== notice-gap
notice-gap []
checkin: ok (ready-issues=0 pr-cap=0 notice-gap=0 missing=0 unmatched=0)
""")
        self.assertEqual(result.stderr, "ready-issues\npr-cap\nnotice-gap\n")

    def test_missing_python3_is_an_error(self):
        result = run([0, 0, 0], without_python3=True)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(result.stdout, """\
== ready-issues
== pr-cap
== notice-gap
checkin: error (ready-issues=127 pr-cap=127 notice-gap=127 missing=0 unmatched=0)
""")


if __name__ == "__main__":
    unittest.main()
