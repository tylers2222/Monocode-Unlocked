"""Run: python3 scripts/process/notice-gap.test.py

The fixtures are recorded from a .backup copy of the MonoCode 2 database
(2026-10-20 22:07) and finished.log, for 10-20 04:30 to 18:25: the four
ADE-68 late notices, the ship 5 relaunch, and the on-time notices between.
"""

import importlib.util
import re
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).parent
SCRIPT = HERE / "notice-gap.py"
LOG = HERE / "fixtures/finished-log.txt"

spec = importlib.util.spec_from_file_location("notice_gap", SCRIPT)
notice_gap = sys.modules["notice_gap"] = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notice_gap)


ROW = re.compile(
    r"^\S+ \S+\s+(\w{8})\s+(\S+ \S+)\s+(?:\S+ \S+|-)\s+(\S+)\s+(LATE|MISSING) ", re.M
)


def rows(output: str) -> dict[str, tuple[str, str]]:
    """Turn end -> (session, delay and verdict), for each LATE or MISSING row."""
    return {end: (session, f"{delay} {verdict}") for session, end, delay, verdict in ROW.findall(output)}


class NoticeGap(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.folder = tempfile.TemporaryDirectory()
        cls.db = Path(cls.folder.name) / "monocode.db"
        with sqlite3.connect(cls.db) as con:
            con.executescript((HERE / "fixtures/notice-gap.sql").read_text())

    @classmethod
    def tearDownClass(cls):
        cls.folder.cleanup()

    def run_script(self, now: str, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, SCRIPT, LOG, "--db", self.db, "--now", now, *args],
            capture_output=True,
            text=True,
        )

    def test_flags_the_four_late_notices_and_the_ship5_relaunch(self):
        run = self.run_script("2026-10-20T18:25:00+11:00")
        self.assertEqual(run.returncode, 1, run.stdout + run.stderr)
        self.assertEqual(
            rows(run.stdout),
            {
                "10-20 05:04:18": ("5ac93608", "34.9m LATE"),
                "10-20 05:42:54": ("77083e1b", "79.1m LATE"),
                "10-20 07:36:51": ("89a0543f", "115.2m LATE"),
                "10-20 15:12:47": ("5c2bdca3", "13.2m LATE"),
                # The relaunch at 17:19:47 dropped these; the 18:20:15 notice
                # reports 7a1eab69's next turn, so it does not count for 17:57:51.
                "10-20 17:19:35": ("7a1eab69", "- MISSING"),
                "10-20 17:19:37": ("872c4aa7", "- MISSING"),
                "10-20 17:57:51": ("7a1eab69", "- MISSING"),
            },
        )
        self.assertIn("51 worker ends, 8 late or missing notices", run.stdout)

    def test_on_time_notices_after_the_relaunch_exit_0(self):
        run = self.run_script("2026-10-20T18:25:00+11:00", "--hours", "0.12")
        self.assertEqual(run.returncode, 0, run.stdout)
        self.assertIn("1.0m  ok", run.stdout)
        self.assertIn("1 worker ends, 0 late or missing notices", run.stdout)

    def test_an_end_without_a_notice_is_pending_for_10_minutes_then_missing(self):
        pending = self.run_script("2026-10-20T18:02:00+11:00", "--hours", "0.1")
        self.assertEqual(pending.returncode, 0, pending.stdout)
        self.assertIn("pending", pending.stdout)
        missing = self.run_script("2026-10-20T18:08:30+11:00", "--hours", "0.2")
        self.assertEqual(missing.returncode, 1, missing.stdout)
        self.assertIn("MISSING", missing.stdout)

    def test_reads_every_time_format_in_finished_log(self):
        entries = notice_gap.parse_log(
            "2026-10-20T00:24:28Z PR #141 ADE-47 r3 review done\n"
            "2026-10-20 05:31:22 +1100 ADE-20 R2 #132 round 1 fixed\n"
            "2026-10-20 05:42 AEDT PR #132 r2 review done\n"
            "M-11 #159 open at e5f15fd8. 2026-10-20 21:09 AEDT (ping failed)\n"
            "a line with no time\n"
        )
        self.assertEqual(
            [entry.at.astimezone(timezone.utc) for entry in entries],
            [
                datetime(2026, 10, 20, 0, 24, 28, tzinfo=timezone.utc),
                datetime(2026, 10, 19, 18, 31, 22, tzinfo=timezone.utc),
                datetime(2026, 10, 19, 18, 42, tzinfo=timezone.utc),
                datetime(2026, 10, 20, 10, 9, tzinfo=timezone.utc),
            ],
        )

    def test_a_shared_head_sha_beats_the_nearer_turn_end(self):
        # 10-20 00:02:54: the #119 reviewer ended 2 s before the #116 fixer.
        (entry,) = notice_gap.parse_log(
            "2026-10-20 00:02:54 +1100 PR7-13 #116 round 1 fixed, head 780966d0"
        )
        at = lambda clock: datetime.fromisoformat(f"2026-10-20T{clock}+11:00")
        reviewer = notice_gap.TurnEnd(
            "0d2f737d", at("00:03:05"), "I reviewed round 1 of #119 at `8b9e2177`."
        )
        fixer = notice_gap.TurnEnd(
            "7a1eab69", at("00:03:07"), "Round 1 of #116 is fixed. The new head is `780966d0`."
        )
        self.assertIs(notice_gap.match_end(entry, [reviewer, fixer]), fixer)


if __name__ == "__main__":
    unittest.main()
