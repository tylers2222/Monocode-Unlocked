#!/usr/bin/env bash
# Test for review-status.sh with a stub gh on PATH that logs each call. A success or
# failure summary that merge_rule.py would not read as a verdict must exit 2 before any
# gh call; a readable one, or any pending summary, must reach gh and post.
# Usage: bash scripts/gate/review-status.test.sh
set -uo pipefail

script=$(cd "$(dirname "$0")" && pwd)/review-status.sh
sha=0123456789abcdef0123456789abcdef01234567
stub=$(mktemp -d)
trap 'rm -rf "$stub"' EXIT
log=$stub/gh.log
cat > "$stub/gh" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$log"
[ "\$1" = pr ] && echo "OPEN $sha"
exit 0
EOF
chmod +x "$stub/gh"

failed=0
case_() {
  local name=$1 want=$2 text=$3 posts=$4 state=$5 summary=$6
  : > "$log"
  local out code
  out=$(PATH=$stub:$PATH bash "$script" 527 "$sha" "$state" "$summary" 2>&1)
  code=$?
  local posted=no
  grep -q "statuses/$sha .*description=$summary" "$log" && posted=yes
  if [ "$code" != "$want" ] || [[ $out != *"$text"* ]] || [ "$posted" != "$posts" ]; then
    echo "FAIL $name: exit $code (want $want), posted $posted (want $posts), output: $out, gh calls: $(cat "$log")"
    failed=1
  elif [ "$posts" = no ] && [ -s "$log" ]; then
    echo "FAIL $name: refused but called gh: $(cat "$log")"
    failed=1
  else
    echo "ok   $name"
  fi
}

case_ 'no round' 2 'with failure' no failure 'changes needed: 1B/0M/0m'
case_ 'counts but no verdict word (#527 r2)' 2 'with success' no success 'r2: 0B 0M 4m. B1/M1 probe passes'
case_ 'two letters' 2 'nothing posted' no success 'r2bb clean: 0B/0M/0m'
case_ 'no word boundary' 2 'nothing posted' no success 'r2 cleanup done'
case_ 'clean on a failure status' 2 'nothing posted' no failure 'r2 clean: 0B/0M/0m'
case_ 'changes needed on a success status' 2 'nothing posted' no success 'r1 changes needed: 1B/0M/0m'
case_ 'clean with a blocker' 2 'nothing posted' no success 'r2 clean: 1B/0M/0m'
case_ 'clean verdict' 0 'posted review/agent success' yes success 'r3 clean: 0B/0M/5m all fixed'
case_ 'changes-needed verdict' 0 'posted review/agent failure' yes failure 'r1 changes needed: 1B/0M/0m'
case_ 'lettered round (#527)' 0 'posted review/agent success' yes success 'r2b clean: every destroy reaches destroy_window'
case_ 'lettered round, failure' 0 'posted review/agent failure' yes failure 'r2b changes needed: 0B/0M/1m'
case_ 'pending is not checked' 0 'posted review/agent pending' yes pending 'review started'

exit $failed
