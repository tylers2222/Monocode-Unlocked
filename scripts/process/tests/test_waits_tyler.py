"""Tests for waits-tyler.py and pr-cap.py on recorded GitHub JSON.

    python3 -m unittest discover -s scripts/process/tests
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
PROCESS = HERE.parent
FIXTURES = HERE / "fixtures"
BRIEF_CATEGORIES = ["login", "payment", "os-permission", "destructive", "repo-security", "frontend-design"]


def run(script, *args, gh_out=None):
    """Run a script with a fake gh on PATH that prints gh_out and logs its arguments."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        (tmp / "out.json").write_text(json.dumps(gh_out))
        gh = tmp / "gh"
        gh.write_text(f'#!/bin/sh\necho "$@" > {tmp}/args\ncat {tmp}/out.json\n')
        gh.chmod(0o755)
        env = {**os.environ, "PATH": f"{tmp}:{os.environ['PATH']}"}
        result = subprocess.run([sys.executable, PROCESS / script, *args], env=env, capture_output=True, text=True)
        result.gh_args = (tmp / "args").read_text() if (tmp / "args").exists() else ""
        return result


def event_with(*names):
    event = json.loads((FIXTURES / "event-labeled.json").read_text())
    shape = event["pull_request"]["labels"][0]
    event["pull_request"]["labels"] = [{**shape, "name": name} for name in names]
    return event


def prs_with(labels_by_number):
    prs = json.loads((FIXTURES / "gh-pr-list.json").read_text())
    for pr in prs:
        pr["labels"] = [{"name": name} for name in labels_by_number.get(pr["number"], [])]
    return prs


class WaitsTylerCheck(unittest.TestCase):
    def check_event(self, *names):
        with tempfile.NamedTemporaryFile("w", suffix=".json") as f:
            json.dump(event_with(*names), f)
            f.flush()
            return run("waits-tyler.py", "--event", f.name)

    def test_recorded_event_with_repo_security_passes(self):
        result = run("waits-tyler.py", "--event", FIXTURES / "event-labeled.json")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("waits-tyler:repo-security", result.stdout)

    def test_wording_fails(self):
        result = self.check_event("waits-tyler:wording", "waits-tyler:repo-security")
        self.assertEqual(result.returncode, 1)
        self.assertIn("'waits-tyler:wording' is not a hold category", result.stdout)
        self.assertNotIn("'waits-tyler:repo-security'", result.stdout)

    def test_every_brief_category_passes(self):
        result = self.check_event(*(f"waits-tyler:{c}" for c in BRIEF_CATEGORIES))
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_off_list_spellings_fail(self):
        for label in ["Waits-Tyler:Wording", "waits-tyler", "waits-tyler:", "waits-tyler:login-later", "WAITS-TYLER:ui"]:
            with self.subTest(label=label):
                self.assertEqual(self.check_event("bug", label).returncode, 1)

    def test_other_labels_pass(self):
        self.assertEqual(self.check_event("bug", "waits-tylerish", "tier:T3").returncode, 0)

    def test_pr_mode_reads_labels_with_gh(self):
        result = run("waits-tyler.py", "--pr", "160", gh_out={"labels": [{"name": "waits-tyler:wording"}]})
        self.assertEqual(result.returncode, 1)
        self.assertIn("pr view 160 -R tylers2222/Monocode-Unlocked", result.gh_args)

    def test_unreadable_event_exits_2(self):
        self.assertEqual(run("waits-tyler.py", "--event", "/nonexistent/event.json").returncode, 2)


class PrCap(unittest.TestCase):
    """The recorded list holds 6 open PRs: #163 #160 #159 #155 #128 #125."""

    def test_recorded_list_is_over_the_cap(self):
        result = run("pr-cap.py", gh_out=prs_with({}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("6 of 4 slots used, 0 held", result.stdout)
        self.assertIn("--base tyler/main --state open", result.gh_args)

    def test_held_prs_free_slots(self):
        held = {160: ["waits-tyler:repo-security"], 159: ["bug", "waits-tyler:frontend-design"], 155: ["waits-tyler:login"]}
        result = run("pr-cap.py", gh_out=prs_with(held))
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("3 of 4 slots used, 3 held", result.stdout)
        self.assertIn("#160 ADE-60: resume an interrupted turn once after a relaunch (not counted: waits-tyler:repo-security)", result.stdout)

    def test_four_counted_is_full(self):
        result = run("pr-cap.py", gh_out=prs_with({160: ["waits-tyler:payment"], 159: ["waits-tyler:destructive"]}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("4 of 4 slots used", result.stdout)

    def test_off_list_label_still_uses_a_slot(self):
        result = run("pr-cap.py", gh_out=prs_with({160: ["waits-tyler:wording"], 159: ["waits-tyler:wording"], 155: ["waits-tyler"]}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("6 of 4 slots used, 0 held", result.stdout)


if __name__ == "__main__":
    unittest.main()
