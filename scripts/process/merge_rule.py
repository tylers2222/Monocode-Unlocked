#!/usr/bin/env python3
"""merge-rule: decides if a PR head may merge on its review/agent verdict.

The reviewer posts its verdict as the commit status review/agent on the commit it read
(scripts/gate/review-status.sh), so a push leaves the new head without a verdict. A
minors-only verdict on an older commit carries to the head when a fixer-proof comment
line by a collaborator covers each minor finding (m1 to mK), the PR's own change since
the verdict adds no file, stays inside the reviewed files and changes at most 60 lines,
and the other checks on the head pass. T2 and T3 carry only a round 1 verdict, and T3
also needs this run's gate/mutation green. Only verdicts posted for this PR count. Any
other head needs a new review verdict.

Usage: python3 scripts/process/merge_rule.py <pr> <T1|T2|T3> [--mutation <result>] [--post-carry]
Prints "merge-rule: <reason>" and exits 0 when the head may merge, 1 when it may not and
2 when gh fails. Only --post-carry writes to GitHub: it posts a carry as review/agent
success on the head. GATE_REPO overrides the repository (default
tylers2222/Monocode-Unlocked).
"""
import argparse
import json
import os
import re
import shlex
import subprocess
import sys
from collections import Counter
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime

REPO = os.environ.get("GATE_REPO") or "tylers2222/Monocode-Unlocked"
TIERS = ("T1", "T2", "T3")
MAX_FIX_LINES = 60
NEW_FILE = frozenset({"added", "renamed", "copied"})
PASSING = frozenset({"success", "skipped", "neutral"})
# The review verdict is what this script decides on, so it is not a check to wait for.
REVIEW_CONTEXT = "review/agent"
TRUSTED = frozenset({"OWNER", "MEMBER", "COLLABORATOR"})

VERDICT = re.compile(r"^r(\d+) (clean|changes needed)\b(?::\s*(\d+)B/(\d+)M/(\d+)m)?")
_TEST = r"[^\s|](?:[^|]*[^\s|])?"
_MUTANT = r"[^\s|]+"
PROOF = re.compile(
    rf"fixer-proof: (?P<finding>[A-Za-z]+[0-9]+) "
    rf"(?:test=(?P<test>{_TEST})(?: \| mutant=(?P<killed>{_MUTANT}))?|mutant=(?P<mutant>{_MUTANT}))"
)
PROOF_FORMAT = "fixer-proof: <id> test=<name>, mutant=<id>, or test=<name> | mutant=<id>"


@dataclass(frozen=True)
class Verdict:
    sha: str
    round: int
    clean: bool
    blockers: int | None
    majors: int | None
    minors: int | None
    at: datetime


@dataclass(frozen=True)
class Proof:
    finding: str
    test: str | None
    mutant: str | None
    at: datetime


@dataclass(frozen=True)
class DiffStat:
    files: dict[str, str]  # path to GitHub compare status: added, modified, removed, renamed
    changed: int  # additions plus deletions


@dataclass(frozen=True)
class Decision:
    ok: bool
    carry: bool
    reason: str


def parse_time(text: str) -> datetime:
    # fromisoformat reads a trailing "Z" only from Python 3.11.
    return datetime.fromisoformat(text.replace("Z", "+00:00"))


def parse_verdict(sha: str, description: str, at: datetime) -> Verdict | None:
    match = VERDICT.match(description)
    if match is None:
        return None
    number, kind, *counts = match.groups()
    if kind == "clean":
        return Verdict(sha, int(number), True, 0, 0, 0, at)
    blockers, majors, minors = (None if count is None else int(count) for count in counts)
    return Verdict(sha, int(number), False, blockers, majors, minors, at)


def parse_proofs(comments: Iterable[Mapping[str, str]]) -> tuple[list[Proof], list[str]]:
    """Proof lines from comments by the repository's owner, members or collaborators; the
    repository is public, so anyone else's comment is ignored."""
    proofs: list[Proof] = []
    errors: list[str] = []
    for comment in comments:
        if comment.get("authorAssociation") not in TRUSTED:
            continue
        at = parse_time(comment["createdAt"])
        for line in comment["body"].splitlines():
            if not line.startswith("fixer-proof:"):
                continue
            match = PROOF.fullmatch(line.rstrip())
            if match is None:
                errors.append(f"malformed fixer-proof line {json.dumps(line, ensure_ascii=False)}; use {PROOF_FORMAT}")
            else:
                proofs.append(Proof(match["finding"], match["test"], match["killed"] or match["mutant"], at))
    return proofs, errors


def collect_verdicts(statuses_by_sha: Mapping[str, Iterable[Mapping[str, str]]], pr_url: str) -> list[Verdict]:
    """Verdicts from the review/agent statuses of the PR's commits. A status belongs to a
    commit, not a PR, so one posted for another PR (its target_url) is not this PR's verdict."""
    return [
        verdict
        for sha, statuses in statuses_by_sha.items()
        for status in statuses
        if status["state"] != "pending" and status.get("target_url") == pr_url
        if (verdict := parse_verdict(sha, status["description"] or "", parse_time(status["created_at"])))
    ]


