"""Tests for checkin.sh, run as a copy next to stub alarm scripts or the real notice-gap.py.

    python3 -m unittest discover -s scripts/process/tests
"""
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
PROCESS = HERE.parent
LOG = HERE / "fixtures/notice-gap-finished-log.txt"
BASH = shutil.which("bash")
CHECKS = ["ready-issues", "pr-cap", "notice-gap"]
SUMMARY = "checkin: {} (ready-issues={} pr-cap={} notice-gap={} missing=0 unmatched=0)"
STUB = """\
import sys
from pathlib import Path
me = Path(__file__)
with open(me.with_name("log"), "a") as log:
    print(me.stem, file=log)
print(me.stem, sys.argv[1:], file=sys.stderr)
out = me.with_suffix(".out")
if out.exists():
    print(out.read_text(), end="")
"""
# notice-gap.py itself, on the fixture database and a finished.log, at a fixed time.
REAL_NOTICE_GAP = """\
import os, sys
from pathlib import Path
with open(Path(__file__).with_name("log"), "a") as log:
    print("notice-gap", file=log)
os.execv(sys.executable, [sys.executable, {script!r}, {log!r}, "--db", {db!r}, "--now", {now!r}, *sys.argv[1:]])
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


def at(clock):
    return f"2026-10-20T{clock}:00+11:00"


class Checkin(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.folder = tempfile.TemporaryDirectory()
        cls.db = Path(cls.folder.name) / "monocode.db"
        with sqlite3.connect(cls.db) as con:
            con.executescript((HERE / "fixtures/notice-gap.sql").read_text())
        empty_log = Path(cls.folder.name) / "empty.log"
        empty_log.write_text("")
        # The table notice-gap prints when nothing is due: the notice-gap stub prints it.
        cls.no_rows = subprocess.run(
            [sys.executable, PROCESS / "notice-gap.py", empty_log, "--db", cls.db, "--now", at("03:00"), "--hours", "1"],
            capture_output=True, text=True, check=True,
        ).stdout

    @classmethod
    def tearDownClass(cls):
        cls.folder.cleanup()

    def run_checkin(self, codes=(0, 0, 0), notice_gap_out=None, now=None, log=LOG, from_its_dir=False, **env):
        """Run a copy of checkin.sh next to stubs that exit with codes (None: hang); result.ran lists what ran.

        The notice-gap stub prints notice_gap_out, by default the real empty table. With now, notice-gap.py
        is the real script on the fixtures at that time. env keys are set; PATH="" leaves no python3 or perl.
        """
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            empty = tmp / "empty"
            empty.mkdir()
            shutil.copy(PROCESS / "checkin.sh", tmp)
            for name, code in zip(CHECKS, codes):
                end = "import time\ntime.sleep(60)\n" if code is None else f"sys.exit({code})\n"
                (tmp / f"{name}.py").write_text(STUB + end)
            (tmp / "notice-gap.out").write_text(self.no_rows if notice_gap_out is None else notice_gap_out)
            if now:
                (tmp / "notice-gap.py").write_text(REAL_NOTICE_GAP.format(
                    script=str(PROCESS / "notice-gap.py"), log=str(log), db=str(self.db), now=at(now)))
            script, cwd = ("checkin.sh", tmp) if from_its_dir else (tmp / "checkin.sh", empty)
            env = {**os.environ, **env}
            if env.get("PATH") == "":
                env["PATH"] = str(empty)
            result = subprocess.run([BASH, script], cwd=cwd, env=env, capture_output=True, text=True)
            log_file = tmp / "log"
            result.ran = log_file.read_text().split() if log_file.exists() else []
            return result

    def assertCheckin(self, status, last_line, *args, **kwargs):
        result = self.run_checkin(*args, **kwargs)
        self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        self.assertEqual(result.stdout.splitlines()[-1], last_line)
        self.assertEqual(result.ran, CHECKS)
        return result

    def test_status_is_the_worst_check(self):
        for case, (codes, status, word) in CASES.items():
            with self.subTest(case):
                self.assertCheckin(status, SUMMARY.format(word, *codes), codes)

    def test_notice_gap_judges_only_the_window(self):
        # In the fixtures, 05:30-09:30 and 15:30-18:30 have late or missing notices; 12:30-14:30 has none.
        with tempfile.TemporaryDirectory() as tmp:
            # The 18:19 line matches an ok turn; its text says MISSING, which must not count.
            log = Path(tmp) / "finished.log"
            log.write_text(LOG.read_text().replace("resumed after Ship 5:", "resumed after Ship 5 (MISSING no more):"))
            for case, now, env, codes, status, last_line in [
                ("old rows outside the window", "14:30", {}, [0, 0],
                 0, "checkin: ok (ready-issues=0 pr-cap=0 notice-gap=0 missing=0 unmatched=0)"),
                ("the same rows in a wider CHECKIN_HOURS (3 late notices)", "14:30", {"CHECKIN_HOURS": "10"}, [0, 0],
                 1, "checkin: alarm (ready-issues=0 pr-cap=0 notice-gap=1 missing=0 unmatched=5)"),
                ("new missing notices", "18:30", {}, [0, 0],
                 1, "checkin: alarm (ready-issues=0 pr-cap=0 notice-gap=1 missing=4 unmatched=5)"),
                ("finished.log rows with no notice turn, notice-gap itself ok", "12:00", {}, [0, 0],
                 1, "checkin: alarm (ready-issues=0 pr-cap=0 notice-gap=0 missing=0 unmatched=2)"),
                ("an error still wins over the rows", "12:00", {}, [2, 0],
                 2, "checkin: error (ready-issues=2 pr-cap=0 notice-gap=0 missing=0 unmatched=2)"),
            ]:
                with self.subTest(case):
                    result = self.assertCheckin(status, last_line, codes, now=now, log=log, **env)
                    self.assertIn("monitored turn ends", result.stdout)

    def test_notice_gap_without_its_table_is_an_error(self):
        for case, code, out in [
            ("a crash", 1, ""),
            ("a renamed verdict column", 0, self.no_rows.replace(" verdict ", " result  ")),
        ]:
            with self.subTest(case):
                self.assertCheckin(2, SUMMARY.format("error", 0, 0, 2), [0, 0, code], notice_gap_out=out)

    def test_a_hanging_script_is_stopped_and_the_rest_still_run(self):
        self.assertCheckin(2, SUMMARY.format("error", 142, 0, 0), [None, 0, 0], CHECKIN_TIMEOUT="1")

    def test_runs_from_its_own_directory(self):
        self.assertCheckin(0, SUMMARY.format("ok", 0, 0, 0), from_its_dir=True)

    def test_each_check_runs_under_its_header_with_its_flags(self):
        result = self.run_checkin()
        self.assertEqual(result.stdout, f"""\
== ready-issues
== pr-cap
== notice-gap
{self.no_rows}checkin: ok (ready-issues=0 pr-cap=0 notice-gap=0 missing=0 unmatched=0)
""")
        self.assertEqual(result.stderr, """\
ready-issues ['--instance', '1']
pr-cap ['--instance', '1']
notice-gap ['--hours', '2']
""")

    def test_missing_tools_are_an_error(self):
        result = self.run_checkin(PATH="")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(result.stdout, """\
== ready-issues
== pr-cap
== notice-gap
checkin: error (ready-issues=127 pr-cap=127 notice-gap=2 missing=0 unmatched=0)
""")


if __name__ == "__main__":
    unittest.main()
