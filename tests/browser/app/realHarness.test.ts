import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { spawnRealHarness } from "./realHarness";

const made: string[] = [];
const tempDir = () => {
  const dir = mkdtempSync(join(tmpdir(), "real-harness-"));
  made.push(dir);
  return dir;
};
afterEach(() => {
  for (const dir of made.splice(0)) rmSync(dir, { recursive: true, force: true });
});

/** Runs `touch <marker>` through the guard; the marker shows it started. */
function start(env: NodeJS.ProcessEnv, ownDir: string) {
  const marker = join(ownDir, "started");
  return {
    marker,
    child: () =>
      spawnRealHarness("touch", [marker], {
        cwd: ownDir,
        env: { PATH: process.env.PATH, ...env },
        ownDir,
      }),
  };
}

describe("spawnRealHarness", () => {
  it.each(["PI_CODING_AGENT_DIR", "CLAUDE_CONFIG_DIR", "CODEX_HOME"])(
    "refuses %s set to a real login dir, before the harness starts",
    (name) => {
      const login = tempDir();
      const own = tempDir();
      const { marker, child } = start({ [name]: login }, own);
      expect(child).toThrow(`real login dir: ${name}=${login}.`);
      expect(existsSync(marker)).toBe(false);
    },
  );

  it("starts the harness with its login dirs inside the test's temp dir", async () => {
    const own = tempDir();
    mkdirSync(join(own, "agent"));
    // tmpdir() is /var/... on macOS, which resolves to /private/var/....
    const { marker, child } = start(
      {
        PI_CODING_AGENT_DIR: join(own, "agent"),
        CLAUDE_CONFIG_DIR: own,
        CODEX_HOME: join(own, "missing"),
      },
      own,
    );
    await once(child(), "exit");
    expect(existsSync(marker)).toBe(true);
  });

  it("refuses a login dir beside the temp dir that shares its name prefix", () => {
    const own = tempDir();
    const sibling = `${own}-login`;
    mkdirSync(sibling);
    made.push(sibling);
    expect(start({ CODEX_HOME: sibling }, own).child).toThrow(
      `real login dir: CODEX_HOME=${sibling}.`,
    );
  });
});
