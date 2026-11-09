"""Tests for flaky_tests.py.

    python3 -m unittest scripts/process/tests/test_flaky_tests.py

The log lines keep the shape of real job logs. Names, ids and SHAs are made up.
"""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

SCRIPT = Path(__file__).parent.parent / "flaky_tests.py"
spec = importlib.util.spec_from_file_location("flaky_tests", SCRIPT)
flaky = importlib.util.module_from_spec(spec)
sys.modules["flaky_tests"] = flaky
spec.loader.exec_module(flaky)

HEAD_A, HEAD_B = "a" * 40, "b" * 40
BASE_1, BASE_2 = "1" * 40, "2" * 40
STAMP = "2026-11-09T01:02:03.0000000Z "


def checkout(head, base):
    return STAMP + "HEAD is now at abc1234 Merge %s into %s\n" % (head, base)


def job(job_id, run_id, name="browser (1/6)", conclusion="failure", attempt=1, head=HEAD_A, branch="x",
        start="2026-11-09T01:00:00Z", end="2026-11-09T01:10:00Z", step="Run npm run test:browser -- --shard=1/6"):
    steps = [{"name": step, "conclusion": "failure"}] if conclusion == "failure" else []
    return {"id": job_id, "run_id": run_id, "name": name, "workflow_name": "CI", "conclusion": conclusion, "run_attempt": attempt,
            "head_sha": head, "head_branch": branch, "started_at": start, "completed_at": end, "steps": steps}


LATER = {"start": "2026-11-09T02:00:00Z", "end": "2026-11-09T02:10:00Z"}
PW_FAIL = "  1 failed\n    [webkit] › tests/browser/a.spec.ts:10:3 › opens a tab \n  3 passed (1.0m)\n"


class Harness:
    """A store directory with job logs, and no network unless a runner is given."""

    def __init__(self, logs=None, runner=None, budget=0, offline=True):
        self.dir = tempfile.TemporaryDirectory()
        self.source = flaky.Source([], self.dir.name, budget, offline, runner=runner)
        for job_id, text in (logs or {}).items():
            (Path(self.source.logs) / ("%d.log" % job_id)).write_text(text)

    def rank(self, jobs):
        return flaky.rank(flaky.find_failures({j["id"]: j for j in jobs}), self.source, fetch=False)


class ParseLog(unittest.TestCase):
    def test_each_runner_format_gives_its_test_name(self):
        text = "\n".join([
            STAMP + "  1 failed",
            STAMP + "    [webkit] › tests/browser/a.spec.ts:10:3 › opens › a tab ",
            STAMP + "  456 passed (34.2m)",
            STAMP + "test scenario::logins::a_real_srt_login ... FAILED",
            STAMP + "test scenario::logins::other ... ok",
            STAMP + " FAIL  src/a.test.ts > group > does a thing",
            STAMP + " FAIL  src/live.test.ts [ src/live.test.ts ]",
            STAMP + "FAIL: test_it (test_mod.Case.test_it)",
            STAMP + "✖ failing tests:",
            STAMP + "✖ node test name (1.2ms)",
        ])
        self.assertEqual(flaky.parse_log(text)["tests"], {
            "browser: a.spec.ts › opens › a tab",
            "cargo: scenario::logins::a_real_srt_login",
            "vitest: src/a.test.ts > group > does a thing",
            "vitest: src/live.test.ts",
            "unittest: test_mod.Case.test_it.test_it",
            "node: node test name",
        })

    def test_merge_ref_is_the_first_checkout_with_ansi_stripped(self):
        text = checkout(HEAD_A, BASE_1) + checkout(HEAD_B, BASE_2)
        self.assertEqual(flaky.parse_log("\x1b[36m" + text)["ref"], (HEAD_A, BASE_1))

    def test_a_scenario_failing_in_some_repeats_is_a_repeat(self):
        text = "\n".join([
            "== build: app.sh build", "== scenarios: r9 #1", "ok", "== scenarios: r9 #2",
            "[giga-eval 07:24:28] ERROR: timed out after 30 s: the Mono starts three leads",
            "== scenarios: r9 #3", "ok",
        ])
        out = flaky.parse_log(text)
        self.assertEqual(out["tests"], {"scenario r9: the Mono starts three leads"})
        self.assertEqual(out["repeats"], {"scenario r9: the Mono starts three leads": (1, 3)})

    def test_a_scenario_failing_in_every_repeat_is_no_repeat(self):
        text = "== scenarios: s0 #1\n[giga-eval 07:24:28] ERROR: timed out after 90 s: the ask\n"
        out = flaky.parse_log(text)
        self.assertEqual(out["tests"], {"scenario s0: the ask"})
        self.assertEqual(out["repeats"], {})

    def test_scenario_names_lose_ids_and_evidence_so_runs_group(self):
        one = "== scenarios: ade89 two #1\n  FAIL rows of app-11111111-1111-1111-1111-111111111111 at the mark: 0, expected 1\n"
        two = "== scenarios: ade89 two #1\n  FAIL rows of app-22222222-2222-2222-2222-222222222222 at the mark: 3, expected 1\n"
        self.assertEqual(flaky.parse_log(one)["tests"], flaky.parse_log(two)["tests"])
        self.assertEqual(flaky.parse_log(one)["tests"], {"scenario ade89 two: rows of app-* at the mark"})

    def test_a_count_line_with_no_detail_still_names_the_scenario(self):
        text = "== scenarios: ade89 term #1\n[giga-eval 08:03:48] ADE-89 term: 0/1 passed\n"
        self.assertEqual(flaky.parse_log(text)["tests"], {"scenario ade89 term: failed, no detail in the log"})

    def test_an_ok_cargo_line_and_passing_counts_are_no_failure(self):
        text = "test a::b ... ok\n== scenarios: s0 #1\n[giga-eval 08:03:48] S0: 1/1 passed\n"
        self.assertEqual(flaky.parse_log(text)["tests"], set())


