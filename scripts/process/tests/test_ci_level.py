"""Tests for ci_level.py, and for how the workflows use its output.

    python3 -m unittest discover -s scripts/process/tests
"""
import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
WORKFLOWS = HERE.parents[2] / ".github/workflows"
SCRIPT = HERE.parent / "ci_level.py"
sys.path.insert(0, str(HERE.parent))
from ci_level import FULL_LABEL, LivePr, decide, parse_live, stale_reason  # noqa: E402

DRAFT = LivePr(True, ())
READY = LivePr(False, ())
HEAD = "a" * 40


def must_not_read():
    raise AssertionError("the PR must not be read")


class Decide(unittest.TestCase):
    def full(self, event, payload, live=must_not_read):
        return decide(event, payload, live if live is must_not_read else lambda: live)[0]

    def test_only_a_draft_in_the_event_and_now_with_no_label_is_fast(self):
        self.assertIs(self.full("pull_request", "true", DRAFT), False)
        self.assertIs(self.full("pull_request", "true", LivePr(True, ("process-fix", "instance:2"))), False)

    def test_a_ready_event_is_full_and_reads_nothing(self):
        for payload in ("false", ""):
            self.assertIs(self.full("pull_request", payload), True)

    def test_events_that_are_not_pull_requests_are_full(self):
        for event in ("push", "workflow_dispatch", ""):
            self.assertIs(self.full(event, "true"), True)

    def test_a_re_run_of_a_draft_run_on_a_ready_pr_is_full(self):
        self.assertIs(self.full("pull_request", "true", READY), True)

    def test_a_pr_that_cannot_be_read_is_full(self):
        self.assertIs(self.full("pull_request", "true", None), True)

    def test_the_label_adds_the_full_set_on_a_draft(self):
        self.assertIs(self.full("pull_request", "true", LivePr(True, (FULL_LABEL,))), True)

    def test_a_near_label_adds_nothing(self):
        self.assertIs(self.full("pull_request", "true", LivePr(True, ("ci:full ", "CI:FULL", "ci"))), False)

    def test_removing_the_label_cannot_make_a_ready_pr_fast(self):
        self.assertIs(self.full("pull_request", "false"), True)
        self.assertIs(self.full("pull_request", "true", LivePr(False, ())), True)


class ParseLive(unittest.TestCase):
    def test_reads_draft_and_labels(self):
        text = json.dumps({"draft": True, "labels": [{"name": "ci:full"}, {"name": "x"}]})
        self.assertEqual(parse_live(text), LivePr(True, ("ci:full", "x")))

    def test_anything_else_is_unreadable(self):
        for text in ("", "not json", "[]", "{}", '{"draft": true}', '{"draft": "true", "labels": []}',
                     '{"draft": null, "labels": []}', '{"draft": true, "labels": [{}]}'):
            self.assertIsNone(parse_live(text), text)


class StaleReason(unittest.TestCase):
    def test_a_merge_commit_holding_the_head_is_fine(self):
        self.assertIsNone(stale_reason(HEAD, HEAD))

    def test_another_head_no_parent_or_no_head_is_stale(self):
        for head, held in ((HEAD, "b" * 40), (HEAD, None), ("", ""), ("", None)):
            self.assertIn("merge commit", stale_reason(head, held))


