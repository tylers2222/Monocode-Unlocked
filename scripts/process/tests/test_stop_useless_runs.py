"""The fail-fast jobs and the gate's concurrency. Conditions are compared whole, and the
shell and jq in the workflows are run, because a substring would pass a broken condition."""
import json
import os
import re
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

WORKFLOWS = Path(__file__).resolve().parents[3] / ".github" / "workflows"
PULL_REQUEST_ONLY = "${{ github.event_name == 'pull_request' }}"
STOP_CALL = "uses: ./.github/workflows/stop-useless-runs.yml"


def text(workflow):
    return (WORKFLOWS / workflow).read_text()


def job(workflow, name):
    """The lines of one job: from its key to the next job's key."""
    match = re.search(rf"(?m)^  {re.escape(name)}:\n((?:(?:    .*)?\n)+)", text(workflow))
    assert match, f"{workflow}: no job {name}"
    return match.group(1)


def setting(block, key):
    """The first value of key at the job's own indent, or None."""
    match = re.search(rf"(?m)^    {key}: (.*)$", block)
    return match.group(1) if match else None


def run_block(block):
    """The script of the job's last `run: |` step."""
    return textwrap.dedent(re.findall(r"(?m)^ +run: \|\n((?:(?: {10}.*)?\n)+)", block)[-1])


def stop_jobs(workflow):
    return {name: job(workflow, name) for name in re.findall(r"(?m)^  (stop-[\w-]+):$", text(workflow))}


class StopJobsTest(unittest.TestCase):
    def test_each_stop_job_waits_for_one_job_and_fires_only_on_a_red_pull_request(self):
        pull_request_red = "${{ failure() && github.event_name == 'pull_request' }}"
        expected = {
            "ci.yml": ({"mobile", "browser", "host", "check"}, pull_request_red),
            "e2e.yml": ({"e2e-linux-run"}, pull_request_red),  # not the aggregate: macOS calls nothing
            "gate.yml": ({"lint"}, "${{ failure() }}"),  # gate.yml runs only on pull_request
        }
        for workflow, (needs, condition) in expected.items():
            jobs = stop_jobs(workflow).values()
            self.assertEqual({setting(block, "needs") for block in jobs}, needs, workflow)
            for block in jobs:
                self.assertEqual(setting(block, "if"), condition, workflow)
                self.assertIn(STOP_CALL, block)
                self.assertIn("      actions: write\n", block)
        self.assertEqual(text("gate.yml").split("\non:\n")[1].split("\n\n")[0], "  pull_request:")

    def test_a_mutation_failure_stops_nothing(self):
        self.assertNotIn(STOP_CALL, text("gate-mutation.yml"), "a mutation-exempt ticket may merge with it red")

    def test_a_pull_request_stops_at_the_first_red_leg_and_a_push_runs_every_leg(self):
        for name in ("browser", "host", "check"):
            self.assertEqual(re.search(r"fail-fast: (.*)", job("ci.yml", name)).group(1), PULL_REQUEST_ONLY, name)
        self.assertEqual(re.search(r"fail-fast: (.*)", job("ci.yml", "rpm")).group(1), "false")


