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
here=$(cd "$(dirname "$0")" && pwd)

case ${1-} in
  rust)
    # The pinned binary the job installed (CARGO_MUTANTS, an absolute path), not the
    # `cargo mutants` subcommand a PR could alias; --no-config ignores its mutants.toml.
    "${CARGO_MUTANTS:?set by the workflow}" mutants --version
    count=$("$CARGO_MUTANTS" mutants --no-config --in-diff "$2" --list | grep -c . || true)
    if [ "$count" -gt "$MAX_MUTANTS" ]; then
      echo "::error::$count Rust mutants, more than $MAX_MUTANTS; split the PR so each part mutates fewer"
      exit 1
    fi
    "$CARGO_MUTANTS" mutants --no-config --in-diff "$2" --jobs 2 --timeout-multiplier 3
    ;;
  ts)
    # The PR's tree, for its tests and its vitest.
    npm ci --ignore-scripts
    # Stryker and its vitest runner in a tree the job owns, from the committed lockfile,
    # so the PR's package.json (overrides, extra packages) cannot reach them. The runner
    # loads the PR's vitest through a link, so it and the PR's tests share one instance.
    tools=$RUNNER_TEMP/stryker
    mkdir -p "$tools"
    cp "$here/stryker/package.json" "$here/stryker/package-lock.json" "$tools/"
    npm ci --prefix "$tools" --ignore-scripts --legacy-peer-deps --registry=https://registry.npmjs.org/
    python3 "$here/stryker_scope.py" "$tools/node_modules" "$PWD/node_modules" "$STRYKER"
    ln -s "$PWD/node_modules/vitest" "$tools/node_modules/vitest"
    stryker=$tools/node_modules/@stryker-mutator/core/bin/stryker.js
    echo "stryker $(node "$stryker" --version)"
    config=$RUNNER_TEMP/stryker.config.json
    SPECS=$2 node -e '
      const mutate = process.env.SPECS.split(",");
      const thresholds = { high: 100, low: 100, break: 100 };
      console.log(JSON.stringify({ testRunner: "vitest", plugins: ["@stryker-mutator/vitest-runner"], mutate, thresholds,
        concurrency: 2, coverageAnalysis: "perTest", ignoreStatic: true, reporters: ["clear-text", "progress"] }));
    ' > "$config"
    node "$stryker" run "$config"
    ;;
  *)
    echo "usage: mutation.sh rust <diff-file> | ts <specs>" >&2
    exit 2
    ;;
esac
