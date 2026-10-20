import sys
import unittest
import unittest.mock
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import merge_rule
from merge_rule import (
    Decision,
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
    """decide() for PR 160 at the head FIX after its minors-only r1, with proof for m1. A
    test changes one input."""
    inputs = {"tier": "T2", "head": FIX, "verdicts": [verdict(MINOR)], "proofs": proven("m1"), "mutation": None}
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
        self.assertEqual(decide_fix(verdicts=[]), Decision(False, "no review verdict yet"))

    def test_clean_verdict_on_head_passes(self):
        for tier in ("T1", "T2"):
            with self.subTest(tier=tier):
                self.assertEqual(decide_fix(tier=tier, verdicts=[verdict(CLEAN_R2, sha=FIX)]), Decision(True, f"r2 clean on head {FIX[:7]}"))

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

    def test_a_minors_only_verdict_with_every_proof_still_needs_a_re_review(self):
        for tier in ("T1", "T2", "T3"):
            with self.subTest(tier=tier):
                self.assertRefused(decide_fix(tier=tier, mutation="success"), "each minor has a fixer proof", "re-review")

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
        self.assertEqual(decision, Decision(False, "no fixer-proof after r1 (7ce96f5) for m2"))

    def test_proof_posted_before_verdict_does_not_count(self):
        self.assertRefused(decide_fix(proofs=proven("m1", at=AT - timedelta(minutes=5))), "no fixer-proof", "m1")
        self.assertRefused(decide_fix(proofs=proven("m1", at=AT)), "no fixer-proof", "m1")


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
    """main() on recorded-shape API answers for PR 160: REVIEWED, then the head FIX. gh
    may only read the PR; any other gh call is a write and fails the test."""

    def run_main(self, statuses, comments=(), tier="T2", check_runs=()):
        answers = {
            "repos/tylers2222/Monocode-Unlocked/pulls/160/commits": [REVIEWED, FIX],
            f"repos/tylers2222/Monocode-Unlocked/commits/{REVIEWED}/statuses": statuses.get(REVIEWED, []),
            f"repos/tylers2222/Monocode-Unlocked/commits/{FIX}/statuses": statuses.get(FIX, []),
            "repos/tylers2222/Monocode-Unlocked/issues/160/comments": list(comments),
            f"repos/tylers2222/Monocode-Unlocked/commits/{FIX}/check-runs": list(check_runs),
        }
        def gh(*args):
            self.assertEqual(args[:2], ("pr", "view"), "merge_rule.py only reads")
            return f'{{"headRefOid": "{FIX}"}}'
        with unittest.mock.patch.object(merge_rule, "api", lambda path, jq, paginate=True: answers[path]), \
                unittest.mock.patch.object(merge_rule, "workflow_path", lambda url: url), \
                unittest.mock.patch.object(merge_rule, "gh", gh), unittest.mock.patch("builtins.print"):
            return merge_rule.main(["160", tier])

    MINOR_STATUS = {"state": "failure", "description": MINOR, "created_at": "2026-10-20T11:01:08Z", "target_url": PR_URL}
    CLEAN_STATUS = {"state": "success", "description": "r2 clean: fixed", "created_at": "2026-10-20T13:00:00Z", "target_url": PR_URL}
    PROOF = {"body": f"fixer-proof: m1 test={TEST}", "createdAt": "2026-10-20T12:30:00Z", "authorAssociation": "OWNER"}

    def test_a_proven_minors_only_verdict_on_an_older_commit_fails_and_posts_nothing(self):
        self.assertEqual(self.run_main({REVIEWED: [self.MINOR_STATUS]}, [self.PROOF]), 1)

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


if __name__ == "__main__":
    unittest.main()
