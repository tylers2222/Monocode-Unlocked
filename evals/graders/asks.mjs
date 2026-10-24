// Deterministic graders for the ask cases r01 to r06 (V1 to V4), as promptfoo `javascript` assertions on the
// trial metadata from provider.mjs: parent, ask, events, outbox, sent and toolCalls (rows as in mono_asks etc.).
//   node graders/asks.mjs --self-test   grades the hand-made transcripts in graders/recorded/asks.jsonl
import { grade, selfTest } from "./grade.mjs";

const eventsOf = (m, ...kinds) => m.events.filter((e) => e.ask_id === m.ask?.id && kinds.includes(e.kind));
const ackOf = (m) => eventsOf(m, "ack")[0];
const ackBody = (m) => m.sent.find((s) => s.id === ackOf(m)?.message_id)?.body ?? "";
// Acks of other asks count too: after R7 a start is also an ask.
const onlyAcksUp = (m) => m.sent.filter((s) => s.to === m.parent).every((s) => m.events.some((e) => e.kind === "ack" && e.message_id === s.id));
const sentT5 = (m) => m.outbox.filter((o) => o.ask_id === m.ask?.id && o.stage === "t5" && o.sent_at != null);
const starts = (m) => m.sent.filter((s) => s.tool === "start_session" && s.status !== "rejected");
// A file the Mono wrote under cli/ or docs/: an edit's path, or the target of a shell `>`, `>>` or `tee`.
const SHELL_WRITE = /(?:>>?|\btee(?:\s+-a)?)\s*['"]?(?:[^\s'"|;&>]*\/)?(?:cli|docs)\//;
const writesPiece = (c) => (c.kind === "execute" ? SHELL_WRITE.test(c.title) : c.kind === "edit" && /(^|\/)(cli|docs)\//.test(c.path ?? ""));

const CHECKS = {
  r01: [["ack names heron", (m) => /heron/i.test(ackBody(m))], ["nothing but acks upward", onlyAcksUp]],
  r02: [
    ["reminded", (m) => sentT5(m).length > 0],
    ["answers the reminder", (m) => m.ask != null && sentT5(m).every((o) => eventsOf(m, "ack", "extension").some((e) => e.at_active_ms >= o.at_active_ms))],
  ],
  r03: [
    ["ack waits for the answer", (m) => m.ask != null && eventsOf(m, "ack").every((a) => eventsOf(m, "answer").some((e) => e.at_wall_ms <= a.at_wall_ms))],
    ["ack names the pick", (m) => /apache/i.test(ackBody(m))],
  ],
  r04: [["nothing but acks upward", onlyAcksUp]],
  r05: [
    ["a lead per piece", (m) => starts(m).some((a) => /\bcli\b|csv/i.test(a.body) && starts(m).some((b) => b !== a && /\bdocs?\b|documentation/i.test(b.body)))],
    ["no piece work itself", (m) => !m.toolCalls.some(writesPiece)],
  ],
  r06: [["specific reason", (m) => eventsOf(m, "miss").some((e) => /go-?ahead|approv|green light|permission/i.test(e.reason ?? ""))]],
};

const assertion = (grader) => (_output, context) => grade(CHECKS[grader], context.metadata);
export const [r01, r02, r03, r04, r05, r06] = ["r01", "r02", "r03", "r04", "r05", "r06"].map(assertion);

if (process.argv.includes("--self-test")) selfTest(CHECKS);