class CancelStepTest(unittest.TestCase):
    """Runs the real step with a fake gh that answers the list through the step's own jq."""
    HEAD, PR, SELF = "abc123", 589, 100
    RUNS = [
        {"id": SELF, "path": ".github/workflows/ci.yml", "status": "in_progress", "pull_requests": [{"number": PR}]},
        {"id": 101, "path": ".github/workflows/e2e.yml", "status": "queued", "pull_requests": [{"number": PR}]},
        {"id": 102, "path": ".github/workflows/gate-mutation.yml", "status": "in_progress", "pull_requests": []},
        {"id": 103, "path": ".github/workflows/gate.yml", "status": "in_progress", "pull_requests": [{"number": PR}]},
        {"id": 104, "path": ".github/workflows/gate-integrity.yml", "status": "queued", "pull_requests": [{"number": PR}]},
        {"id": 105, "path": ".github/workflows/native.yml", "status": "queued", "pull_requests": [{"number": PR}]},
        {"id": 106, "path": ".github/workflows/e2e.yml", "status": "completed", "pull_requests": [{"number": PR}]},
        {"id": 107, "path": ".github/workflows/e2e.yml", "status": "queued", "pull_requests": [{"number": 7}]},
    ]
    FAKE_GH = """#!/bin/bash
printf '%s\\n' "$*" >> "$GH_LOG"
[[ -n "$GH_LIST_FAILS" && "$*" == *"-X GET"* ]] && exit 1
if [[ "$*" == *"-X POST"* ]]; then
  [[ "$*" == *"/runs/$GH_REFUSE/cancel"* ]] && exit 1
  exit 0
fi
while [ $# -gt 0 ]; do
  if [ "$1" = --jq ]; then exec jq -r "$2" "$GH_FIXTURE"; fi
  shift
done
"""

    def step(self):
        return job("stop-useless-runs.yml", "cancel")

    def run_step(self, refuse="", list_fails=""):
        with tempfile.TemporaryDirectory() as folder:
            tmp = Path(folder)
            fake = tmp / "gh"
            fake.write_text(self.FAKE_GH)
            fake.chmod(0o755)
            (tmp / "runs.json").write_text(json.dumps({"workflow_runs": self.RUNS}))
            log = tmp / "gh.log"
            env = {**os.environ, "PATH": f"{tmp}{os.pathsep}{os.environ['PATH']}", "GH_LOG": str(log), "GH_REFUSE": refuse, "GH_LIST_FAILS": list_fails,
                   "GH_FIXTURE": str(tmp / "runs.json"), "REPO": "o/r", "HEAD_SHA": self.HEAD, "PR": str(self.PR), "SELF": str(self.SELF)}
            done = subprocess.run(["bash", "-c", run_block(self.step())], env=env, capture_output=True, text=True)
            return done, log.read_text().splitlines()

    @staticmethod
    def cancelled(calls):
        return [int(re.search(r"/runs/(\d+)/cancel", call).group(1)) for call in calls if "-X POST" in call]

    def test_it_lists_only_pull_request_runs_of_this_head(self):
        _, calls = self.run_step()
        self.assertIn("-X GET repos/o/r/actions/runs -f head_sha=abc123 -f event=pull_request -F per_page=100 --jq", calls[0])

    def test_it_cancels_this_prs_ci_e2e_and_mutation_runs_and_no_other(self):
        done, calls = self.run_step()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertCountEqual(self.cancelled(calls), [self.SELF, 101, 102])

    def test_its_own_run_is_cancelled_last(self):
        _, calls = self.run_step()
        self.assertEqual(self.cancelled(calls)[-1], self.SELF, "cancelling itself ends the step")

    def test_a_refused_cancel_warns_and_the_others_still_go(self):
        done, calls = self.run_step(refuse="101")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("::warning::run 101 was not cancelled", done.stdout)
        self.assertEqual(self.cancelled(calls)[-1], self.SELF)

    def test_a_failed_list_fails_the_step_and_cancels_nothing(self):
        done, calls = self.run_step(list_fails="1")
        self.assertNotEqual(done.returncode, 0, "a stop that read no runs must not look like a stop that worked")
        self.assertEqual(self.cancelled(calls), [])
        self.assertEqual(run_block(self.step()).splitlines()[0], "set -euo pipefail")

    def test_it_reads_the_pr_head_with_a_write_token_and_no_pull_request_code(self):
        block = self.step()
        self.assertEqual(re.search(r"(?m)^    permissions:\n      (.*)$", block).group(1), "actions: write")
        for line in ("GH_TOKEN: ${{ github.token }}", "HEAD_SHA: ${{ github.event.pull_request.head.sha }}",
                     "PR: ${{ github.event.pull_request.number }}", "SELF: ${{ github.run_id }}"):
            self.assertIn(f"          {line}\n", block)
        self.assertNotIn("uses:", block, "no checkout and no action: this job holds actions: write")


