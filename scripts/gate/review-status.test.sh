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

case_ 'lettered round (#527)' 2 'rN clean: xB/yM/zm' no success 'r2b clean: rebase identical, CI green'
case_ 'lettered round, failure' 2 'rN changes needed: xB/yM/zm' no failure 'r2b changes needed: 0B/0M/1m'
case_ 'no round' 2 'nothing posted' no failure 'changes needed: 1B/0M/0m'
case_ 'clean verdict' 0 'posted review/agent success' yes success 'r3 clean: 0B/0M/5m all fixed'
case_ 'changes-needed verdict' 0 'posted review/agent failure' yes failure 'r1 changes needed: 1B/0M/0m'
case_ 'pending is not checked' 0 'posted review/agent pending' yes pending 'r2b in review'

exit $failed
