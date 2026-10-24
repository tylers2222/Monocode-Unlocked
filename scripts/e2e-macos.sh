#!/usr/bin/env bash
# The e2e-macos job of .github/workflows/e2e.yml, on a hosted macOS runner.
#   e2e-macos.sh <out> <all|scenarios|milestones>
# Needs the instance that `scripts/scenarios/app.sh build` builds. Records the
# screen to <out>/screen.mov, writes one log per scenario and a screenshot of
# each failure, and writes <out>/results.tsv and junit.xml in the row format of
# docker/e2e-run.sh. Exits 1 when a row failed.
set -uo pipefail
trap '' PIPE
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/scripts/scenarios/app.sh"
OUT=$1
ROWS="$OUT/results.tsv"

row() { printf '%s\t%s\t%s\t%s\t%s\n' "$@" >>"$ROWS"; }

# screencapture -v records until a character arrives on its stdin.
record_start() {
  mkfifo "$OUT/.stop"
  screencapture -v -C "$OUT/screen.mov" <"$OUT/.stop" &
  RECORDER=$!
  exec 3>"$OUT/.stop"
}
record_stop() {
  echo >&3
  exec 3>&-
  wait "$RECORDER" || echo "e2e-macos: screencapture exited $?" >&2
  rm -f "$OUT/.stop"
}

suite_scenarios() {
  local code=0 name started log status
  for name in $(bash "$APP" list); do
    started=$SECONDS log="$OUT/$name.log"
    echo "== scenarios: $name"
    bash "$APP" "$name" 2>&1 | tee "$log"
    status=${PIPESTATUS[0]}
    if ((status == 0)); then
      row scenarios "$name" passed $((SECONDS - started)) ""
    else
      screencapture -x "$OUT/$name-failed.png"
      row scenarios "$name" failed $((SECONDS - started)) \
        "exit $status: $(grep -m1 -E 'FAIL|ERROR' "$log" | tr '\t' ' ')"
      code=1
    fi
    # A scenario that stops early leaves its instance running.
    bash "$ROOT/scripts/giga-eval/instance.sh" stop
  done
  return "$code"
}

# On macOS the milestones E2E holds its turns on omp with a real model, and CI
# has no model. The fx puppet mode (#337) runs it in the Linux image.
suite_milestones() {
  row milestones "scripts/milestones-e2e/run.sh" skipped 0 "needs omp and a real model on macOS; CI has none"
}

case ${2:-} in
  all) suites="scenarios milestones" ;;
  scenarios | milestones) suites=$2 ;;
  *) sed -n '2,7p' "$0"; exit 2 ;;
esac
mkdir -p "$OUT"
printf 'suite\ttest\tstatus\tseconds\tnote\n' >"$ROWS"
record_start
code=0
for suite in $suites; do "suite_$suite" || code=1; done
record_stop
node "$ROOT/docker/e2e-report.mjs" junit "$ROWS" >"$OUT/junit.xml" || code=1
exit "$code"
