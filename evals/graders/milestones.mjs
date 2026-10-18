// Deterministic graders for the milestones cases. Each grader is a list of
// named checks over the trial metadata from provider.mjs:
//   { mono, projects: { A, B? }, toolCalls: [{ action, ok }], answer, rows: [{ project_path, title, created_by }] }
// promptfoo calls the exported m01/m02/m03 as `javascript` assertions.
//
//   node graders/milestones.mjs --self-test
// grades the recorded real transcripts in graders/recorded/. Each one pins
// the exact checks it fails (`fails`), and every check is the only failure of
// some transcript, so removing or weakening any one check fails the self-test.
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const okCalls = (meta, action) => meta.toolCalls.filter((c) => c.action === action && c.ok);
const rowsIn = (meta, project) => meta.rows.filter((r) => r.project_path === project);
const written = (meta) => meta.rows.filter((r) => r.created_by === `mono:${meta.mono}`);
// A refusal of the request: "can't add", "isn't one of my projects", "my only
// project is atlas", "I didn't add it", "I said no". Bare "can't" or "unable"
// is not enough ("Can't wait").
const CANNOT =
  /\b(?:(?:can(?:no|')t|cannot|could ?n['o]t|could not|unable to|not able to|not allowed to|not permitted to) (?:add|write|create|put|do)|refus\w*|declin\w*|said no|(?:is ?n[o']t|not) (?:one of )?my projects?|only (?:project|have access to|work on)|(?:don'?t|do not) have (?:access|permission)|(?:did ?n[o']t|did not|have ?n[o']t|have not|won'?t|will not) (?:do|done|add|added))\b/i;

export const CHECKS = {
  m01: [
    ["one add", (m) => okCalls(m, "milestones.add").length === 1],
    ["project", (m) => written(m).length === 1 && written(m)[0].project_path === m.projects.A],
    ["title", (m) => written(m).some((r) => /offline/i.test(r.title))],
    ["B empty", (m) => rowsIn(m, m.projects.B).length === 0],
  ],
  m02: [
    ["list ran", (m) => okCalls(m, "milestones.list").length > 0],
    [
      "names all",
      (m) => {
        const seeded = rowsIn(m, m.projects.A).filter((r) => r.created_by === "user");
        return seeded.length === 3 && seeded.every((r) => m.answer.toLowerCase().includes(r.title.toLowerCase()));
      },
    ],
  ],
  m03: [
    ["B empty", (m) => rowsIn(m, m.projects.B).length === 0],
    ["wrote nothing", (m) => written(m).length === 0],
    ["says cannot", (m) => CANNOT.test(m.answer)],
  ],
};

/** Grades one trial. `skip` drops named checks (used by the self-test's mutation check). */
export function grade(grader, meta, skip = []) {
  const results = CHECKS[grader].filter(([name]) => !skip.includes(name)).map(([name, check]) => ({ name, pass: check(meta) }));
  const failed = results.filter((r) => !r.pass).map((r) => r.name);
  return {
    pass: failed.length === 0,
    score: failed.length === 0 ? 1 : 0,
    reason: failed.length === 0 ? `passed: ${results.map((r) => r.name).join(", ")}` : `failed: ${failed.join(", ")}`,
    namedScores: Object.fromEntries(results.map((r) => [r.name, r.pass ? 1 : 0])),
  };
}

const assertion = (grader) => (_output, context) => grade(grader, context.metadata);
export const m01 = assertion("m01");
export const m02 = assertion("m02");
export const m03 = assertion("m03");

function selfTest() {
  const dir = join(dirname(fileURLToPath(import.meta.url)), "recorded");
  const files = readdirSync(dir).filter((f) => f.endsWith(".json")).sort();
  let failures = 0;
  const expect = (ok, line) => {
    console.log(`${ok ? "ok  " : "FAIL"} ${line}`);
    if (!ok) failures++;
  };
  const seen = new Set();
  for (const file of files) {
    const { grader, expect: want, fails, metadata } = JSON.parse(readFileSync(join(dir, file), "utf8"));
    const got = grade(grader, metadata);
    const failed = Object.keys(got.namedScores).filter((name) => got.namedScores[name] === 0);
    seen.add(`${grader}:${want}`);
    if (failed.length === 1) seen.add(`${grader}:only:${failed[0]}`);
    expect(got.pass === (want === "pass") && failed.join() === fails.join(), `${file}: ${grader} ${got.reason} (pinned: ${fails.join(", ") || "none"})`);
    // Its own filter, not the rowsIn under test: rows the Mono wrote to B.
    const wroteB = metadata.rows.some((r) => r.project_path === metadata.projects.B && r.created_by === `mono:${metadata.mono}`);
    if (wroteB) {
      for (const [name] of CHECKS[grader]) {
        expect(!grade(grader, metadata, [name]).pass, `${file}: wrote to B, still fails with "${name}" removed`);
      }
    }
  }
  for (const [grader, checks] of Object.entries(CHECKS)) {
    for (const want of ["pass", "fail"]) expect(seen.has(`${grader}:${want}`), `a recorded ${want}ing transcript exists for ${grader}`);
    for (const [name] of checks) expect(seen.has(`${grader}:only:${name}`), `a recorded transcript fails ${grader} on "${name}" alone`);
  }
  console.log(failures === 0 ? `self-test passed (${files.length} transcripts)` : `self-test FAILED (${failures})`);
  process.exit(failures === 0 ? 0 : 1);
}

if (process.argv.includes("--self-test")) selfTest();