class ClassifyStep(unittest.TestCase):
    def test_classes(self):
        for step, want in (("Install omp 18.2.11 (sha256-checked release binary)", "setup"), ("Set up job", "setup"),
                           ("Run the scenarios", "test"), ("Run npm test", "test"), ("Run cargo test", "test"),
                           ("Every job passed or was skipped", "rollup"), ("Every needed job succeeded", "rollup"),
                           ("ESLint", "check"), ("cargo mutants on the changed Rust lines", "check")):
            self.assertEqual(flaky.classify_step([step]), want, step)
        self.assertEqual(flaky.classify_step([]), "check")


class RefVerdict(unittest.TestCase):
    def verdict(self, jobs, logs):
        failure = flaky.find_failures({j["id"]: j for j in jobs})[0]
        harness = Harness(logs)  # held so its directory lives through the call
        return flaky.ref_verdict(failure, harness.source, fetch=False)

    def test_rerun_on_the_same_merge_is_the_same_ref(self):
        jobs = [job(1, 10), job(2, 10, "browser (1/6)", "success", 2, **LATER)]
        self.assertEqual(self.verdict(jobs, {1: checkout(HEAD_A, BASE_1), 2: checkout(HEAD_A, BASE_1)}), "same_ref")

    def test_rerun_with_no_log_is_the_same_ref_by_rule_only(self):
        jobs = [job(1, 10), job(2, 10, "browser (1/6)", "success", 2, **LATER)]
        self.assertEqual(self.verdict(jobs, {}), "same_ref_by_rule")

    def test_new_run_on_a_new_base_is_a_new_ref(self):
        jobs = [job(1, 10), job(2, 11, "browser (1/6)", "success", **LATER)]
        self.assertEqual(self.verdict(jobs, {1: checkout(HEAD_A, BASE_1), 2: checkout(HEAD_A, BASE_2)}), "new_ref")

    def test_new_run_on_the_same_base_is_the_same_ref(self):
        jobs = [job(1, 10), job(2, 11, "browser (1/6)", "success", **LATER)]
        self.assertEqual(self.verdict(jobs, {1: checkout(HEAD_A, BASE_1), 2: checkout(HEAD_A, BASE_1)}), "same_ref")

    def test_new_run_with_no_log_is_unread_and_never_the_same_ref(self):
        jobs = [job(1, 10), job(2, 11, "browser (1/6)", "success", **LATER)]
        self.assertEqual(self.verdict(jobs, {}), "new_ref_unread")

    def test_a_later_head_is_only_a_later_head(self):
        jobs = [job(1, 10), job(2, 11, "browser (1/6)", "success", head=HEAD_B, **LATER)]
        self.assertEqual(self.verdict(jobs, {}), "later_head")

    def test_a_pass_before_the_failure_is_no_evidence(self):
        jobs = [job(1, 10, **LATER), job(2, 11, "browser (1/6)", "success")]
        self.assertEqual(self.verdict(jobs, {}), "none")

    def test_a_pass_of_another_job_or_branch_is_no_evidence(self):
        jobs = [job(1, 10), job(2, 10, "browser (2/6)", "success", 2, **LATER), job(3, 11, "browser (1/6)", "success", branch="y", **LATER)]
        self.assertEqual(self.verdict(jobs, {}), "none")


