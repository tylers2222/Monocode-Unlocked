// promptfoo provider: one call = one trial of one case against the running
// GigaEval instance. The sender Mono sends each event message to the trial's
// Mono with `messages.send` through the app CLI; the provider waits for that
// turn to end, then reads the Mono's transcript and the milestones rows
// read-only from monocode.db. No UI is driven.
// It reads the ask rows too. A `lead` trial drives a lead session that the
// sender starts. Trials that ask or move the fake clock run one at a time.
//
// Infrastructure failures (instance down, no sender token, turn timeout) come
// back as `error`, which promptfoo records as ERROR, never as a model failure.
import { execFile } from "node:child_process";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { SENDER, env } from "./fixture.mjs";

const run = promisify(execFile);
const CLI = join(dirname(fileURLToPath(import.meta.url)), "../scripts/giga-eval/cli.sh");
const TURN_TIMEOUT_MS = Number(process.env.EVAL_TURN_TIMEOUT_MS ?? 300_000);
const POLL_MS = 3_000;
// A turn has ended when the app stamps `durationMs` on the message that opened
// it and the transcript has then held still for SETTLED_POLLS. Builds before
// ADE-8 never stamp a Mono message; for those, EVAL_LEGACY_TURN_END=1 ends
// the turn once `turnMetrics` exists and the transcript has been quiet for
// LEGACY_QUIET_POLLS. `turnMetrics` is written during the turn, so this rule
// is never used on current builds.
const SETTLED_POLLS = 2;
const LEGACY_QUIET_POLLS = process.env.EVAL_LEGACY_TURN_END === "1" ? 10 : Infinity;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const LEAD_BRIEF = "You are the team lead on this project for Beta, a Mono, and Beta sends you the work.";

// USD per million tokens, by turnModel.id. omp:default is
// anthropic/claude-opus-5-5 (~/.omp/agent/config.yml modelRoles.default);
// rates are the ones omp itself bills in ~/.omp/stats.db. Unlisted models
// report tokens with no cost.
const PRICES = {
  "omp:default": { input: 4, output: 20, cacheRead: 0.2, cacheWrite: 8 },
};

class InfraError extends Error {}

async function query(db, statement, ...params) {
  const bound = params.reduce((text, p) => text.replace("?", `'${String(p).replaceAll("'", "''")}'`), statement);
  try {
    const { stdout } = await run("sqlite3", ["-json", `file:${db}?mode=ro`, bound], { maxBuffer: 64 << 20 });
    return stdout.trim() ? JSON.parse(stdout) : [];
  } catch (e) {
    throw new InfraError(`sqlite read failed: ${e.stderr || e.message}`);
  }
}

/** One app action as the sender Mono: its result. A refusal exits 1 with the reply on stdout. */
async function app(action, input) {
  let stdout;
  try {
    ({ stdout } = await run(CLI, [SENDER, action, JSON.stringify(input)], { env: { ...process.env, GIGA_EVAL_TARGET: "instance" } }));
  } catch (e) {
    const why = e.code === 3 ? "the sender Mono has no active turn (no app token)" : (e.stdout || e.stderr || e.message).trim().split("\n").pop();
    throw new InfraError(`${action} failed: ${why}`);
  }
  const reply = JSON.parse(stdout);
  if (!reply.ok) throw new InfraError(`${action} refused: ${stdout.trim()}`);
  return reply.result;
}

function instanceAlive() {
  try {
    process.kill(Number(readFileSync(join(process.env.GIGA_EVAL_OUT ?? "/tmp/giga-eval", "instance.pid"), "utf8")), 0);
    return true;
  } catch {
    return false;
  }
}

