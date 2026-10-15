#!/usr/bin/env bash
# Write /tmp/giga-eval/report.md: a pass/fail table with evidence paths.
#
#   report.sh [run-id]   (default: the latest run in results.tsv)
source "$(dirname "$0")/lib.sh"

[[ -s "$RESULTS" ]] || die "no results in $RESULTS"
run="${1:-$(tail -1 "$RESULTS" | cut -f1)}"
out="$EVAL_OUT/report.md"
rows="$(awk -F'\t' -v r="$run" '$1 == r' "$RESULTS")"
[[ -n "$rows" ]] || die "no rows for run $run"
pass="$(awk -F'\t' '$4 == "PASS"' <<<"$rows" | grep -c . || true)"
fail="$(awk -F'\t' '$4 == "FAIL"' <<<"$rows" | grep -c . || true)"
{
  echo "# Change F live eval, run $run"
  echo
  echo "$pass passed, $fail failed. Evidence under \`$EVAL_OUT/run-$run/\`."
  echo
  echo "| Case | Verdict | Check | Evidence |"
  echo "|---|---|---|---|"
  awk -F'\t' '{ gsub(/\|/, "\\|"); printf "| %s | %s | %s | `%s` |\n", $2, $4, $3, $5 }' <<<"$rows"
  echo
  echo "## Per case"
  echo
  awk -F'\t' '
    { total[$2]++; if ($4 == "FAIL") bad[$2]++ }
    END { for (c in total) printf "- Case %s: %s (%d checks, %d failed)\n", c, (bad[c] ? "FAIL" : "PASS"), total[c], bad[c] }
  ' <<<"$rows" | sort
} >"$out"
echo "$out ($pass pass, $fail fail)"