class Rank(unittest.TestCase):
    def test_setup_failures_are_counted_apart_from_tests(self):
        h = Harness({1: "curl: (22) The requested URL returned error: 504\n"})
        ranked = h.rank([job(1, 10, step="Install omp 18.2.11 (sha256-checked release binary)"), job(2, 10, "browser (2/6)", "success", 2, **LATER)])
        self.assertEqual(dict(ranked["tests"]), {})
        self.assertEqual(ranked["counts"]["setup"], 1)
        self.assertEqual(list(ranked["setup"]), ["Install omp 18.2.11 [curl: (22)]"])

    def test_a_network_error_in_a_test_step_with_no_test_name_is_setup(self):
        h = Harness({1: "curl: (22) The requested URL returned error: 504\n"})
        ranked = h.rank([job(1, 10, step="Run the scenarios")])
        self.assertEqual(dict(ranked["tests"]), {})
        self.assertEqual(list(ranked["setup"]), ["Run the scenarios [curl: (22)]"])

    def test_a_network_line_beside_a_named_test_stays_a_test(self):
        h = Harness({1: "curl: (22) The requested URL returned error: 504\n" + PW_FAIL})
        ranked = h.rank([job(1, 10)])
        self.assertIn("browser: a.spec.ts › opens a tab", ranked["tests"])
        self.assertEqual(dict(ranked["setup"]), {})

    def test_rollups_and_other_checks_are_no_test(self):
        h = Harness({1: PW_FAIL, 2: PW_FAIL})
        ranked = h.rank([job(1, 10, "ci", step="Every job passed or was skipped"), job(2, 10, "lint", step="ESLint")])
        self.assertEqual(dict(ranked["tests"]), {})
        self.assertEqual((ranked["counts"]["rollup"], ranked["counts"]["check"]), (1, 1))

    def test_count_jobs_and_minutes_of_a_rerun_pass(self):
        h = Harness({1: checkout(HEAD_A, BASE_1) + PW_FAIL, 2: checkout(HEAD_A, BASE_1)})
        ranked = h.rank([job(1, 10), job(2, 10, "browser (1/6)", "success", 2, **LATER)])
        row = ranked["tests"]["browser: a.spec.ts › opens a tab"]
        self.assertEqual((row["same_ref"], row["by_rule"], len(row["jobs"])), (1, 0, 1))
        self.assertEqual((row["est_jobs"], row["est_minutes"], row["estimate"]), (1.0, 10.0, False))

    def test_a_copied_failure_of_a_later_attempt_counts_once(self):
        h = Harness({1: PW_FAIL, 2: PW_FAIL})
        ranked = h.rank([job(1, 10), job(2, 10, attempt=2)])
        self.assertEqual(ranked["counts"]["test"], 1)

    def test_a_sampled_failure_is_scaled_and_labelled_as_an_estimate(self):
        h = Harness({1: PW_FAIL})
        ranked = h.rank([job(i, 10 + i, branch="b%d" % i) for i in (1, 2, 3, 4)])
        row = ranked["tests"]["browser: a.spec.ts › opens a tab"]
        self.assertEqual((len(row["jobs"]), row["est_jobs"], row["est_minutes"], row["estimate"]), (1, 4.0, 40.0, True))
        self.assertEqual(flaky.cost(row), ("~4", "~40"))

    def test_a_job_with_many_failed_tests_is_counted_apart(self):
        names = "".join("    [webkit] › tests/browser/a.spec.ts:%d:1 › t%d\n" % (i, i) for i in range(flaky.MASS_FAILURE + 1))
        h = Harness({1: "  11 failed\n" + names})
        ranked = h.rank([job(1, 10)])
        self.assertEqual(dict(ranked["tests"]), {})
        self.assertEqual(ranked["mass"][0][1], flaky.MASS_FAILURE + 1)

    def test_same_ref_ranks_above_new_ref_whatever_the_minutes(self):
        same = {"same_ref": 1, "by_rule": 0, "repeat": 0, "est_minutes": 1.0}
        new = {"same_ref": 0, "by_rule": 0, "repeat": 0, "est_minutes": 99.0}
        self.assertEqual([n for n, _ in flaky.ordered({"new": new, "same": same})], ["same", "new"])


