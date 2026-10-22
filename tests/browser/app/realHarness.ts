import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

/** Login dirs a harness reads from its env, besides `HOME`. */
const LOGIN_VARS = [
  "PI_CODING_AGENT_DIR",
  "CLAUDE_CONFIG_DIR",
  "CODEX_HOME",
  "CLAUDE_SECURESTORAGE_CONFIG_DIR",
];

/**
 * `os.homedir()`, the home a harness falls back to, reads `HOME` on Unix and
 * `USERPROFILE` on Windows. On Windows the child gets the parent's
 * `USERPROFILE` (libuv copies it) unless the env sets one.
 */
const HOME_VAR = process.platform === "win32" ? "USERPROFILE" : "HOME";

/**
 * Each dir the harness would read a login from, resolved as the harness does:
 * by text against the child's `cwd` (`path.resolve`, as omp does). An unset
 * home var means the parent's home. omp keeps its config at the home joined
 * with `PI_CONFIG_DIR` (default `.omp`).
 */
function loginDirs(env: NodeJS.ProcessEnv, cwd: string): [string, string][] {
  const at = (value: string | undefined) => (value ? resolve(cwd, value) : homedir());
  const home = at(env[HOME_VAR]);
  const profile: [string, string][] = [];
  return [
    ["HOME", at(env.HOME)],
    ...profile,
    ["PI_CONFIG_DIR", join(home, env.PI_CONFIG_DIR || ".omp")],
    ...LOGIN_VARS.flatMap((name): [string, string][] => {
      const dir = env[name];
      return dir ? [[name, resolve(cwd, dir)]] : [];
    }),
  ];
}

const within = (path: string, root: string) => {
  const rest = relative(root, path);
  return rest !== ".." && !rest.startsWith(`..${sep}`) && !isAbsolute(rest);
};

/**
 * True when `path` is inside `ownDir` by text, and its nearest existing
 * ancestor (itself when it exists) is inside it once links are followed.
 * A path that does not exist yet is judged by where it would be made.
 */
function inside(path: string, ownDir: string): boolean {
  const own = realpathSync(ownDir);
  if (!within(path, resolve(ownDir)) && !within(path, own)) return false;
  let existing = path;
  while (!existsSync(existing)) existing = dirname(existing);
  return within(realpathSync(existing), own);
}

/**
 * Spawns a real harness for a test rig. The child's env is built here, not
 * taken from the caller: `PATH`, then `HOME` (and `USERPROFILE` on Windows)
 * and the harness login dirs inside `ownDir` (the test's temp dir), then
 * `env`, which holds only what the rig adds. Before anything starts, it
 * refuses when any dir the harness would read a login from resolves outside
 * `ownDir`.
 */
export function spawnRealHarness(
  command: string,
  args: string[],
  { cwd, ownDir, env = {} }: { cwd: string; ownDir: string; env?: NodeJS.ProcessEnv },
): ChildProcess {
  const childEnv: NodeJS.ProcessEnv = {
    PATH: process.env.PATH,
    HOME: ownDir,
    PI_CODING_AGENT_DIR: join(ownDir, ".omp", "agent"),
    CLAUDE_CONFIG_DIR: join(ownDir, ".claude"),
    CODEX_HOME: join(ownDir, ".codex"),
    ...env,
  };
  const outside = loginDirs(childEnv, cwd)
    .filter(([, dir]) => !inside(dir, ownDir))
    .map(([name, dir]) => `${name}=${dir}`);
  if (outside.length)
    throw new Error(
      `Refusing to start ${command}. It would read a login outside the test's temp dir: ` +
        `${outside.join(", ")}. A test rig keeps the harness's login inside ${ownDir}.`,
    );
  return spawn(command, args, { cwd, env: childEnv });
}
