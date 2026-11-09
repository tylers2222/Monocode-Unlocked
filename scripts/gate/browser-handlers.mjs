// Fails when a pull request invokes a command that tests/browser never names, or
// removes the last mention of one. The browser job fails any spec that sends a command
// with no handler (job 125406442573: spaces_settings_get), after about 10 min.
// It compares <base> to <head>, so old gaps stay allowed. It cannot see a PR that only
// makes a spec reach a command that already has a gap: the browser job still catches that.
// Usage: node scripts/gate/browser-handlers.mjs <base-rev> [<head-rev>]
import { execFileSync } from "node:child_process";
import { realpathSync } from "node:fs";
import { pathToFileURL } from "node:url";

const INVOKE = /\binvoke\s*(?:<(?:[^()"'`]|\([^)]*\))*?>)?\s*\(\s*["'](\w+)["']/g;
const TYPESCRIPT = /\.tsx?$/;
const TEST_FILE = /\.test\.|\.realdb\.|\/tests?\//;

/** Commands the app invokes that no tests/browser file names. Input: [path, text] pairs. */
export function unhandledCommands(files) {
  const words = new Set();
  const commands = new Map();
  for (const [path, text] of files) {
    if (path.startsWith("tests/browser/")) {
      for (const word of text.match(/\w+/g) ?? []) words.add(word);
    } else if (!TEST_FILE.test(path)) {
      for (const [, command] of text.matchAll(INVOKE)) {
        if (!commands.has(command)) commands.set(command, path);
      }
    }
  }
  return new Map([...commands].filter(([command]) => !words.has(command)));
}

/** The problems <head> adds. Input: [path, text] pairs of each tree. */
export function newUnhandled(baseFiles, headFiles) {
  const before = unhandledCommands(baseFiles);
  return [...unhandledCommands(headFiles)]
    .filter(([command]) => !before.has(command))
    .map(
      ([command, path]) =>
        `${path}: invokes ${command}, and tests/browser has no handler. Add it to HANDLERS in tests/browser/app/backend.ts with the answer the Rust command gives.`,
    );
}

function treeFiles(rev) {
  const git = (args, input) =>
    execFileSync("git", args, { input, maxBuffer: 1 << 30, stdio: ["pipe", "pipe", "ignore"] });
  const paths = git(["ls-tree", "-r", "--name-only", "-z", rev, "src/", "tests/browser/"])
    .toString()
    .split("\0")
    .filter((path) => TYPESCRIPT.test(path));
  const batch = git(["cat-file", "--batch"], paths.map((path) => `${rev}:${path}\n`).join(""));
  let at = 0;
  return paths.map((path) => {
    const headerEnd = batch.indexOf(10, at);
    const size = Number(batch.toString("latin1", at, headerEnd).split(" ")[2]);
    at = headerEnd + 1 + size + 1;
    return [path, batch.toString("utf8", headerEnd + 1, headerEnd + 1 + size)];
  });
}

if (import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  const [base, head = "HEAD"] = process.argv.slice(2);
  if (!base) {
    console.error("Usage: browser-handlers.mjs <base-rev> [<head-rev>]");
    process.exit(2);
  }
  const problems = newUnhandled(treeFiles(base), treeFiles(head));
  if (problems.length > 0) {
    console.log(problems.join("\n"));
    process.exit(1);
  }
}
