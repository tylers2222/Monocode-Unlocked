import re
import shutil
import subprocess
import tempfile
import sys
import unittest
import unittest.mock
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import merge_rule
from merge_rule import (
    Decision,
    DiffStat,
    Proof,
    Verdict,
    classify_checks,
    collect_verdicts,
    decide,
    parse_proofs,
    parse_verdict,
    pr_change_since,
)

# PR 160: r1 read head 7ce96f5 at AT and found one minor. FIX is a later push with the fix
# and EARLIER a commit before 7ce96f5.
REVIEWED = "7ce96f5bd1067a557120bd87ca6565aa07307242"
FIX = "5d0c1b2a3948576677889900aabbccddeeff0011"
EARLIER = "4a1b9c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b"
AT = datetime(2026, 10, 20, 11, 1, 8, tzinfo=timezone.utc)
SPEC = "tests/browser/ade60-resume-once.spec.ts"
TEST = "a relaunch resumes each interrupted session once and times its turn"
REVIEWED_FILES = DiffStat({"src/app/App.tsx": "modified", SPEC: "added"}, 199)

# Real review/agent descriptions, by PR.
MINOR = "r1 changes needed: 0B/0M/1m (vacuous steer assertion, spec:156)"  # 160
MAJOR = "r1 changes needed: 0B/1M/6m (Mono chat reset strands acks; part 1 lessons not applied to ack)"  # 155
BLOCKER = "r1 changes needed: 1B/0M/2m (status script posts on current head, not the reviewed one)"  # 127
NO_COUNTS = "r1 changes needed: 1 blocker, 1 major, 3 minor (b7e6a7ff)"  # 108
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
    """decide() for a 3-line fix of PR 160's minor with proof for m1. A test changes one input."""
    inputs = {
        "tier": "T3",
        "head": FIX,
        "verdicts": [verdict(MINOR)],
        "proofs": proven("m1"),
        "reviewed_files": REVIEWED_FILES,
        "since_review": DiffStat({SPEC: "modified"}, 3),
        "checks_failed": [],
        "checks_pending": [],
        "mutation": "success",
    }
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


