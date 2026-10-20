import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from rerun_integrity import event_target, pick, stale
from workflow_on import on_events

WORKFLOWS = Path(__file__).resolve().parents[3] / ".github" / "workflows"
OWN = "gate-integrity.yml"


class RerunListTest(unittest.TestCase):
    def test_rerun_follows_every_workflow_whose_checks_land_on_the_head(self):
        """merge_rule.py waits for every check on the head, so gate-integrity-rerun.yml must
        re-run gate/integrity when any pull_request or pull_request_target workflow (other
        than gate-integrity itself) finishes. A workflow whose on: cannot be read fails."""
        on_head, unreadable = set(), []
        for path in sorted(WORKFLOWS.glob("*.y*ml")):
            text = path.read_text()
            events = on_events(text)
            if events is None:
                unreadable.append(path.name)
            elif path.name != OWN and events & {"pull_request", "pull_request_target"}:
                on_head.add(re.search(r"(?m)^name: (.+)$", text).group(1).strip())
        self.assertEqual(unreadable, [])
        rerun = (WORKFLOWS / "gate-integrity-rerun.yml").read_text()
        listed = re.search(r"(?m)^    workflows: \[(.*)\]$", rerun).group(1)
        self.assertEqual({name.strip() for name in listed.split(",")}, on_head)


class OnEventsTest(unittest.TestCase):
    def test_every_form_of_on(self):
        forms = {
            "on: pull_request\n": {"pull_request"},
            "on: [push, pull_request]\n": {"push", "pull_request"},
            "on: {pull_request: {branches: [x]}, push: {}}\n": None,
            "on: {pull_request: null, push: null}\n": {"pull_request", "push"},
            "on:\n  - push\n  - pull_request_target\n": {"push", "pull_request_target"},
            "on:\n  pull_request:\n    branches: [tyler/main]\n  push:\n\njobs: {}\n": {"pull_request", "push"},
            '"on":\n  pull_request_target:  # comment\n': {"pull_request_target"},
            "name: x\njobs: {}\n": None,
            "on:\n  pull_request:\n  - push\n": None,
        }
        for text, events in forms.items():
            with self.subTest(text=text):
                self.assertEqual(on_events(text), events)


class RerunRulesTest(unittest.TestCase):
    def test_each_event_names_its_pr_or_its_commit(self):
        self.assertEqual(event_target("issue_comment", {"issue": {"number": 195}}), (195, None))
        self.assertEqual(event_target("status", {"sha": "abc"}), (None, "abc"))
        run = {"head_sha": "def", "pull_requests": [{"number": 196}]}
        self.assertEqual(event_target("workflow_run", {"workflow_run": run}), (196, "def"))

    def test_an_event_for_an_old_commit_is_stale(self):
        self.assertTrue(stale("old", "new"))
        self.assertFalse(stale("new", "new"))
        self.assertFalse(stale(None, "new"), "a comment is about the PR, so its current head")

    def jobs(self, tier):
        return [{"id": 1, "name": "gate/tier", "conclusion": tier}, {"id": 2, "name": "gate/integrity", "conclusion": "failure"}]

    def test_the_newest_usable_run_is_picked(self):
        runs = [{"id": 10}, {"id": 11}, {"id": 12}]
        jobs = {10: self.jobs("success"), 11: self.jobs("success"), 12: self.jobs("cancelled")}
        jobs = {run: [{**job, "id": job["id"] + run * 10} for job in js] for run, js in jobs.items()}
        self.assertEqual(pick(runs, jobs, "gate/integrity"), ("job", 112, 11), "the newest run's gate/tier was cancelled")

    def test_a_run_without_the_job_is_skipped(self):
        runs = [{"id": 10}, {"id": 11}]
        jobs = {10: [{"id": 102, "name": "gate/tier", "conclusion": "success"}, {"id": 103, "name": "gate/integrity", "conclusion": "failure"}],
                11: []}
        self.assertEqual(pick(runs, jobs, "gate/integrity"), ("job", 103, 10))

    def test_no_usable_run_reruns_the_newest_whole(self):
        runs = [{"id": 10}, {"id": 11}]
        self.assertEqual(pick(runs, {10: self.jobs("cancelled"), 11: self.jobs("failure")}, "gate/integrity"), ("run", 11))


if __name__ == "__main__":
    unittest.main()
