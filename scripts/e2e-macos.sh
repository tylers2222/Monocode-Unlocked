#!/usr/bin/env bash
# The e2e-macos job of .github/workflows/e2e.yml, on a hosted macOS runner.
#   e2e-macos.sh <out> <all|scenarios|milestones> [scenario] [repeat] [env]
# Runs every scenario of SCENARIOS that app.sh in this checkout has, or only
# [scenario] (an app.sh command line), each [repeat] times (default 1), and
# builds the instance each one needs. [env] holds space-separated switches
# that a scenario reads, NAME=value with a scenario prefix such as R6_.
# Writes one log per run, a screenshot of each failure, and results.tsv and
# junit.xml in the row format of docker/e2e-run.sh. Exits 1 when a row failed.
set -uo pipefail
trap '' PIPE
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/scripts/scenarios/app.sh"
OUT=$1
ROWS="$OUT/results.tsv"
# app.sh's default, unless the job sets it.
EVAL_HOME=${GIGA_EVAL_HOME:-/tmp/scenario-app-home}

# command | the app.sh build it runs on | why CI skips it (empty: CI runs it).
# A dispatch that names the command (scenario input) runs it anyway.
SCENARIOS=(
  "home-isolation|build|"
  "s0|build|"
  "d1|build|"
  "d2|build|"
  "d4|build|"
  "d4 d1|build|"
  "r6|build|"
  "r9|build|"
  "r11|build|"
  "ade89 kill|build|"
  "ade89 term|build|"
  "approvals|build|reads a copy of the real app database, which a CI runner does not have"
  "ade89 two|build-two|PR #411 review r1 M2 (change-linear/reviews/411-round1.md): 12/20, notices lost before the agent gets them; the 20x dispatch runs it"
)

row() { printf '%s\t%s\t%s\t%s\t%s\n' "$@" >>"$ROWS"; }

# True when app.sh has a case label for the command's first word.
has() { grep -qE "^  ${1%% *}\)" "$APP"; }

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
  # screencapture -v never stopped on a hosted runner (run 40095797823 hung
  # until the job timed out; ADE-119), so this shot gets 20 s before SIGALRM.
  perl -e 'alarm 20; exec @ARGV' screencapture -x "${log%.log}-failed.png"
  # The fx puppet's log and a copy of the instance database, to read the
  # failure from the artifact (ADE-124). Both hold only the run's own data.
  cp "$EVAL_HOME/.puppet/log.jsonl" "${log%.log}-puppet.jsonl" 2>/dev/null || true
  sqlite3 "file:$EVAL_HOME/Library/Application Support/com.monocode.desktop.gigaeval/monocode.db?mode=ro" \
    ".backup '${log%.log}-failed.db'" 2>/dev/null || true
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
    if [[ -n $skip && -z $ONLY ]]; then
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
      run scenarios "$command #$i" "$OUT/$slug.log" bash "$APP" "${args[@]}" || code=1
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
node "$ROOT/docker/e2e-report.mjs" junit "$ROWS" >"$OUT/junit.xml" || code=1
exit "$code"
