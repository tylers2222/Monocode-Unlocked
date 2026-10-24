import { once } from "node:events";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { spawnRealHarness } from "./realHarness";

const made: string[] = [];
const tempDir = () => {
  const dir = mkdtempSync(join(tmpdir(), "real-harness-"));
  made.push(dir);
  return dir;
};
afterEach(() => {
  for (const dir of made.splice(0)) rmSync(dir, { recursive: true, force: true });
  vi.unstubAllEnvs();
});

/** A dir outside the test's own that holds a harness login, like ~/.omp-2. */
function realLogin() {
  const dir = tempDir();
  mkdirSync(join(dir, ".omp", "agent"), { recursive: true });
  return dir;
}

/** Starts `node`, which writes the env it got to a file; reads that file back. */
async function startAndRead(ownDir: string, cwd: string, env?: NodeJS.ProcessEnv) {
  const out = join(ownDir, "seen.json");
  const script = `require("fs").writeFileSync(process.argv[1], JSON.stringify(process.env))`;
  const child = spawnRealHarness(process.execPath, ["-e", script, out], { cwd, ownDir, env });
  await once(child, "exit");
  return JSON.parse(readFileSync(out, "utf8")) as Record<string, string>;
}

/** Throws when the guard refuses; `marker` exists only if the harness started. */
function tryStart(ownDir: string, env: NodeJS.ProcessEnv, cwd = ownDir) {
  const marker = join(ownDir, "started");
  return {
    marker,
    start: () => spawnRealHarness("touch", [marker], { cwd, ownDir, env }),
  };
}

/**
 * Keys the OS or Node's process layer (libuv) puts in every child's env,
 * whatever env the helper passes. macOS adds its text encoding. On Windows,
 * libuv copies these from the parent when the given env lacks them.
 * USERPROFILE is not here: it is the Windows home, so the helper sets it.
 */
const OS_ADDED: Partial<Record<NodeJS.Platform, string[]>> = {
  darwin: ["__CF_USER_TEXT_ENCODING"],
  win32: [
    "HOMEDRIVE",
    "HOMEPATH",
    "LOGONSERVER",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "USERDOMAIN",
    "USERNAME",
    "WINDIR",
  ],
};

describe("spawnRealHarness", () => {
  it("builds the harness env itself: PATH, and HOME and login dirs in the temp dir, nothing else from the parent", async () => {
    // The agent shell that runs these tests holds a live app token like this.
    vi.stubEnv("MONOCODE_APP_TOKEN", "sentinel-from-the-test-process");
    const own = tempDir();
    const seen = await startAndRead(own, own);
    expect(seen.MONOCODE_APP_TOKEN, "the parent's env does not reach the harness").toBeUndefined();
    for (const key of OS_ADDED[process.platform] ?? []) delete seen[key];
    expect(seen).toEqual({
      PATH: process.env.PATH,
      HOME: own,
      ...(process.platform === "win32" ? { USERPROFILE: own } : {}),
      PI_CODING_AGENT_DIR: join(own, ".omp", "agent"),
      CLAUDE_CONFIG_DIR: join(own, ".claude"),
      CODEX_HOME: join(own, ".codex"),
    });
  });

  it("refuses unset login vars when the home holds a real login", () => {
    const own = tempDir();
    const home = realLogin();
    // The home the harness falls back to: USERPROFILE on Windows, HOME elsewhere.
    const { marker, start } = tryStart(own, {
      [process.platform === "win32" ? "USERPROFILE" : "HOME"]: home,
      PI_CODING_AGENT_DIR: undefined,
      CLAUDE_CONFIG_DIR: undefined,
      CODEX_HOME: undefined,
    });
    expect(start).toThrow(`PI_CONFIG_DIR=${join(home, ".omp")}`);
    expect(existsSync(marker)).toBe(false);
  });

  it.runIf(process.platform === "win32")(
    "refuses a rig that unsets USERPROFILE on Windows, where it is the home",
    () => {
      const own = tempDir();
      const { marker, start } = tryStart(own, { USERPROFILE: undefined });
      expect(start).toThrow(`USERPROFILE=${homedir()}`);
      expect(existsSync(marker)).toBe(false);
    },
  );

  it("refuses a real HOME even with every login var inside the temp dir", () => {
    const own = tempDir();
    const home = realLogin();
    expect(tryStart(own, { HOME: home }).start).toThrow(`HOME=${home}`);
  });

  it.each([
    "PI_CODING_AGENT_DIR",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
  ])("refuses %s set to a real login dir, before the harness starts", (name) => {
    const own = tempDir();
    const login = realLogin();
    const { marker, start } = tryStart(own, { [name]: login });
    expect(start).toThrow(`${name}=${login}`);
    expect(existsSync(marker)).toBe(false);
  });

  it("refuses a PI_CONFIG_DIR that climbs out of HOME", () => {
    const own = tempDir();
    const login = realLogin();
    expect(tryStart(own, { PI_CONFIG_DIR: `../${basename(login)}` }).start).toThrow(
      `PI_CONFIG_DIR=${login}`,
    );
  });

  it("resolves .. by text, the way the harness does, through a link like /var", () => {
    const own = tempDir();
    const login = realLogin();
    // own/a/link -> own/b/c/d, as /var -> /private/var. By text,
    // own/a/link/../../.. is own's parent, so the path names `login`. Through
    // the link it names own/<login's name>, which does not exist.
    mkdirSync(join(own, "a"));
    mkdirSync(join(own, "b", "c", "d"), { recursive: true });
    symlinkSync(join(own, "b", "c", "d"), join(own, "a", "link"));
    const dir = join(own, "a", "link") + `/../../../${basename(login)}`;
    expect(tryStart(own, { PI_CODING_AGENT_DIR: dir }).start).toThrow(
      `PI_CODING_AGENT_DIR=${login}`,
    );
  });

  it("resolves a relative path against the harness's cwd, not the test's", async () => {
    const own = tempDir();
    mkdirSync(join(own, "work"));
    const seen = await startAndRead(own, join(own, "work"), {
      PI_CODING_AGENT_DIR: "../agent",
    });
    expect(seen.PI_CODING_AGENT_DIR).toBe("../agent");

    const login = realLogin();
    expect(
      tryStart(own, { PI_CODING_AGENT_DIR: ".omp/agent" }, login).start,
    ).toThrow(`PI_CODING_AGENT_DIR=${join(login, ".omp", "agent")}`);
  });

  it("refuses a login dir outside the temp dir that does not exist yet", () => {
    const own = tempDir();
    const missing = join(realLogin(), "nope", "agent");
    expect(tryStart(own, { PI_CODING_AGENT_DIR: missing }).start).toThrow(
      `PI_CODING_AGENT_DIR=${missing}`,
    );
  });

  it("refuses a link inside the temp dir that leads to a real login", () => {
    const own = tempDir();
    const login = realLogin();
    symlinkSync(login, join(own, "linked"));
    expect(
      tryStart(own, { CODEX_HOME: join(own, "linked", "not-yet") }).start,
    ).toThrow(`CODEX_HOME=${join(own, "linked", "not-yet")}`);
  });

  it("refuses a dir beside the temp dir that shares its name prefix", () => {
    const own = tempDir();
    const sibling = `${own}-login`;
    mkdirSync(sibling);
    made.push(sibling);
    expect(tryStart(own, { CODEX_HOME: sibling }).start).toThrow(
      `CODEX_HOME=${sibling}`,
    );
  });
});
