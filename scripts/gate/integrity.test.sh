#!/usr/bin/env bash
# Probe test for integrity.sh: each probe is one commit on a small base repo, and the
# script must exit with the expected code and print the expected text.
# Usage: bash scripts/gate/integrity.test.sh
set -uo pipefail

script=$(cd "$(dirname "$0")" && pwd)/integrity.sh
repo=$(mktemp -d)
trap 'rm -rf "$repo"' EXIT
cd "$repo" || exit 2
git init -q -b main
git config user.name probe
git config user.email probe@example.com
mkdir -p src scripts/gate .github/workflows
printf '[workspace.lints.clippy]\ntodo = "deny"\n' > Cargo.toml
printf '{\n  "name": "x",\n  "scripts": {\n    "test": "vitest"\n  }\n}\n' > package.json
printf '{\n  "a.ts": {\n    "complexity": {\n      "count": 1\n    }\n  }\n}\n' > eslint-suppressions.json
printf 'pub fn f() {}\n' > src/lib.rs
printf 'export const a = 1;\n' > src/a.ts
echo 'echo gate' > scripts/gate/x.sh
git add -A && git commit -qm base

lines() { for i in $(seq "$2"); do echo "export const v$i = $i;"; done > "$1"; }
control_char_dir() { local d="src/x"$'\n'"::warning"; mkdir -p "$d" && echo 'export default [];' > "$d/eslint.config.js"; }
rust_test_mod() { { printf '#[cfg(test)]\nmod t {\n'; for i in $(seq 600); do echo "    const C$i: u32 = $i;"; done; echo '}'; } >> src/lib.rs; }

failed=0
probe() {
  local name=$1 want=$2 text=$3
  shift 3
  git checkout -q --detach main
  "$@"
  git add -A && git commit -qm "$name"
  local out code
  out=$(bash "$script" main HEAD 2>&1)
  code=$?
  if [ "$code" != "$want" ] || [[ $out != *"$text"* ]]; then
    echo "FAIL $name: exit $code (want $want), output: $out"
    failed=1
  else
    echo "ok   $name"
  fi
}

probe allow 1 'adds a suppression: allow(dead_code)' eval 'printf "#[allow(dead_code)]\nfn g() {}\n" >> src/lib.rs'
probe eslint-disable 1 'adds a suppression: eslint-disable' eval 'echo "// eslint-disable-next-line" >> src/a.ts'
probe baseline-grows 1 'baseline counts grow by 1' perl -pi -e 's/"count": 1/"count": 2/' eslint-suppressions.json
probe gate-file 1 'scripts/gate/x.sh: edits a gate file' eval 'echo "# x" >> scripts/gate/x.sh'
probe nested-eslint 1 'edits a gate file' eval 'echo "export default [];" > src/eslint.config.js'
probe workflow 1 'edits a gate file' eval 'echo "on: push" > .github/workflows/ci.yml'
probe npmrc 1 '.npmrc: edits a gate file' eval 'echo "node-options=--require=./x.cjs" > .npmrc'
probe cargo-lint 1 'Cargo.toml:2: edits a lint or install setting' perl -pi -e 's/"deny"/"allow"/' Cargo.toml
probe postinstall 1 'package.json:4: edits a lint or install setting' perl -pi -e 's/"scripts": \{/"scripts": {\n    "postinstall": "node x.js",/' package.json
probe over-500 1 'changes 501 non-test lines' lines src/big.ts 501
probe at-500 0 'ok, 500 non-test lines' lines src/big.ts 500
probe test-file 0 'ok, 0 non-test lines' lines src/big.test.ts 600
probe fixture 0 'ok, 0 non-test lines' eval 'mkdir -p src/fixtures && lines src/fixtures/f.ts 600'
probe cfg-test 0 'ok, 0 non-test lines' rust_test_mod
probe binary 1 'diffs as binary' eval 'printf "// \0\n" > src/bin.ts'
probe control-char 1 'src/x\x0a::warning' control_char_dir
probe clean 0 'ok, 1 non-test lines' eval 'echo "export const b = 2;" >> src/a.ts'
exit "$failed"
