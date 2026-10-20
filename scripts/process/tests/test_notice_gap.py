"""Tests for notice-gap.py.

    python3 -m unittest discover -s scripts/process/tests

The fixtures are recorded from a .backup copy of the MonoCode 2 database
(2026-10-20 22:07) and finished.log, for 10-19 23:05 to 23:25 and 10-20 04:30
to 18:25: the four ADE-68 late notices, the ship 5 relaunch, the 10-19 23:20
send without notifyOnComplete, and the on-time notices between. Each worker
report keeps only its head SHAs and PR numbers; the script reads no more.
"""

import importlib.util
import re
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).parent
SCRIPT = HERE.parent / "notice-gap.py"
LOG = HERE / "fixtures/notice-gap-finished-log.txt"

spec = importlib.util.spec_from_file_location("notice_gap", SCRIPT)
notice_gap = sys.modules["notice_gap"] = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notice_gap)

ROW = re.compile(
    r"^\S+(?: \S+)?\s+(\w{8})\s+(\S+ \S+)\s+(?:\S+ \S+|-)\s+(\S+)\s+(\S+)\s+(.*)$", re.M
)
AT = lambda clock: datetime.fromisoformat(f"2026-10-20T{clock}+11:00")


def rows(output: str) -> dict[tuple[str, str], tuple[str, str]]:
    """(session, turn end) -> (delay, verdict) for each turn row."""
    return {(s, end): (delay, verdict) for s, end, delay, verdict, _ in ROW.findall(output)}


def failing(output: str) -> dict[tuple[str, str], tuple[str, str]]:
    return {key: value for key, value in rows(output).items() if value[1] in ("LATE", "MISSING")}


LANE_B = "app-86984142-5f1a-4018-af62-ddb105cb6cb4-2c24f0c3-b5f8-496f-8b47-48a831440f92"
LANE_D = "app-86984142-5f1a-4018-af62-ddb105cb6cb4-a9e910e7-6f7f-4a2e-8eae-f46866931919"