async function waitTurnEnd(db, session, messageId) {
  const deadline = Date.now() + TURN_TIMEOUT_MS;
  let last = null;
  let still = 0;
  while (Date.now() < deadline) {
    if (!instanceAlive()) throw new InfraError(`the instance exited while ${messageId} was in its turn`);
    const [row] = await query(
      db,
      "SELECT (SELECT json_extract(block_json,'$.durationMs') IS NOT NULL FROM mono_blocks WHERE session_id=? AND block_id=?) AS stamped, (SELECT json_extract(block_json,'$.turnMetrics') IS NOT NULL FROM mono_blocks WHERE session_id=? AND block_id=?) AS metered, (SELECT count(*) || ':' || total(length(block_json)) FROM mono_blocks WHERE session_id=?) AS shape, (SELECT json_extract(block_json,'$.text') FROM mono_blocks WHERE session_id=? AND json_extract(block_json,'$.role')='system' AND json_extract(block_json,'$.notice')='error' AND seq > (SELECT seq FROM mono_blocks WHERE session_id=? AND block_id=?) ORDER BY seq LIMIT 1) AS error",
      session,
      messageId,
      session,
      messageId,
      session,
      session,
      session,
      messageId,
    );
    // The app writes a harness failure (overload, lost connection, expired
    // login) as a system block with notice "error" (apply.ts session.error).
    if (row.error != null) throw new InfraError(`harness error in the turn for ${messageId}: ${row.error}`);
    still = row.shape === last ? still + 1 : 0;
    last = row.shape;
    if ((row.stamped && still >= SETTLED_POLLS) || (row.metered && still >= LEGACY_QUIET_POLLS)) return;
    await sleep(POLL_MS);
  }
  throw new InfraError(`turn for ${messageId} did not end within ${TURN_TIMEOUT_MS / 1000}s`);
}

async function settleLead(db, lead, project, tables) {
  const inFlight = ["SELECT count(*) FROM mono_messages WHERE to_session_id=? AND status NOT IN ('rejected','dropped') AND delivered_at IS NULL"];
  // Only rows to the lead: the `why` notice goes to the held sender, whose part the provider plays itself.
  if (tables.has("mono_outbox")) inFlight.push("SELECT count(*) FROM mono_outbox WHERE recipient='session:' || ? AND sent_at IS NULL");
  const sql = `SELECT ${inFlight.map((q) => `(${q})`).join(" + ")} AS waiting`;
  const deadline = Date.now() + TURN_TIMEOUT_MS;
  let still = 0;
  while (Date.now() < deadline) {
    if (!instanceAlive()) throw new InfraError(`the instance exited while lead ${lead} worked`);
    const { sessions } = await app("sessions.list", { project });
    const [{ waiting }] = await query(db, sql, ...inFlight.map(() => lead));
    still = sessions.find((s) => s.id === lead)?.busy === false && waiting === 0 ? still + 1 : 0;
    if (still >= SETTLED_POLLS) return;
    await sleep(POLL_MS);
  }
  throw new InfraError(`lead ${lead} did not settle within ${TURN_TIMEOUT_MS / 1000}s`);
}

// As advance() in scripts/scenarios/app.sh: the app adds at most 30 s per 1 s
// tick, so the clock moves in 20 s steps, 3 per minute, each stored before the next.
async function advance(db, minutes) {
  const file = join(dirname(db), "fake-clock.json");
  const reading = JSON.parse(readFileSync(file, "utf8"));
  const activeMs = async () => (await query(db, "SELECT active_ms FROM mono_clock WHERE id=1"))[0]?.active_ms ?? 0;
  for (let want = await activeMs(), steps = minutes * 3; steps > 0; steps--) {
    want += 20_000;
    for (const ns of ["uptime_ns", "monotonic_ns"]) reading[ns] += 20e9;
    reading.wall_ms += 20_000;
    writeFileSync(`${file}.tmp`, JSON.stringify(reading));
    renameSync(`${file}.tmp`, file);
    for (const until = Date.now() + 30_000; (await activeMs()) < want; await sleep(250)) {
      if (Date.now() > until) throw new InfraError(`the app did not reach active_ms ${want} within 30s`);
    }
  }
}

// One app, one clock: trials that ask or advance run one at a time in this chain.
let clockChain = Promise.resolve();

