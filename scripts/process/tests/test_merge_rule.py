import sys
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from merge_rule import (  # noqa: E402
    Decision,
    DiffStat,
    Proof,
    Verdict,
    classify_checks,
    decide,
    parse_proofs,
    parse_verdict,
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
            parse_proofs([{"body": body, "createdAt": "2026-10-20T12:30:00Z"}]),
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
        proofs, errors = parse_proofs([{"body": body, "createdAt": "2026-10-20T12:30:00Z"}])
        self.assertEqual([proof.finding for proof in proofs], ["m2"])
        self.assertEqual(len(errors), len(malformed))
        for line, error in zip(malformed, errors):
            self.assertIn(f'"{line}"', error)


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


if __name__ == "__main__":
    unittest.main()
