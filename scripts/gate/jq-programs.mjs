// Fails when a shell script holds a jq program that does not parse. A broken one
// otherwise waits for its E2E job: #371's r9_rules cost an 18 min e2e-macos run.
// It reads only .sh files the PR changes, and only a literal program of jq where a
// command starts. It skips what it cannot read for certain: a program in a variable or
// double quotes, an argument string such as bash -c "jq ...", a heredoc body, a comment.
// jq compiles each one without running it, so an argument it would get at run time
// ($app) is no problem: only a syntax error counts.
// Usage: node scripts/gate/jq-programs.mjs <base-rev> | <file ...>
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { pathToFileURL } from "node:url";

const TWO_VALUE_FLAGS = new Set(["--arg", "--argjson", "--slurpfile", "--rawfile"]);
const NO_LITERAL_PROGRAM = new Set(["-f", "--from-file"]);
const COMMAND_BEFORE = /(?:^|[;|&(!{]|\b(?:if|then|elif|else|do|while|until))\s*(?:\w+=\S*\s+)*(?![\s\S])/m;
const HEREDOC = /^<<(?!<)(-?)\s*(['"]?)([\w.-]+)\2/;

/** Moves past one shell character. The stack holds the open quotes and $( ). */
function step(stack, text, at) {
  const char = text[at];
  const top = stack.at(-1);
  if (top === "'") {
    if (char === "'") stack.pop();
    return at + 1;
  }
  if (char === "\\") return at + 2;
  if (char === "$" && text[at + 1] === "(") {
    stack.push("(");
    return at + 2;
  }
  if (char === ")" && top === "(") stack.pop();
  else if (char === '"' && top === '"') stack.pop();
  else if (char === '"' || (char === "'" && top !== '"')) stack.push(char);
  return at + 1;
}

/** Index after the shell word that starts at `from`. Quotes and $( ) stay in one word. */
function wordEnd(text, from) {
  const stack = [];
  let at = from;
  while (at < text.length && (stack.length > 0 || !/[\s;|&)]/.test(text[at]))) at = step(stack, text, at);
  return at;
}

/** The single-quoted program text that starts at `from`: 'a'\''b' is the program a'b. */
function singleQuoted(text, from) {
  let program = "";
  let at = from;
  while (text[at] === "'") {
    const close = text.indexOf("'", at + 1);
    if (close < 0) return null;
    program += text.slice(at + 1, close);
    at = close + 1;
    if (!text.startsWith("\\''", at)) break;
    program += "'";
    at += 2;
  }
  // '...'"$1" builds the program at run time, so it is not a literal.
  return /[\s;|&)]|^$/.test(text[at] ?? "") ? program : null;
}

function skipBlanks(text, from) {
  let at = from;
  while (/[ \t]/.test(text[at] ?? "") || text.startsWith("\\\n", at)) at += text[at] === "\\" ? 2 : 1;
  return at;
}

function programAt(text, from) {
  let at = skipBlanks(text, from);
  while (text[at] === "-") {
    const end = wordEnd(text, at);
    const flag = text.slice(at, end);
    if (NO_LITERAL_PROGRAM.has(flag)) return null;
    at = skipBlanks(text, end);
    if (TWO_VALUE_FLAGS.has(flag)) {
      at = skipBlanks(text, wordEnd(text, at));
      at = skipBlanks(text, wordEnd(text, at));
    }
  }
  return text[at] === "'" ? singleQuoted(text, at) : null;
}

/** Start of the line after the body of each heredoc opened on the line that just ended. */
function afterHeredocs(text, from, heredocs) {
  let at = from;
  for (const { dash, tag } of heredocs) {
    const end = new RegExp(`^${dash ? "\\s*" : ""}${tag.replace(/\./g, "\\.")}$`, "m").exec(text.slice(at));
    at = end ? at + end.index + end[0].length : text.length;
  }
  return at;
}

/** The index after each `jq` that starts a command, outside quotes, comments and heredocs. */
function jqCommandEnds(text) {
  const stack = [];
  const ends = [];
  let heredocs = [];
  let at = 0;
  while (at < text.length) {
    const char = text[at];
    const top = stack.at(-1);
    const opener = char === "<" ? HEREDOC.exec(text.slice(at, at + 200)) : null;
    if (top === "'" || top === '"') {
      at = step(stack, text, at);
    } else if (char === "\n" && heredocs.length > 0) {
      at = afterHeredocs(text, at + 1, heredocs);
      heredocs = [];
    } else if (char === "#" && /[\s;(]/.test(text[at - 1] ?? " ")) {
      at = text.indexOf("\n", at) < 0 ? text.length : text.indexOf("\n", at);
    } else if (opener) {
      heredocs.push({ dash: opener[1] === "-", tag: opener[3] });
      at += opener[0].length;
    } else {
      if (char === "(") stack.push(char);
      const before = text.slice(Math.max(0, at - 200), at).replaceAll("\\\n", " ");
      if (/^jq[ \t\\]/.test(text.slice(at, at + 3)) && COMMAND_BEFORE.test(before)) ends.push(at + 2);
      at = char === "(" ? at + 1 : step(stack, text, at);
    }
  }
  // Open quotes at the end mean this pass lost its place, so it is sure of nothing.
  return stack.length > 0 ? [] : ends;
}

/** Each literal jq program in a shell script, with the line its command starts on. */
export function jqPrograms(text) {
  const programs = [];
  for (const end of jqCommandEnds(text)) {
    const program = programAt(text, end);
    if (program !== null) programs.push({ line: text.slice(0, end).split("\n").length, program });
  }
  return programs;
}

/** jq's first syntax error for a program, or null. Throws when jq could not judge it. */
export function syntaxError(program) {
  const { stderr, status, signal, error } = spawnSync("jq", ["-n", `if false then (${program}\n) else empty end`], { encoding: "utf8" });
  if (error) throw error;
  if (status === 0) return null;
  const line = stderr.split("\n").find((text) => text.includes("syntax error"));
  if (line) return line;
  // Exit 3 with no syntax error: a name only the run provides, such as $app.
  if (status === 3) return null;
  throw new Error(`jq could not check a program: ${signal ?? `exit ${status}`}`);
}

/** The .sh files <base> to HEAD changes and still has. */
function changedFiles(base) {
  const args = ["diff", "--name-only", "-z", "--diff-filter=d", `${base}...HEAD`, "--", "*.sh", ":!vendor"];
  return execFileSync("git", args, { encoding: "utf8" }).split("\0").filter(Boolean);
}

if (import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  const args = process.argv.slice(2);
  if (args.length === 0) {
    console.error("Usage: jq-programs.mjs <base-rev> | <file ...>");
    process.exit(2);
  }
  const files = existsSync(args[0]) ? args : changedFiles(args[0]);
  let checked = 0;
  const problems = [];
  for (const file of files) {
    for (const { line, program } of jqPrograms(readFileSync(file, "utf8"))) {
      checked += 1;
      const error = syntaxError(program);
      if (error) problems.push(`${file}:${line}: ${error}`);
    }
  }
  console.log(`jq programs: ${checked} checked, ${problems.length} do not parse`);
  if (problems.length > 0) {
    console.log(problems.join("\n"));
    process.exit(1);
  }
}
