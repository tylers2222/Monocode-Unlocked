import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { jqPrograms, syntaxError } from "./jq-programs.mjs";

const programs = (text) => jqPrograms(text).map(({ program }) => program);

test("#371: the r9_rules program with one closing brace too few does not parse", () => {
  const script = `r9_rules() {
  jq -n --arg app "$(target_bin instance)" '{app: $app, rules: [
    {match: "R9", tools: [
      {action: "sessions.start", arguments: {prompt: "R9-BUSY", due_minutes: 10},
      {action: "sessions.start", arguments: {prompt: "R9-LATE", due_minutes: 10}}
    ]}
  ]}'
}`;
  const [found] = jqPrograms(script);
  assert.equal(found.line, 2);
  assert.match(syntaxError(found.program), /syntax error/);
});

test("run-time arguments, functions and a comment at the end all parse", () => {
  for (const good of ['{app: $app, n: ($x | tonumber)}', "def f: .a; [.[] | f]", ". # note", 'select(.rule == $rule)']) {
    assert.equal(syntaxError(good), null, good);
  }
});

test("each flag form is skipped to reach the program", () => {
  assert.deepEqual(programs("jq -n '.a'"), [".a"]);
  assert.deepEqual(programs("x=$(jq -s --argjson up \"$1\" --arg n 'v' '.b' f)"), [".b"]);
  assert.deepEqual(programs("jq -n --arg app \"$(bin \"a b\")\" \\\n  --arg z y \\\n  '.c'"), [".c"]);
  assert.deepEqual(programs("cat f | jq -r '.d' | head"), [".d"]);
});

test("an escaped quote stays in the program", () => {
  assert.deepEqual(programs("jq -n '.a == \"it'\\''s\"'"), ['.a == "it\'s"']);
});

test("a program that is not a literal is left out, and so is a comment or another word", () => {
  assert.deepEqual(programs('jq -n "$prog"'), []);
  assert.deepEqual(programs("jq -f prog.jq f"), []);
  assert.deepEqual(programs("# jq -n '{'"), []);
  assert.deepEqual(programs("mjq -n '{'\necho jq"), []);
});

test("a program built at run time or inside a double-quoted string is left out", () => {
  assert.deepEqual(programs(`jq -c '\n  def f: .;\n  '"$1" <<<"$2"`), []);
  assert.deepEqual(programs(String.raw`bash -c "[[ -n \"\$(jq -c 'select(.a == \"b\")' f)\" ]]"`), []);
  assert.deepEqual(programs(`x="$(jq -n '.a')"`), [".a"]);
});

test("#584 r1 M1: jq that is not a command is left out", () => {
  const notCommands = [
    "echo '{}' > /dev/null # not: jq '{'",
    "echo jq '{'",
    "need jq 'brew install {'",
    "cat <<'EOF'\njq '{'\nEOF",
    "cat <<-EOF\n\tjq '{'\n\tEOF",
    "cat <<EOF | sh\njq '{'\nEOF",
    'echo "first line\njq \'{\'\nlast line"',
    "# jq '{'",
    "  # jq '{'",
    "cat <<< jq '{'",
    "a;\necho jq '{'",
    "f() {\n  find . -exec jq '{' {} \\;\n}",
    "a;\nd4_jq '{'",
  ];
  for (const script of notCommands) assert.deepEqual(programs(script), [], script);
});

test("each command position is read", () => {
  const commands = {
    "jq -n '.a'": ".a",
    "  jq -n '.a'": ".a",
    "a; jq -n '.a'": ".a",
    "a && jq -n '.a'": ".a",
    "a || jq -n '.a'": ".a",
    "a | jq -n '.a'": ".a",
    "a |\n  jq -n '.a'": ".a",
    "a | \\\n  jq -n '.a'": ".a",
    "(jq -n '.a')": ".a",
    "x=$(jq -n '.a')": ".a",
    'x="$(a | jq -n \'.a\')"': ".a",
    "b <(jq -n '.a') c": ".a",
    "if jq -e '.a' f; then b; fi": ".a",
    "if b; then jq -n '.a'; fi": ".a",
    "FOO=1 jq -n '.a'": ".a",
  };
  for (const [script, program] of Object.entries(commands)) assert.deepEqual(programs(script), [program], script);
});

