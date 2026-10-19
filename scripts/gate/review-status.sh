#!/usr/bin/env bash
# Posts the reviewer's verdict as commit status `review/agent` on the commit that was
# reviewed. The reviewer records the PR head at the start of the review and passes it
# here. The script refuses unless that commit is still the PR's head, so a push during
# the review stops the verdict. A later push makes a new head with no `review/agent`
# status, so the verdict lapses on every push.
# Usage: scripts/gate/review-status.sh <pr> <reviewed-sha> <success|failure|pending> <summary>
# GATE_REPO overrides the repository (default tylers2222/Monocode-Unlocked).
set -euo pipefail

repo=${GATE_REPO:-tylers2222/Monocode-Unlocked}
fail() { echo "review-status: $*" >&2; exit 2; }

[ $# -eq 4 ] || fail "usage: review-status.sh <pr> <reviewed-sha> <success|failure|pending> <summary>"
pr=$1 sha=$2 state=$3 summary=$4
[[ $pr =~ ^[0-9]+$ ]] || fail "PR must be a number, got: $pr"
[[ $sha =~ ^[0-9a-f]{40}$ ]] || fail "reviewed SHA must be 40 hex characters, got: $sha"
case $state in success | failure | pending) ;; *) fail "state must be success, failure or pending, got: $state" ;; esac
[ -n "$summary" ] || fail "summary is empty"
[ "${#summary}" -le 140 ] || fail "summary is ${#summary} characters; GitHub keeps at most 140"

read -r pr_state head < <(gh pr view "$pr" -R "$repo" --json state,headRefOid -q '"\(.state) \(.headRefOid)"') \
  || fail "cannot read PR #$pr in $repo"
[ "$pr_state" = OPEN ] || fail "PR #$pr is $pr_state"
[ "$head" = "$sha" ] || fail "PR #$pr head is $head, not the reviewed $sha; review the new head and post again"

gh api -X POST "repos/$repo/statuses/$sha" -f state="$state" -f context=review/agent -f description="$summary" \
  -f target_url="https://github.com/$repo/pull/$pr" > /dev/null || fail "GitHub refused the status for $sha"
echo "review-status: posted review/agent $state on $sha (PR #$pr)"
