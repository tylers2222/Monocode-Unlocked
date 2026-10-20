#!/usr/bin/env bash
# gate/mutation for a T3 PR: mutates only the lines the PR changed and fails when a
# mutant survives. gate-integrity.yml runs this default-branch copy inside a checkout
# of the PR head, in a job with no token permissions.
#   mutation.sh rust <diff-file>   cargo mutants --in-diff on the changed Rust lines
#   mutation.sh ts <specs>         Stryker on the changed TypeScript lines; specs are
#                                  mutation_targets.py's comma-separated path:start-end
set -euo pipefail

case ${1-} in
  rust)
    # The binary, not `cargo mutants`: a PR could alias that cargo subcommand, and
    # --no-config ignores a .cargo/mutants.toml it adds.
    cargo-mutants mutants --no-config --in-diff "$2" --jobs 2 --timeout-multiplier 3
    ;;
  ts)
    npm ci --ignore-scripts
    npm install --no-save --ignore-scripts @stryker-mutator/core@9 @stryker-mutator/vitest-runner@9
    config=$RUNNER_TEMP/stryker.config.json
    SPECS=$2 node -e '
      const mutate = process.env.SPECS.split(",");
      const thresholds = { high: 100, low: 100, break: 100 };
      console.log(JSON.stringify({ testRunner: "vitest", mutate, thresholds, concurrency: 2,
        coverageAnalysis: "perTest", reporters: ["clear-text", "progress"] }));
    ' > "$config"
    node node_modules/@stryker-mutator/core/bin/stryker.js run "$config"
    ;;
  *)
    echo "usage: mutation.sh rust <diff-file> | ts <specs>" >&2
    exit 2
    ;;
esac