/** Starts the lead of a `lead` trial, runs the events, waiting for the agent after each, and reads its transcript. */
async function drive(plan, trial, tables) {
  const lead = trial.agent === "lead"
    ? (await app("sessions.start", { prompt: LEAD_BRIEF, harness: plan.harness, model: plan.model, project: trial.projects.A, notifyOnComplete: false })).id
    : null;
  const messageId = async (action, input) => (await app(action, input)).message_id ?? Promise.reject(new InfraError(`${action} answered no message_id`));
  const messageIds = [];
  const settle = () => (lead ? settleLead(plan.db, lead, trial.projects.A, tables) : waitTurnEnd(plan.db, trial.session, messageIds.at(-1)));
  if (lead) await settle();
  const to = lead ?? trial.mono;
  let askId = null, lastSentAt = 0;
  for (const event of trial.events) {
    const at = Date.now();
    if ("send" in event) messageIds.push(await messageId("messages.send", { message: event.send, send_to: to, wanting_reply: false, intent: "new" }));
    else if ("ask" in event) askId = await messageId("messages.ask", { message: event.ask, send_to: to, due_minutes: event.due_minutes });
    else if ("advance_minutes" in event) await advance(plan.db, event.advance_minutes);
    else if (!("answer" in event) || !askId || !(await query(plan.db, "SELECT 1 AS x FROM mono_ask_events WHERE ask_id=? AND kind='question'", askId)).length) continue;
    else await app("messages.answer", { ask_id: askId, message: event.answer });
    lastSentAt = at;
    await settle();
  }
  if (lead) {
    const [session] = await query(plan.db, "SELECT blocks_json FROM sessions WHERE id=?", lead);
    return { lead, askId, lastSentAt, blocks: JSON.parse(session?.blocks_json ?? "[]") };
  }
  const all = (await query(plan.db, "SELECT block_json FROM mono_blocks WHERE session_id=? ORDER BY seq", trial.session)).map((r) => JSON.parse(r.block_json));
  return { askId, lastSentAt, blocks: all.slice(all.findIndex((b) => b.id === messageIds[0])) };
}

async function askRows(db, tables, actor, askId) {
  const rows = (table, cols, order) => (tables.has(table) ? query(db, `SELECT ${cols} FROM ${table} x JOIN mono_asks a ON a.id=x.ask_id WHERE a.child_actor=? ORDER BY ${order}`, actor) : []);
  return {
    ask: askId && tables.has("mono_asks") ? ((await query(db, "SELECT id, state, due_active_ms FROM mono_asks WHERE id=?", askId))[0] ?? null) : null,
    events: await rows("mono_ask_events", "x.ask_id, x.kind, x.seq, x.reason, x.message_id, x.at_active_ms, x.at_wall_ms", "x.at_wall_ms, x.rowid"),
    outbox: await rows("mono_outbox", 'x."key", x.ask_id, x.stage, x.recipient, x.at_active_ms, x.sent_at', "x.at_active_ms"),
  };
}

const ACTION = [
  [/\bmilestones\.(add|list|update|remove|reorder)\b/, (m) => `milestones.${m[1]}`],
  [/\b(add|list|update|remove|reorder)_milestones?\b/, (m) => `milestones.${m[1]}`],
  [/\bmessages\.(send|reply)\b|\b(send_message|reply)\b/, (m) => `messages.${m[1] ?? (m[2] === "reply" ? "reply" : "send")}`],
];

/** The app action a tool block ran, from the CLI command or the host/MCP tool name. */
export function actionOf(tool, text) {
  const haystack = `${tool.kind ?? ""} ${tool.title ?? ""} ${text ?? ""}`;
  for (const [pattern, name] of ACTION) {
    const m = haystack.match(pattern);
    if (m) return name(m);
  }
  return null;
}

