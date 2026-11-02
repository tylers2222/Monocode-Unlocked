#!/usr/bin/env node
// A stand-in for the claude and codex CLIs (ADE-167). bin/claude and bin/codex
// start it, so the app takes either one through its CLI path override
// (resolve_harness_binary_override in harness.rs). No model runs.
//
// Replay (bin/claude, bin/codex): prints the fixture that script.json maps to
// the login folder the CLI was started with (CLAUDE_CONFIG_DIR or CODEX_HOME,
// "default" when unset), with its recorded timing divided by `speed`. Each
// recorded input waits for the client's next line of the same kind.
// `--version` prints the recorded CLI's version. log.jsonl gets one line per
// start with the folder.
//   $USAGE_LIMIT_SHIM_DIR (else $HOME/.usage-limit)/script.json:
//   {"speed": 1, "accounts": {"<folder>": "<file in fixtures/>"}}
//
// Redact: node shim.mjs redact <fixture...> applies today's redaction to
// fixtures recorded before it, in place.
//
// Record: node shim.mjs record <claude|codex> <label> <real cli> [folder]
// runs the real CLI with the app's opening messages and the prompt "say ok",
// and writes fixtures/<provider>-<version>-<date>-<label>.jsonl. Record from a
// login that is really out of usage: a refused request is not billed. A CLI
// that exits early still gets its fixture, and record exits with its code.
// Emails, names, ids, tokens, MCP server names, the host and local paths
// become fixed placeholders.
//
// A fixture is one header line ({"provider", "version", "recorded"}), then one
// line per event at `ms` after the start: {"in"} a line the client sent,
// {"out"} a line the CLI printed, {"eof"} the client closed stdin, {"exit"}
// the CLI's exit code.
import { execFileSync, spawn } from "node:child_process";
import {
  appendFileSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir, hostname, tmpdir, userInfo } from "node:os";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), "fixtures");
const PROMPT = "say ok";
const FOLDER_ENV = { claude: "CLAUDE_CONFIG_DIR", codex: "CODEX_HOME" };
const ARGS = {
  claude: [
    "--output-format", "stream-json", "--verbose", "--input-format", "stream-json",
    "--permission-prompt-tool", "stdio", "--include-partial-messages",
    "--no-session-persistence", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
    "--settings", '{"disableAllHooks":true}', "--model", "haiku",
  ],
  codex: ["app-server"],
};

const parse = (line) => {
  try {
    return JSON.parse(line);
  } catch {
    return line;
  }
};

/** What a client line asks for, to pair a live line with a recorded one. */
const kind = (msg) =>
  typeof msg !== "object" || msg === null
    ? "text"
    : (msg.method ??
      (msg.type === "control_request" ? `control_request:${msg.request?.subtype}` : msg.type) ??
      "response");

/** The app's side of one turn: what claude.ts and codex.ts send first. */
const CLIENT = {
  async claude(send, until) {
    send({ type: "control_request", request_id: "monocode_1", request: { subtype: "initialize" } });
    await until((msg) => msg.type === "control_response");
    send({
      type: "user",
      session_id: "",
      parent_tool_use_id: null,
      message: { role: "user", content: [{ type: "text", text: PROMPT }] },
    });
    await until((msg) => msg.type === "result");
  },
  async codex(send, until, cwd) {
    send({
      id: 1,
      method: "initialize",
      params: {
        clientInfo: { name: "monocode", title: "MonoCode", version: "0.1.0" },
        capabilities: { experimentalApi: true },
      },
    });
    await until((msg) => msg.id === 1);
    send({ method: "initialized" });
    send({
      id: 2,
      method: "thread/start",
      params: { cwd, approvalPolicy: "never", sandbox: "read-only", ephemeral: true },
    });
    const opened = await until((msg) => msg.id === 2);
    send({
      id: 3,
      method: "turn/start",
      params: {
        threadId: opened?.result?.thread?.id,
        input: [{ type: "text", text: PROMPT }],
      },
    });
    await until((msg) => msg.method === "turn/completed");
  },
};

