import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { jqPrograms, syntaxError } from "./jq-programs.mjs";

const programs = (text) => jqPrograms(text).programs.map(({ program }) => program);

test("#371: the r9_rules program with one closing brace too few does not parse", () => {
  const script = `r9_rules() {
  jq -n --arg app "$(target_bin instance)" '{app: $app, rules: [
    {match: "R9", tools: [
      {action: "sessions.start", arguments: {prompt: "R9-BUSY", due_minutes: 10},
      {action: "sessions.start", arguments: {prompt: "R9-LATE", due_minutes: 10}}
    ]}
  ]}'
}`;
  const [found] = jqPrograms(script).programs;
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
  assert.deepEqual(jqPrograms("cat <<EOF\njq '{'\nEOF\njq -n '.a'").programs, [{ line: 4, program: ".a" }]);
  assert.deepEqual(programs("a # it's a note\njq -n '.a'"), [".a"]);
  assert.deepEqual(programs("echo $# ${#x} ${y:-z}\njq -n '.a'"), [".a"]);
  assert.deepEqual(programs(`\${lock[@]+"\${lock[@]}"} a\njq -n '.a'`), [".a"]);
  assert.deepEqual(programs("echo $'a\\n' b\njq -n '.a'"), [".a"]);
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

// #584 r2 review: one test per witness. False reds first: each one would block a good PR.
function repo() {
  const dir = scratch();
  const git = (...args) => execFileSync("git", args, { cwd: dir, stdio: "ignore" });
  git("init", "-q");
  git("config", "user.email", "test@example.com");
  git("config", "user.name", "test");
  const commit = (name, text) => {
    mkdirSync(join(dir, dirname(name)), { recursive: true });
    shellFile(dir, name, text);
    git("add", "-A");
    git("commit", "-qm", name);
  };
  return { dir, git, commit };
}

test("r2 M1: a PR that deletes a .sh file passes", () => {
  const { dir, git, commit } = repo();
  commit("gone.sh", "jq -n '.a'\n");
  git("branch", "base");
  git("rm", "-q", "gone.sh");
  git("commit", "-qm", "delete");
  const run = cli(["base"], { cwd: dir });
  assert.equal(run.status, 0, run.stderr);
  assert.match(run.stdout, /0 checked, 0 do not parse/);
});

test("r2 M5: a file that is not .sh is not read", () => {
  const { dir, git, commit } = repo();
  commit("a.sh", "true\n");
  git("branch", "base");
  commit("README.md", "    jq -n '{'\n");
  assert.equal(cli(["base"], { cwd: dir }).status, 0);
});

test("r2: a .sh file under vendor/ is not read", () => {
  const { dir, git, commit } = repo();
  commit("a.sh", "true\n");
  git("branch", "base");
  commit("vendor/v.sh", "jq -n '{'\n");
  assert.equal(cli(["base"], { cwd: dir }).status, 0);
});

test("r2: a file only the base changed is not read (base...HEAD, not base..HEAD)", () => {
  const { dir, git, commit } = repo();
  commit("old.sh", "jq -n '{'\n");
  git("branch", "pr");
  commit("old.sh", "jq -n '.a'\n");
  git("branch", "base");
  git("checkout", "-q", "pr");
  commit("new.sh", "jq -n '.b'\n");
  const run = cli(["base"], { cwd: dir });
  assert.equal(run.status, 0);
  assert.match(run.stdout, /1 checked, 0 do not parse/);
});

test("r2 M3: jq -f 'x{' names a file, not a program", () => {
  assert.deepEqual(programs("jq -f 'x{' f"), []);
});

test("r2 M3: jq --from-file 'a{.jq' names a file, not a program", () => {
  assert.deepEqual(programs("jq --from-file 'a{.jq' x"), []);
});

test("r2 minor 2: a short flag group with f in it (-rf) names a file", () => {
  assert.deepEqual(programs("jq -rf 'x{' f"), []);
});

test("r2 minor 2: the value of -L is not the program", () => {
  assert.deepEqual(programs("jq -L 'lib{' -n '.a'"), [".a"]);
});

test("r2 minor 2: the value of --indent is skipped to reach the program", () => {
  assert.deepEqual(programs("jq --indent 2 -n '.a'"), [".a"]);
});

test("r2 M5: a heredoc ends only on a line that is just its tag", () => {
  assert.deepEqual(jqPrograms("cat <<'EOF'\nEOFX\njq '{'\nEOF\n"), { programs: [], skipped: [] });
});

test("r2 M5: an argument after a backslash-newline is not a command", () => {
  assert.deepEqual(programs("echo a \\\n  jq '{'"), []);
});

test("r2 M5: a word that starts with jq is not jq", () => {
  assert.deepEqual(programs("jq-wrap '{'"), []);
});

test("r2 M5: an assignment chain longer than a short lookback still starts a command", () => {
  assert.deepEqual(programs("a; b=1 c=22 d=333 e=4444 jq -n '.a'"), [".a"]);
});

// False passes: each one would let a broken program through.
test("r2 M5: # inside ${#x} does not start a comment", () => {
  assert.deepEqual(programs("echo ${#x}; jq -n '.a'"), [".a"]);
});

test("r2 M5: jq after ! is a command", () => {
  assert.deepEqual(programs("if ! jq -e '.a' f; then :; fi"), [".a"]);
});

test("r2 M5: jq after { is a command", () => {
  assert.deepEqual(programs("{ jq -n '.a'; }"), [".a"]);
});

test("r2 M5: both values of --rawfile are skipped", () => {
  assert.deepEqual(programs("jq --rawfile t f.txt -n '.a'"), [".a"]);
});

test("r2 M5: both values of --slurpfile are skipped", () => {
  assert.deepEqual(programs("jq --slurpfile t f.json -n '.a'"), [".a"]);
});

test("r2 M5: a ' inside double quotes opens nothing", () => {
  assert.deepEqual(programs("echo \"it's\"\njq -n '.a'"), [".a"]);
});

test("r2 M2: a <( ) inside $( ) stays balanced", () => {
  assert.deepEqual(programs("x=\"$(a <(b) 'x\"y')\"\njq -n '.a'"), [".a"]);
});

test("r2 M2: a subshell inside $( ) stays balanced", () => {
  assert.deepEqual(programs("x=\"$( (a); echo 'x\"y')\"\njq -n '.a'"), [".a"]);
});

// What the check cannot read safely it skips, and says so in one line.
function checkText(text) {
  const run = cli([shellFile(scratch(), "s.sh", text)]);
  return { ...run, skip: run.stdout.split("\n").filter((line) => line.includes(" skipped ")) };
}

test("r2 M4: a <<- heredoc skips the file with one line", () => {
  const run = checkText("cat <<-EOF\n\tbody\n\tEOF\njq -n '{'\n");
  assert.equal(run.status, 0);
  assert.equal(run.skip.length, 1);
  assert.match(run.skip[0], /^jq programs: skipped \S+s\.sh the whole file, for a heredoc it cannot read: <<-EOF$/);
});

test("r2: a heredoc tag it cannot read (<<\\EOF) skips the file with one line", () => {
  const run = checkText("cat <<\\EOF\njq '{'\nEOF\n");
  assert.equal(run.status, 0);
  assert.match(run.skip.join("\n"), /^jq programs: skipped \S+ the whole file, for a heredoc it cannot read: <<\\EOF$/);
});

test("r2 M5: a heredoc with no end line skips the file with one line", () => {
  const run = checkText("cat <<EOF\njq '{'\n");
  assert.equal(run.status, 0);
  assert.match(run.skip.join("\n"), /^jq programs: skipped \S+ the whole file, for a heredoc with no end line$/);
});

test("r2 minor 1: a <( ) in a flag value skips that program with one line", () => {
  const run = checkText(`jq -n --argjson a "$(jq -s . <(f "x y") <(f "z w"))" '{'\njq -n '['\n`);
  assert.equal(run.status, 1);
  assert.match(run.stdout, /1 checked, 1 do not parse/);
  assert.match(run.skip.join("\n"), /^jq programs: skipped \S+ line 1, for a <\( \) or >\( \) in a flag value$/);
});

test("a scan that ends inside a quote is unsure, so it skips the file with one line", () => {
  const run = checkText("jq -n '{'\necho \"open\n");
  assert.equal(run.status, 0);
  assert.match(run.skip.join("\n"), /^jq programs: skipped \S+ the whole file, for an open quote or \$\( at the end$/);
});

test("r2: with no jq on PATH the check fails on jq's own error", () => {
  const run = cli([shellFile(scratch(), "good.sh", "jq -n '.a'\n")], { env: { ...process.env, PATH: scratch() } });
  assert.notEqual(run.status, 0);
  assert.match(run.stderr, /spawnSync jq ENOENT/);
});

test("r3: a >( ) in a flag value skips that program too", () => {
  assert.deepEqual(jqPrograms(`jq --arg a "$(f >(g "x y"))" -n '{'`), {
    programs: [],
    skipped: ["line 1, for a <( ) or >( ) in a flag value"],
  });
});

test("r3: a here-string (<<<) is not a heredoc", () => {
  assert.deepEqual(jqPrograms("cat <<< x\njq -n '.a'"), { programs: [{ line: 2, program: ".a" }], skipped: [] });
});

test("r3: a . in a heredoc tag matches only a .", () => {
  assert.deepEqual(programs("cat <<E.F\nExF\njq '{'\nE.F\njq -n '.a'"), [".a"]);
});

test("r3: two heredocs on one line both end before code is read again", () => {
  assert.deepEqual(programs("cat <<A <<B\nx\nA\njq '{'\nB\njq -n '.a'"), [".a"]);
});