class Main(unittest.TestCase):
    """The script itself, on a real merge commit. A push event reads no PR and no git."""

    def git(self, repo, *args):
        return subprocess.run(["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@example.com", *args],
                              capture_output=True, text=True, check=True).stdout.strip()

    def run_script(self, cwd, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *args], cwd=cwd, capture_output=True, text=True)

    def test_prints_the_level_then_fails_on_a_stale_merge_ref(self):
        with tempfile.TemporaryDirectory() as repo:
            self.git(repo, "init", "-q", "-b", "base")
            self.git(repo, "commit", "-q", "--allow-empty", "-m", "base")
            self.git(repo, "checkout", "-q", "-b", "head")
            self.git(repo, "commit", "-q", "--allow-empty", "-m", "head")
            head = self.git(repo, "rev-parse", "HEAD")
            self.git(repo, "checkout", "-q", "base")
            self.git(repo, "merge", "-q", "--no-ff", "-m", "merge", "head")

            good = self.run_script(repo, "pull_request", "false", "1", "o/r", head)
            self.assertEqual((good.returncode, good.stdout), (0, "level=full\n"))

            stale = self.run_script(repo, "pull_request", "false", "1", "o/r", "b" * 40)
            self.assertEqual(stale.returncode, 1)
            # The output comes first, so the full set still runs and `ci` reports the red.
            self.assertTrue(stale.stdout.startswith("level=full\n"), stale.stdout)
            self.assertIn(head, stale.stdout)

    def test_a_push_needs_no_merge_ref(self):
        with tempfile.TemporaryDirectory() as repo:
            self.git(repo, "init", "-q")
            result = self.run_script(repo, "push", "", "", "o/r", "")
            self.assertEqual((result.returncode, result.stdout), (0, "level=full\n"))

    def test_wrong_arguments_exit_2(self):
        self.assertEqual(self.run_script(HERE, "push").returncode, 2)


class Workflows(unittest.TestCase):
    """What a real run proved, kept from being edited away. Text checks: PyYAML is not here."""

    def text(self, name):
        return (WORKFLOWS / name).read_text()

    def job(self, name, job_id):
        match = re.search(rf"^  {re.escape(job_id)}:\n(.*?)(?=^  \S|\Z)", self.text(name), re.S | re.M)
        self.assertIsNotNone(match, job_id)
        return match.group(1)

    def test_each_two_level_workflow_starts_on_ready_for_review(self):
        for name in ("ci.yml", "e2e.yml", "native.yml"):
            self.assertIn("types: [opened, synchronize, reopened, ready_for_review]", self.text(name), name)

    def test_required_names_are_absent_from_the_fast_level(self):
        self.assertIn("name: ${{ needs.changes.outputs.level == 'fast' && 'ci (fast)' || 'ci' }}\n",
                      self.job("ci.yml", "ci"))
        self.assertIn("name: ${{ needs.changes.outputs.level == 'fast' && 'e2e-linux (not run on a draft)' || 'e2e-linux' }}\n",
                      self.job("e2e.yml", "e2e-linux"))

    def test_a_required_job_is_never_skipped_on_a_pull_request(self):
        self.assertIn("if: always()\n", self.job("ci.yml", "ci"))
        self.assertIn("if: ${{ always() && github.event_name != 'schedule' }}\n", self.job("e2e.yml", "e2e-linux"))

    def test_no_other_job_uses_a_required_name(self):
        for name in ("ci.yml", "e2e.yml"):
            for required in ("ci", "e2e-linux"):
                self.assertNotRegex(self.text(name), rf"(?m)^    name: {required}$", name)

    def test_the_required_checks_wait_for_every_job_of_the_full_set(self):
        self.assertIn("needs: [changes, mobile, browser, host, check, appimage, rpm, rpm-clean-install]",
                      self.job("ci.yml", "ci"))
        self.assertIn("needs: [changes, e2e-linux-run, e2e-macos-1, e2e-macos-2]", self.job("e2e.yml", "e2e-linux"))

    def test_the_required_checks_take_their_result_from_ci_verdict(self):
        self.assertIn("scripts/process/ci_verdict.py ci", self.job("ci.yml", "ci"))
        self.assertIn("scripts/process/ci_verdict.py e2e", self.job("e2e.yml", "e2e-linux"))

    def test_the_level_is_not_a_path_decision(self):
        # merge-when-green.sh accepts a skipped e2e job on the `<job>=false` log line of
        # `Changed paths (e2e)`, so the draft level must never print one.
        source = SCRIPT.read_text()
        self.assertEqual(re.findall(r'print\(f"(\w[\w-]*)=', source), ["level"])


if __name__ == "__main__":
    unittest.main()
