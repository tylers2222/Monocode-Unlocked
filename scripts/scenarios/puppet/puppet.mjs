#!/usr/bin/env node
// A puppet agent for scripts/scenarios/app.sh. Installed as $HOME/.local/bin/fx,
// the first place the app looks for fx. It runs no model. For each prompt it logs the prompt, runs
// the first rule of $HOME/.puppet/script.json whose `match` the prompt
// contains, and ends the turn.
//
// script.json: {"app": "<monocode binary>", "rules": [{"match": "...",
//   "exec": [["sqlite3", "x.db", ".tables"]],
//   "calls": [{"tool": "send_message", "arguments": {...}}], "reply": "..."}]}
// Each call goes through `<app> app-mcp`, the app's MCP server, with the app
// token this process got from the app. log.jsonl gets one line per prompt
// ({"prompt", "rule", "steer"}) and per call ({"tool", "input", "result"}).
// Each exec runs in the agent's folder without a shell and logs {"exec", "code",
// "stdout", "bytes", "stderr"}.
import { spawn, spawnSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

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

async function runTurn(text, steer) {
  const script = JSON.parse(readFileSync(join(dir, "script.json"), "utf8"));
  const rule = script.rules.find((candidate) => text.includes(candidate.match));
  log({ prompt: text, rule: rule?.match ?? null, steer });
  if (!rule) return "";
  for (const [program, ...rest] of rule.exec ?? []) {
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
  const server = mcp(script.app);
  for (const { tool, arguments: input } of rule.calls ?? []) {
    log({ tool, input, result: await server.call(tool, input) });
  }
  server.close();
  return rule.reply ?? "";
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
