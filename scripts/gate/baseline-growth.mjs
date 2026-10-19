// Fails when a lint baseline grows from <base> to <head>. The baselines are the
// eslint-suppressions.json counts per (file, rule) and the Rust lint attributes
// per (file, allow or expect, lint), outer, inner and cfg_attr forms alike.
// Fixed debt may leave them; new debt may not enter. A fixed marker can still be
// traded for a new one with the same (file, lint) key; gate/integrity catches the
// added attribute line.
// Usage: node scripts/gate/baseline-growth.mjs <base-rev> [<head-rev>]
import { execFileSync } from "node:child_process";

const [base, head = "HEAD"] = process.argv.slice(2);

const git = (...args) =>
  execFileSync("git", args, { encoding: "utf8", maxBuffer: 1 << 28, stdio: ["ignore", "pipe", "ignore"] });

function show(rev, path) {
  try {
    return git("show", `${rev}:${path}`);
  } catch {
    return null;
  }
}

function eslintCounts(rev) {
  const counts = new Map();
  const suppressions = JSON.parse(show(rev, "eslint-suppressions.json") ?? "{}");
  for (const [file, rules] of Object.entries(suppressions)) {
    for (const [rule, { count }] of Object.entries(rules)) counts.set(`${file} ${rule}`, count);
  }
  return counts;
}

// Comments, strings and char literals, so text inside them never reads as an attribute.
const NOISE = /\/\/[^\n]*|\/\*[\s\S]*?\*\/|r(#*)"[\s\S]*?"\1|"(?:\\[\s\S]|[^\\"])*"|'(?:\\u\{[\da-fA-F]+\}|\\.|[^\\'\n])'/g;

function closing(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    if (text[i] === "[" || text[i] === "(") depth++;
    else if ((text[i] === "]" || text[i] === ")") && --depth === 0) return i;
  }
  return text.length;
}

function* lintAttributes(source) {
  const code = source.replace(NOISE, " ");
  for (const attribute of code.matchAll(/#!?\[/g)) {
    const open = attribute.index + attribute[0].length - 1;
    const body = code.slice(open, closing(code, open));
    for (const call of body.matchAll(/(?<![\w.])(allow|expect)\s*\(/g)) {
      const args = call.index + call[0].length - 1;
      for (const item of body.slice(args + 1, closing(body, args)).split(",")) {
        const lint = item.replace(/\s+/g, "");
        if (lint && !lint.startsWith("reason")) yield `${call[1]}(${lint})`;
      }
    }
  }
}

function rustCounts(rev) {
  const counts = new Map();
  const files = git("ls-tree", "-r", "--name-only", rev).split("\n");
  for (const file of files.filter((path) => path.endsWith(".rs") && !path.startsWith("vendor/"))) {
    for (const lint of lintAttributes(show(rev, file))) {
      const key = `${file} ${lint}`;
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
  }
  return counts;
}

function growth(what, before, after) {
  return [...after]
    .filter(([key, count]) => count > (before.get(key) ?? 0))
    .map(([key, count]) => `${what} grew: ${key} ${before.get(key) ?? 0} -> ${count}`);
}

if (show(base, "eslint-suppressions.json") === null) {
  console.log(`${base} has no eslint-suppressions.json, so there is no baseline to compare.`);
  process.exit(0);
}

const grew = [
  ...growth("eslint-suppressions.json", eslintCounts(base), eslintCounts(head)),
  ...growth("Rust lint attribute", rustCounts(base), rustCounts(head)),
];
if (grew.length > 0) {
  console.log(grew.join("\n"));
  console.log("Lint baselines may only shrink. Fix the new problem instead of suppressing it.");
  process.exit(1);
}