// omp keeps a write's path in preview.path and an edit's in its detail as `[path#tag]`; a shell command is the title.
const toolCallsOf = (blocks) =>
  blocks
    .filter((b) => b.role === "tool" && b.tool)
    .map(({ tool, text }) => ({
      action: actionOf(tool, text),
      title: tool.title ?? tool.kind ?? "",
      ok: /"ok"\s*:\s*true/.test(tool.detail ?? ""),
      kind: tool.kind ?? null,
      path: tool.preview?.path ?? tool.detail?.match(/^\[([^\]#]+)#/)?.[1] ?? null,
    }));

function usage(turns) {
  const t = { prompt: 0, completion: 0, cached: 0, total: 0 };
  let cost = 0;
  let priced = true;
  for (const { turnMetrics: m = {}, turnModel } of turns) {
    const input = m.inputTokens ?? 0, output = m.outputTokens ?? 0, read = m.cacheReadTokens ?? 0, write = m.cacheWriteTokens ?? 0;
    t.prompt += input + read + write;
    t.completion += output;
    t.cached += read;
    const p = PRICES[turnModel?.id];
    if (p) cost += (input * p.input + output * p.output + read * p.cacheRead + write * p.cacheWrite) / 1e6;
    else priced = false;
  }
  t.total = t.prompt + t.completion;
  return { tokenUsage: t, cost: priced ? cost : undefined };
}

export default class MonoProvider {
  id() {
    return "monocode-mono";
  }

  async callApi(_prompt, context) {
    const caseId = context.vars.case_id;
    const index = context.repeatIndex ?? 0;
    try {
      const { manifest } = env();
      let plan;
      try {
        plan = JSON.parse(readFileSync(manifest, "utf8"));
      } catch (e) {
        throw new InfraError(`no trial plan at ${manifest} (evals/run.sh seeds it): ${e.message}`);
      }
      const trial = plan.trials[caseId]?.[index];
      if (!trial) throw new InfraError(`no seeded trial ${caseId}#${index}; seed with a k of at least ${index + 1}`);
      const tables = new Set((await query(plan.db, "SELECT name FROM sqlite_master WHERE type='table'")).map((r) => r.name));
      const runTrial = () => drive(plan, trial, tables);
      const timed = trial.events.some((e) => "ask" in e || "advance_minutes" in e);
      const { lead, askId, lastSentAt, blocks } = await (timed ? (clockChain = clockChain.then(runTrial, runTrial)) : runTrial());
      const [actor, from, id] = lead ? [`session:${lead}`, "from_session_id", lead] : [`mono:${trial.mono}`, "from_mono_id", trial.mono];
      const lastTurn = blocks.slice(blocks.findLastIndex((b) => b.role === "user"));
      // A Mono often answers the sender with send_message or reply; the body
      // is only in mono_messages, not in its tool block.
      const sent = await query(
        plan.db,
        `SELECT id, tool, CASE WHEN to_mono_id IS NULL THEN 'session:' || to_session_id ELSE 'mono:' || to_mono_id END AS "to", status, body, created_at FROM mono_messages WHERE ${from}=? ORDER BY created_at`,
        id,
      );
      const replies = sent.filter((m) => m.status !== "rejected" && m.created_at >= lastSentAt);
      const answer = [...lastTurn.filter((b) => b.role === "assistant" && b.text).map((b) => b.text), ...replies.map((m) => m.body)].join("\n");
      const paths = Object.values(trial.projects);
      const rows = tables.has("milestones")
        ? await query(
            plan.db,
            `SELECT project_path, title, status, created_by FROM milestones WHERE project_path IN (${paths.map(() => "?").join(",")}) ORDER BY project_path, sort_order`,
            ...paths,
          )
        : [];
      const toolCalls = toolCallsOf(blocks);
      const turns = blocks.filter((b) => b.role === "user");
      const asks = await askRows(plan.db, tables, actor, askId);
      const metadata = {
        trial: trial.trial,
        mono: trial.mono,
        agent: trial.agent,
        actor,
        parent: `mono:${SENDER}`,
        harness: plan.harness,
        model: turns.at(-1)?.turnModel?.id ?? plan.model,
        projects: trial.projects,
        toolCalls,
        answer,
        rows,
        ...asks,
        sent,
      };
      const askEvents = asks.events.filter((e) => e.ask_id === askId).map((e) => e.kind);
      return {
        output: JSON.stringify({ actions: toolCalls.map((c) => c.action).filter(Boolean), askEvents, answer: answer.slice(0, 500) }),
        metadata,
        ...usage(turns),
      };
    } catch (e) {
      if (e instanceof InfraError) return { error: `infrastructure: ${e.message}` };
      throw e;
    }
  }
}
