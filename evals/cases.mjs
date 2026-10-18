// promptfoo tests, one per case file. A case names its grader as
// `<file>:<export>`, resolved in graders/, so a new grader file needs no edit here.
import { loadCases } from "./fixture.mjs";

export default function tests() {
  return loadCases().map((c) => ({
    description: `${c.id}@${c.version}`,
    vars: { case_id: c.id },
    metadata: { case_id: c.id, version: c.version, tier: c.tier },
    assert: [{ type: "javascript", value: `file://graders/${c.grader.replace(":", ".mjs:")}`, metric: c.id }],
  }));
}