def latest(verdicts: Iterable[Verdict]) -> Verdict | None:
    return max(verdicts, key=lambda verdict: verdict.at, default=None)


def describe(verdict: Verdict) -> str:
    if verdict.clean:
        return f"r{verdict.round} clean"
    if None in (verdict.blockers, verdict.majors, verdict.minors):
        return f"r{verdict.round} changes needed (no B/M/m counts)"
    return f"r{verdict.round} changes needed ({verdict.blockers}B/{verdict.majors}M/{verdict.minors}m)"


def refuse(reason: str) -> Decision:
    return Decision(False, False, reason)


def decide(
    tier: str,
    head: str,
    verdicts: Sequence[Verdict],
    proofs: Iterable[Proof],
    reviewed_files: DiffStat,
    since_review: DiffStat,
    checks_failed: Sequence[str],
    checks_pending: Sequence[str],
    review_in_progress: bool = False,
    mutation: str | None = None,
) -> Decision:
    """Decides on the latest verdict. reviewed_files is the PR diff at the verdict's commit;
    since_review is the PR's own change since then. mutation is the gate/mutation result
    of this run (None when there is no such job)."""
    verdict = latest(verdicts)
    if verdict is None:
        return refuse("no review verdict yet")
    name, sha7, head7 = describe(verdict), verdict.sha[:7], head[:7]
    if verdict.sha == head:
        if verdict.clean:
            return Decision(True, False, f"{name} on head {head7}")
        if verdict.blockers == 0 and verdict.majors == 0 and verdict.minors:
            return refuse(f"{name} on head {head7}: push the minor fixes, then post a fixer-proof line for each")
        return refuse(f"{name} on head {head7}: fix the findings, then get a new review verdict")
    if verdict.minors is None or verdict.blockers or verdict.majors:
        return refuse(f"{name} on {sha7}: a blocker or major needs a new review verdict on the head {head7}")
    if verdict.clean:
        return refuse(f"{name} on {sha7}, but the head {head7} has new commits after the clean verdict; it needs a new review verdict")
    if not verdict.minors:
        return refuse(f"{name} on {sha7} names no minor to prove, so the head {head7} needs a new review verdict")
    if tier != "T1" and verdict.round != 1:
        return refuse(f"{name} on {sha7}: {tier} carries only a round 1 verdict, so the head {head7} needs a new review verdict")
    proven = {proof.finding for proof in proofs if proof.at > verdict.at}
    missing = [f"m{n}" for n in range(1, verdict.minors + 1) if f"m{n}" not in proven]
    if missing:
        return refuse(f"no fixer-proof after r{verdict.round} ({sha7}) for {', '.join(missing)}")
    since = f"since r{verdict.round} ({sha7}) the head"
    added = sorted(path for path, status in since_review.files.items() if status in NEW_FILE)
    if added:
        return refuse(f"{since} adds a new file ({', '.join(added)}); it needs a new review verdict")
    outside = sorted(path for path in since_review.files if path not in reviewed_files.files)
    if outside:
        return refuse(f"{since} touches files outside the reviewed diff ({', '.join(outside)}); it needs a new review verdict")
    if since_review.changed > MAX_FIX_LINES:
        return refuse(f"{since} changes {since_review.changed} lines, more than {MAX_FIX_LINES}; it needs a new review verdict")
    if tier == "T3" and mutation != "success":
        return refuse(f"T3 carries only with a green gate/mutation (it is {mutation or 'missing'})")
    if checks_failed:
        return refuse(f"failed checks: {', '.join(checks_failed)}")
    if checks_pending:
        return refuse(f"waiting for checks: {', '.join(checks_pending)}")
    if review_in_progress:
        return refuse(f"a review of the head {head7} is in progress; wait for its verdict")
    return Decision(True, True, f"carried from {sha7}: minors-only + fixer proof ({tier})")


def classify_checks(
    check_runs: Iterable[Mapping[str, str | None]], statuses: Iterable[Mapping[str, str]], own_run: str
) -> tuple[list[str], list[str]]:
    """Returns the failed and the pending names among the head's latest check runs and
    commit statuses. The jobs of this script's own workflow run (own_run) are skipped,
    so the gate does not wait for itself."""
    own = f"/actions/runs/{own_run}/"
    results = [(run["name"], run["status"] == "completed", run["conclusion"]) for run in check_runs
               if own not in (run["details_url"] or "")]
    results += [(status["context"], status["state"] != "pending", status["state"]) for status in statuses
                if status["context"] != REVIEW_CONTEXT]
    failed = {name for name, done, outcome in results if done and outcome not in PASSING}
    pending = {name for name, done, _ in results if not done}
    return sorted(failed), sorted(pending)