/** Fixed placeholders for everything that names a person, a login or a machine. */
function redactor(paths) {
  const numbered = (make) => {
    const seen = new Map();
    return (value) => {
      if (!seen.has(value)) seen.set(value, make(seen.size + 1));
      return seen.get(value);
    };
  };
  const rules = [
    ...paths.map(([from, to]) => [new RegExp(from.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "g"), () => to]),
    [/[\w.+-]+@[\w-]+(?:\.[\w-]+)+/g, () => "user@example.com"],
    [/\b(?:sk-[\w-]{8,}|eyJ[\w-]+\.[\w-]+\.[\w-]+)/g, () => "sk-redacted"],
    [/cf-ray: [\w-]+/g, () => "cf-ray: redacted"],
    [
      /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi,
      numbered((n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`),
    ],
    [/\b(?:req|msg|org|user|acct|resp)_[A-Za-z0-9]{8,}\b/g, numbered((n) => `id_redacted${n}`)],
  ];
  // The login's owner, and its own skills, plugins, agents and commands.
  const PRIVATE = {
    organization: "Example Org",
    ...Object.fromEntries(
      ["commands", "slash_commands", "skills", "agents", "plugins", "mcp_servers", "additional_directories"].map(
        (key) => [key, []],
      ),
    ),
  };
  // MCP servers the login configured, by name, and why each failed to start.
  const server = numbered((n) => `mcp_${n}`);
  const servers = (entry) => {
    if (entry.out?.method !== "mcpServer/startupStatus/updated") return entry;
    const params = { ...entry.out.params, name: server(entry.out.params.name) };
    delete params.error;
    delete params.failureReason;
    return { ...entry, out: { ...entry.out, params } };
  };
  const strip = (value) => {
    if (Array.isArray(value)) return value.map(strip);
    if (typeof value !== "object" || value === null) return value;
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, key in PRIVATE ? PRIVATE[key] : strip(item)]),
    );
  };
  return (entry) =>
    rules.reduce((text, [pattern, to]) => text.replace(pattern, (match) => to(match)), JSON.stringify(strip(servers(entry))));
}

async function record([provider, label, real, folder]) {
  if (!CLIENT[provider] || !label || !real) {
    throw new Error("usage: shim.mjs record <claude|codex> <label> <real cli> [folder]");
  }
  const version = execFileSync(real, ["--version"], { encoding: "utf8" }).trim();
  // The environment harness.rs apply_provider_account gives a login.
  const env = { ...process.env };
  for (const name of [
    FOLDER_ENV[provider], "CLAUDE_SECURESTORAGE_CONFIG_DIR", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN", "OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN",
  ]) delete env[name];
  if (folder) {
    env[FOLDER_ENV[provider]] = folder;
    if (provider === "claude") env.CLAUDE_SECURESTORAGE_CONFIG_DIR = folder;
  }
  const cwd = realpathSync(mkdtempSync(join(tmpdir(), "usage-limit-")));
  const child = spawn(real, ARGS[provider], { cwd, env, stdio: ["pipe", "pipe", "inherit"] });
  const started = Date.now();
  const events = [];
  const note = (event) => events.push({ ms: Date.now() - started, ...event });
  let waiting = () => {};
  let gone = false;
  // "close" comes after the CLI's last stdout line, so the capture is whole.
  const exited = new Promise((resolve) =>
    child.on("close", (code) => {
      gone = true;
      waiting(null);
      resolve(code);
    }),
  );
  child.stdin.on("error", () => {});
  createInterface({ input: child.stdout }).on("line", (line) => {
    const msg = parse(line);
    note({ out: msg });
    waiting(msg);
  });
  const send = (msg) => {
    note({ in: msg });
    child.stdin.write(`${JSON.stringify(msg)}\n`);
  };
  // Rejects once the CLI has exited, which ends the client's script early.
  const until = (done) =>
    new Promise((resolve, reject) => {
      waiting = (msg) => (msg === null ? reject(new Error("exited")) : done(msg) && resolve(msg));
      if (gone) waiting(null);
    });
  const finished = await CLIENT[provider](send, until, cwd).then(
    () => true,
    () => false,
  );
  if (finished) {
    note({ eof: true });
    child.stdin.end();
  }
  const code = await exited;
  note({ exit: code });

  const date = new Date().toISOString().slice(0, 10);
  const number = version.match(/\d+\.\d+\.\d+/)?.[0] ?? "unknown";
  const file = join(FIXTURES, `${provider}-${number}-${date}-${label}.jsonl`);
  const local = (path, to) => {
    const resolved = realpathSync(path);
    // Claude names a project folder after its path with each symbol a dash.
    return [resolved, resolved.replace(/^\/private/, "")].flatMap((form) => [
      [form, to],
      [form.replace(/[^A-Za-z0-9]/g, "-"), to.replace(/[^A-Za-z0-9]/g, "-")],
    ]);
  };
  const redact = redactor([
    ...local(cwd, "/work"),
    ...(folder ? local(folder, "/login") : []),
    [homedir(), "/home/user"],
    [hostname(), "host.local"],
    [userInfo().username, "user"],
  ]);
  const header = { provider, version, recorded: date };
  writeFileSync(file, [header, ...events].map(redact).join("\n") + "\n");
  rmSync(cwd, { recursive: true, force: true });
  console.log(file);
  if (!finished) process.exitCode = code ?? 1;
}

/** The client's stdin. `next(kind)` is its next line of that kind, null once closed. */
function clientLines() {
  const lines = [];
  let wake = () => {};
  let closed = false;
  const input = () => new Promise((resolve) => (wake = resolve));
  createInterface({ input: process.stdin })
    .on("line", (line) => {
      lines.push(parse(line));
      wake();
    })
    .on("close", () => {
      closed = true;
      wake();
    });
  return {
    async next(want) {
      for (;;) {
        const at = lines.findIndex((msg) => kind(msg) === want);
        if (at >= 0) return lines.splice(0, at + 1).pop();
        if (closed) return null;
        await input();
      }
    },
    async closed() {
      while (!closed) await input();
    },
  };
}

async function play(events, speed) {
  const client = clientLines();
  let clock = { at: Date.now(), ms: 0 };
  for (const event of events) {
    if ("in" in event) {
      if ((await client.next(kind(event.in))) === null) process.exit(0);
      clock = { at: Date.now(), ms: event.ms };
    } else if ("out" in event) {
      const wait = clock.at + (event.ms - clock.ms) / speed - Date.now();
      if (wait > 0) await new Promise((resolve) => setTimeout(resolve, wait));
      process.stdout.write(`${typeof event.out === "string" ? event.out : JSON.stringify(event.out)}\n`);
    } else if ("eof" in event) {
      await client.closed();
    } else {
      process.exit(event.exit ?? 1);
    }
  }
}

async function replay(provider, args) {
  const dir = process.env.USAGE_LIMIT_SHIM_DIR ?? join(homedir(), ".usage-limit");
  const script = JSON.parse(readFileSync(join(dir, "script.json"), "utf8"));
  const folder = process.env[FOLDER_ENV[provider]] || "default";
  const version = args[0] === "--version";
  // The app checks `--version` before it picks a login.
  const name =
    script.accounts?.[folder] ??
    (version ? Object.values(script.accounts ?? {}).find((file) => file.startsWith(`${provider}-`)) : undefined);
  const found = name !== undefined && existsSync(join(FIXTURES, name));
  if (!version) {
    const started = { at: Date.now(), provider, folder, fixture: found ? name : null, args };
    appendFileSync(join(dir, "log.jsonl"), `${JSON.stringify(started)}\n`);
  }
  if (!found) {
    console.error(`usage-limit shim: no ${provider} fixture for login folder ${folder}`);
    process.exit(2);
  }
  const [header, ...events] = readFileSync(join(FIXTURES, name), "utf8").trim().split("\n").map(parse);
  if (version) console.log(header.version);
  else await play(events, script.speed ?? 1);
}

const [first, ...rest] = process.argv.slice(2);
if (first === "record") await record(rest);
else if (first === "redact") {
  for (const file of rest) {
    const lines = readFileSync(file, "utf8").trim().split("\n").map(parse);
    writeFileSync(file, lines.map(redactor([])).join("\n") + "\n");
  }
}
else if (FOLDER_ENV[first]) await replay(first, rest);
else {
  console.error("usage: shim.mjs record ... | shim.mjs redact <fixture...> | shim.mjs <claude|codex> <cli args>");
  process.exit(2);
}
