// promptfoo tests, one per case file. Each case names its grader.
import { loadCases } from "./fixture.mjs";

export default function tests() {
  return loadCases().map((c) => ({
    description: `${c.id}@${c.version}`,
    vars: { case_id: c.id },
    metadata: { case_id: c.id, version: c.version, tier: c.tier },
    assert: [{ type: "javascript", value: `file://graders/milestones.mjs:${c.grader}`, metric: c.id }],
  }));
}
