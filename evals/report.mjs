// Turns promptfoo's results.json into a per-case table, writes report.md and
// report.json beside it, and compares each case with baseline.json.
//
//   node report.mjs <results.json> [--wall-seconds N] [--update-baseline]
//
// Per case: k, passes, model failures, infrastructure errors, pass rate over
// graded trials, pass^k and a Wilson 95% interval. Exit 0 ok, 1 a core case
// below FLOOR or below its baseline, 3 any infrastructure error.
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const FLOOR = 0.8;
const BASELINE = join(dirname(fileURLToPath(import.meta.url)), "baseline.json");
const ERROR = 2; // promptfoo ResultFailureReason.ERROR

export function wilson(passes, n, z = 1.96) {
  if (n === 0) return [0, 1];
  const p = passes / n;
  const centre = p + (z * z) / (2 * n);
  const spread = z * Math.sqrt((p * (1 - p) + (z * z) / (4 * n)) / n);
  const denom = 1 + (z * z) / n;
  return [(centre - spread) / denom, (centre + spread) / denom];
}

export function summarize(rows) {
  const cases = new Map();
  for (const row of rows) {
    const { case_id: id, version, tier } = row.testCase.metadata;
    const c = cases.get(id) ?? { id, version, tier, k: 0, passes: 0, failures: 0, errors: 0, cost: 0, priced: true, tokens: 0, failed: [] };
    c.k++;
    c.cost += row.cost ?? 0;
    c.tokens += row.response?.tokenUsage?.total ?? 0;
    if (row.response && row.response.cost == null && row.failureReason !== ERROR) c.priced = false;
    if (row.failureReason === ERROR) {
      c.errors++;
      c.failed.push(`ERROR ${row.error}`);
    } else if (row.success) {
      c.passes++;
    } else {
      c.failures++;
      c.failed.push(`${row.response?.metadata?.trial ?? "?"}: ${row.gradingResult?.reason ?? row.error}`);
    }
    cases.set(id, c);
  }
  return [...cases.values()]
    .sort((a, b) => a.id.localeCompare(b.id))
    .map((c) => {
      const graded = c.passes + c.failures;
      return { ...c, rate: graded ? c.passes / graded : null, passAllK: c.errors === 0 && c.passes === c.k, ci: wilson(c.passes, graded) };
    });
}

export function verdict(cases, baseline) {
  const problems = [];
  for (const c of cases) {
    if (c.errors) problems.push({ exit: 3, text: `${c.id}: ${c.errors} infrastructure error(s); not a model result` });
    const base = baseline[`${c.id}@${c.version}`];
    if (c.rate == null) continue;
    if (c.tier === "core" && c.rate < FLOOR) problems.push({ exit: 1, text: `${c.id}: ${pct(c.rate)} is below the ${pct(FLOOR)} floor` });
    if (base && c.rate < base.pass_rate) {
      problems.push({ exit: 1, text: `${c.id}: ${pct(c.rate)} is below baseline ${pct(base.pass_rate)}; re-run with K=10 before calling it a regression` });
    }
  }
  return { exit: Math.max(0, ...problems.map((p) => p.exit)), problems };
}

const money = (c) => (c.priced ? `$${c.cost.toFixed(4)}` : `unpriced, ${c.tokens} tokens`);
const pct = (x) => (x == null ? "n/a" : `${Math.round(x * 100)}%`);

function markdown(cases, v, meta) {
  const lines = [
    "# Eval report",
    "",
    `Harness ${meta.harness}, model ${meta.model}, app ${meta.app_rev}. Wall time ${meta.wall}. Model cost ${meta.cost}.`,
    "",
    "| case | k | pass | fail | infra error | pass rate | pass^k | Wilson 95% | cost |",
    "|---|---|---|---|---|---|---|---|---|",
    ...cases.map(
      (c) =>
        `| ${c.id}@${c.version} | ${c.k} | ${c.passes} | ${c.failures} | ${c.errors} | ${pct(c.rate)} | ${c.passAllK ? "yes" : "no"} | ${pct(c.ci[0])}-${pct(c.ci[1])} | ${money(c)} |`,
    ),
    "",
    v.problems.length ? `Verdict: ${v.exit === 3 ? "INFRA" : "FAIL"}` : "Verdict: PASS",
    ...v.problems.map((p) => `- ${p.text}`),
  ];
  const failed = cases.filter((c) => c.failed.length);
  if (failed.length) {
    lines.push("", "## Failed trials", "");
    for (const c of failed) for (const f of c.failed) lines.push(`- ${c.id} ${f}`);
  }
  return `${lines.join("\n")}\n`;
}

function main() {
  const args = process.argv.slice(2);
  const file = args[0];
  if (!file) {
    console.error("usage: node report.mjs <results.json> [--wall-seconds N] [--update-baseline]");
    process.exit(2);
  }
  const wallIndex = args.indexOf("--wall-seconds");
  const wallSeconds = wallIndex >= 0 ? Number(args[wallIndex + 1]) : null;
  const results = JSON.parse(readFileSync(file, "utf8"));
  const rows = results.results.results;
  const cases = summarize(rows);
  const baseline = JSON.parse(readFileSync(BASELINE, "utf8"));
  const v = verdict(cases, baseline);
  const dir = dirname(file);
  const plan = existsSync(join(dir, "trials.json")) ? JSON.parse(readFileSync(join(dir, "trials.json"), "utf8")) : {};
  const tags = results.config?.tags ?? {};
  const total = cases.reduce((sum, c) => sum + c.cost, 0);
  const priced = cases.every((c) => c.priced);
  const meta = {
    harness: plan.harness ?? "?",
    model: plan.model ?? "?",
    app_rev: tags.app_rev ?? "?",
    wall: wallSeconds == null ? "n/a" : `${Math.floor(wallSeconds / 60)}m${String(wallSeconds % 60).padStart(2, "0")}s`,
    cost: priced ? `$${total.toFixed(4)}` : `unpriced, ${cases.reduce((sum, c) => sum + c.tokens, 0)} tokens`,
  };
  const md = markdown(cases, v, meta);
  writeFileSync(join(dir, "report.md"), md);
  writeFileSync(join(dir, "report.json"), JSON.stringify({ ...meta, wall_seconds: wallSeconds, cost_usd: priced ? total : null, cases, verdict: v }, null, 2));
  process.stdout.write(md);
  if (args.includes("--update-baseline")) {
    const date = new Date().toISOString().slice(0, 10);
    for (const c of cases) {
      if (c.errors) continue;
      baseline[`${c.id}@${c.version}`] = { k: c.k, passes: c.passes, pass_rate: c.rate, app_rev: meta.app_rev, harness: meta.harness, model: meta.model, date };
    }
    writeFileSync(BASELINE, `${JSON.stringify(baseline, null, 2)}\n`);
    console.log(`baseline updated: ${BASELINE}`);
  }
  process.exit(v.exit);
}

if (import.meta.url === `file://${process.argv[1]}`) main();
