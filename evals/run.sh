#!/usr/bin/env bash
# One command for the Mono evals.
#
#   evals/run.sh [smoke|full] [--update-baseline]
#
# smoke runs the cases with `tier: core`, full runs every case; both run k
# trials per case. Steps: build (or reuse) the GigaEval app, seed one fresh
# instance with a Mono and projects per trial, start it, hold the sender
# Mono's turn, run promptfoo, stop the instance, write out/report.md and
# compare with baseline.json. Exit 0 ok, 1 below floor or baseline, 3
# infrastructure (a failed step before grading, a trial error, or no report).
#
# Env: EVAL_APP=<built .app> skips the build. EVAL_HARNESS (omp) and
# EVAL_MODEL (<harness>:default) pick the Mono under test. K (5) trials per
# case, EVAL_JOBS (5) trials at once, EVAL_CASES_DIR (evals/cases).
# EVAL_LEGACY_TURN_END=1 only for app builds before ADE-8 (see provider.mjs).
set -euo pipefail
EVALS="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$EVALS")"
G="$REPO/scripts/giga-eval"
tier="${1:-smoke}"; shift || true
case "$tier" in smoke) filter=(--filter-metadata tier=core) ;; full) filter=() ;; *) sed -n '2,17p' "$0"; exit 2 ;; esac

export GIGA_EVAL_OUT="${GIGA_EVAL_OUT:-/tmp/monocode-evals}"
export GIGA_EVAL_HOME="${GIGA_EVAL_HOME:-/tmp/monocode-evals-home}"
# The Mono under test runs on a real model: copies of the real logins.
export GIGA_EVAL_LOGINS=1
export EVAL_OUT_DIR="${EVAL_OUT_DIR:-$EVALS/out}"
export PROMPTFOO_CONFIG_DIR="$EVAL_OUT_DIR/.promptfoo" PROMPTFOO_DISABLE_TELEMETRY=1 PROMPTFOO_DISABLE_UPDATE=1
export PROMPTFOO_FAILED_TEST_EXIT_CODE=0
K="${K:-5}"
started=$(date +%s)
mkdir -p "$EVAL_OUT_DIR"
rm -f "$EVAL_OUT_DIR"/{results,trials,report}.json "$EVAL_OUT_DIR/report.md"
# Every step before grading is infrastructure: it exits 3, never 1, and the
# trap stops any instance this run started.
infra() { echo "INFRA: $*" >&2; exit 3; }
trap '"$G/instance.sh" stop' EXIT

[[ -d "$EVALS/node_modules/promptfoo" ]] || (cd "$EVALS" && npm ci --no-audit --no-fund) || infra "npm ci failed"
if [[ -n "${EVAL_APP:-}" ]]; then
  [[ -x "$EVAL_APP/Contents/MacOS/monocode" ]] || { echo "EVAL_APP is not a built app: $EVAL_APP" >&2; exit 2; }
  mkdir -p "$GIGA_EVAL_OUT" && echo "$EVAL_APP" >"$GIGA_EVAL_OUT/instance.app"
else
  "$G/instance.sh" build "$REPO" evals || infra "build failed"
fi

"$G/instance.sh" stop || infra "could not stop a previous instance"
"$G/instance.sh" start --fresh-home || infra "first start (creates monocode.db) failed"
"$G/instance.sh" stop || infra "stop after the first start failed"
(cd "$EVALS" && node fixture.mjs "$K" "$tier") || infra "seeding the trials failed"
"$G/instance.sh" start || infra "start failed"
held=0
for _ in $(seq 1 120); do
  GIGA_EVAL_TARGET=instance "$G/cli.sh" sender monos.list >/dev/null 2>&1 && { held=1; break; }
  sleep 5
done
[[ $held == 1 ]] || infra "the sender Mono never held an app token"

(cd "$EVALS" && npx promptfoo eval -c promptfooconfig.yaml --repeat "$K" --no-cache -j "${EVAL_JOBS:-5}" \
  ${filter[@]+"${filter[@]}"} -o "$EVAL_OUT_DIR/results.json" --no-progress-bar \
  --tag "app_rev=$(cat "$GIGA_EVAL_OUT/src/evals/.giga-eval-rev" 2>/dev/null || echo prebuilt)" --tag "harness=${EVAL_HARNESS:-omp}") \
  || infra "promptfoo failed"
"$G/instance.sh" stop || infra "stop failed"
trap - EXIT
verdict=0
node "$EVALS/report.mjs" "$EVAL_OUT_DIR/results.json" --wall-seconds "$(( $(date +%s) - started ))" "$@" || verdict=$?
[[ -f "$EVAL_OUT_DIR/report.json" ]] || infra "report.mjs wrote no report"
exit "$verdict"
