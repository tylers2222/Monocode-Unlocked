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
# infrastructure errors.
#
# Env: EVAL_APP=<built .app> skips the build. EVAL_HARNESS (omp) and
# EVAL_MODEL (<harness>:default) pick the Mono under test. K (5) trials per
# case, EVAL_JOBS (5) trials at once, EVAL_CASES_DIR (evals/cases).
set -euo pipefail
EVALS="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$EVALS")"
G="$REPO/scripts/giga-eval"
tier="${1:-smoke}"; shift || true
case "$tier" in smoke) filter=(--filter-metadata tier=core) ;; full) filter=() ;; *) sed -n '2,17p' "$0"; exit 2 ;; esac

export GIGA_EVAL_OUT="${GIGA_EVAL_OUT:-/tmp/monocode-evals}"
export GIGA_EVAL_HOME="${GIGA_EVAL_HOME:-/tmp/monocode-evals-home}"
export EVAL_OUT_DIR="${EVAL_OUT_DIR:-$EVALS/out}"
export PROMPTFOO_CONFIG_DIR="$EVAL_OUT_DIR/.promptfoo" PROMPTFOO_DISABLE_TELEMETRY=1 PROMPTFOO_DISABLE_UPDATE=1
export PROMPTFOO_FAILED_TEST_EXIT_CODE=0
K="${K:-5}"
started=$(date +%s)
mkdir -p "$EVAL_OUT_DIR"
rm -f "$EVAL_OUT_DIR/results.json" "$EVAL_OUT_DIR/trials.json"

[[ -d "$EVALS/node_modules/promptfoo" ]] || (cd "$EVALS" && npm ci --no-audit --no-fund)
if [[ -n "${EVAL_APP:-}" ]]; then
  [[ -x "$EVAL_APP/Contents/MacOS/monocode" ]] || { echo "EVAL_APP is not a built app: $EVAL_APP" >&2; exit 2; }
  mkdir -p "$GIGA_EVAL_OUT" && echo "$EVAL_APP" >"$GIGA_EVAL_OUT/instance.app"
else
  "$G/instance.sh" build "$REPO" evals
fi

"$G/instance.sh" stop
"$G/instance.sh" start --fresh-home && "$G/instance.sh" stop
(cd "$EVALS" && node fixture.mjs "$K" "$tier")
"$G/instance.sh" start
trap '"$G/instance.sh" stop' EXIT
for _ in $(seq 1 120); do
  GIGA_EVAL_TARGET=instance "$G/cli.sh" sender monos.list >/dev/null 2>&1 && break
  sleep 5
done

(cd "$EVALS" && npx promptfoo eval -c promptfooconfig.yaml --repeat "$K" --no-cache -j "${EVAL_JOBS:-5}" \
  ${filter[@]+"${filter[@]}"} -o "$EVAL_OUT_DIR/results.json" --no-progress-bar \
  --tag "app_rev=$(cat "$GIGA_EVAL_OUT/src/evals/.giga-eval-rev" 2>/dev/null || echo prebuilt)" --tag "harness=${EVAL_HARNESS:-omp}")
"$G/instance.sh" stop
trap - EXIT
node "$EVALS/report.mjs" "$EVAL_OUT_DIR/results.json" --wall-seconds "$(( $(date +%s) - started ))" "$@"
