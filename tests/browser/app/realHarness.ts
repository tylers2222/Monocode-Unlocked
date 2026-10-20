import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { isAbsolute, relative, sep } from "node:path";

/**
 * Env vars that point a harness at a login: its credentials, sessions and
 * settings. A rig whose harness reads a real one can run real models and
 * write into the user's sessions.
 */
const LOGIN_DIR_VARS = ["PI_CODING_AGENT_DIR", "CLAUDE_CONFIG_DIR", "CODEX_HOME"];

function isInside(dir: string, ownDir: string): boolean {
  const path = relative(realpathSync(ownDir), realpathSync(dir));
  return path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path);
}

/**
 * Spawns a real harness for a test rig. It refuses, before anything starts,
 * when `env` would give the harness an existing login dir outside `ownDir`,
 * the test's temp dir, where the rig keeps the harness's own login.
 */
export function spawnRealHarness(
  command: string,
  args: string[],
  { cwd, env, ownDir }: { cwd: string; env: NodeJS.ProcessEnv; ownDir: string },
): ChildProcess {
  const real = LOGIN_DIR_VARS.flatMap((name) => {
    const dir = env[name];
    return dir && existsSync(dir) && !isInside(dir, ownDir) ? [`${name}=${dir}`] : [];
  });
  if (real.length)
    throw new Error(
      `Refusing to start ${command}. These env vars name a real login dir: ` +
        `${real.join(", ")}. A test rig gives the harness its own login dir inside ${ownDir}.`,
    );
  return spawn(command, args, { cwd, env });
}