def pr_change_since(then: Iterable[Mapping[str, str | None]], now: Iterable[Mapping[str, str | None]]) -> DiffStat:
    """The PR's own change between two of its versions, from the GitHub compare files of
    base...reviewed (then) and base...head (now). Each side is measured from its own merge
    base, so merging the base branch in ("Update branch") adds nothing. A file counts the
    +/- patch lines that differ between the two sides; a file whose content differs but
    has no patch (binary, too large) counts as more than the limit."""
    before = {file["filename"]: file for file in then}
    after = {file["filename"]: file for file in now}
    files, changed = {}, 0
    for path in sorted(before.keys() | after.keys()):
        old, new = before.get(path), after.get(path)
        if old and new and (old["sha"], old["status"]) == (new["sha"], new["status"]):
            continue
        files[path] = "modified" if old and new else new["status"] if new else "reverted"
        sides = [file for file in (old, new) if file and file["status"] != "removed"]
        if any(file.get("patch") is None for file in sides):
            changed += MAX_FIX_LINES + 1
            continue
        a, b = (Counter(l for l in (file or {}).get("patch", "").splitlines() if l[:1] in "+-") for file in (old, new))
        changed += sum(((a - b) + (b - a)).values())
    return DiffStat(files, changed)


def gh(*args: str) -> str:
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def api(path: str, jq: str, paginate: bool = True) -> list:
    """Reads a GitHub API path; jq picks the values, one JSON value per line."""
    args = ["api", path, "--jq", f"{jq} | tojson", *(["--paginate"] if paginate else [])]
    return [json.loads(line) for line in gh(*args).splitlines()]


def post_carry(head: str, pr: int, reason: str) -> None:
    gh(
        "api", "-X", "POST", f"repos/{REPO}/statuses/{head}",
        "-f", "state=success",
        "-f", "context=review/agent",
        "-f", f"description={reason}",
        "-f", f"target_url=https://github.com/{REPO}/pull/{pr}",
    )
    print(f"merge-rule: posted review/agent success on {head}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="merge_rule.py", description="Decides if a PR head may merge on its review/agent verdict.")
    parser.add_argument("pr", type=int)
    parser.add_argument("tier", choices=TIERS)
    parser.add_argument("--mutation", help="this run's gate/mutation result; a T3 carry needs success")
    parser.add_argument("--post-carry", action="store_true", help="post a carry as review/agent success on the head (writes to GitHub)")
    args = parser.parse_args(argv)

    view = json.loads(gh("pr", "view", str(args.pr), "-R", REPO, "--json", "headRefOid,baseRefName"))
    head, base = view["headRefOid"], view["baseRefName"]
    review_statuses = f'.[] | select(.context == "{REVIEW_CONTEXT}") | {{state, description, created_at, target_url}}'
    statuses_by_sha = {sha: api(f"repos/{REPO}/commits/{sha}/statuses", review_statuses)
                       for sha in api(f"repos/{REPO}/pulls/{args.pr}/commits", ".[].sha")}
    verdicts = collect_verdicts(statuses_by_sha, f"https://github.com/{REPO}/pull/{args.pr}")
    comments = ".[] | {body, createdAt: .created_at, authorAssociation: .author_association}"
    proofs, errors = parse_proofs(api(f"repos/{REPO}/issues/{args.pr}/comments", comments))
    reviewed = latest(verdicts)
    if reviewed is None or reviewed.sha == head:
        reviewed_files = since_review = DiffStat({}, 0)
    else:
        files = ".files[] | {filename, status, sha, patch}"
        then = api(f"repos/{REPO}/compare/{base}...{reviewed.sha}", files, paginate=False)
        reviewed_files = DiffStat({file["filename"]: file["status"] for file in then}, 0)
        since_review = pr_change_since(then, api(f"repos/{REPO}/compare/{base}...{head}", files, paginate=False))
    head_statuses = api(f"repos/{REPO}/commits/{head}/status", ".statuses[] | {context, state, description}")
    head_review = next((s for s in head_statuses if s["context"] == REVIEW_CONTEXT), None)
    checks_failed, checks_pending = classify_checks(
        api(f"repos/{REPO}/commits/{head}/check-runs", ".check_runs[] | {name, status, conclusion, details_url}"),
        head_statuses,
        os.environ.get("GITHUB_RUN_ID", ""),
    )

    decision = decide(args.tier, head, verdicts, proofs, reviewed_files, since_review, checks_failed, checks_pending,
                      review_in_progress=bool(head_review and head_review["state"] == "pending"), mutation=args.mutation)
    for line in [*errors, decision.reason]:
        print(f"merge-rule: {line}")
    already = head_review and head_review["state"] == "success" and head_review["description"] == decision.reason
    if args.post_carry and decision.carry and not already:
        post_carry(head, args.pr, decision.reason)
    return 0 if decision.ok else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        print(f"merge-rule: {shlex.join(error.cmd)} failed: {error.stderr.strip()}", file=sys.stderr)
        sys.exit(2)
