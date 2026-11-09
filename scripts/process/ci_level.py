#!/usr/bin/env python3
"""ci-level: which of the two levels of CI a run is, for the `changes` jobs of ci.yml,
e2e.yml and native.yml.

Usage:
  ci_level.py <event> <payload-draft> <pr> <repo> <head>
      Prints `level=fast|full` (for GITHUB_OUTPUT) and the reason on stderr. Exits 1 after
      printing when a pull_request run's merge commit does not hold <head>.

level=fast is the fast set: lint, unit tests, Ubuntu. Only one state gives it: a pull
request that is a draft both in the event and when this runs, with no `ci:full` label.
Everything else is the full set, also when the PR cannot be read:
  - The payload alone is not enough: a re-run keeps the payload of its first attempt, so a
    run started on a draft says draft for ever. That is why a re-run reads the PR live.
  - The label only adds jobs. It is read on a draft and means nothing on a ready PR.

A job the fast set leaves out must not report a required check name as skipped, because
GitHub counts a skipped required check as passed. The jobs that report `ci` and `e2e-linux`
take another name when level=fast, so on a draft those contexts stay absent. Never make
this script print a path decision: merge-when-green.sh accepts a skipped e2e job only on
the `<job>=false` line of ci_changes.py, and a draft is not a reason it may accept.

A merge ref can go stale, and a run on a stale ref says nothing about the head GitHub
files its checks under. So a pull_request run must hold the event's head as its second
parent, else the `changes` job goes red, and `ci` and `e2e-linux` fail with it.
"""
import json
import subprocess
import sys
from collections.abc import Sequence
from dataclasses import dataclass

FULL_LABEL = "ci:full"


@dataclass(frozen=True)
class LivePr:
    draft: bool
    labels: tuple[str, ...]


def decide(event: str, payload_draft: str, read_live) -> tuple[bool, str]:
    """(run the full set, why). read_live is called only when the payload says draft; it
    returns a LivePr, or None when the PR could not be read."""
    if event != "pull_request":
        return True, f"{event or 'no'} event: full set"
    if payload_draft != "true":
        return True, f"event says ready (draft={payload_draft or 'empty'}): full set"
    live = read_live()
    if live is None:
        return True, "the PR could not be read: full set"
    if not live.draft:
        return True, "a draft in the event, ready now (a re-run): full set"
    if FULL_LABEL in live.labels:
        return True, f"a draft with the {FULL_LABEL} label: full set"
    return False, "a draft: fast set"


def parse_live(text: str) -> LivePr | None:
    """A LivePr from the REST pull request JSON. The draft field must be a real boolean."""
    try:
        data = json.loads(text)
        draft = data["draft"]
        labels = tuple(label["name"] for label in data["labels"])
    except (ValueError, KeyError, TypeError):
        return None
    return LivePr(draft, labels) if isinstance(draft, bool) else None


def read_pr(repo: str, pr: str) -> LivePr | None:
    result = subprocess.run(["gh", "api", f"repos/{repo}/pulls/{pr}"], capture_output=True, text=True)
    if result.returncode != 0:
        print(f"ci-level: gh api failed: {result.stderr.strip()}", file=sys.stderr)
        return None
    return parse_live(result.stdout)


def merge_head() -> str | None:
    """The second parent of the checked-out merge commit, or None when it has none."""
    result = subprocess.run(["git", "rev-parse", "--verify", "--quiet", "HEAD^2"], capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else None


def stale_reason(head: str, held: str | None) -> str | None:
    """Why a pull_request run proves nothing about head, or None when it does."""
    if head and held == head:
        return None
    return f"the merge commit holds {held or 'no second parent'}, not the PR head {head or '(empty)'}"


def main(argv: Sequence[str]) -> int:
    if len(argv) != 5:
        print(__doc__, file=sys.stderr)
        return 2
    event, payload_draft, pr, repo, head = argv
    full, why = decide(event, payload_draft, lambda: read_pr(repo, pr))
    print(f"ci-level: {why}", file=sys.stderr)
    print(f"level={'full' if full else 'fast'}")
    stale = stale_reason(head, merge_head()) if event == "pull_request" else None
    if stale:
        # On stderr: stdout goes to GITHUB_OUTPUT, and a line like this would void the file.
        print(f"::error::ci-level: {stale}. Push again or wait for GitHub to update the merge ref.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
