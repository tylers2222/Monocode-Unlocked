#!/usr/bin/env node
// Claude Code PreToolUse hook: refuses `gh pr merge` when the PR's base is not tyler/main.
// The ruleset covers only tyler/main, so on a stacked base GitHub merges at once, unreviewed.
// Reads the hook payload on stdin. Exit 2 refuses and stderr goes to the agent. Exit 0 passes.
// A failed lookup passes, and says so on stderr. A selector only known at run time
// ($PR, xargs) is refused, because the hook cannot tell which PR it would merge.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { basename, resolve } from "node:path";

const PROTECTED_BASE = "tyler/main";
const LOOKUP_TIMEOUT_MS = 20_000;
const REDIRECT = /<<<|<<-?|>>|>&|<&|&>|>\||<>|>|</y;
const KEYWORDS = new Set(["do", "then", "else", "elif", "if", "while", "until", "!", "{"]);
const WRAPPERS = new Set(["command", "builtin", "exec", "env", "nohup", "sudo", "time", "timeout", "nice", "caffeinate"]);
const SHELLS = new Set(["sh", "bash", "zsh", "dash"]);
const ASSIGNMENT = /^[A-Za-z_]\w*=/;
const GH_VALUE_FLAGS = /^(-t|--subject|-b|--body|-F|--body-file|-A|--author-email|--match-head-commit)$/;
const NOT_A_MERGE_FLAGS = new Set(["-h", "--help", "--disable-auto"]);

/** Reads shell text into simple commands, each a list of words: { text, dynamic }. A
 * dynamic word holds an expansion. Quotes and heredoc bodies stay data, so text that only
 * mentions a command is not a command. Substitutions add their inner commands. */
class ShellScanner {
  constructor(source) {
    this.source = source;
    this.at = 0;
    this.commands = [];
  }

  scan(insideSubstitution = false) {
    const list = { words: [], word: null, redirect: null, heredocs: [] };
    let depth = 0;
    while (this.at < this.source.length) {
      const char = this.source[this.at];
      if (char === ")" && depth === 0 && insideSubstitution) {
        this.at += 1;
        break;
      }
      if (char === "(") depth += 1;
      if (char === ")") depth -= 1;
      this.step(list, char);
    }
    this.endCommand(list);
    return this.commands;
  }

  step(list, char) {
    if (char === "\n") return this.newline(list);
    if (";|()".includes(char) || (char === "&" && this.source[this.at + 1] !== ">")) {
      this.at += 1;
      return this.endCommand(list);
    }
    if (char === "<" || char === ">" || char === "&") return this.redirect(list);
    if (char === " " || char === "\t") {
      this.at += 1;
      return this.endWord(list);
    }
    if (char === "#" && !list.word) return this.skipLine();
    return this.wordPart(this.currentWord(list), char);
  }

  newline(list) {
    this.at += 1;
    this.endCommand(list);
    for (const delimiter of list.heredocs.splice(0)) this.skipHeredoc(delimiter);
  }

  skipLine() {
    const end = this.source.indexOf("\n", this.at);
    this.at = end === -1 ? this.source.length : end;
  }

  skipHeredoc(delimiter) {
    while (this.at < this.source.length) {
      const start = this.at;
      this.skipLine();
      const line = this.source.slice(start, this.at).replace(/^\t+/, "");
      this.at = Math.min(this.at + 1, this.source.length);
      if (line === delimiter) return;
    }
  }

  redirect(list) {
    const adjacent = list.word;
    this.endWord(list);
    // The 2 in 2>&1 is a file descriptor, not an argument.
    if (adjacent && /^\d+$/.test(adjacent.text)) list.words.pop();
    REDIRECT.lastIndex = this.at;
    const operator = REDIRECT.exec(this.source)[0];
    this.at += operator.length;
    list.redirect = operator.startsWith("<<") && operator !== "<<<" ? "heredoc" : "target";
  }

  currentWord(list) {
    list.word ??= { text: "", dynamic: false };
    return list.word;
  }

  endWord(list) {
    if (!list.word) return;
    if (list.redirect === "heredoc") list.heredocs.push(list.word.text);
    if (!list.redirect) list.words.push(list.word);
    list.word = null;
    list.redirect = null;
  }