def end(session: str, at: datetime, report: str = "", first: bool = False, prompt: str = "Brief"):
    return notice_gap.TurnEnd(session, at - timedelta(minutes=5), at, report, first, (prompt,))


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

    def run_script(self, now: str, *args: str, log: Path = LOG) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, SCRIPT, log, "--db", self.db, "--now", now, *args],
            capture_output=True,
            text=True,
        )

    def test_flags_the_four_late_notices_and_the_ship5_drops(self):
        run = self.run_script("2026-10-20T18:25:00+11:00")
        self.assertEqual(run.returncode, 1, run.stdout + run.stderr)
        self.assertEqual(
            failing(run.stdout),
            {
                ("5ac93608", "10-20 05:04:18"): ("34.9m", "LATE"),
                ("77083e1b", "10-20 05:42:54"): ("79.1m", "LATE"),
                ("89a0543f", "10-20 07:36:51"): ("115.2m", "LATE"),
                ("5c2bdca3", "10-20 15:12:47"): ("13.2m", "LATE"),
                # The relaunch at 17:19:47 dropped these. The 18:20:15 notice
                # reports 7a1eab69's next turn, so it does not count for 17:57:51.
                ("30f5be8b", "10-20 16:36:09"): ("-", "MISSING"),
                ("7a1eab69", "10-20 17:19:35"): ("-", "MISSING"),
                ("872c4aa7", "10-20 17:19:35"): ("-", "MISSING"),
                ("872c4aa7", "10-20 17:19:37"): ("-", "MISSING"),
                ("7a1eab69", "10-20 17:57:51"): ("-", "MISSING"),
            },
        )
        self.assertIn("9 late or missing notices", run.stdout)

    def test_a_send_without_notify_on_complete_is_not_asked(self):
        run = self.run_script("2026-10-19T23:30:00+11:00", "--hours", "0.5")
        self.assertEqual(run.returncode, 0, run.stdout)
        found = rows(run.stdout)
        self.assertEqual(found[("780ac286", "10-19 23:19:35")], ("0.3m", "ok"))
        self.assertEqual(found[("780ac286", "10-19 23:20:50")], ("-", "not-asked"))

    def test_turns_with_no_log_line_still_get_a_row(self):
        with tempfile.NamedTemporaryFile("w", suffix=".txt") as empty:
            run = self.run_script("2026-10-20T18:25:00+11:00", "--hours", "14", log=Path(empty.name))
        self.assertEqual(run.returncode, 1, run.stdout)
        self.assertEqual(rows(run.stdout)[("89a0543f", "10-20 07:36:51")], ("115.2m", "LATE"))
        self.assertIn("(0 with a finished.log line)", run.stdout)

    def test_a_line_with_no_shared_identity_is_unmatched(self):
        run = self.run_script("2026-10-20T05:10:00+11:00", "--hours", "0.25")
        # 05:00:17 is another instance's ADE-52 line; 4746761f ended 2 min later.
        self.assertRegex(run.stdout, r"10-20 05:00 +- .* unmatched +2026-10-20 05:00:17")
        self.assertRegex(run.stdout, r"10-20 05:01 +4746761f +10-20 05:02:23 .* ok +2026-10-20 05:01:50")
        self.assertIn("1 log lines unmatched", run.stdout)
        # The 05:39:13 notice comes after --now, so 5ac93608 is still in time.
        self.assertEqual(rows(run.stdout)[("5ac93608", "10-20 05:04:18")], ("-", "pending"))
        self.assertEqual(run.returncode, 0, run.stdout)

    def test_a_line_that_names_the_session_matches_without_a_sha(self):
        # Status lines use the first 8 characters of the session's own uuid.
        (entry,) = notice_gap.parse_log("2026-10-20 23:27 AEDT lane-d a9e910e7 D3 opened")
        named = end(LANE_D, AT("23:28:00"))
        other = end(LANE_B, AT("23:27:10"), "D3 is opened")
        self.assertIs(notice_gap.match_end(entry, [other, named]), named)

    def test_each_turn_gets_one_row_whatever_the_number_of_lines(self):
        turn = end(LANE_B, AT("10:00:00"), "head 1234567")
        lines = notice_gap.parse_log(
            "2026-10-20 09:59 head 1234567 done\n2026-10-20 10:00 addendum for 1234567\n"
        )
        gaps = notice_gap.gaps(lines, [turn], [], [], AT("09:00:00"), AT("10:05:00"))
        self.assertEqual([(gap.end, len(gap.lines), gap.verdict) for gap in gaps], [(turn, 2, "pending")])

    def test_late_means_more_than_ten_minutes(self):
        turn = end(LANE_B, AT("10:00:00"), first=True)
        notice = lambda after: notice_gap.Notice(turn.at + after, frozenset({(LANE_B, "Brief")}))
        verdict = lambda after: notice_gap.verdict(turn, notice(after), [], AT("12:00:00"))
        self.assertEqual(verdict(timedelta(minutes=10)), "ok")
        self.assertEqual(verdict(timedelta(minutes=10.01)), "LATE")

    def test_missing_means_no_notice_for_more_than_ten_minutes(self):
        turn = end(LANE_B, AT("10:00:00"), first=True)
        self.assertEqual(notice_gap.verdict(turn, None, [], AT("10:10:00")), "pending")
        self.assertEqual(notice_gap.verdict(turn, None, [], AT("10:10:00.600")), "MISSING")

    def test_a_notice_counts_for_the_turn_whose_prompt_it_quotes(self):
        # Lane A, 10-20: a queued follow-up started as the first turn ended, and
        # the first turn's notice came after the follow-up had ended too.
        first = end(LANE_B, AT("23:50:50"), first=True, prompt="Process fixes, lane a. Read the brief")
        follow_up = end(LANE_B, AT("23:51:53"), prompt="#164 review r1: 0B/0M/3m")
        notices = [
            notice_gap.Notice(AT("23:53:28"), frozenset({(LANE_B, "Process fixes, lane a.")})),
            notice_gap.Notice(AT("23:54:20"), frozenset({(LANE_B, "#164 review r1: 0B/0M/3m")})),
        ]
        self.assertEqual(notice_gap.notice_for(first, notices), notices[0])
        self.assertEqual(notice_gap.notice_for(follow_up, notices), notices[1])

    def test_a_steer_with_notify_makes_a_turn_asked_and_its_notice_counts(self):
        # A turn opened by a send without notifyOnComplete and later steered with it:
        # the app reports the steer's request, quoting its prompt. A notify send of
        # another prompt (queued for a later turn) does not make this turn asked.
        opened = int(AT("17:19:36").timestamp() * 1000)
        _, turn = notice_gap.turn_ends(LANE_B, [
            {"role": "user", "startedAt": opened - 60_000, "durationMs": 1_000, "text": "Brief"},
            {"role": "user", "startedAt": opened, "durationMs": 2_295_000, "text": "Continue from where you left off."},
            {"role": "user", "appDelivery": {"via": "steered"}, "text": "Also run the 20-run check"},
            {"role": "assistant", "text": "Done"},
        ])
        quiet = notice_gap.Send(
            AT("17:19:00"), f'sessions.send {LANE_B[-36:]} "prompt":"Continue from where you left off."', False
        )
        steer = notice_gap.Send(
            AT("17:30:00"),
            f'sessions.send {LANE_B[-36:]} "notifyOnComplete":true,"prompt":"Also run the 20-run check"',
            True,
        )
        unrelated = notice_gap.Send(
            AT("17:31:00"), f'sessions.send {LANE_B[-36:]} "notifyOnComplete":true,"prompt":"Queued for later"', True
        )
        self.assertEqual(notice_gap.verdict(turn, None, [quiet, unrelated], AT("18:30:00")), "not-asked")
        self.assertEqual(notice_gap.verdict(turn, None, [quiet], AT("18:30:00")), "not-asked")
        self.assertEqual(notice_gap.verdict(turn, None, [quiet, steer], AT("18:30:00")), "MISSING")
        notice = notice_gap.Notice(AT("17:58:00"), frozenset({(LANE_B, "Also run the 20-run check")}))
        self.assertEqual(notice_gap.notice_for(turn, [notice]), notice)

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

    def test_more_shared_identity_beats_the_nearer_turn_end(self):
        # 10-20 00:02:54 had the #119 reviewer end 2 s before the #116 fixer; here a
        # reviewer of #116 itself ends nearer, and the fixer's head SHA decides.
        (entry,) = notice_gap.parse_log("2026-10-20 00:02:54 +1100 PR7-13 #116 round 1 fixed, head 780966d0")
        reviewer = end(LANE_D, AT("00:02:56"), "I reviewed #116 at `c2b896f2`.")
        fixer = end(LANE_B, AT("00:03:07"), "Round 1 of https://github.com/o/r/pull/116 is fixed at `780966d0aa`.")
        self.assertIs(notice_gap.match_end(entry, [reviewer, fixer]), fixer)
        self.assertIs(notice_gap.match_end(entry, [reviewer]), reviewer)


if __name__ == "__main__":
    unittest.main()
