#!/usr/bin/env bash
# The e2e image's entrypoint: e2e-run.sh <all|browser|evals|milestones|scenarios>.
# Writes /results: results.tsv and junit.xml (see docker/e2e-report.mjs),
# browser/ (the Playwright JSON report, screenshots and failure traces), and
# last the done file holding the exit code. stdout is the run log.
#   e2e-run.sh --check <suite>   exit 0 when <suite> is a suite name, else 2
set -uo pipefail
# Each suite is the function suite_<name>; all runs them in this order.
SUITES=(browser evals milestones scenarios)
OUT=/results
ROWS="$OUT/results.tsv"

suite_list() {
  if [[ $1 == all ]]; then echo "${SUITES[@]}"; return; fi
  [[ " ${SUITES[*]} " == *" $1 "* ]] && echo "$1" && return
  echo "unknown suite '$1'; pick all or one of: ${SUITES[*]}" >&2
  return 2
}

row() { printf '%s\t%s\t%s\t%s\t%s\n' "$@" >>"$ROWS"; }

# One row for one command, for suites without a per-test report.
step() {
  local suite=$1 name=$2 started=$SECONDS
  shift 2
  echo "== $suite: $name"
  if "$@"; then
    row "$suite" "$name" passed $((SECONDS - started)) ""
  else
    row "$suite" "$name" failed $((SECONDS - started)) "exit $?"
    return 1
  fi
}

# Headed browsers on Xvfb, as on a desktop.
suite_browser() {
  echo "== browser: npx playwright test --headed"
  mkdir -p "$OUT/browser"
  PLAYWRIGHT_JSON_OUTPUT_NAME="$OUT/browser/report.json" xvfb-run -a -s "-screen 0 1280x1024x24" \
    npx playwright test --headed --reporter=list,json --output="$OUT/browser/test-results"
  local code=$?
  node docker/e2e-report.mjs playwright "$OUT/browser/report.json" >>"$ROWS" || code=1
  return "$code"
}

# The grader self-tests grade the recorded transcripts. The live eval run
# (evals/run.sh) builds a macOS GigaEval app and calls a real model, so it
# stays off this image.
suite_evals() {
  local code=0
  step evals "graders/milestones.mjs --self-test" node evals/graders/milestones.mjs --self-test || code=1
  step evals "graders/asks.mjs --self-test" node evals/graders/asks.mjs --self-test || code=1
  row evals "evals/run.sh (live Monos)" skipped 0 "needs a macOS GigaEval app and a real model"
  return "$code"
}

not_ported() {
  echo "== $1: not ported yet"
  row "$1" "$1 suite" skipped 0 "not ported yet (ADE-107 later part)"
}
suite_milestones() { not_ported milestones; }
suite_scenarios() { not_ported scenarios; }

if [[ "${1:-}" == --check ]]; then
  suite_list "${2:-}" >/dev/null
  exit
fi

suites=$(suite_list "${1:-all}") || exit
printf 'suite\ttest\tstatus\tseconds\tnote\n' >"$ROWS"
code=0
for suite in $suites; do
  "suite_$suite" || code=1
  echo "== $suite: done"
done
node docker/e2e-report.mjs junit "$ROWS" >"$OUT/junit.xml" || code=1
echo "$code" >"$OUT/done.tmp" && mv "$OUT/done.tmp" "$OUT/done"
exit "$code"