class RequiredChecksTest(unittest.TestCase):
    """The stop relies on these: a cancelled job must never let its required check pass."""

    def run_ci(self, outputs=None, **results):
        with tempfile.NamedTemporaryFile() as output:
            needs = {name: {"result": result} for name, result in results.items()}
            needs["changes"]["outputs"] = outputs or {}
            env = {**os.environ, "NEEDS": json.dumps(needs), "EVENT": "pull_request", "GITHUB_OUTPUT": output.name}
            done = subprocess.run(["bash", "-c", run_block(job("ci.yml", "ci"))], env=env, capture_output=True, text=True,
                                  cwd=WORKFLOWS.parents[1])
            return done.returncode, Path(output.name).read_text()

    def test_ci_fails_on_a_cancelled_or_failed_job(self):
        for bad in ("cancelled", "failure"):
            code, output = self.run_ci({"host": "false"}, changes="success", browser=bad, host="skipped")
            self.assertEqual((code, output), (1, "failed=browser\n"), bad)

    def test_ci_passes_when_every_job_passed_or_was_skipped_for_a_reason(self):
        self.assertEqual(self.run_ci({"host": "false"}, changes="success", browser="success", host="skipped"), (0, "failed=\n"))
        self.assertEqual(self.run_ci(changes="success", browser="success", host="skipped"), (1, "failed=host\n"))

    def test_ci_always_reports_on_every_job_that_can_stop(self):
        block = job("ci.yml", "ci")
        self.assertEqual(setting(block, "if"), "always()")
        self.assertEqual(setting(block, "needs"), "[changes, mobile, browser, host, check, appimage, rpm, rpm-clean-install]")

    def test_e2e_linux_is_never_skipped_by_a_failed_changes_job_or_a_pull_request_stop(self):
        block = job("e2e.yml", "e2e-linux")
        self.assertEqual(setting(block, "name"), "${{ github.event_name == 'workflow_dispatch' && 'e2e-linux (dispatch)' || "
                         "needs.changes.outputs.level == 'fast' && 'e2e-linux (not run on a draft)' || 'e2e-linux' }}")
        self.assertEqual(setting(block, "needs"), "[changes, e2e-linux-run, e2e-macos-1, e2e-macos-2]")
        self.assertEqual(setting(block, "if"), "${{ always() && github.event_name != 'schedule' }}")
        suites = job("e2e.yml", "e2e-linux-run")
        self.assertEqual(setting(suites, "needs"), "changes")
        self.assertEqual(setting(suites, "if"), "${{ !cancelled() && github.event_name != 'schedule' && "
                         "needs.changes.outputs.level != 'fast' && needs.changes.outputs.e2e-linux != 'false' }}")

    def test_a_cancelled_mutation_job_fails_gate_mutation(self):
        block = job("gate-mutation.yml", "gate")
        self.assertEqual(setting(block, "if"), "${{ always() }}")
        self.assertEqual(setting(block, "needs"), "[plan, rust, ts]")
        self.assertIn('[ "$PLAN" = success ] && [ "$RUST" = "$RUST_WANTED" ] && [ "$TS" = "$TS_WANTED" ]', block)


class GateTest(unittest.TestCase):
    TEXT = text("gate-integrity.yml")

    def test_no_pull_request_event_cancels_a_running_gate_of_its_head(self):
        self.assertIn("  group: ${{ github.workflow }}-${{ github.event.pull_request.number }}-${{ github.event.pull_request.head.sha }}\n"
                      "  cancel-in-progress: false\n", self.TEXT)

    def test_the_gate_still_hears_every_event(self):
        self.assertIn("    types: [opened, synchronize, reopened, edited, labeled, unlabeled]\n", self.TEXT)

    def test_a_red_or_skipped_gate_tier_never_skips_gate_integrity(self):
        """A skipped required check counts as passed."""
        tier, integrity = job("gate-integrity.yml", "tier"), job("gate-integrity.yml", "integrity")
        self.assertEqual(setting(tier, "name"), "gate/tier")
        self.assertIsNone(setting(tier, "if"), "gate/tier always runs")
        self.assertEqual(setting(integrity, "name"), "gate/integrity")
        self.assertEqual(setting(integrity, "if"), "${{ !cancelled() }}")


if __name__ == "__main__":
    unittest.main()
