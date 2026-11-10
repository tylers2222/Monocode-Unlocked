#!/usr/bin/env bash
# gate/mutation for a T3 PR (.github/workflows/gate-mutation.yml): mutates only the lines
# the PR changed and fails when a mutant survives.
#   mutation.sh rust <diff-file> <k/n>
#                                  cargo mutants --in-diff on shard k of n of the changed
#                                  Rust lines; more than MAX_MUTANTS in the whole diff
#                                  fails at once (split the PR). A missed or timed-out
#                                  mutant exits non-zero, which fails the shard's job.
#                                  Two passes (mutation_narrow.py): nextest on the tests
#                                  that reach the mutated modules, then cargo test on the
#                                  whole package for every mutant pass 1 did not prove
#                                  caught. MUTATION_BASELINE=skip skips pass 2's baseline
#                                  (only when this head already passed the test job).
#   mutation.sh ts <specs>         Stryker on the changed TypeScript lines; specs are
#                                  mutation_targets.py's comma-separated path:start-end;
#                                  stryker_scope.py decides (one run per file)
# The workflow's step timeouts (110 minutes per Rust shard, 55 for Stryker) are the last
# bound: a timed-out step fails.
set -euo pipefail
MAX_MUTANTS=150
NEXTEST_VERSION=0.9.151
NEXTEST_SHA256=1d4ce302b820c41f4b89c5fb9bce45f0f0c94882c1cf08fc360caf08be6e7df4
here=$(cd "$(dirname "$0")" && pwd)

case ${1-} in
  rust)
    # The pinned binary the job installed (CARGO_MUTANTS, an absolute path), not the
    # `cargo mutants` subcommand a PR could alias; --no-config ignores its mutants.toml.
    "${CARGO_MUTANTS:?set by the workflow}" mutants --version
    # run builds the real app event loop: only app.sh can call it, cargo test cannot (#284 review r1, CI run 39825194390).
    mutants=("$CARGO_MUTANTS" mutants --no-config --in-diff "$2"
             --exclude-re '^src-tauri/src/lib\.rs:[0-9]+:[0-9]+: replace run with \(\)$')
    count=$("${mutants[@]}" --list | grep -c . || true)
    if [ "$count" -gt "$MAX_MUTANTS" ]; then
      echo "::error::$count Rust mutants, more than $MAX_MUTANTS; split the PR so each part mutates fewer"
      exit 1
    fi
    # Every shard lists the same mutants and runs its round-robin share, so one file's
    # mutants spread over all shards.
    shard=(--shard "${3:?k/n}" --sharding round-robin)
    # Half the CPUs: each job's build uses the rest (2 on a 4-vCPU runner, as before).
    jobs=$(( $(nproc) / 2 )); [ "$jobs" -ge 1 ] || jobs=1
    # cargo-nextest, pinned, into the job's own tools directory.
    tools=${RUNNER_TEMP:?}/tools/bin
    mkdir -p "$tools"
    curl -fsSL --retry 3 -o "$RUNNER_TEMP/nextest.tar.gz" "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$NEXTEST_VERSION/cargo-nextest-$NEXTEST_VERSION-x86_64-unknown-linux-gnu.tar.gz"
    echo "$NEXTEST_SHA256  $RUNNER_TEMP/nextest.tar.gz" | sha256sum -c -
    tar -xzf "$RUNNER_TEMP/nextest.tar.gz" -C "$tools"
    export PATH=$tools:$PATH
    # Pass 1, a speed-up only: nextest (fail-fast) on the tests that can reach the mutated
    # modules (mutation_narrow.py scope), or on the whole package when that mapping is not
    # safe. Its own baseline runs: the head passing cargo test does not prove that these
    # tests pass under nextest, and a test that fails unmutated would catch every mutant.
    scope=$("${mutants[@]}" "${shard[@]}" --list | python3 "$here/mutation_narrow.py" scope src-tauri/src)
    echo "mutation scope: $scope"
    narrow=(--no-tests=pass)
    [ "${scope%% *}" = narrow ] && narrow+=(-E "${scope#narrow }")
    rc=0
    "${mutants[@]}" "${shard[@]}" --jobs "$jobs" --timeout-multiplier 3 --test-tool nextest -- "${narrow[@]}" || rc=$?
    # 0 all caught, 2 missed, 3 timed out. 4, its baseline failed under nextest: the whole
    # shard goes to pass 2. Anything else (usage, internal error) is final.
    case $rc in
      # A plain substitution, so a failing retry stops the job (set -e) instead of
      # reading as an empty list, which would pass every mutant.
      0|2|3) list=$(python3 "$here/mutation_narrow.py" retry mutants.out)
             again=(); [ -z "$list" ] || mapfile -t again <<<"$list" ;;
      4) again=(shard) ;;
      *) exit "$rc" ;;
    esac
    if [ "${#again[@]}" -eq 0 ]; then
      python3 "$here/mutation_narrow.py" merge mutants.out
      echo "mutation: pass 1 caught every mutant with a failing test"
      exit 0
    fi
    # Pass 2, the verdict: cargo test on the whole package (the old run) for each mutant
    # pass 1 did not prove caught: exactly those names, or the whole shard when the names
    # do not select them. Its baseline is skipped only when the caller proved this head
    # already passed the test job (MUTATION_BASELINE=skip).
    pick=(); for r in "${again[@]}"; do pick+=(--re "$r"); done
    if [ "$rc" = 4 ] || [ "$("${mutants[@]}" "${pick[@]}" --list | grep -c .)" != "${#again[@]}" ]; then
      pick=("${shard[@]}")
    fi
    if [ "${MUTATION_BASELINE:-run}" = skip ]; then base=(--baseline skip --timeout 600); else base=(--timeout-multiplier 3); fi
    echo "mutation: pass 2, cargo test on the whole package for ${#again[@]} mutants (or the shard), baseline ${MUTATION_BASELINE:-run}"
    rc=0
    "${mutants[@]}" "${pick[@]}" --jobs "$jobs" "${base[@]}" --output mutants.full || rc=$?
    if [ -f mutants.full/mutants.out/outcomes.json ] && [ -f mutants.out/outcomes.json ]; then
      python3 "$here/mutation_narrow.py" merge mutants.out mutants.full/mutants.out
      cp -a mutants.full/mutants.out mutants.out/full
    fi
    exit "$rc"
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
