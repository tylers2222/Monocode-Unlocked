// The app's core/child for vitest (ADE-167): the adapters run as in the app,
// and each spawn starts this folder's bin/claude or bin/codex the way
// harness_spawn does, as a real process. A login other than "default" gets
// the folder harness.rs apply_provider_account would give it, here
// $USAGE_LIMIT_SHIM_DIR/logins/<provider>/<id>.
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const BIN = join(dirname(fileURLToPath(import.meta.url)), "bin");
const FOLDER_ENV = { claude: "CLAUDE_CONFIG_DIR", codex: "CODEX_HOME" };
const children = new Map();
const watchers = new Map();

export const loginFolder = (provider, id) =>
  join(process.env.USAGE_LIMIT_SHIM_DIR, "logins", provider, id);

export const child = {
  resolveClaudeBinary: async () => ({ path: join(BIN, "claude") }),
  resolveCodexBinary: async () => ({ path: join(BIN, "codex") }),
  restoreMonoCodexAgentState: async () => undefined,
  async spawnChild(sessionId, path, args, cwd, account) {
    const env = { ...process.env };
    if (account && account.id !== "default") {
      env[FOLDER_ENV[account.provider]] = loginFolder(account.provider, account.id);
    }
    const proc = spawn(path, args, { cwd, env });
    proc.stdin.on("error", () => {});
    children.set(sessionId, proc);
    const watcher = () => watchers.get(sessionId);
    createInterface({ input: proc.stdout }).on("line", (line) => watcher()?.line(line));
    createInterface({ input: proc.stderr }).on("line", (line) => watcher()?.stderr?.(line));
    proc.on("exit", (code) => {
      children.delete(sessionId);
      watcher()?.exit(code);
    });
    return proc.pid;
  },
  watchChild(sessionId, line, exit, stderr) {
    watchers.set(sessionId, { line, exit, stderr });
  },
  unwatchChild(sessionId) {
    watchers.delete(sessionId);
  },
  async writeChild(sessionId, line) {
    children.get(sessionId)?.stdin.write(`${line}\n`);
  },
  async killChild(sessionId) {
    watchers.delete(sessionId);
    children.get(sessionId)?.kill();
  },
};
