// Shared by the grader files: `grade` runs named checks on one trial's metadata.
// `selfTest` grades recorded/: each transcript pins the checks it fails, each grader
// needs a passing and a failing one, and each check must be the only failure of some
// transcript, so removing or weakening a check fails. `extra` adds a file's own checks.
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/** Grades one trial on `checks` ([name, fn] pairs). `skip` drops named checks (used by mutation checks). */
export function grade(checks, meta, skip = []) {
  const results = checks.filter(([name]) => !skip.includes(name)).map(([name, check]) => ({ name, pass: check(meta) }));
  const failed = results.filter((r) => !r.pass).map((r) => r.name);
  return {
    pass: failed.length === 0,
    score: failed.length === 0 ? 1 : 0,
    reason: failed.length === 0 ? `passed: ${results.map((r) => r.name).join(", ")}` : `failed: ${failed.join(", ")}`,
    namedScores: Object.fromEntries(results.map((r) => [r.name, r.pass ? 1 : 0])),
  };
}

/** Every transcript in recorded/: a *.json file is one, and each line of a *.jsonl file is one, named <file>:<line>. */
function recorded() {
  const dir = join(dirname(fileURLToPath(import.meta.url)), "recorded");
  return readdirSync(dir)
    .filter((file) => /\.jsonl?$/.test(file))
    .sort()
    .flatMap((file) => {
      const text = readFileSync(join(dir, file), "utf8");
      return file.endsWith(".json") ? [{ ...JSON.parse(text), name: file }] : text.trim().split("\n").map((line, i) => ({ ...JSON.parse(line), name: `${file}:${i + 1}` }));
    });
}

export function selfTest(CHECKS, extra = () => {}) {
  const transcripts = recorded().filter((t) => t.grader in CHECKS);
  let failures = 0;
  const expect = (ok, line) => {
    console.log(`${ok ? "ok  " : "FAIL"} ${line}`);
    if (!ok) failures++;
  };
  const seen = new Set();
  for (const t of transcripts) {
    const got = grade(CHECKS[t.grader], t.metadata);
    const failed = Object.keys(got.namedScores).filter((name) => got.namedScores[name] === 0);
    seen.add(`${t.grader}:${t.expect}`);
    if (failed.length === 1) seen.add(`${t.grader}:only:${failed[0]}`);
    expect(got.pass === (t.expect === "pass") && failed.join() === t.fails.join(), `${t.name}: ${t.grader} ${got.reason} (pinned: ${t.fails.join(", ") || "none"})`);
    extra(t, expect);
  }
  for (const [grader, checks] of Object.entries(CHECKS)) {
    for (const want of ["pass", "fail"]) expect(seen.has(`${grader}:${want}`), `a recorded ${want}ing transcript exists for ${grader}`);
    for (const [name] of checks) expect(seen.has(`${grader}:only:${name}`), `a recorded transcript fails ${grader} on "${name}" alone`);
  }
  console.log(failures === 0 ? `self-test passed (${transcripts.length} transcripts)` : `self-test FAILED (${failures})`);
  process.exit(failures === 0 ? 0 : 1);
}
