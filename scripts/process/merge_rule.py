#!/usr/bin/env python3
"""merge-rule: blocks a PR head that has no clean review verdict on it.

The reviewer posts its verdict as the commit status review/agent on the commit it read
(scripts/gate/review-status.sh), so a push leaves the new head without a verdict. The
head may merge only when the latest verdict posted for this PR is clean and on the
head; a T3 head also needs a green gate/mutation check run on it from the
gate-mutation workflow, unless its title's ticket is in mutation-exempt-tickets.txt
(read beside this script, so from the gate's default-branch checkout). For a
minors-only verdict the reason names each minor (m1 to mK) that has no fixer-proof
comment line by a collaborator yet; with every proof in, the head still needs a
re-review verdict. A verdict is never carried to a newer head.

Usage: python3 scripts/process/merge_rule.py <pr> <T1|T2|T3>
Prints "merge-rule: <reason>" and exits 0 when the head may merge, 1 when it may not and
2 when gh fails. It only reads GitHub. GATE_REPO overrides the repository (default
tylers2222/Monocode-Unlocked).
"""
import argparse
import json
import os
import re
import shlex
import subprocess
import sys
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from pr_body import ADE_TITLE

REPO = os.environ.get("GATE_REPO") or "tylers2222/Monocode-Unlocked"
TIERS = ("T1", "T2", "T3")
REVIEW_CONTEXT = "review/agent"
TRUSTED = frozenset({"OWNER", "MEMBER", "COLLABORATOR"})
# gate/mutation counts only from this check name, app (GitHub Actions) and workflow file.
MUTATION_CHECK, MUTATION_APP, MUTATION_WORKFLOW = "gate/mutation", 15368, ".github/workflows/gate-mutation.yml"
EXEMPT_LIST = Path(__file__).with_name("mutation-exempt-tickets.txt")

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
class Decision:
    ok: bool
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
    return Decision(False, reason)


def exempt_ticket(title: str, listed: str) -> str | None:
    """The PR's ticket (ADE-<n> at the start of its title, as pr_body.py reads it) when
    listed, the text of the exempt list, holds it on a line of its own."""
    match = ADE_TITLE.match(title)
    tickets = {line.strip() for line in listed.splitlines() if not line.startswith("#")}
    return match[0] if match and match[0] in tickets else None


def decide(tier: str, head: str, verdicts: Sequence[Verdict], proofs: Iterable[Proof],
           mutation: str | None = None, exempt: str | None = None) -> Decision:
    """Decides on the latest verdict. mutation is mutation_result() for the head: a
    conclusion, "pending", or None when there is no trusted gate/mutation run. exempt is
    exempt_ticket() for the PR: a T3 head of that ticket needs no green gate/mutation."""
    verdict = latest(verdicts)
    if verdict is None:
        return refuse("no review verdict yet")
    name, sha7, head7 = describe(verdict), verdict.sha[:7], head[:7]
    where = f"on head {head7}" if verdict.sha == head else f"on {sha7}, not the head {head7}"
    if verdict.clean and verdict.sha == head:
        if tier == "T3" and mutation != "success":
            if exempt is None:
                return refuse(f"T3 needs a green gate/mutation on the head (it is {mutation or 'missing'})")
            return Decision(True, f"{name} on head {head7}; {exempt} is exempt from gate/mutation until F1 (it is {mutation or 'missing'})")
        return Decision(True, f"{name} on head {head7}")
    if verdict.clean:
        return refuse(f"{name} {where}: the head has new commits; it needs a new review verdict")
    if verdict.minors is None or verdict.blockers or verdict.majors or not verdict.minors:
        return refuse(f"{name} {where}: fix the findings, then get a new review verdict on the head")
    proven = {proof.finding for proof in proofs if proof.at > verdict.at}
    missing = [f"m{n}" for n in range(1, verdict.minors + 1) if f"m{n}" not in proven]
    if missing:
        return refuse(f"no fixer-proof after r{verdict.round} ({sha7}) for {', '.join(missing)}")
    return refuse(f"{name} {where}: each minor has a fixer proof; the head needs a re-review verdict")


def mutation_result(check_runs: Iterable[Mapping], head: str, workflow_paths: Mapping[int, str]) -> str | None:
    """The head's gate/mutation result: the newest check run named gate/mutation, from
    GitHub Actions, for exactly this head, whose workflow (workflow_paths, by check run id)
    is the gate-mutation workflow. Its conclusion, "pending" while it runs, or None."""
    runs = [run for run in check_runs if run["name"] == MUTATION_CHECK and run["app_id"] == MUTATION_APP
            and run["head_sha"] == head and workflow_paths.get(run["id"]) == MUTATION_WORKFLOW]
    if not runs:
        return None
    run = max(runs, key=lambda run: run["id"])
    return run["conclusion"] if run["status"] == "completed" else "pending"


def gh(*args: str) -> str:
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def api(path: str, jq: str, paginate: bool = True) -> list:
    """Reads a GitHub API path; jq picks the values, one JSON value per line."""
    args = ["api", path, "--jq", f"{jq} | tojson", *(["--paginate"] if paginate else [])]
    return [json.loads(line) for line in gh(*args).splitlines()]


def workflow_path(details_url: str | None) -> str | None:
    """The workflow file of a GitHub Actions check run, from its run id."""
    match = re.search(r"/actions/runs/(\d+)/", details_url or "")
    return json.loads(gh("api", f"repos/{REPO}/actions/runs/{match[1]}", "--jq", ".path | tojson")) if match else None


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="merge_rule.py", description="Blocks a PR head that has no clean review verdict on it.")
    parser.add_argument("pr", type=int)
    parser.add_argument("tier", choices=TIERS)
    args = parser.parse_args(argv)

    pr = json.loads(gh("pr", "view", str(args.pr), "-R", REPO, "--json", "headRefOid,title"))
    head = pr["headRefOid"]
    review_statuses = f'.[] | select(.context == "{REVIEW_CONTEXT}") | {{state, description, created_at, target_url}}'
    statuses_by_sha = {sha: api(f"repos/{REPO}/commits/{sha}/statuses", review_statuses)
                       for sha in api(f"repos/{REPO}/pulls/{args.pr}/commits", ".[].sha")}
    verdicts = collect_verdicts(statuses_by_sha, f"https://github.com/{REPO}/pull/{args.pr}")
    comments = ".[] | {body, createdAt: .created_at, authorAssociation: .author_association}"
    proofs, errors = parse_proofs(api(f"repos/{REPO}/issues/{args.pr}/comments", comments))
    runs = api(f"repos/{REPO}/commits/{head}/check-runs", ".check_runs[] | {id, name, status, conclusion, details_url, head_sha, app_id: .app.id}")
    workflow_paths = {run["id"]: workflow_path(run["details_url"]) for run in runs if run["name"] == MUTATION_CHECK}

    exempt = exempt_ticket(pr["title"], EXEMPT_LIST.read_text())
    decision = decide(args.tier, head, verdicts, proofs, mutation_result(runs, head, workflow_paths), exempt)
    for line in [*errors, decision.reason]:
        print(f"merge-rule: {line}")
    return 0 if decision.ok else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        print(f"merge-rule: {shlex.join(error.cmd)} failed: {error.stderr.strip()}", file=sys.stderr)
        sys.exit(2)
