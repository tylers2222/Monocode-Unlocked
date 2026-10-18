// promptfoo provider: one call = one trial of one case against the running
// GigaEval instance. The sender Mono sends each event message to the trial's
// Mono with `messages.send` through the app CLI; the provider waits for that
// turn to end, then reads the Mono's transcript and the milestones rows
// read-only from monocode.db. No UI is driven.
//
// Infrastructure failures (instance down, no sender token, turn timeout) come
// back as `error`, which promptfoo records as ERROR, never as a model failure.
import { execFile } from "node:child_process";
import { readFileSync } from "node:fs";
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

async function send(to, message) {
  const json = JSON.stringify({ message, send_to: to, wanting_reply: false, intent: "new" });
  let stdout;
  try {
    ({ stdout } = await run(CLI, [SENDER, "messages.send", json], { env: { ...process.env, GIGA_EVAL_TARGET: "instance" } }));
  } catch (e) {
    const why = e.code === 3 ? "the sender Mono has no active turn (no app token)" : (e.stderr || e.message).trim().split("\n").pop();
    throw new InfraError(`messages.send failed: ${why}`);
  }
  const reply = JSON.parse(stdout);
  if (!reply.ok || !reply.result?.message_id) throw new InfraError(`messages.send refused: ${stdout.trim()}`);
  return reply.result.message_id;
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

const toolCallsOf = (blocks) =>
  blocks
    .filter((b) => b.role === "tool" && b.tool)
    .map((b) => ({ action: actionOf(b.tool, b.text), title: b.tool.title ?? b.tool.kind ?? "", ok: /"ok"\s*:\s*true/.test(b.tool.detail ?? "") }));

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
      let lastSentAt = 0;
      const messageIds = [];
      for (const message of trial.messages) {
        lastSentAt = Date.now();
        const id = await send(trial.mono, message);
        await waitTurnEnd(plan.db, trial.session, id);
        messageIds.push(id);
      }
      const all = (await query(plan.db, "SELECT block_json FROM mono_blocks WHERE session_id=? ORDER BY seq", trial.session)).map((r) =>
        JSON.parse(r.block_json),
      );
      const blocks = all.slice(all.findIndex((b) => b.id === messageIds[0]));
      const lastTurn = blocks.slice(blocks.findLastIndex((b) => b.role === "user"));
      // A Mono often answers the sender with send_message or reply; the body
      // is only in mono_messages, not in its tool block.
      const sent = await query(
        plan.db,
        "SELECT body FROM mono_messages WHERE from_mono_id=? AND status<>'rejected' AND created_at>=? ORDER BY created_at",
        trial.mono,
        lastSentAt,
      );
      const answer = [...lastTurn.filter((b) => b.role === "assistant" && b.text).map((b) => b.text), ...sent.map((m) => m.body)].join("\n");
      const paths = Object.values(trial.projects);
      const hasTable = (await query(plan.db, "SELECT 1 AS x FROM sqlite_master WHERE name='milestones'")).length > 0;
      const rows = hasTable
        ? await query(
            plan.db,
            `SELECT project_path, title, status, created_by FROM milestones WHERE project_path IN (${paths.map(() => "?").join(",")}) ORDER BY project_path, sort_order`,
            ...paths,
          )
        : [];
      const toolCalls = toolCallsOf(blocks);
      const turns = blocks.filter((b) => b.role === "user");
      const metadata = {
        trial: trial.trial,
        mono: trial.mono,
        harness: plan.harness,
        model: turns.at(-1)?.turnModel?.id ?? plan.model,
        projects: trial.projects,
        toolCalls,
        answer,
        rows,
      };
      return {
        output: JSON.stringify({ actions: toolCalls.map((c) => c.action).filter(Boolean), answer: answer.slice(0, 500) }),
        metadata,
        ...usage(turns),
      };
    } catch (e) {
      if (e instanceof InfraError) return { error: `infrastructure: ${e.message}` };
      throw e;
    }
  }
}
