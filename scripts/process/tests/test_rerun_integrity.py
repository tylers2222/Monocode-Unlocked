import json
import os
import re
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import merge_rule
import rerun_integrity
from rerun_integrity import event_target, pick, resolve_pr, stale

WORKFLOWS = Path(__file__).resolve().parents[3] / ".github" / "workflows"


class RerunListTest(unittest.TestCase):
    def test_rerun_follows_exactly_the_workflows_merge_rule_reads(self):
        """merge_rule.py reads one check that a pull_request workflow posts: gate/mutation
        from .github/workflows/gate-mutation.yml. gate-integrity-rerun.yml re-runs
        gate/integrity when that workflow finishes, and for no other workflow."""
        rerun = (WORKFLOWS / "gate-integrity-rerun.yml").read_text()
        listed = re.search(r"(?m)^    workflows: \[(.*)\]$", rerun).group(1)
        self.assertEqual({name.strip() for name in listed.split(",")}, {Path(merge_rule.MUTATION_WORKFLOW).stem})
        workflow = Path(__file__).resolve().parents[3] / merge_rule.MUTATION_WORKFLOW
        if workflow.exists():  # it lands with #196; workflows: matches a workflow's name
            self.assertEqual(re.search(r"(?m)^name: (.+)$", workflow.read_text()).group(1).strip(), Path(workflow).stem)


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


class ResolvePrTest(unittest.TestCase):
    def test_the_open_pr_whose_head_is_the_commit(self):
        stack = [{"number": 196, "state": "open", "head": "H196"}, {"number": 210, "state": "open", "head": "H210"}]
        self.assertEqual(resolve_pr(stack, "H210"), 210, "a child with a lower number holds the base's head too")
        self.assertIsNone(resolve_pr([{"number": 210, "state": "closed", "head": "H210"}], "H210"))
        self.assertIsNone(resolve_pr(stack, "OLD"))


class MainTest(unittest.TestCase):
    """main() with gh answered by a scripted API. heads: what pulls/{pr} returns on each
    read; polls: the gate-integrity runs (id, status) on each poll of the head."""

    JOBS = {7: [{"id": 70, "name": "gate/tier", "conclusion": "success"}, {"id": 71, "name": "gate/integrity", "conclusion": "failure"}]}

    def run_main(self, name, event, heads, polls=([(7, "completed")],), prs=()):
        heads, polls, posts, reads, last = list(heads), list(polls), [], [], []
        def gh(*args):
            if args[:3] == ("api", "-X", "POST"):
                posts.append(args[3])
                return ""
            path = args[1]
            reads.append(path)
            if path.endswith("/pulls/210") or path.endswith("/pulls/196"):
                last.append(heads.pop(0) if len(heads) > 1 else heads[0])
                return last[-1]
            if "/commits/" in path:
                return json.dumps(list(prs))
            if "/runs?" in path:
                self.assertIn(f"head_sha={last[-1]}&", path)
                self.assertIn("event=pull_request_target", path)
                runs = polls.pop(0) if len(polls) > 1 else polls[0]
                return json.dumps([{"id": i, "status": status} for i, status in runs])
            if path.endswith("/jobs"):
                return json.dumps(self.JOBS[int(path.split("/")[-2])])
            raise AssertionError(path)
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(event, f)
        self.addCleanup(os.unlink, f.name)
        env = {"GITHUB_EVENT_NAME": name, "GITHUB_EVENT_PATH": f.name, "GATE_REPO": "o/r"}
        with unittest.mock.patch.dict(os.environ, env), unittest.mock.patch.object(rerun_integrity, "gh", gh), \
                unittest.mock.patch.object(rerun_integrity.time, "sleep", lambda s: None), unittest.mock.patch("builtins.print"):
            code = rerun_integrity.main()
        return code, posts, reads

    COMMENT = {"issue": {"number": 210}}
    STACK = [{"number": 196, "state": "open", "head": "H196"}, {"number": 210, "state": "open", "head": "H1"}]

    def test_a_status_for_an_old_commit_posts_nothing(self):
        code, posts, _ = self.run_main("status", {"sha": "OLD"}, ["H1"], prs=[{"number": 210, "state": "open", "head": "H1"}])
        self.assertEqual((code, posts), (0, []))

    def test_it_waits_for_the_runs_then_re_runs_the_job_once(self):
        code, posts, reads = self.run_main("status", {"sha": "H1"}, ["H1"], polls=([(7, "in_progress")], [(7, "completed")]), prs=self.STACK)
        self.assertEqual((code, posts), (0, ["repos/o/r/actions/jobs/71/rerun"]), "the stacked child #196 is not picked")
        self.assertEqual(sum("/runs?" in path for path in reads), 2)

    def test_a_head_that_moves_before_the_re_run_posts_nothing(self):
        code, posts, _ = self.run_main("issue_comment", self.COMMENT, ["H1", "H2"])
        self.assertEqual((code, posts), (0, []))

    def test_a_head_that_moves_during_a_comments_wait_posts_nothing(self):
        code, posts, _ = self.run_main("issue_comment", self.COMMENT, ["H1", "H2", "H2"], polls=([(7, "in_progress")], [(7, "completed")]))
        self.assertEqual((code, posts), (0, []))

    def test_a_commit_that_is_no_open_prs_head_posts_nothing(self):
        code, posts, _ = self.run_main("status", {"sha": "H9"}, ["H1"], prs=self.STACK)
        self.assertEqual((code, posts), (0, []))


if __name__ == "__main__":
    unittest.main()
