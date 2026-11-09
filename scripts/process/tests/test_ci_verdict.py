"""Tests for ci_verdict.py, with the real ci-map.toml for the path-map skips.

    python3 -m unittest discover -s scripts/process/tests
"""
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).parent
SCRIPT = HERE.parent / "ci_verdict.py"
sys.path.insert(0, str(HERE.parent))
from ci_changes import MAP, parse, select  # noqa: E402
from ci_verdict import failed  # noqa: E402

CI_MAP = parse(MAP.read_text())
CI_JOBS = ("mobile", "browser", "host", "check", "appimage", "rpm", "rpm-clean-install")
E2E_JOBS = ("e2e-linux-run", "e2e-macos-1", "e2e-macos-2")


def outputs_for(changed, level="full"):
    """What the `changes` job outputs for these changed files, from the real map."""
    run, _ = select(CI_MAP, "pull_request", changed)
    return {"level": level, **{job: str(on).lower() for job, on in run.items()}}


def needs(jobs, outputs, **results):
    """All jobs succeed unless named; a keyword uses _ for - in the job id."""
    built = {"changes": {"result": "success", "outputs": outputs}}
    built.update({job: {"result": results.get(job.replace("-", "_"), "success")} for job in jobs})
    return built


class ReadyPr(unittest.TestCase):
    def test_a_job_skipped_by_level_on_a_ready_pr_fails_ci(self):
        # src/ runs browser by the map, so on the full level nothing can excuse its skip.
        got = failed("ci", needs(CI_JOBS, outputs_for(["src/a.ts"]), browser="skipped",
                                 appimage="skipped", rpm="skipped", rpm_clean_install="skipped"), "pull_request")
        self.assertEqual(got, ["browser"])

    def test_a_path_map_skip_passes_ci(self):
        got = failed("ci", needs(CI_JOBS, outputs_for(["src-tauri/src/a.rs"]), browser="skipped", mobile="skipped",
                                 host="skipped", appimage="skipped", rpm="skipped", rpm_clean_install="skipped"),
                     "pull_request")
        self.assertEqual(got, [])

    def test_a_job_skipped_by_level_on_a_ready_pr_fails_e2e(self):
        got = failed("e2e", needs(E2E_JOBS, outputs_for(["src/a.ts"]), e2e_linux_run="skipped"), "pull_request")
        self.assertEqual(got, ["e2e-linux-run"])
        got = failed("e2e", needs(E2E_JOBS, outputs_for(["src/a.ts"]), e2e_macos_2="skipped"), "pull_request")
        self.assertEqual(got, ["e2e-macos-2"])

    def test_a_path_map_skip_passes_e2e(self):
        got = failed("e2e", needs(E2E_JOBS, outputs_for(["docs/a.md"]), e2e_linux_run="skipped",
                                  e2e_macos_1="skipped", e2e_macos_2="skipped"), "pull_request")
        self.assertEqual(got, [])

    def test_a_file_outside_the_map_or_in_all_excuses_no_skip(self):
        for changed in (["docs/a.md", "newtool/x"], ["docs/a.md", ".github/workflows/ci.yml"]):
            got = failed("e2e", needs(E2E_JOBS, outputs_for(changed), e2e_linux_run="skipped"), "pull_request")
            self.assertEqual(got, ["e2e-linux-run"], changed)

    def test_the_level_scripts_run_every_job(self):
        for path in ("scripts/process/ci_level.py", "scripts/process/ci_verdict.py"):
            self.assertTrue(all(select(CI_MAP, "pull_request", [path])[0].values()), path)

    def test_failed_and_cancelled_jobs_fail(self):
        for result in ("failure", "cancelled"):
            got = failed("e2e", needs(E2E_JOBS, outputs_for(["docs/a.md"]), e2e_linux_run=result,
                                      e2e_macos_1="skipped", e2e_macos_2="skipped"), "pull_request")
            self.assertEqual(got, ["e2e-linux-run"], result)

    def test_a_job_this_file_does_not_name_must_succeed(self):
        got = failed("ci", {**needs(CI_JOBS[:0], outputs_for(["docs/a.md"])), "newjob": {"result": "skipped"}}, "pull_request")
        self.assertEqual(got, ["newjob"])


class Changes(unittest.TestCase):
    def test_a_failed_changes_job_with_empty_outputs_fails_and_excuses_nothing(self):
        broken = needs(E2E_JOBS, {}, e2e_linux_run="skipped")
        broken["changes"] = {"result": "failure", "outputs": {}}
        self.assertEqual(failed("e2e", broken, "pull_request"), ["changes", "e2e-linux-run"])


class Fast(unittest.TestCase):
    def test_the_fast_level_passes_with_its_jobs_skipped(self):
        draft = outputs_for([".github/workflows/ci.yml"], level="fast")
        self.assertEqual(failed("e2e", needs(E2E_JOBS, draft, e2e_linux_run="skipped", e2e_macos_1="skipped",
                                             e2e_macos_2="skipped"), "pull_request"), [])
        self.assertEqual(failed("ci", needs(CI_JOBS, draft, browser="skipped", appimage="skipped", rpm="skipped",
                                            rpm_clean_install="skipped"), "pull_request"), [])

    def test_the_fast_level_excuses_only_the_jobs_it_leaves_out(self):
        draft = outputs_for([".github/workflows/ci.yml"], level="fast")
        got = failed("ci", needs(CI_JOBS, draft, browser="skipped", check="skipped", appimage="skipped",
                                 rpm="skipped", rpm_clean_install="skipped"), "pull_request")
        self.assertEqual(got, ["check"])


class Event(unittest.TestCase):
    OUT = {"level": "full"}

    def test_packaging_must_run_on_a_push(self):
        got = failed("ci", needs(CI_JOBS, self.OUT, rpm="skipped", rpm_clean_install="skipped"), "push")
        self.assertEqual(got, ["rpm", "rpm-clean-install"])

    def test_a_dispatch_may_leave_out_macos_only_when_it_asked_to(self):
        jobs = needs(E2E_JOBS, self.OUT, e2e_macos_1="skipped", e2e_macos_2="skipped")
        self.assertEqual(failed("e2e", jobs, "workflow_dispatch", True), [])
        self.assertEqual(failed("e2e", jobs, "workflow_dispatch", False), ["e2e-macos-1", "e2e-macos-2"])


class Main(unittest.TestCase):
    def run_script(self, workflow, needs_json):
        env = {**os.environ, "NEEDS": json.dumps(needs_json), "EVENT": "pull_request"}
        return subprocess.run([sys.executable, str(SCRIPT), workflow], capture_output=True, text=True, env=env)

    def test_prints_failed_and_sets_the_exit_code(self):
        red = self.run_script("e2e", needs(E2E_JOBS, outputs_for(["src/a.ts"]), e2e_linux_run="skipped"))
        self.assertEqual((red.returncode, red.stdout), (1, "failed=e2e-linux-run\n"))
        green = self.run_script("e2e", needs(E2E_JOBS, outputs_for(["src/a.ts"])))
        self.assertEqual((green.returncode, green.stdout), (0, "failed=\n"))


if __name__ == "__main__":
    unittest.main()
