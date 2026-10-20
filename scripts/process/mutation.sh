#!/usr/bin/env bash
# gate/mutation for a T3 PR (.github/workflows/gate-mutation.yml): mutates only the lines
# the PR changed and fails when a mutant survives.
#   mutation.sh rust <diff-file>   cargo mutants --in-diff on the changed Rust lines;
#                                  more than MAX_MUTANTS fails at once (split the PR)
#   mutation.sh ts <specs>         Stryker on the changed TypeScript lines; specs are
#                                  mutation_targets.py's comma-separated path:start-end
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
    # The PR's tree, for its tests.
    npm ci --ignore-scripts
    # Stryker in a tree the job owns, from the committed lockfile, so the PR's package.json
    # (overrides, extra packages) cannot reach it. The command runner runs the PR's tests
    # as a child process (vitest related: the tests that import a changed file), so no PR
    # module ever loads in Stryker's own process.
    tools=$RUNNER_TEMP/stryker
    mkdir -p "$tools"
    cp "$here/stryker/package.json" "$here/stryker/package-lock.json" "$tools/"
    npm ci --prefix "$tools" --ignore-scripts --registry=https://registry.npmjs.org/
    python3 "$here/stryker_scope.py" check "$tools/node_modules" "$STRYKER"
    stryker=$tools/node_modules/@stryker-mutator/core/bin/stryker.js
    echo "stryker $(node "$stryker" --version)"
    config=$RUNNER_TEMP/stryker.config.json
    python3 "$here/stryker_scope.py" config "$2" > "$config"
    cat "$config"
    node "$stryker" run "$config"
    ;;
  *)
    echo "usage: mutation.sh rust <diff-file> | ts <specs>" >&2
    exit 2
    ;;
esac