  endCommand(list) {
    this.endWord(list);
    if (list.words.length > 0) this.commands.push(list.words);
    list.words = [];
  }

  wordPart(word, char) {
    if (char === "\\") {
      const next = this.source[this.at + 1] ?? "";
      if (next !== "\n") word.text += next;
      this.at += 2;
    } else if (char === "'") {
      this.singleQuoted(word);
    } else if (char === '"') {
      this.doubleQuoted(word);
    } else if (char === "$" || char === "`") {
      this.expansion(word, char);
    } else {
      word.text += char;
      this.at += 1;
    }
  }

  singleQuoted(word) {
    const end = this.source.indexOf("'", this.at + 1);
    const stop = end === -1 ? this.source.length : end;
    word.text += this.source.slice(this.at + 1, stop);
    this.at = stop + 1;
  }

  doubleQuoted(word) {
    this.at += 1;
    while (this.at < this.source.length && this.source[this.at] !== '"') {
      const char = this.source[this.at];
      if (char === "\\" || char === "$" || char === "`") {
        this.wordPart(word, char);
      } else {
        word.text += char;
        this.at += 1;
      }
    }
    this.at += 1;
  }

  expansion(word, char) {
    word.dynamic = true;
    word.text += "$";
    if (char === "`") {
      const end = this.source.indexOf("`", this.at + 1);
      const stop = end === -1 ? this.source.length : end;
      this.commands.push(...parseCommands(this.source.slice(this.at + 1, stop)));
      this.at = stop + 1;
    } else if (this.source[this.at + 1] === "(") {
      this.at += 2;
      this.scan(true);
    } else {
      this.at += 1;
    }
  }
}

function parseCommands(source) {
  return new ShellScanner(source).scan();
}

const isDurationOrFlag = (text) => text.startsWith("-") || /^\d+[smhd]?$/.test(text) || ASSIGNMENT.test(text);

/** Strips keywords, assignments and wrappers. fromStdin marks xargs. */
function unwrap(words) {
  let i = 0;
  let envRepo = null;
  const assign = (word) => {
    if (word.text.startsWith("GH_REPO=")) envRepo = { ...word, text: word.text.slice(8) };
  };
  for (; i < words.length; i += 1) {
    const { text } = words[i];
    if (ASSIGNMENT.test(text)) assign(words[i]);
    else if (WRAPPERS.has(basename(text))) {
      while (words[i + 1] && isDurationOrFlag(words[i + 1].text)) {
        i += 1;
        assign(words[i]);
      }
    } else if (!KEYWORDS.has(text)) break;
  }
  const rest = words.slice(i);
  if (rest[0] && basename(rest[0].text) === "xargs") {
    const gh = rest.findIndex((word) => basename(word.text) === "gh");
    return { words: gh === -1 ? [] : rest.slice(gh), envRepo, fromStdin: true };
  }
  return { words: rest, envRepo, fromStdin: false };
}

/** The shell text a `sh -c '...'` or `eval ...` runs, or null. */
function innerShellText({ words }) {
  const name = words[0] ? basename(words[0].text) : "";
  if (name === "eval") return words.slice(1).map((word) => word.text).join(" ");
  if (!SHELLS.has(name)) return null;
  const flag = words.findIndex((word) => /^-[a-z]*c[a-z]*$/.test(word.text));
  return flag === -1 ? null : (words[flag + 1]?.text ?? null);
}

function flatten(source) {
  return parseCommands(source).flatMap((words) => {
    const command = unwrap(words);
    const inner = innerShellText(command);
    return inner === null ? [command] : flatten(inner);
  });
}

function readGhArgs(args) {
  const parsed = { positional: [], repo: null, flags: new Set() };
  for (let i = 0; i < args.length; i += 1) {
    const { text, dynamic } = args[i];
    if (text === "-R" || text === "--repo") {
      i += 1;
      parsed.repo = args[i] ?? null;
    } else if (text.startsWith("--repo=")) parsed.repo = { text: text.slice(7), dynamic };
    else if (/^-R./.test(text)) parsed.repo = { text: text.slice(2).replace(/^=/, ""), dynamic };
    else if (GH_VALUE_FLAGS.test(text)) i += 1;
    else if (text.startsWith("-")) parsed.flags.add(text);
    else parsed.positional.push(args[i]);
  }
  return parsed;
}

