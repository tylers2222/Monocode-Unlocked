#!/usr/bin/env node
// A puppet agent for scripts/scenarios/app.sh. Installed as $HOME/.local/bin/fx,
// the first place the app looks for fx. It runs no model. For each prompt it logs the prompt, runs
// the first rule of $HOME/.puppet/script.json whose `match` the prompt
// contains, and ends the turn.
//
// script.json: {"app": "<monocode binary>", "whoami": false, "rules": [{"match": "...",
//   "exec": [["sqlite3", "x.db", ".tables"]],
//   "calls": [{"tool": "send_message", "arguments": {...}}], "reply": "..."}]}
// Each tool call goes through `<app> app-mcp`, the app's MCP server, and each
// `{"action": "sessions.start", "arguments": {...}}` call through the CLI
// `<app> app <action>`, both with the app token this process got from the
// app. A string argument "$card:<heading>" is the id of the card that an
// earlier logged call filed or updated under that heading, "$prev.<field>" is
// that field of the previous call's answer, "$<name>" is attribute <name>
// of the prompt's first <mono_message>, and "$<name>@<text>" is attribute
// <name> of the <mono_message> whose body starts with <text> (a turn can take
// several messages, in either order). log.jsonl gets
// one line per prompt ({"prompt", "rule", "steer"}, plus "who", the Mono
// that get_monos marks `you`, when `whoami` is set) and per call ({"tool"
// or "action", "input", "result"}).
// Each exec runs in the agent's folder without a shell and logs {"exec", "code",
// "stdout", "bytes", "stderr"}.
import { execFile, spawn, spawnSync } from "node:child_process";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { promisify } from "node:util";

const dir = join(homedir(), ".puppet");
const args = process.argv.slice(2);

if (args[0] === "--version") {
  console.log("fx 0.0.1 (puppet)");
  process.exit(0);
}
// harness.rs `is_fx_agent` takes a file named fx whose `--help` names `acp`
// with `ask` (`fx_help_mentions_acp`); the app then runs it as `fx acp`.
if (args[0] === "--help") {
  console.log("fx acp: ask over the Agent Client Protocol (puppet)");
  process.exit(0);
}
if (args[0] !== "acp") process.exit(1);
// While $HOME/.puppet/hold-start exists, a new agent reads no request, so the
// app's prompt stays unread (app.sh r9 kills the app in that gap).
while (existsSync(join(dir, "hold-start"))) spawnSync("sleep", ["0.2"]);

const log = (entry) =>
  appendFileSync(
    join(dir, "log.jsonl"),
    `${JSON.stringify({ at: Date.now(), pid: process.pid, cwd: process.cwd(), ...entry })}\n`,
  );
const send = (message) =>
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);

/** One MCP session on `<app> app-mcp`; `call` sends one tools/call. */
function mcp(app) {
  const child = spawn(app, ["app-mcp"], { stdio: ["pipe", "pipe", "inherit"] });
  const waiting = new Map();
  let next = 1;
  createInterface({ input: child.stdout }).on("line", (line) => {
    const reply = JSON.parse(line);
    waiting.get(reply.id)?.(reply);
    waiting.delete(reply.id);
  });
  const request = (method, params) =>
    new Promise((resolve) => {
      const id = next++;
      waiting.set(id, resolve);
      child.stdin.write(
        `${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`,
      );
    });
  const ready = request("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "puppet", version: "0.0.1" },
  });
  return {
    call: async (name, args) => {
      await ready;
      const reply = await request("tools/call", { name, arguments: args });
      return reply.result?.structuredContent ?? reply.error;
    },
    close: () => child.stdin.end(),
  };
}

/** What a call answered: the app's `{ok, result}` envelope opened. */
const payload = (result) => result?.result ?? result;

/** Each card id an earlier call answered, by the card's heading. */
function cardIds() {
  const path = join(dir, "log.jsonl");
  const ids = new Map();
  if (!existsSync(path)) return ids;
  for (const line of readFileSync(path, "utf8").split("\n")) {
    const card = line ? payload(JSON.parse(line).result) : undefined;
    if (card?.id && card.heading) ids.set(card.heading, card.id);
  }
  return ids;
}

function withCardIds(input) {
  const ids = cardIds();
  return JSON.parse(JSON.stringify(input ?? {}), (_key, value) =>
    typeof value === "string" && value.startsWith("$card:")
      ? (ids.get(value.slice(6)) ?? value)
      : value,
  );
}

