#!/usr/bin/env bash
# Probe test for integrity.sh: each probe is one commit on a small base repo, and the
# script must exit with the expected code and print the expected text. A probe whose
# edit changes nothing fails, so a passing probe always tested a real diff.
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
printf '// eslint-disable-next-line no-console\nconsole.log(1);\nexport const a = 1;\n' > src/a.ts
echo 'echo gate' > scripts/gate/x.sh
git add -A && git commit -qm base

# A git that adds an escape integrity.sh does not know: real git, with \t in quoted names as \q.
shim=$repo.shim
trap 'rm -rf "$repo" "$shim"' EXIT
mkdir -p "$shim"
printf '#!/usr/bin/env bash\nset -o pipefail\n%q "$@" | sed "s/\\\\\\\\t/\\\\\\\\q/g"\n' "$(command -v git)" > "$shim/git"
chmod +x "$shim/git"

lines() { for i in $(seq "$2"); do echo "export const v$i = $i;"; done > "$1"; }
control_char_dir() { local d="src/x"$'\n'"::warning"; mkdir -p "$d" && echo 'export default [];' > "$d/eslint.config.js"; }
tab_name() { printf '// eslint-disable-next-line\n' > "src/x"$'\t'"y.ts"; }
rust_test_mod() { { printf '#[cfg(test)]\nmod t {\n'; for i in $(seq 600); do echo "    const C$i: u32 = $i;"; done; echo '}'; } >> src/lib.rs; }
cfg_test_one_line() { { printf '#[cfg(test)]\nfn marker() {}\npub fn big() -> u32 {\n'; for i in $(seq 510); do echo "    $i +"; done; printf '    0\n}\n'; } >> src/lib.rs; }
cfg_test_variant() { { printf 'pub enum K {\n    #[cfg(test)]\n    Fake,\n    B,\n}\npub fn big() -> u32 {\n'; for i in $(seq 510); do echo "    $i +"; done; printf '    0\n}\n'; } >> src/lib.rs; }
move_disable() { printf 'console.log(1);\nexport const a = 1;\n// eslint-disable-next-line no-console\n' > src/a.ts; }

failed=0
probe() {
  local name=$1 want=$2 text=$3
  shift 3
  git checkout -q --detach main
  "$@"
  git add -A && git commit -qm "$name"
  local out code
  out=$(PATH=${probe_path:-$PATH} bash "$script" main HEAD 2>&1)
  code=$?
  if git diff --quiet main HEAD; then
    echo "FAIL $name: the probe's edit changed nothing"
    failed=1
  elif [ "$code" != "$want" ] || [[ $out != *"$text"* ]]; then
    echo "FAIL $name: exit $code (want $want), output: $out"
    failed=1
  else
    echo "ok   $name"
  fi
}

probe allow 1 'adds a suppression: allow(dead_code)' eval 'printf "#[allow(dead_code)]\nfn g() {}\n" >> src/lib.rs'
probe forwarded-attr 1 'adds a suppression: attr([$a])' eval 'echo "macro_rules! m { (\$a:meta) => { #[\$a] fn g() {} } }" >> src/lib.rs'
probe eslint-disable 1 'adds a suppression: eslint-disable' eval 'echo "// eslint-disable-next-line" >> src/a.ts'
probe ts-ignore 1 'adds a suppression: @ts-ignore' eval 'echo "// @ts-ignore" >> src/a.ts'
probe moved-disable 0 'ok, 2 non-test lines' move_disable
probe baseline-grows 1 'baseline counts grow by 1' perl -pi -e 's/"count": 1/"count": 2/' eslint-suppressions.json
probe gate-file 1 'scripts/gate/x.sh: edits a gate file' eval 'echo "# x" >> scripts/gate/x.sh'
probe gate-rename 1 'scripts/gate/x.sh: edits a gate file' git mv scripts/gate/x.sh scripts/x.sh
for path in scripts/process/tier.py src/eslint.config.js .github/workflows/ci.yml .github/actions/x/action.yml src-tauri/clippy.toml .clippy.toml \
  .cargo/config.toml rust-toolchain.toml scripts/install-linux-deps-debian.sh .npmrc; do
  probe "gate-path $path" 1 "$path: edits a gate file" eval "mkdir -p '$(dirname "$path")' && echo x > '$path'"
done
probe cargo-lint 1 'Cargo.toml:2: edits a lint or install setting' perl -pi -e 's/"deny"/"allow"/' Cargo.toml
probe postinstall 1 'package.json:4: edits a lint or install setting' perl -pi -e 's/"scripts": \{/"scripts": {\n    "postinstall": "node x.js",/' package.json
probe prepare 1 'package.json:4: edits a lint or install setting' perl -pi -e 's/"scripts": \{/"scripts": {\n    "prepare": "node x.js",/' package.json
probe dependencies-object 0 'ok, 3 non-test lines' eval 'mkdir -p tools && printf "{\n  \"dependencies\": {\n  }\n" > tools/package.json'
probe over-500 1 'changes 501 non-test lines' lines src/big.ts 501
probe at-500 0 'ok, 500 non-test lines' lines src/big.ts 500
probe test-file 0 'ok, 0 non-test lines' lines src/big.test.ts 600
probe fixture 0 'ok, 0 non-test lines' eval 'mkdir -p src/fixtures && lines src/fixtures/f.ts 600'
probe cfg-test 0 'ok, 0 non-test lines' rust_test_mod
probe rust-tests-module 0 'ok, 7 non-test lines' eval 'mkdir -p src/foo && lines src/foo/tests.rs 600 && lines src/foo/tests_util.rs 2 && lines src/foo/mod.rs 3 && lines src/foo/mytests.rs 1 && lines src/foo/tests.rs.bak 1'
probe cfg-test-one-line 1 'more than 500' cfg_test_one_line
probe cfg-test-variant 1 'more than 500' cfg_test_variant
probe binary 1 'diffs as binary' eval 'printf "// \0\n" > src/bin.ts'
probe control-char 1 'src/x\x0a::warning' control_char_dir
probe quoted-name 1 'src/x\x09y.ts:1: adds a suppression' tab_name
probe_path=$shim:$PATH probe unknown-escape 1 'escape git does not document' tab_name
probe clean 0 'ok, 1 non-test lines' eval 'echo "export const b = 2;" >> src/a.ts'
exit "$failed"
