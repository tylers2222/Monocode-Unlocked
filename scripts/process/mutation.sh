#!/usr/bin/env bash
# gate/mutation for a T3 PR (.github/workflows/gate-mutation.yml): mutates only the lines
# the PR changed and fails when a mutant survives.
#   mutation.sh rust <diff-file>   cargo mutants --in-diff on the changed Rust lines;
#                                  more than MAX_MUTANTS fails at once (split the PR)
#   mutation.sh ts <specs>         Stryker on the changed TypeScript lines; specs are
#                                  mutation_targets.py's comma-separated path:start-end;
#                                  static mutants are skipped (ignoreStatic)
# The workflow's 55-minute step timeouts are the last bound: a timed-out step fails.
set -euo pipefail
MAX_MUTANTS=150
STRYKER=9.6.1

case ${1-} in
  rust)
    # The pinned binary the job installed (CARGO_MUTANTS, an absolute path), not the
    # `cargo mutants` subcommand a PR could alias; --no-config ignores its mutants.toml.
    count=$("${CARGO_MUTANTS:?set by the workflow}" mutants --no-config --in-diff "$2" --list | grep -c . || true)
    if [ "$count" -gt "$MAX_MUTANTS" ]; then
      echo "::error::$count Rust mutants, more than $MAX_MUTANTS; split the PR so each part mutates fewer"
      exit 1
    fi
    "$CARGO_MUTANTS" mutants --no-config --in-diff "$2" --jobs 2 --timeout-multiplier 3
    ;;
  ts)
    npm ci --ignore-scripts
    # Stryker and its vitest runner, pinned, from the public registry. They go into the
    # project's node_modules because the runner must load the same vitest that the PR's
    # tests import; the config is written here, outside the checkout.
    npm install --no-save --ignore-scripts --registry=https://registry.npmjs.org/ \
      "@stryker-mutator/core@$STRYKER" "@stryker-mutator/vitest-runner@$STRYKER"
    config=$RUNNER_TEMP/stryker.config.json
    SPECS=$2 node -e '
      const mutate = process.env.SPECS.split(",");
      const thresholds = { high: 100, low: 100, break: 100 };
      console.log(JSON.stringify({ testRunner: "vitest", mutate, thresholds, concurrency: 2,
        coverageAnalysis: "perTest", ignoreStatic: true, reporters: ["clear-text", "progress"] }));
    ' > "$config"
    node "$PWD/node_modules/@stryker-mutator/core/bin/stryker.js" run "$config"
    ;;
  *)
    echo "usage: mutation.sh rust <diff-file> | ts <specs>" >&2
    exit 2
    ;;
esac
