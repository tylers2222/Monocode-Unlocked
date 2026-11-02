// The app's core/child for vitest (ADE-167): the adapters run as in the app,
// and each spawn starts the shim as a real process, as bin/claude or
// bin/codex would. Node runs it directly: Windows cannot run those scripts.
// A login other than "default" gets the folder harness.rs
// apply_provider_account would give it, here
// $USAGE_LIMIT_SHIM_DIR/logins/<provider>/<id>.
import { spawn } from "node:child_process";
import { once } from "node:events";
import { basename, dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const BIN = join(HERE, "bin");
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
    const shim = [join(HERE, "shim.mjs"), basename(path), ...args];
    const proc = spawn(process.execPath, shim, { cwd, env });
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
  // Resolves once the shim has exited: Windows cannot remove a folder that a
  // live process has as its cwd.
  async killChild(sessionId) {
    watchers.delete(sessionId);
    const proc = children.get(sessionId);
    if (!proc) return;
    const exited = once(proc, "exit");
    proc.kill();
    await exited;
  },
};
