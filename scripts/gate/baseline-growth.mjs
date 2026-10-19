// Fails when a lint baseline grows from <base> to <head>. The baselines are the
// eslint-suppressions.json counts per (file, rule) and the Rust lint suppressions
// per (file, lint) that rust-lint-attributes.mjs finds. Fixed debt may leave them;
// new debt may not enter. A fixed marker can still be traded for a new one with
// the same (file, lint) key; gate/integrity catches the added attribute line.
// Usage: node scripts/gate/baseline-growth.mjs <base-rev> [<head-rev>]
import { execFileSync } from "node:child_process";
import { lintAttributes } from "./rust-lint-attributes.mjs";

const [base, head = "HEAD"] = process.argv.slice(2);

const git = (...args) =>
  execFileSync("git", args, { encoding: "utf8", maxBuffer: 1 << 28, stdio: ["ignore", "pipe", "ignore"] });

for (const rev of [base, head]) {
  try {
    git("rev-parse", "--verify", `${rev}^{commit}`);
  } catch {
    console.error(`Unknown git revision: ${rev}. Usage: baseline-growth.mjs <base-rev> [<head-rev>]`);
    process.exit(2);
  }
}

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