function fill(input, text, prev) {
  const escape = (raw) => raw.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const attribute = (name, body) =>
    text.match(
      new RegExp(
        `<mono_message [^>]*\\b${name}="([^"]*)"${body === undefined ? "" : `[^>]*>\\n${escape(body)}`}`,
      ),
    )?.[1];
  const value = (raw) => {
    if (typeof raw !== "string" || !raw.startsWith("$") || raw.startsWith("$card:")) return raw;
    if (raw.startsWith("$prev.")) return payload(prev)?.[raw.slice(6)];
    const at = raw.indexOf("@");
    return at < 0 ? attribute(raw.slice(1)) : attribute(raw.slice(1, at), raw.slice(at + 1));
  };
  return Object.fromEntries(
    Object.entries(input).map(([key, raw]) => [key, value(raw)]),
  );
}

/** The mono_id that get_monos marks as this caller, or null. */
function you(value) {
  if (Array.isArray(value)) return value.map(you).find(Boolean) ?? null;
  if (value && typeof value === "object")
    return value.you === true ? value.mono_id : you(Object.values(value));
  return null;
}

async function cli(app, action, input, requestId) {
  try {
    const { stdout } = await promisify(execFile)(app, [
      "app", action, "--json", JSON.stringify(input),
      ...(requestId ? ["--request-id", requestId] : []),
    ]);
    return JSON.parse(stdout);
  } catch (error) {
    return error.stdout ? JSON.parse(error.stdout) : { ok: false, error: String(error) };
  }
}

function runExecs(execs) {
  for (const [program, ...rest] of execs) {
    const run = spawnSync(program, rest, { encoding: "latin1" });
    const stdout = run.stdout ?? "";
    log({
      exec: [program, ...rest],
      code: run.status,
      stdout: stdout.slice(0, 400),
      bytes: stdout.length,
      stderr: (run.stderr ?? String(run.error)).slice(0, 400),
    });
  }
}

async function runTurn(text, steer) {
  const script = JSON.parse(readFileSync(join(dir, "script.json"), "utf8"));
  const rule = script.rules.find((candidate) => text.includes(candidate.match));
  if (!rule && !script.whoami) {
    log({ prompt: text, rule: null, steer });
    return "";
  }
  const server = mcp(script.app);
  const who = script.whoami ? { who: you(await server.call("get_monos", {})) } : {};
  log({ prompt: text, rule: rule?.match ?? null, steer, ...who });
  runExecs(rule?.exec ?? []);
  let prev;
  for (const call of rule?.calls ?? []) {
    const input = fill(withCardIds(call.arguments), text, prev);
    prev = call.action
      ? await cli(script.app, call.action, input, call.request_id)
      : await server.call(call.tool, input);
    log({ ...(call.action ? { action: call.action } : { tool: call.tool }), input, result: prev });
  }
  server.close();
  return rule?.reply ?? "";
}

let turns = 0;
const handlers = {
  initialize: () => ({ protocolVersion: 1, agentCapabilities: {} }),
  "session/new": () => ({ sessionId: `puppet-${process.pid}-${Date.now()}` }),
  "session/set_config_option": () => ({}),
  "session/set_mode": () => ({}),
  "session/prompt": async ({ sessionId, prompt }) => {
    const text = prompt.map((block) => block.text ?? "").join("\n");
    // fx cannot be steered, so the app queues; a prompt during a turn would
    // show the app steering it anyway.
    const steer = turns > 0;
    turns += 1;
    try {
      const reply = await runTurn(text, steer);
      send({
        method: "session/update",
        params: {
          sessionId,
          update: {
            sessionUpdate: "agent_message_chunk",
            content: { type: "text", text: reply },
          },
        },
      });
      return { stopReason: "end_turn" };
    } finally {
      turns -= 1;
    }
  },
};

createInterface({ input: process.stdin }).on("line", async (line) => {
  const { id, method, params } = JSON.parse(line);
  const handler = handlers[method];
  if (id === undefined) return;
  if (!handler) {
    send({
      id,
      error: { code: -32601, message: `Method not found: ${method}` },
    });
    return;
  }
  try {
    send({ id, result: await handler(params) });
  } catch (error) {
    send({ id, error: { code: -32603, message: String(error) } });
  }
});
