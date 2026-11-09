"""The rules of the fail-fast jobs and of the gate's label filter, read from the workflow files."""
import importlib.util
import re
import sys
import unittest
from pathlib import Path

PROCESS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PROCESS))
WORKFLOWS = PROCESS.parents[1] / ".github" / "workflows"
STOP = "uses: ./.github/workflows/stop-useless-runs.yml"


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), PROCESS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def stop_needs(workflow):
    """The job each stop job of a workflow waits for."""
    text = (WORKFLOWS / workflow).read_text()
    blocks = re.findall(r"(?m)^  stop-[\w-]+:\n((?:    .*\n|      .*\n)+)", text)
    complete = [block for block in blocks if STOP in block and "actions: write" in block and "failure()" in block]
    assert len(blocks) == len(complete), f"{workflow}: a stop job lacks failure(), the token or the call"
    return {re.search(r"needs: (\S+)", block).group(1) for block in blocks}


class StopJobsTest(unittest.TestCase):
    def test_every_job_under_a_required_check_has_a_stop_job(self):
        self.assertEqual(stop_needs("ci.yml"), {"mobile", "browser", "host", "check"})
        self.assertEqual(stop_needs("gate.yml"), {"lint"})
        self.assertEqual(stop_needs("e2e.yml"), {"e2e-linux"}, "the macOS jobs are not required")

    def test_a_mutation_failure_stops_nothing(self):
        self.assertNotIn(STOP, (WORKFLOWS / "gate-mutation.yml").read_text(), "a mutation-exempt ticket may merge with it red")

    def test_a_push_never_stops(self):
        for workflow in ("ci.yml", "e2e.yml"):
            text = (WORKFLOWS / workflow).read_text()
            for block in re.findall(r"(?m)^  stop-[\w-]+:\n((?:    .*\n|      .*\n)+)", text):
                self.assertIn("github.event_name == 'pull_request'", block)
        self.assertNotIn("fail-fast: true", (WORKFLOWS / "ci.yml").read_text())

    def test_the_cancel_job_runs_no_pull_request_code(self):
        text = (WORKFLOWS / "stop-useless-runs.yml").read_text()
        code = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
        self.assertNotIn("actions/checkout", code)
        self.assertNotIn("uses:", code)

    def test_only_these_workflows_are_cancelled(self):
        text = (WORKFLOWS / "stop-useless-runs.yml").read_text()
        self.assertEqual(set(re.findall(r"\.github/workflows/([\w-]+\.yml)", text)), {"ci.yml", "e2e.yml", "gate-mutation.yml"},
                         "gate/lint and gate/integrity are never cancelled")

    def test_the_aggregates_stay_always(self):
        ci = (WORKFLOWS / "ci.yml").read_text()
        self.assertRegex(ci, r"(?m)^    name: ci\n    needs: \[.*\]\n    if: always\(\)$")
        self.assertIn("if: ${{ always() }}", (WORKFLOWS / "gate-mutation.yml").read_text())


class LabelFilterTest(unittest.TestCase):
    TEXT = (WORKFLOWS / "gate-integrity.yml").read_text()
    FILTER = re.compile(r"""contains\(fromJSON\('(\[[^']*\])'\), github\.event\.label\.name\)""")

    def non_input(self):
        lists = set(self.FILTER.findall(self.TEXT))
        self.assertEqual(len(lists), 1, "every copy of the filter holds the same list")
        return {name.strip(' "') for name in next(iter(lists)).strip("[]").split(",")}

    def test_no_gate_check_reads_a_label_on_the_list(self):
        waits, tier = load("waits-tyler"), load("tier")
        for label in self.non_input():
            self.assertIsNone(waits.category(label), label)
            self.assertFalse(label.lower().startswith("instance:"), label)
            self.assertIsNone(tier.TIER_LABEL.match(label), label)
            self.assertNotEqual(label, tier.INTERROGATE_LABEL)

    def test_a_required_name_is_never_kept_on_a_skip(self):
        for required, other in (("gate/integrity", "no gate input changed"), ("gate/tier", "no gate input changed (tier)")):
            line = next(l for l in self.TEXT.splitlines() if f"|| '{required}' }}}}" in l)
            self.assertTrue(line.strip().startswith("name: ${{ contains(fromJSON("), line)
            self.assertIn(f"&& '{other}' ||", line)
        self.assertEqual(len(re.findall(r"(?m)^    if: \$\{\{.*!contains\(fromJSON", self.TEXT)), 2, "both jobs skip on the list and only there")

    def test_an_event_never_cancels_a_running_gate(self):
        self.assertIn("cancel-in-progress: false", self.TEXT)
        self.assertNotIn("cancel-in-progress: true", self.TEXT)

    def test_the_gate_still_hears_every_event(self):
        self.assertIn("types: [opened, synchronize, reopened, edited, labeled, unlabeled]", self.TEXT)


if __name__ == "__main__":
    unittest.main()