class DecideTest(unittest.TestCase):
    def assertRefused(self, decision, *words):
        self.assertFalse(decision.ok, decision.reason)
        self.assertFalse(decision.carry, decision.reason)
        for word in words:
            self.assertIn(word, decision.reason)

    def test_no_verdict_fails(self):
        self.assertEqual(decide_fix(verdicts=[]), Decision(False, False, "no review verdict yet"))

    def test_latest_verdict_decides(self):
        older = verdict(MAJOR, sha=EARLIER, at=AT - timedelta(hours=2))
        newer = verdict(MINOR_R2)
        for verdicts in ([older, newer], [newer, older]):
            with self.subTest(first=verdicts[0].round):
                self.assertTrue(decide_fix(tier="T1", verdicts=verdicts).carry)

    def test_clean_verdict_on_head_passes_without_carry(self):
        decision = decide_fix(verdicts=[verdict(CLEAN_R2, sha=FIX)])
        self.assertTrue(decision.ok, decision.reason)
        self.assertFalse(decision.carry)

    def test_blocker_or_major_on_head_fails(self):
        for description in (MAJOR, BLOCKER):
            with self.subTest(description=description):
                decision = decide_fix(verdicts=[verdict(description, sha=FIX)], proofs=proven("m1", "m2", "m3", "m4", "m5", "m6"))
                self.assertRefused(decision, f"on head {FIX[:7]}", "new review verdict")

    def test_minors_only_on_head_fails(self):
        self.assertRefused(decide_fix(verdicts=[verdict(MINOR, sha=FIX)]), f"on head {FIX[:7]}", "fixer-proof")

    def test_blocker_or_major_on_older_commit_needs_new_verdict(self):
        for description in (MAJOR, BLOCKER):
            with self.subTest(description=description):
                decision = decide_fix(verdicts=[verdict(description)], proofs=proven("m1", "m2", "m3", "m4", "m5", "m6"))
                self.assertRefused(decision, "blocker or major", "new review verdict")

    def test_unknown_counts_need_new_verdict(self):
        self.assertRefused(decide_fix(verdicts=[verdict(NO_COUNTS)]), "blocker or major", "new review verdict")

    def test_clean_verdict_on_older_commit_fails(self):
        self.assertRefused(decide_fix(verdicts=[verdict(CLEAN_R1)]), "new commits")

    def test_changes_needed_without_minor_does_not_carry(self):
        self.assertRefused(decide_fix(verdicts=[verdict("r1 changes needed: 0B/0M/0m (nits only)")]), "new review verdict")

    def test_t2_and_t3_carry_only_round_1(self):
        for tier in ("T2", "T3"):
            with self.subTest(tier=tier):
                self.assertRefused(decide_fix(tier=tier, verdicts=[verdict(MINOR_R2)]), "round 1")

    def test_t1_carries_later_round(self):
        self.assertEqual(
            decide_fix(tier="T1", verdicts=[verdict(MINOR_R2)]),
            Decision(True, True, "carried from 7ce96f5: minors-only + fixer proof (T1)"),
        )

    def test_each_minor_needs_proof(self):
        decision = decide_fix(verdicts=[verdict(THREE_MINORS)], proofs=proven("m1", "m3"))
        self.assertRefused(decision, "m2")
        self.assertNotIn("m1", decision.reason)
        self.assertNotIn("m3", decision.reason)

    def test_proof_posted_before_verdict_does_not_count(self):
        self.assertRefused(decide_fix(proofs=proven("m1", at=AT - timedelta(minutes=5))), "m1")

    def test_fix_that_adds_file_needs_new_verdict(self):
        for status in ("added", "renamed"):
            with self.subTest(status=status):
                since = DiffStat({SPEC: "modified", "tests/browser/ade60-steer.spec.ts": status}, 20)
                self.assertRefused(decide_fix(since_review=since), "new file", "tests/browser/ade60-steer.spec.ts")

    def test_fix_outside_reviewed_files_needs_new_verdict(self):
        since = DiffStat({SPEC: "modified", "src/app/inFlight.ts": "modified"}, 8)
        self.assertRefused(decide_fix(since_review=since), "outside the reviewed diff", "src/app/inFlight.ts")

    def test_fix_may_change_at_most_60_lines(self):
        self.assertTrue(decide_fix(since_review=DiffStat({SPEC: "modified"}, 60)).carry)
        self.assertRefused(decide_fix(since_review=DiffStat({SPEC: "modified"}, 61)), "61 lines")

    def test_failed_check_blocks_carry(self):
        self.assertRefused(decide_fix(checks_failed=["gate/lint"], checks_pending=["browser"]), "gate/lint")

    def test_pending_check_blocks_carry(self):
        self.assertRefused(decide_fix(checks_pending=["browser", "check (macos-latest)"]), "waiting for checks: browser")

    def test_review_in_progress_on_head_blocks_carry(self):
        self.assertRefused(decide_fix(review_in_progress=True), "in progress")

    def test_t3_carry_needs_green_mutation(self):
        for result in (None, "failure", "cancelled", "skipped"):
            with self.subTest(result=result):
                self.assertRefused(decide_fix(mutation=result), "gate/mutation")
        self.assertTrue(decide_fix(tier="T2", mutation=None).carry, "T2 needs no mutation run")

    def test_small_proven_fix_carries_verdict(self):
        self.assertEqual(decide_fix(), Decision(True, True, "carried from 7ce96f5: minors-only + fixer proof (T3)"))


