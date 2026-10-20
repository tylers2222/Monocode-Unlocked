"""Tests for ready-issues.py on data recorded 2026-10-20 22:57 AEDT.

    python3 -m unittest discover -s scripts/process/tests

fixtures/linear-issues.json is the script's Linear query result, with the
relations of projects/process-fixes/blocked-by.json applied (they were not set
in Linear yet). gh-prs.json is the gh pr list result. sessions.json and
worktrees.json are the app sessions.list and worktrees.list results, cut to the
busy sessions and their worktrees.
"""
import copy
import importlib.util
import json
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).parent
_spec = importlib.util.spec_from_file_location("ready_issues", HERE.parent / "ready-issues.py")
ri = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ri)

RECORDED_AT = datetime(2026, 10, 20, 11, 57, tzinfo=timezone.utc)


def load(name):
    return json.loads((HERE / "fixtures" / name).read_text())


class ReadyIssues(unittest.TestCase):
    def setUp(self):
        self.issues = load("linear-issues.json")
        self.prs = load("gh-prs.json")
        self.sessions = load("sessions.json")["result"]["sessions"]
        self.worktrees = load("worktrees.json")["result"]["worktrees"]

    def ready(self):
        return {r.id: r for r in ri.ready_issues(self.issues, self.prs, self.sessions, self.worktrees)}

    def issue(self, ident):
        return next(i for i in self.issues if i["identifier"] == ident)

    def pr(self, number):
        return next(pr for pr in self.prs if pr["number"] == number)

    def blocker(self, blocked, ident):
        return next(r["issue"] for r in self.issue(blocked)["inverseRelations"]["nodes"] if r["issue"]["identifier"] == ident)

    def test_recorded_data(self):
        ready = self.ready()
        self.assertEqual(sorted(ready), ["ADE-46", "ADE-61", "ADE-62", "ADE-64", "ADE-65", "ADE-66"])
        self.assertEqual(ready["ADE-46"].since, ri.parse_time(self.pr(105)["mergedAt"]), "ADE-15's PR #105 merge")
        self.assertIsNone(ready["ADE-61"].since)
        lines, code = ri.report(list(ready.values()), RECORDED_AT)
        self.assertEqual(code, 1, "ADE-46 has waited a day")
        self.assertIn("ready-issues: 6 ready with no builder, 1 waiting more than 60 min.", lines)

    def test_open_pr_on_a_blocker_blocks(self):
        self.assertNotIn("ADE-63", self.ready(), "ADE-23 part 2 (#155) is open")
        self.pr(155).update(state="MERGED", mergedAt="2026-10-20T12:30:00Z")
        self.assertEqual(self.ready()["ADE-63"].since, datetime(2026, 10, 20, 12, 30, tzinfo=timezone.utc))

    def test_pr_closed_without_merge_does_not_block(self):
        self.pr(155).update(state="CLOSED")
        since = self.ready()["ADE-63"].since
        self.assertEqual(since, ri.parse_time(self.pr(150)["mergedAt"]), "only #150 is left, and it merged")

    def test_blocker_with_an_open_sub_issue_has_not_merged(self):
        r20_blockers = self.issue("ADE-38")["inverseRelations"]["nodes"]
        ade14 = next(r for r in r20_blockers if r["issue"]["identifier"] == "ADE-14")
        self.issue("ADE-61")["inverseRelations"]["nodes"] = [copy.deepcopy(ade14)]
        self.assertNotIn("ADE-61", self.ready())
        for child in self.issue("ADE-61")["inverseRelations"]["nodes"][0]["issue"]["children"]["nodes"]:
            child["state"]["type"] = "completed"
        self.assertIn("ADE-61", self.ready(), "every ADE-14 PR has merged")

    def test_done_and_canceled_blockers_have_merged(self):
        ade11 = self.blocker("ADE-62", "ADE-11")
        self.pr(161).update(state="OPEN", mergedAt=None)
        self.assertNotIn("ADE-62", self.ready())
        ade11.update(state={"type": "canceled"}, canceledAt="2026-10-20T11:00:00Z")
        self.assertEqual(self.ready()["ADE-62"].since, datetime(2026, 10, 20, 11, 0, tzinfo=timezone.utc))
        ade11.update(state={"type": "completed"}, completedAt="2026-10-20T11:10:00Z")
        self.assertEqual(self.ready()["ADE-62"].since, datetime(2026, 10, 20, 11, 10, tzinfo=timezone.utc), "no merged PR, so completedAt")

    def test_a_busy_session_naming_the_ticket_is_a_builder(self):
        builder = {**self.sessions[0], "id": "app-x-builder", "title": "omp · Linear ade-46, PR 1. Read /Users/…"}
        for busy, archived, expect_ready in [(True, False, False), (False, False, True), (True, True, True)]:
            with self.subTest(busy=busy, archived=archived):
                self.sessions[-1:] = [{**builder, "busy": busy, "archived": archived}]
                self.assertEqual("ADE-46" in self.ready(), expect_ready)

    def test_a_busy_session_on_the_ticket_branch_is_a_builder(self):
        session = next(s for s in self.sessions if s["busy"] and "ADE-" not in s["title"])
        self.worktrees.append({"branch": "fix/ade-46-completion-groups", "sessionIds": [session["id"]]})
        self.assertNotIn("ADE-46", self.ready())

    def test_only_todo_issues_without_a_pr_are_candidates(self):
        ready = self.ready()
        self.assertNotIn("ADE-55", ready, "Todo, but PR #163 is open")
        self.assertNotIn("ADE-60", ready, "In Progress")
        self.prs.append({"number": 999, "title": "ADE-64: end the turn record only with the provider", "headRefName": "fix/steer-miss", "state": "OPEN", "mergedAt": None})
        self.assertNotIn("ADE-64", self.ready(), "its PR is open, so it has been built")
        self.issue("ADE-65")["state"]["type"] = "backlog"
        self.assertNotIn("ADE-65", self.ready())

    def test_ready_since_is_the_latest_blocker_merge(self):
        nodes = self.issue("ADE-61")["inverseRelations"]["nodes"]
        nodes += [{"type": "blocks", "issue": self.blocker("ADE-46", "ADE-15")}, {"type": "blocks", "issue": self.blocker("ADE-62", "ADE-11")}]
        self.assertEqual(self.ready()["ADE-61"].since, ri.parse_time(self.pr(161)["mergedAt"]), "ADE-11's #161 merged after ADE-15's #105")

    def test_only_blocks_relations_block(self):
        self.issue("ADE-64")["inverseRelations"]["nodes"].append({"type": "related", "issue": self.blocker("ADE-63", "ADE-23")})
        self.assertIn("ADE-64", self.ready(), "a related open issue does not block")

    def test_open_pr_linked_by_attachment_or_branch_blocks(self):
        ade11 = self.blocker("ADE-62", "ADE-11")
        part2 = {"number": 998, "title": "Save tabs per window", "headRefName": "fix/window-tabs", "state": "OPEN", "mergedAt": None}
        self.prs.append(part2)
        self.assertIn("ADE-62", self.ready(), "#998 names no ticket yet")
        ade11["attachments"]["nodes"].append({"url": "https://github.com/tylers2222/Monocode-Unlocked/pull/998"})
        self.assertNotIn("ADE-62", self.ready(), "linked by a Linear attachment")
        ade11["attachments"]["nodes"].pop()
        part2["headRefName"] = "fix/ade-11-pr-3-window-tabs"
        self.assertNotIn("ADE-62", self.ready(), "linked by its branch")

    def test_wait_limit_is_more_than_60_minutes(self):
        ready = [self.ready()["ADE-62"]]
        since = ready[0].since
        self.assertEqual(ri.report(ready, since + timedelta(minutes=60))[1], 0)
        self.assertEqual(ri.report(ready, since + timedelta(minutes=61))[1], 1)


if __name__ == "__main__":
    unittest.main()