test("a command after a heredoc, a comment or a $ form is still read", () => {
  assert.deepEqual(jqPrograms("cat <<EOF\njq '{'\nEOF\njq -n '.a'"), [{ line: 4, program: ".a" }]);
  assert.deepEqual(programs("a # it's a note\njq -n '.a'"), [".a"]);
  assert.deepEqual(programs("echo $# ${#x} ${y:-z}\njq -n '.a'"), [".a"]);
  assert.deepEqual(programs(`\${lock[@]+"\${lock[@]}"} a\njq -n '.a'`), [".a"]);
  assert.deepEqual(programs("echo $'a\\n' b\njq -n '.a'"), [".a"]);
});

test("a scan that ends inside a quote is unsure, so it reads nothing", () => {
  assert.deepEqual(programs("jq -n '.a'\necho \"open"), []);
});

const script = fileURLToPath(new URL("./jq-programs.mjs", import.meta.url));
const scratch = () => mkdtempSync(join(tmpdir(), "jq-programs-test-"));
function cli(args, options = {}) {
  return spawnSync(process.execPath, [script, ...args], { encoding: "utf8", ...options });
}
function shellFile(dir, name, text) {
  const path = join(dir, name);
  writeFileSync(path, text);
  return path;
}

test("#584 r1 M2: the check exits 1 on a bad file and names it, and 0 on a good one", () => {
  const dir = scratch();
  const bad = cli([shellFile(dir, "bad.sh", "true\njq -n '{'\n")]);
  assert.equal(bad.status, 1);
  assert.match(bad.stdout, /1 checked, 1 do not parse/);
  assert.match(bad.stdout, /bad\.sh:2: .*syntax error/);
  const good = cli([shellFile(dir, "good.sh", "jq -n '.a'\n")]);
  assert.equal(good.status, 0);
  assert.match(good.stdout, /1 checked, 0 do not parse/);
});

test("with a base rev the check reads only the .sh files changed since it", () => {
  const dir = scratch();
  const git = (...args) => execFileSync("git", args, { cwd: dir, stdio: "ignore" });
  git("init", "-q");
  git("config", "user.email", "test@example.com");
  git("config", "user.name", "test");
  shellFile(dir, "old.sh", "jq -n '{'\n");
  git("add", ".");
  git("commit", "-qm", "base");
  git("branch", "base");
  shellFile(dir, "good.sh", "jq -n '.a'\n");
  git("add", ".");
  git("commit", "-qm", "good");
  const good = cli(["base"], { cwd: dir });
  assert.equal(good.status, 0);
  assert.match(good.stdout, /1 checked, 0 do not parse/);
  shellFile(dir, "bad.sh", "jq -n '['\n");
  git("add", ".");
  git("commit", "-qm", "bad");
  assert.equal(cli(["base"], { cwd: dir }).status, 1);
});

test("with no argument the check prints its usage and exits 2", () => {
  assert.equal(cli([]).status, 2);
});

test("#584 r1 minor 2: a jq that cannot judge a program fails the check", () => {
  const good = shellFile(scratch(), "good.sh", "jq -n '.a'\n");
  for (const body of ["exit 5", "kill -9 $$"]) {
    const bin = scratch();
    chmodSync(shellFile(bin, "jq", `#!/bin/sh\n${body}\n`), 0o755);
    const run = cli([good], { env: { ...process.env, PATH: `${bin}:${process.env.PATH}` } });
    assert.notEqual(run.status, 0, body);
    assert.match(run.stderr, /jq could not check a program/, body);
  }
});
