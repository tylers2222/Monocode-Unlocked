#!/usr/bin/env bash
# The e2e-macos job of .github/workflows/e2e.yml, on a hosted macOS runner.
#   e2e-macos.sh <out> <all|scenarios|milestones> [scenario] [repeat] [env]
# Runs every scenario of SCENARIOS that app.sh in this checkout has, or only
# [scenario] (an app.sh command line), each [repeat] times (default 1), and
# builds the instance each one needs. [env] holds space-separated switches
# that a scenario reads, NAME=value with a scenario prefix such as R6_.
# Writes one log and one screen recording per run, a screenshot of each
# failure, and results.tsv and junit.xml in the row format of
# docker/e2e-run.sh. Exits 1 when a row failed.
set -uo pipefail
trap '' PIPE
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/scripts/scenarios/app.sh"
OUT=$1
ROWS="$OUT/results.tsv"

# command | the app.sh build it runs on | why CI skips it (empty: CI runs it)
SCENARIOS=(
  "s0|build|"
  "d1|build|"
  "d4|build|"
  "d4 d1|build|"
  "r6|build|"
  "ade89 kill|build|"
  "ade89 term|build|"
  "approvals|build|reads a copy of the real app database, which a CI runner does not have"
  "ade89 two|build-two|"
)

row() { printf '%s\t%s\t%s\t%s\t%s\n' "$@" >>"$ROWS"; }

# True when app.sh has a case label for the command's first word.
has() { grep -qE "^  ${1%% *}\)" "$APP"; }

# screencapture -v records until a key arrives on its terminal. A pipe on its
# stdin is not enough (run 40095797823 hung on it until the job timed out, and
# SIGTERM in run 40248124930 left no .mov), so python's pty.spawn gives it a
# pty and copies the fifo's newline into it (script(1) refuses a fifo stdin).
# If it still runs, record_stop sends SIGTERM and last SIGKILL, each after
# 10 s. Not SIGINT: a background job of a script starts with SIGINT ignored.
record_start() {
  rm -f "$OUT/.stop"
  mkfifo "$OUT/.stop"
  python3 -c 'import pty, sys; sys.exit(pty.spawn(sys.argv[1:]) >> 8)' \
    screencapture -v -C "$1" <"$OUT/.stop" >"${1%.mov}-recorder.log" 2>&1 &
  RECORDER=$!
  exec 3>"$OUT/.stop"
}
exited() {
  local i
  for i in {1..20}; do
    kill -0 "$1" 2>/dev/null || return 0
    sleep 0.5
  done
  return 1
}
record_stop() {
  local signal
  echo >&3 2>/dev/null
  exec 3>&-
  for signal in TERM KILL; do
    exited "$RECORDER" && break
    echo "e2e-macos: screencapture still running; sending SIG$signal" >&2
    kill "-$signal" "$RECORDER" 2>/dev/null
  done
  wait "$RECORDER" || echo "e2e-macos: screencapture exited $?" >&2
}

# run <suite> <name> <log> <command...>: one row; a screenshot when it fails.
run() {
  local suite=$1 name=$2 log=$3 started=$SECONDS status
  shift 3
  echo "== $suite: $name"
  "$@" 2>&1 | tee "$log"
  status=${PIPESTATUS[0]}
  if ((status == 0)); then
    row "$suite" "$name" passed $((SECONDS - started)) ""
    return 0
  fi
  screencapture -x "${log%.log}-failed.png"
  row "$suite" "$name" failed $((SECONDS - started)) \
    "exit $status: $(grep -m1 -E 'FAIL|ERROR' "$log" | tr '\t' ' ')"
  return 1
}

# use_build <build>: build the instance once; later calls reuse the result.
BUILT=""
BUILD_FAILED=""
use_build() {
  [[ $BUILT == "$1" ]] && return 0
  [[ " $BUILD_FAILED " != *" $1 "* ]] || return 1
  if run build "app.sh $1" "$OUT/$1.log" bash "$APP" "$1"; then
    BUILT=$1
  else
    BUILD_FAILED+=" $1"
    return 1
  fi
}

suite_scenarios() {
  local code=0 entry command build skip i slug args matched=""
  for entry in "${SCENARIOS[@]}"; do
    IFS='|' read -r command build skip <<<"$entry"
    [[ -z $ONLY || $ONLY == "$command" ]] || continue
    matched=1
    if ! has "$command"; then
      row scenarios "$command" skipped 0 "not in this checkout's app.sh"
      continue
    fi
    if [[ -n $skip ]]; then
      row scenarios "$command" skipped 0 "$skip"
      continue
    fi
    if ! use_build "$build"; then
      row scenarios "$command" skipped 0 "app.sh $build failed"
      code=1
      continue
    fi
    read -ra args <<<"$command"
    for i in $(seq 1 "$REPEAT"); do
      slug="${command// /-}-$i"
      record_start "$OUT/$slug.mov"
      run scenarios "$command #$i" "$OUT/$slug.log" bash "$APP" "${args[@]}" || code=1
      record_stop
      # A scenario that stops early leaves its instance running.
      bash "$ROOT/scripts/giga-eval/instance.sh" stop
    done
  done
  if [[ -z $matched ]]; then
    row scenarios "$ONLY" failed 0 "not one of SCENARIOS in scripts/e2e-macos.sh"
    code=1
  fi
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
  *) sed -n '2,10p' "$0"; exit 2 ;;
esac
ONLY=${3:-}
REPEAT=${4:-1}
read -ra switches <<<"${5:-}"
for switch in ${switches[@]+"${switches[@]}"}; do
  [[ $switch =~ ^[A-Z][A-Z0-9]*[0-9]_[A-Z0-9_]+=[A-Za-z0-9._/-]*$ ]] \
    || { echo "e2e-macos: $switch is not NAME=value with a scenario prefix such as R6_" >&2; exit 2; }
  echo "e2e-macos: scenarios run with $switch"
  export "$switch"
done
mkdir -p "$OUT"
printf 'suite\ttest\tstatus\tseconds\tnote\n' >"$ROWS"
code=0
for suite in $suites; do "suite_$suite" || code=1; done
rm -f "$OUT/.stop"
node "$ROOT/docker/e2e-report.mjs" junit "$ROWS" >"$OUT/junit.xml" || code=1
exit "$code"
