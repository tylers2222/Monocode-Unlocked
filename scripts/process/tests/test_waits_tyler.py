"""Tests for waits-tyler.py, instance-label.py and pr-cap.py on recorded GitHub JSON.

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
    """Run a script with a fake gh on PATH that prints gh_out (JSON, or a str as is) and logs its arguments."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        (tmp / "out").write_text(gh_out if isinstance(gh_out, str) else json.dumps(gh_out))
        gh = tmp / "gh"
        gh.write_text(f'#!/bin/sh\necho "$@" > {tmp}/args\ncat {tmp}/out\n')
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


def check_event(script, event):
    with tempfile.NamedTemporaryFile("wb", suffix=".json") as f:
        f.write(event if isinstance(event, bytes) else json.dumps(event).encode())
        f.flush()
        return run(script, "--event", f.name)


class WaitsTylerCheck(unittest.TestCase):
    def check(self, *names):
        return check_event("waits-tyler.py", event_with(*names))

    def test_recorded_event_with_repo_security_passes(self):
        result = run("waits-tyler.py", "--event", FIXTURES / "event-labeled.json")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("waits-tyler:repo-security", result.stdout)

    def test_wording_fails(self):
        result = self.check("waits-tyler:wording", "waits-tyler:repo-security")
        self.assertEqual(result.returncode, 1)
        self.assertIn("'waits-tyler:wording' is not a hold category", result.stdout)
        self.assertNotIn("'waits-tyler:repo-security'", result.stdout)

    def test_every_brief_category_passes(self):
        result = self.check(*(f"waits-tyler:{c}" for c in BRIEF_CATEGORIES))
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_labels_that_read_as_holds_need_the_exact_form(self):
        for label in ["Waits-Tyler:Wording", "waits-tyler", "waits-tyler:", "waits-tyler:login-later", "WAITS-TYLER:ui",
                      "waits-tylerish", "waits-tyler-wording", "waits-tyler wording", "waits-tyler/wording",
                      "waits_tyler:wording", "waits-tyler :login", "waits-tyler: login", "waitstyler:login"]:
            with self.subTest(label=label):
                self.assertEqual(self.check("bug", label).returncode, 1)

    def test_other_labels_pass(self):
        self.assertEqual(self.check("bug", "tier:T3", "instance:2", "awaits-tyler").returncode, 0)

    def test_pr_mode_reads_labels_with_gh(self):
        result = run("waits-tyler.py", "--pr", "160", gh_out={"labels": [{"name": "waits-tyler:wording"}]})
        self.assertEqual(result.returncode, 1)
        self.assertIn("pr view 160 -R tylers2222/Monocode-Unlocked", result.gh_args)

    def test_unreadable_input_exits_2(self):
        self.assertEqual(run("waits-tyler.py", "--event", "/nonexistent/event.json").returncode, 2)
        self.assertEqual(check_event("waits-tyler.py", b"\xff\xfe\x00bad").returncode, 2, "not UTF-8")
        self.assertEqual(check_event("waits-tyler.py", {"pull_request": {"labels": [{"name": 5}]}}).returncode, 2)
        self.assertEqual(run("waits-tyler.py", "--pr", "1", gh_out="<html>rate limited</html>").returncode, 2)


class InstanceLabel(unittest.TestCase):
    def test_exactly_one_instance_label_passes(self):
        for label in ["instance:1", "instance:2"]:
            with self.subTest(label=label):
                result = check_event("instance-label.py", event_with("process-fix", label))
                self.assertEqual(result.returncode, 0, result.stdout)

    def test_missing_extra_or_unknown_instance_fails(self):
        for labels in [("bug",), (), ("instance:1", "instance:2"), ("instance:3",), ("Instance:2",), ("Instance:2", "instance:2")]:
            with self.subTest(labels=labels):
                self.assertEqual(check_event("instance-label.py", event_with(*labels)).returncode, 1)

    def test_recorded_unlabelled_pr_fails_with_gh(self):
        prs = json.loads((FIXTURES / "gh-pr-list.json").read_text())
        pr170 = next(pr for pr in prs if pr["number"] == 170)
        result = run("instance-label.py", "--pr", "170", gh_out={"labels": pr170["labels"]})
        self.assertEqual(result.returncode, 1)
        self.assertIn("it has none", result.stdout)

    def test_unreadable_input_exits_2(self):
        self.assertEqual(check_event("instance-label.py", {"pull_request": {}}).returncode, 2)


def prs_with(changes):
    """The recorded open PRs, with the labels of some PRs replaced."""
    prs = json.loads((FIXTURES / "gh-pr-list.json").read_text())
    for pr in prs:
        if pr["number"] in changes:
            pr["labels"] = [{"name": name} for name in changes[pr["number"]]]
    return prs


class PrCap(unittest.TestCase):
    """Recorded: instance 1 has 10 open PRs (7 stacked on feat/ade-53 and feat/ade-54 bases); instance 2 has
    #160 #159 #155 plus 6 process-fix PRs; #170 has no instance label."""

    def test_instance_1_counts_stacked_prs(self):
        result = run("pr-cap.py", "--instance", "1", gh_out=prs_with({}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("instance 1: 10 of 4 slots used, 0 not counted", result.stdout)
        self.assertIn("#152 -> feat/ade-53-approvals-decide", result.stdout)
        self.assertIn("--state open", result.gh_args)
        self.assertNotIn("--base", result.gh_args)

    def test_instance_2_leaves_out_process_fix(self):
        result = run("pr-cap.py", "--instance", "2", gh_out=prs_with({}))
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("instance 2: 3 of 4 slots used, 6 not counted. A slot is free.", result.stdout)
        self.assertIn("#164 -> tyler/main  Process: waits-tyler label check and PR cap script (not counted: process-fix)", result.stdout)
        self.assertNotIn("#163", result.stdout)
        self.assertNotIn("#170", result.stdout)

    def test_four_counted_is_full(self):
        result = run("pr-cap.py", "--instance", "2", gh_out=prs_with({164: ["instance:2"]}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("4 of 4 slots used", result.stdout)

    def test_held_pr_frees_a_slot(self):
        held = {164: ["instance:2"], 160: ["instance:2", "waits-tyler:repo-security"]}
        result = run("pr-cap.py", "--instance", "2", gh_out=prs_with(held))
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("(not counted: waits-tyler:repo-security)", result.stdout)

    def test_off_list_label_still_uses_a_slot(self):
        result = run("pr-cap.py", "--instance", "2", gh_out=prs_with({164: ["instance:2", "waits-tyler:wording"]}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("4 of 4 slots used", result.stdout)

    def test_instance_is_required(self):
        self.assertEqual(run("pr-cap.py", gh_out=prs_with({})).returncode, 2)

    def test_unreadable_gh_output_exits_2(self):
        for out in ["<html>rate limited</html>", [{"number": 1, "title": "x"}]]:
            with self.subTest(out=out):
                self.assertEqual(run("pr-cap.py", "--instance", "2", gh_out=out).returncode, 2)


if __name__ == "__main__":
    unittest.main()
