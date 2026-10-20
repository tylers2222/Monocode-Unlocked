"""Tests for ready-issues.py on data recorded 2026-10-21 00:04 AEDT.

    python3 -m unittest discover -s scripts/process/tests

fixtures/linear-issues.json is the script's two Linear queries (the open ADE
issues, then the closed issues that block one), taken while the Orchestrator
was adding the instance labels. fixtures/sessions.json is instance 2's
`app sessions.list`, cut to the busy sessions and three idle ones.
"""
import copy
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).parent
SCRIPT = HERE.parent / "ready-issues.py"
_spec = importlib.util.spec_from_file_location("ready_issues", SCRIPT)
ri = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ri)

RECORDED_AT = datetime(2026, 10, 20, 13, 4, tzinfo=timezone.utc)
ISSUES = json.loads((HERE / "fixtures/linear-issues.json").read_text())
SESSIONS = json.loads((HERE / "fixtures/sessions.json").read_text())["result"]["sessions"]


def at(text):
    return datetime.fromisoformat(text).replace(tzinfo=timezone.utc)


class ReadyIssues(unittest.TestCase):
    def setUp(self):
        self.issues = copy.deepcopy(ISSUES)
        self.sessions = copy.deepcopy(SESSIONS)

    def run_instance(self, instance="2"):
        return ri.ready_issues(self.issues, self.sessions, instance)

    def ready(self, instance="2"):
        return {r.id: r for r in self.run_instance(instance)[0]}

    def issue(self, ident):
        return next(i for i in self.issues if i["identifier"] == ident)

    def label(self, ident, *names):
        self.issue(ident)["labels"]["nodes"] += [{"name": name} for name in names]

    def event(self, ident, when, state=None, added=()):
        self.issue(ident)["history"]["nodes"].append(
            {"createdAt": when + "Z", "toState": state and {"type": state}, "addedLabels": [{"name": n} for n in added] or None})

    def set_state(self, ident, state, when):
        self.issue(ident)["state"]["type"] = state
        self.event(ident, when, state=state)

    def test_recorded_data(self):
        ready, unlabelled = self.run_instance()
        self.assertEqual(ready, [], "every labelled Todo issue waits on an open blocker")
        self.assertEqual([i["identifier"] for i in unlabelled], ["ADE-66", "ADE-65", "ADE-64", "ADE-63", "ADE-62", "ADE-61", "ADE-55"])
        lines, code = ri.report(ready, unlabelled, RECORDED_AT)
        self.assertEqual(code, 1, "unlabelled issues keep the labels complete")
        self.assertIn("ready-issues: 0 ready with no builder, 0 waiting more than 60 min, 7 unlabelled.", lines)

    def test_in_progress_blocker_blocks_until_code_complete(self):
        self.label("ADE-62", "instance:2")
        self.assertNotIn("ADE-62", self.ready(), "ADE-11 is In Progress with PR 1 of 3 merged")
        self.label("ADE-11", "code-complete")
        self.event("ADE-11", "2026-10-20T12:30:00", added=["code-complete"])
        self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T12:30:00"))

    def test_code_complete_before_done_counts_from_the_label(self):
        self.label("ADE-62", "instance:2")
        self.label("ADE-11", "code-complete")
        self.event("ADE-11", "2026-10-20T12:30:00", added=["code-complete"])
        self.set_state("ADE-11", "completed", "2026-10-20T13:00:00")
        self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T12:30:00"), "the earlier of the two")

    def test_done_canceled_and_duplicate_unblock_from_the_history_entry(self):
        for state in ["completed", "canceled", "duplicate"]:
            with self.subTest(state=state):
                self.setUp()
                self.label("ADE-62", "instance:2")
                self.set_state("ADE-11", state, "2026-10-20T11:40:00")
                self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T11:40:00"))

    def test_blocker_reopened_then_done_counts_from_the_last_done(self):
        self.label("ADE-62", "instance:2")
        self.set_state("ADE-11", "completed", "2026-10-20T09:00:00")
        self.set_state("ADE-11", "started", "2026-10-20T10:00:00")
        self.set_state("ADE-11", "completed", "2026-10-20T11:00:00")
        self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T11:00:00"))

    def test_closed_blocker_without_history_counts_from_updated_at(self):
        self.label("ADE-62", "instance:2")
        ade11 = self.issue("ADE-11")
        ade11.update(state={"type": "completed"}, updatedAt="2026-10-20T10:15:00Z")
        ade11["history"]["nodes"] = []
        self.assertEqual(self.ready()["ADE-62"].since, at("2026-10-20T10:15:00"))

    def test_ready_since_is_the_last_blocker(self):
        self.label("ADE-63", "instance:2")
        self.issue("ADE-63")["inverseRelations"]["nodes"].append({"type": "blocks", "issue": {"identifier": "ADE-11"}})
        self.set_state("ADE-23", "completed", "2026-10-20T12:00:00")
        self.assertNotIn("ADE-63", self.ready(), "ADE-11 still blocks")
        self.set_state("ADE-11", "completed", "2026-10-20T12:20:00")
        self.assertEqual(self.ready()["ADE-63"].since, at("2026-10-20T12:20:00"))

    def test_only_blocks_relations_block(self):
        self.label("ADE-64", "instance:2")
        self.issue("ADE-64")["inverseRelations"]["nodes"].append({"type": "related", "issue": {"identifier": "ADE-23"}})
        self.assertIn("ADE-64", self.ready())

    def test_no_blockers_ready_since_entering_todo(self):
        self.label("ADE-64", "instance:2")
        self.assertEqual(self.ready()["ADE-64"].since, ri.parse_time(self.issue("ADE-64")["createdAt"]), "no history: createdAt")
        self.event("ADE-64", "2026-10-20T11:00:00", state="started")
        self.event("ADE-64", "2026-10-20T12:00:00", state="unstarted")
        ready = self.ready()["ADE-64"]
        self.assertEqual(ready.since, at("2026-10-20T12:00:00"))
        self.assertEqual(ri.report([ready], [], ready.since + timedelta(minutes=60))[1], 0)
        self.assertEqual(ri.report([ready], [], ready.since + timedelta(minutes=61))[1], 1)

    def test_builder_is_a_busy_linear_session_for_the_exact_ticket(self):
        self.label("ADE-61", "instance:2")
        linear = {**SESSIONS[0], "id": "app-x", "title": "omp · Linear ADE-61, PR 1. Read /Users/…", "busy": True, "archived": False}
        cases = [
            ({}, False),
            ({"busy": False}, True),
            ({"archived": True}, True),
            ({"title": "omp · Review ADE-61 change brief"}, True),
            ({"title": "omp · Read /Users/…/briefs/ade-61.md. Linear ADE-61"}, True),
            ({"title": "omp · Linear ADE-6, PR 1. Read /Users/…"}, True),
            ({"title": "omp · Linear ADE-610, PR 1. Read /Users/…"}, True),
        ]
        for change, expect_ready in cases:
            with self.subTest(change=change):
                self.sessions[:] = [*SESSIONS, {**linear, **change}]
                self.assertEqual("ADE-61" in self.ready(), expect_ready)

    def test_recorded_linear_builder_hides_its_ticket(self):
        self.set_state("ADE-46", "unstarted", "2026-10-20T12:00:00")
        self.assertNotIn("ADE-46", self.ready(), "'omp · Linear ADE-46 builder. Read …' is busy")
        self.sessions[:] = [s for s in self.sessions if "ADE-46" not in s["title"]]
        self.assertIn("ADE-46", self.ready())

    def test_only_this_instances_issues_are_reported(self):
        self.label("ADE-61", "instance:1")
        self.label("ADE-64", "instance:2")
        self.assertEqual(set(self.ready("2")), {"ADE-64"})
        self.assertEqual(set(self.ready("1")), {"ADE-61"})

    def test_todo_issue_without_exactly_one_instance_label_is_unlabelled(self):
        self.label("ADE-61", "instance:1", "instance:2")
        self.label("ADE-64", "instance:2")
        ready, unlabelled = self.run_instance()
        self.assertIn("ADE-61", [i["identifier"] for i in unlabelled])
        self.assertNotIn("ADE-64", [i["identifier"] for i in unlabelled])
        self.assertNotIn("ADE-56", [i["identifier"] for i in unlabelled], "Backlog is not judged")
        self.assertEqual(ri.report([r for r in ready if r.id == "ADE-64"], unlabelled, ready[0].since)[1], 1)


class CommandLine(unittest.TestCase):
    def run_script(self, *args, cli_out):
        with tempfile.TemporaryDirectory() as tmp:
            cli = Path(tmp) / "monocode"
            cli.write_text(f"#!/bin/sh\ncat <<'EOF'\n{cli_out}\nEOF\n")
            cli.chmod(0o755)
            env = {**os.environ, "MONOCODE_CLI": str(cli), "LINEAR_API_KEY": "lin_api_unused"}
            return subprocess.run([sys.executable, SCRIPT, *args], env=env, capture_output=True, text=True)

    def test_instance_is_required(self):
        self.assertEqual(self.run_script(cli_out="{}").returncode, 2)

    def test_unexpected_answer_exits_2(self):
        for out in ['{"ok": true, "result": null}', '{"ok": false, "error": "MONOCODE_APP_TOKEN is not set"}', "not json"]:
            with self.subTest(out=out):
                result = self.run_script("--instance", "2", cli_out=out)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
