#!/usr/bin/env bash
# gate/mutation for a T3 PR (.github/workflows/gate-mutation.yml): mutates only the lines
# the PR changed and fails when a mutant survives.
#   mutation.sh rust <diff-file> <k/n>
#                                  cargo mutants --in-diff on shard k of n of the changed
#                                  Rust lines; more than MAX_MUTANTS in the whole diff
#                                  fails at once (split the PR). A missed or timed-out
#                                  mutant exits non-zero, which fails the shard's job.
#   mutation.sh ts <specs>         Stryker on the changed TypeScript lines; specs are
#                                  mutation_targets.py's comma-separated path:start-end;
#                                  stryker_scope.py decides (one run per file)
# The workflow's step timeouts (110 minutes per Rust shard, 55 for Stryker) are the last
# bound: a timed-out step fails.
set -euo pipefail
MAX_MUTANTS=150
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
    # Every shard lists the same mutants and runs its round-robin share, so one file's
    # mutants spread over all shards.
    # run builds the real app event loop: only app.sh can call it, cargo test cannot (#284 review r1, CI run 39825194390).
    "$CARGO_MUTANTS" mutants --no-config --in-diff "$2" --shard "${3:?k/n}" --sharding round-robin \
      --jobs 2 --timeout-multiplier 3 \
      --exclude-re '^src-tauri/src/lib\.rs:[0-9]+:[0-9]+: replace run with \(\)$'
    ;;
  ts)
    # The PR's tree, for its tests and source imports.
    npm ci --ignore-scripts
    # Stryker, its vitest runner and vitest in a tree the job owns, from the committed
    # lockfile. The PR's node_modules/vitest then points at the job's vitest, so the PR's
    # overrides and lockfile cannot change anything Stryker or vitest load.
    tools=$RUNNER_TEMP/stryker
    mkdir -p "$tools"
    cp "$here/stryker/package.json" "$here/stryker/package-lock.json" "$tools/"
    npm ci --prefix "$tools" --ignore-scripts --registry=https://registry.npmjs.org/
    rm -rf node_modules/vitest
    ln -s "$tools/node_modules/vitest" node_modules/vitest
    echo "stryker $(node "$tools/node_modules/@stryker-mutator/core/bin/stryker.js" --version), vitest $(node "$tools/node_modules/vitest/vitest.mjs" --version)"
    python3 "$here/stryker_scope.py" "$tools/node_modules" "$2"
    ;;
  *)
    echo "usage: mutation.sh rust <diff-file> <k/n> | ts <specs>" >&2
    exit 2
    ;;
esac