/** The merge a simple command would run, or null when it runs none. */
function mergeOf({ words, envRepo, fromStdin }) {
  if (!words[0] || basename(words[0].text) !== "gh") return null;
  const { positional, repo, flags } = readGhArgs(words.slice(1));
  if (positional[0]?.text !== "pr" || positional[1]?.text !== "merge") return null;
  if ([...flags].some((flag) => NOT_A_MERGE_FLAGS.has(flag))) return null;
  return { selector: positional[2] ?? null, repo: repo ?? envRepo, fromStdin };
}

function nextDirectory(cwd, words) {
  const target = words[1];
  if (!target) return homedir();
  if (target.dynamic || cwd === null || target.text === "-") return null;
  return resolve(cwd, target.text.replace(/^~(?=\/|$)/, homedir()));
}

/** Every merge in the command, in order, with the directory gh would run it from. */
function findMerges(source, startCwd) {
  const merges = [];
  let cwd = startCwd;
  for (const command of flatten(source)) {
    if (command.words[0]?.text === "cd") cwd = nextDirectory(cwd, command.words);
    const merge = mergeOf(command);
    if (merge) merges.push({ ...merge, cwd });
  }
  return merges;
}

function isUnresolved({ selector, repo, cwd, fromStdin }) {
  if (fromStdin || selector?.dynamic || repo?.dynamic) return true;
  // gh takes the repository from the directory, unless a flag or a URL names it.
  return cwd === null && !repo && !selector?.text.includes("://");
}

/** Asks gh the way the merge itself would resolve the PR. { base } or { error }. */
function lookupBase({ selector, repo, cwd }) {
  const args = ["pr", "view"];
  if (selector) args.push(selector.text);
  if (repo) args.push("-R", repo.text);
  args.push("--json", "baseRefName", "--jq", ".baseRefName");
  const result = spawnSync("gh", args, { cwd, encoding: "utf8", timeout: LOOKUP_TIMEOUT_MS });
  const base = result.stdout?.trim();
  if (result.status === 0 && base) return { base };
  return { error: result.error?.message ?? (result.stderr?.trim() || `gh exited ${result.status}`) };
}

function refusalFor(merge) {
  const name = merge.selector?.text ?? "for the current branch";
  if (isUnresolved(merge)) {
    return (
      `Refused: this gh pr merge names its PR or repository only at run time, so the base cannot be checked. ` +
      `Run it again with the literal PR number and -R, one PR per command.`
    );
  }
  const { base, error } = lookupBase(merge);
  if (error) {
    process.stderr.write(`gh-merge-guard: base lookup failed for PR ${name}, so this merge passes unchecked: ${error}\n`);
    return null;
  }
  if (base === PROTECTED_BASE) return null;
  return (
    `Refused: PR ${name} has base ${base}, not ${PROTECTED_BASE}. Only ${PROTECTED_BASE} has required checks, so on ` +
    `this base GitHub merges at once, unreviewed, with or without --auto. Instead: leave this PR unmerged until its ` +
    `base PR has merged into ${PROTECTED_BASE}, retarget it with gh pr edit --base ${PROTECTED_BASE}, then merge it ` +
    `after its checks and review. Do not work around this with gh api.`
  );
}

function main() {
  const payload = JSON.parse(readFileSync(0, "utf8"));
  const command = payload.tool_input?.command;
  if (typeof command !== "string") return 0;
  const refusals = findMerges(command, payload.cwd ?? process.cwd()).map(refusalFor).filter(Boolean);
  if (refusals.length === 0) return 0;
  process.stderr.write(`${refusals.join("\n")}\n`);
  return 2;
}

try {
  process.exitCode = main();
} catch (error) {
  process.stderr.write(`gh-merge-guard: hook error, so this command passes unchecked: ${error.message}\n`);
  process.exitCode = 0;
}
