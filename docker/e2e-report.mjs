// The E2E results are rows of results.tsv: suite, test, status, seconds, note.
// status is passed, failed, flaky or skipped.
//
//   node docker/e2e-report.mjs playwright <report.json>   print the rows of a Playwright JSON report
//   node docker/e2e-report.mjs junit <results.tsv>        print results.tsv as JUnit XML
import { readFileSync } from "node:fs";

const PLAYWRIGHT_STATUS = { expected: "passed", unexpected: "failed", flaky: "flaky", skipped: "skipped" };

const field = (text) => text.replace(/\x1b\[[0-9;]*m/g, "").replace(/\s+/g, " ").trim();

function* playwrightRows(suite, titles) {
  for (const spec of suite.specs ?? []) {
    const title = [...titles, spec.title].join(" › ");
    for (const test of spec.tests) {
      const seconds = test.results.reduce((sum, result) => sum + result.duration, 0) / 1000;
      const error = test.results.at(-1)?.error?.message ?? "";
      yield ["browser", `[${test.projectName}] ${spec.file}:${spec.line} › ${title}`, PLAYWRIGHT_STATUS[test.status], seconds.toFixed(1), error.split("\n")[0]];
    }
  }
  for (const child of suite.suites ?? []) yield* playwrightRows(child, [...titles, child.title]);
}

const xml = (text) => text.replace(/[<>&"]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;", '"': "&quot;" })[c]);

function junit(rows) {
  const suites = Map.groupBy(rows, (row) => row.suite);
  const body = [...suites].map(([suite, cases]) => {
    const count = (status) => cases.filter((c) => c.status === status).length;
    const time = cases.reduce((sum, c) => sum + Number(c.seconds), 0).toFixed(1);
    const testcases = cases.map((c) => {
      const open = `    <testcase classname="${xml(suite)}" name="${xml(c.test)}" time="${c.seconds}"`;
      if (c.status === "failed") return `${open}><failure message="${xml(c.note)}"/></testcase>`;
      if (c.status === "skipped") return `${open}><skipped message="${xml(c.note)}"/></testcase>`;
      return `${open}/>`;
    });
    return [
      `  <testsuite name="${xml(suite)}" tests="${cases.length}" failures="${count("failed")}" skipped="${count("skipped")}" time="${time}">`,
      ...testcases,
      "  </testsuite>",
    ].join("\n");
  });
  return ['<?xml version="1.0" encoding="UTF-8"?>', "<testsuites>", ...body, "</testsuites>"].join("\n");
}

function parseTsv(text) {
  const [header, ...lines] = text.trimEnd().split("\n");
  const keys = header.split("\t");
  return lines.map((line) => Object.fromEntries(line.split("\t").map((value, i) => [keys[i], value])));
}

const [mode, file] = process.argv.slice(2);
const text = readFileSync(file, "utf8");
if (mode === "playwright") {
  for (const suite of JSON.parse(text).suites) {
    for (const row of playwrightRows(suite, [])) console.log(row.map(field).join("\t"));
  }
} else if (mode === "junit") {
  console.log(junit(parseTsv(text)));
} else {
  console.error("usage: e2e-report.mjs playwright <report.json> | junit <results.tsv>");
  process.exit(2);
}