class Report(unittest.TestCase):
    def build(self, h, jobs, candidates=()):
        return flaky.report(h.rank(jobs), h.source, list(candidates), "w")

    def test_lists_split_on_the_evidence(self):
        logs = {1: checkout(HEAD_A, BASE_1) + PW_FAIL, 2: checkout(HEAD_A, BASE_1), 3: "test a::b ... FAILED\n"}
        text = self.build(Harness(logs), [job(1, 10), job(2, 10, "browser (1/6)", "success", 2, **LATER), job(3, 12, "check", branch="y", step="Run cargo test")])
        ranked = text.split("## Passed only on a new ref")[0]
        self.assertIn("`browser: a.spec.ts › opens a tab`", ranked)
        self.assertIn("10#1", ranked)
        self.assertNotIn("a::b", text)
        self.assertIn("Left out of the lists: 1 tests", text)

    def test_a_candidate_with_no_failure_says_so(self):
        text = self.build(Harness({}), [job(1, 10)], ["no such test"])
        self.assertIn("| `no such test` | no failure in the logs read |", text)


class Source(unittest.TestCase):
    def test_offline_runs_nothing(self):
        def runner(*_a, **_k):
            raise AssertionError("no call when offline")
        h = Harness(runner=runner, budget=5)
        self.assertIsNone(h.source.get("anything"))

    def test_budget_is_a_hard_cap(self):
        calls = []

        def runner(args, **_k):
            calls.append(args)
            return SimpleNamespace(returncode=0, stdout="x", stderr="")
        h = Harness(runner=runner, budget=2, offline=False)
        for job_id in (1, 2, 3):
            h.source.log(job_id, fetch=True)
        self.assertEqual(len(calls), 2)

    def test_a_refusal_stops_every_later_call(self):
        calls = []

        def runner(args, **_k):
            calls.append(args)
            return SimpleNamespace(returncode=1, stdout="", stderr="HTTP 403: rate limit")
        h = Harness(runner=runner, budget=9, offline=False)
        for job_id in (1, 2):
            h.source.log(job_id, fetch=True)
        self.assertEqual(len(calls), 1)
        self.assertIn("rate limit", h.source.stopped)

    def test_only_get_paths_are_called(self):
        calls = []

        def runner(args, **_k):
            calls.append(args)
            return SimpleNamespace(returncode=0, stdout=json.dumps({"jobs": []}), stderr="")
        h = Harness(runner=runner, budget=2, offline=False)
        h.source.fetch_run_jobs({"id": 1, "run_attempt": 1})
        h.source.log(2, fetch=True)
        self.assertTrue(all(c[:2] == ["gh", "api"] and len(c) == 3 for c in calls))

    def test_cache_and_raw_job_files_load_and_the_cache_is_not_written(self):
        with tempfile.TemporaryDirectory() as cache, tempfile.TemporaryDirectory() as store:
            (Path(cache) / "a.json").write_text(json.dumps({"key": "k", "body": {"jobs": [job(1, 10)]}}))
            (Path(cache) / "b.json").write_text(json.dumps({"jobs": [job(2, 11)]}))
            before = sorted(p.name for p in Path(cache).iterdir())

            def runner(*_a, **_k):
                return SimpleNamespace(returncode=0, stdout=json.dumps({"jobs": [job(3, 12)]}), stderr="")
            source = flaky.Source([cache], store, 1, False, runner=runner)
            source.fetch_run_jobs({"id": 12, "run_attempt": 1})
            self.assertEqual(sorted(source.jobs), [1, 2, 3])
            self.assertEqual(sorted(p.name for p in Path(cache).iterdir()), before)

    def test_runs_worth_reading_keeps_reruns_and_mixed_heads(self):
        def run(i, name, head, end, attempt=1):
            return {"id": i, "name": name, "head_sha": head, "conclusion": end, "status": "completed", "run_attempt": attempt}
        runs = [run(1, "CI", "a", "failure"), run(2, "CI", "a", "success"), run(3, "CI", "b", "failure"), run(4, "CI", "b", "failure"),
                run(5, "E2E", "c", "failure", 2), run(6, "gate-integrity", "d", "failure", 3)]
        self.assertEqual([r["id"] for r in flaky.runs_worth_reading({r["id"]: r for r in runs})], [1, 2, 5])


if __name__ == "__main__":
    unittest.main()
