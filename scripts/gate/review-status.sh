#!/usr/bin/env bash
# Posts the reviewer's verdict as commit status `review/agent` on a PR's head commit.
# It reads the head SHA, reads it again just before posting, and refuses if the head
# moved, so a verdict lands only on the commit that was reviewed. A later push makes
# a new head with no `review/agent` status, so the verdict lapses on every push.
# Usage: scripts/gate/review-status.sh <pr> <success|failure|pending> <summary>
# GATE_REPO overrides the repository (default tylers2222/Monocode-Unlocked).
set -euo pipefail

repo=${GATE_REPO:-tylers2222/Monocode-Unlocked}
fail() { echo "review-status: $*" >&2; exit 2; }

[ $# -eq 3 ] || fail "usage: review-status.sh <pr> <success|failure|pending> <summary>"
pr=$1 state=$2 summary=$3
[[ $pr =~ ^[0-9]+$ ]] || fail "PR must be a number, got: $pr"
case $state in success | failure | pending) ;; *) fail "state must be success, failure or pending, got: $state" ;; esac
[ -n "$summary" ] || fail "summary is empty"
[ "${#summary}" -le 140 ] || fail "summary is ${#summary} characters; GitHub keeps at most 140"

head_of() {
  gh pr view "$pr" -R "$repo" --json state,headRefOid -q 'if .state == "OPEN" then .headRefOid else "closed" end' \
    || fail "cannot read PR #$pr in $repo"
}

sha=$(head_of)
[ "$sha" != closed ] || fail "PR #$pr is not open"
again=$(head_of)
[ "$again" = "$sha" ] || fail "the head of PR #$pr moved from $sha to $again; review the new head and post again"

gh api -X POST "repos/$repo/statuses/$sha" -f state="$state" -f context=review/agent -f description="$summary" \
  -f target_url="https://github.com/$repo/pull/$pr" > /dev/null || fail "GitHub refused the status for $sha"
echo "review-status: posted review/agent $state on $sha (PR #$pr)"