class ClassifyChecksTest(unittest.TestCase):
    def test_failed_and_pending_skip_own_run_and_review(self):
        own = "https://github.com/o/r/actions/runs/77/job/"
        other = "https://github.com/o/r/actions/runs/76/job/"
        runs = [
            {"name": "gate/lint", "status": "completed", "conclusion": "failure", "details_url": other + "1"},
            {"name": "check (macos-latest)", "status": "completed", "conclusion": "cancelled", "details_url": other + "2"},
            {"name": "browser", "status": "in_progress", "conclusion": None, "details_url": other + "3"},
            {"name": "Clean rpm install", "status": "completed", "conclusion": "skipped", "details_url": other + "4"},
            {"name": "host (macos-latest)", "status": "completed", "conclusion": "neutral", "details_url": None},
            {"name": "check (ubuntu-latest)", "status": "completed", "conclusion": "success", "details_url": other + "5"},
            {"name": "gate/integrity", "status": "in_progress", "conclusion": None, "details_url": own + "6"},
            {"name": "gate/tier", "status": "completed", "conclusion": "failure", "details_url": own + "7"},
            {"name": "gate/mutation", "status": "queued", "conclusion": None, "details_url": own + "8"},
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



def compare_file(path, status="modified", sha="s1", patch="@@ -1 +1 @@\n-a\n+b"):
    return {"filename": path, "status": status, "sha": sha, "patch": patch}


class PrChangeSinceTest(unittest.TestCase):
    def test_base_merged_in_adds_nothing(self):
        # "Update branch": the file's blob changed (base edits) but the PR's own patch did not.
        then = [compare_file(SPEC)]
        self.assertEqual(pr_change_since(then, [compare_file(SPEC, sha="s2")]), DiffStat({}, 0))

    def test_changed_patch_lines_are_counted(self):
        now = [compare_file(SPEC, sha="s2", patch="@@ -1 +1,2 @@\n-a\n+b\n+c")]
        self.assertEqual(pr_change_since([compare_file(SPEC)], now), DiffStat({SPEC: "modified"}, 1))

    def test_a_new_path_keeps_its_status(self):
        now = [compare_file(SPEC), compare_file("src/new.ts", "added", "s3", "@@ -0,0 +1 @@\n+x")]
        self.assertEqual(pr_change_since([compare_file(SPEC)], now), DiffStat({"src/new.ts": "added"}, 1))

    def test_a_file_without_a_patch_is_over_the_limit(self):
        now = [compare_file(SPEC, sha="s2", patch=None)]
        self.assertGreater(pr_change_since([compare_file(SPEC)], now).changed, 60)


class MainTest(unittest.TestCase):
    """main() on recorded-shape API answers: a minors-only r1 on REVIEWED, a 1-line fix on FIX."""

    def run_main(self, statuses, comments, head_review=None, argv=("160", "T2", "--post-carry")):
        posted = []
        answers = {
            "repos/tylers2222/Monocode-Unlocked/pulls/160/commits": [REVIEWED, FIX],
            f"repos/tylers2222/Monocode-Unlocked/commits/{REVIEWED}/statuses": statuses,
            f"repos/tylers2222/Monocode-Unlocked/commits/{FIX}/statuses": [],
            "repos/tylers2222/Monocode-Unlocked/issues/160/comments": comments,
            f"repos/tylers2222/Monocode-Unlocked/compare/tyler/main...{REVIEWED}": [compare_file(SPEC)],
            f"repos/tylers2222/Monocode-Unlocked/compare/tyler/main...{FIX}": [compare_file(SPEC, sha="s2", patch="@@ -1 +1 @@\n-a\n+c")],
            f"repos/tylers2222/Monocode-Unlocked/commits/{FIX}/status": [head_review] if head_review else [],
            f"repos/tylers2222/Monocode-Unlocked/commits/{FIX}/check-runs": [],
        }
        def gh(*args):
            if args[:2] == ("pr", "view"):
                return f'{{"headRefOid": "{FIX}", "baseRefName": "tyler/main"}}'
            posted.append(args)
            return ""
        with unittest.mock.patch.object(merge_rule, "api", lambda path, jq, paginate=True: answers[path]), \
                unittest.mock.patch.object(merge_rule, "gh", gh), unittest.mock.patch("builtins.print"):
            code = merge_rule.main(list(argv))
        return code, posted

    STATUS = {"state": "failure", "description": MINOR, "created_at": "2026-10-20T11:01:08Z", "target_url": PR_URL}
    PROOF = {"body": f"fixer-proof: m1 test={TEST}", "createdAt": "2026-10-20T12:30:00Z", "authorAssociation": "OWNER"}

    def test_carry_is_posted_once(self):
        code, posted = self.run_main([self.STATUS], [self.PROOF])
        self.assertEqual((code, len(posted)), (0, 1))
        self.assertIn(f"repos/tylers2222/Monocode-Unlocked/statuses/{FIX}", posted[0])
        already = {"context": "review/agent", "state": "success", "description": "carried from 7ce96f5: minors-only + fixer proof (T2)"}
        self.assertEqual(self.run_main([self.STATUS], [self.PROOF], already), (0, []))

    def test_no_post_without_the_flag(self):
        self.assertEqual(self.run_main([self.STATUS], [self.PROOF], argv=("160", "T2")), (0, []))

    def test_another_prs_verdict_is_not_carried(self):
        foreign = {**self.STATUS, "target_url": "https://github.com/tylers2222/Monocode-Unlocked/pull/159"}
        self.assertEqual(self.run_main([foreign], [self.PROOF]), (1, []))

    def test_review_in_progress_blocks(self):
        pending = {"context": "review/agent", "state": "pending", "description": "r2 in review"}
        self.assertEqual(self.run_main([self.STATUS], [self.PROOF], pending), (1, []))



WORKFLOWS = Path(__file__).resolve().parents[3] / ".github" / "workflows"


def triggers(text: str) -> set[str]:
    on = re.search(r"(?ms)^on:\n(.*?)(?=^\S)", text + "\nend:")
    return set(re.findall(r"(?m)^  ([a-z_]+):", on.group(1))) if on else set()


class RerunListTest(unittest.TestCase):
    def test_rerun_follows_every_pull_request_workflow(self):
        """merge_rule.py waits for every check on the head, so gate-integrity-rerun.yml must
        re-run gate/integrity when any pull_request workflow finishes."""
        on_pr = set()
        for path in WORKFLOWS.glob("*.yml"):
            text = path.read_text()
            if "pull_request" in triggers(text):
                on_pr.add(re.search(r"(?m)^name: (.+)$", text).group(1).strip())
        rerun = (WORKFLOWS / "gate-integrity-rerun.yml").read_text()
        listed = re.search(r"(?m)^    workflows: \[(.*)\]$", rerun).group(1)
        self.assertEqual({name.strip() for name in listed.split(",")}, on_pr)



class UpdateBranchMergeTest(unittest.TestCase):
    """pr_change_since on real git objects: GitHub's compare A...B is the diff from
    merge-base(A, B) to B, and this builds the same file list from a temporary repo."""

    def git(self, *args):
        return subprocess.run(["git", "-C", self.repo, *args], check=True, capture_output=True, text=True).stdout

    def write(self, path, lines):
        (Path(self.repo) / path).write_text("".join(f"{line}\n" for line in lines))

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-qm", message)
        return self.git("rev-parse", "HEAD").strip()

    def compare(self, base, head):
        merge_base = self.git("merge-base", base, head).strip()
        files = []
        for row in self.git("diff", "--name-status", "--no-renames", merge_base, head).splitlines():
            code, path = row.split("\t")
            status = {"A": "added", "D": "removed"}.get(code, "modified")
            patch = self.git("diff", merge_base, head, "--", path)
            patch = patch[patch.index("@@"):] if "@@" in patch else None
            sha = self.git("rev-parse", f"{head}:{path}").strip() if status != "removed" else None
            files.append({"filename": path, "status": status, "sha": sha, "patch": patch})
        return files

    def setUp(self):
        self.repo = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.repo)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "t@example.com")
        self.git("config", "user.name", "t")
        self.body = [f"line {n}" for n in range(1, 101)]
        self.write("a.ts", self.body)
        self.write("b.ts", ["other"])
        self.commit("base")
        self.git("checkout", "-qb", "pr")
        self.write("a.ts", self.body[:50] + ["pr change"] + self.body[50:])
        self.reviewed = self.commit("pr: reviewed")
        self.git("checkout", "-q", "main")
        self.write("a.ts", ["main change at the top"] + self.body)
        self.write("b.ts", ["other, changed on main"])
        self.main = self.commit("main moves")
        self.git("checkout", "-q", "pr")

    def merge_main(self, edit=None):
        """Update branch: merge main into the PR, with an extra edit hidden in the merge commit."""
        self.git("merge", "-q", "--no-commit", "--no-ff", "main")
        if edit:
            edit()
        return self.commit("Merge branch 'main' into pr")

    def change_since_review(self, head):
        then = self.compare(self.main, self.reviewed)
        return then, pr_change_since(then, self.compare(self.main, head))

    def test_clean_update_branch_adds_nothing(self):
        _, since = self.change_since_review(self.merge_main())
        self.assertEqual(since, DiffStat({}, 0), "main's own edits to a.ts and b.ts are not the PR's change")

    def test_edit_hidden_in_the_merge_counts(self):
        lines = (Path(self.repo) / "a.ts").read_text
        hidden = lambda: (Path(self.repo) / "a.ts").write_text(lines().replace("line 80\n", "line 80, edited in the merge\n"))
        _, since = self.change_since_review(self.merge_main(hidden))
        self.assertEqual(since, DiffStat({"a.ts": "modified"}, 2), "one changed line: its - and + lines")

    def test_61_lines_hidden_in_the_merge_block_the_carry(self):
        def hidden():
            path = Path(self.repo) / "a.ts"
            path.write_text(path.read_text() + "".join(f"merge line {n}\n" for n in range(61)))
        then, since = self.change_since_review(self.merge_main(hidden))
        self.assertEqual(since.changed, 61)
        reviewed_files = DiffStat({file["filename"]: file["status"] for file in then}, 0)
        decision = decide_fix(tier="T2", reviewed_files=reviewed_files, since_review=since)
        self.assertFalse(decision.ok)
        self.assertIn("61 lines", decision.reason)

    def test_new_file_added_in_the_merge_blocks_the_carry(self):
        _, since = self.change_since_review(self.merge_main(lambda: self.write("c.ts", ["new in the merge"])))
        self.assertEqual(since.files, {"c.ts": "added"})
        decision = decide_fix(tier="T2", reviewed_files=DiffStat({"a.ts": "modified"}, 0), since_review=since)
        self.assertFalse(decision.ok)
        self.assertIn("adds a new file (c.ts)", decision.reason)


if __name__ == "__main__":
    unittest.main()
