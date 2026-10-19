#!/usr/bin/env bash
# Fails when a lint baseline grows from <base> to <head>: an eslint-suppressions.json
# entry (file + rule) whose count went up, or a Rust file with more #[expect] lint
# attributes. Fixed debt may leave the baselines; new debt may not enter them.
# Usage: scripts/gate/baseline-growth.sh <base-rev> [<head-rev>]
set -euo pipefail

base=$1
head=${2:-HEAD}
expect_attr='(^|[^.[:alnum:]_])expect\(( *$|clippy::|[a-z_]+ *[,)])'

if ! git cat-file -e "$base:eslint-suppressions.json" 2>/dev/null; then
  echo "$base has no eslint-suppressions.json, so there is no baseline to compare."
  exit 0
fi

eslint_counts() {
  { git show "$1:eslint-suppressions.json" 2>/dev/null || echo '{}'; } |
    jq -r 'to_entries[] | .key as $file | .value | to_entries[] | "\($file) \(.key)\t\(.value.count)"'
}

expect_counts() {
  { git grep -cE "$expect_attr" "$1" -- '*.rs' ':!vendor' || true; } |
    awk -v skip="${#1}" '{ line = substr($0, skip + 2); at = match(line, /:[0-9]+$/); print substr(line, 1, at - 1) "\t" substr(line, at + 1) }'
}

report_growth() {
  awk -F'\t' -v what="$1" '
    NR == FNR { base[$1] = $2; next }
    $2 > base[$1] + 0 { printf "%s grew: %s %d -> %d\n", what, $1, base[$1], $2; grew = 1 }
    END { exit grew }' "$2" "$3"
}

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
eslint_counts "$base" > "$dir/eslint.base"
eslint_counts "$head" > "$dir/eslint.head"
expect_counts "$base" > "$dir/expect.base"
expect_counts "$head" > "$dir/expect.head"

status=0
report_growth "eslint-suppressions.json" "$dir/eslint.base" "$dir/eslint.head" || status=1
report_growth "#[expect] markers" "$dir/expect.base" "$dir/expect.head" || status=1
if [ "$status" -ne 0 ]; then
  echo "Lint baselines may only shrink. Fix the new problem instead of suppressing it."
fi
exit "$status"
