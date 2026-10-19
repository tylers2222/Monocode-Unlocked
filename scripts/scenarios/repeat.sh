#!/usr/bin/env bash
# Runs the scenario tests matching FILTER 20 times (Change R plan v2,
# section 3) and fails unless all 20 pass. Each run holds the shared heavy
# lock when HEAVY_LOCK names its script (heavy-lock.sh sets
# CARGO_BUILD_JOBS=4). Set MONOCODE_SCENARIO_DB to run on a /tmp copy of a
# real monocode.db.
#
# Usage: scripts/scenarios/repeat.sh FILTER [RUNS]
set -euo pipefail

filter="${1:?usage: repeat.sh FILTER [RUNS]}"
runs="${2:-20}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
lock=()
if [[ -n "${HEAVY_LOCK:-}" ]]; then
  lock=("$HEAVY_LOCK")
fi

# Build once, so each run measures the scenario and not the compile.
${lock[@]+"${lock[@]}"} cargo test --manifest-path "$root/src-tauri/Cargo.toml" --lib --no-run --quiet

passed=0
for run in $(seq 1 "$runs"); do
  if out="$(${lock[@]+"${lock[@]}"} cargo test --manifest-path "$root/src-tauri/Cargo.toml" --lib --quiet -- "scenario::$filter" 2>&1)" \
    && grep -Eq "test result: ok\. [1-9][0-9]* passed" <<<"$out"; then
    passed=$((passed + 1))
  else
    echo "run $run failed:" >&2
    echo "$out" | tail -40 >&2
  fi
done
echo "$filter: $passed/$runs passed"
[[ "$passed" -eq "$runs" ]]
