import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type Handler, test } from "../app/fixture";

// ADE-189: every harness helper goes through Rust's spawn hook, which picks
// its folder. A helper of no session (a catalog probe) runs in a neutral
// folder that is no project checkout, and the window must tell its agent that
// folder, not the one it asked for (parts 1-2). A helper of a session (the
// `/` picker's skills probe) names that session as its owner, so the hook
// places it in the session's Space (part 3).

const HOME = "/Users/test";
const PROJECT = `${HOME}/acme`;
/** Where spaces/mod.rs `prepare_spawn` starts a helper of no session. */
const NEUTRAL = "/app-cache/helper-cwd";

const session = (
  id: string,
  harness: string,
  model: string,
  title: string,
): SessionSummary => ({
  id,
  cwd: PROJECT,
  harness,
  model,
  runtimeMode: "supervised",
  title,
  createdAt: 1,
  updatedAt: 1,
});

type Rpc = { id?: number | string; method?: string; params?: unknown };

declare global {
  interface Window {
    /** Each ACP request a Cursor child got: its child id, method and params. */
    acpRequests?: { child: string; method: string; params: unknown }[];
  }
}

/**
 * Cursor's ACP agent. A catalog probe child is answered as a CLI with no
 * models listed, so the probe opens a session to read them; Rust starts
 * every probe in the neutral folder.
 */
const cursor: Record<string, Handler> = {
  harness_resolve_cursor: () => ({ path: "/usr/local/bin/cursor-agent" }),
  harness_spawn: (_store, { sessionId, cwd }) =>
    window.appBackend!.spawned(
      4243,
      String(sessionId).startsWith("monocode-cursor-probe")
        ? "/app-cache/helper-cwd"
        : cwd,
    ),
  harness_exec: (_store, { cwd }) => window.appBackend!.ran("", cwd),
  harness_write: (_store, { sessionId, line }) => {
    const rpc = JSON.parse(line as string) as Rpc;
    if (rpc.id === undefined || !rpc.method) return null;
    (window.acpRequests ??= []).push({
      child: sessionId as string,
      method: rpc.method,
      params: rpc.params,
    });
    const results: Record<string, unknown> = {
      initialize: { protocolVersion: 1, agentCapabilities: {} },
      "cursor/list_available_models": { models: [] },
      "session/new": {
        sessionId: "probe-session",
        models: { availableModels: [{ modelId: "auto", name: "Auto" }] },
      },
    };
    setTimeout(() =>
      window.appBackend!.emit("harness-stdout", {
        sessionId,
        line: JSON.stringify({
          jsonrpc: "2.0",
          id: rpc.id,
          result: results[rpc.method!] ?? {},
        }),
      }),
    );
    return null;
  },
};

/** Pi in RPC mode answers every request at once, with no commands. */
const pi: Record<string, Handler> = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as { id?: string; type?: string };
    if (request.id)
      void window.appBackend!.emit("harness-stdout", {
        sessionId,
        line: JSON.stringify({
          type: "response",
          id: request.id,
          command: request.type,
          success: true,
          data: { commands: [] },
        }),
      });
    return null;
  },
};

function spawns(page: Page, prefix: string) {
  return page.evaluate(
    (prefix) =>
      window
        .appBackend!.calls.filter(
          ({ command, args }) =>
            command === "harness_spawn" &&
            String((args as { sessionId: string }).sessionId).startsWith(prefix),
        )
        .map(({ args }) => args as Record<string, unknown>),
    prefix,
  );
}

async function open(app: AppShell, page: Page, seed: SessionSummary, handlers: Record<string, Handler>) {
  await app.mount({ projects: [PROJECT], sessions: [seed] }, { handlers });
  await page.getByText(seed.title).click();
}

test("a catalog probe of no session tells its agent the neutral folder Rust started it in", async ({
  app,
  page,
}) => {
  await open(app, page, session("c-1", "cursor", "cursor:auto", "Cursor work"), cursor);
  await expect
    .poll(
      () =>
        page.evaluate(() =>
          (window.acpRequests ?? [])
            .filter(
              ({ child, method }) =>
                child.startsWith("monocode-cursor-probe") &&
                method === "session/new",
            )
            .map(({ params }) => (params as { cwd: string }).cwd),
        ),
      "the probe's session/new names the folder Rust placed it in",
    )
    .toContainEqual(NEUTRAL);
  const told = await page.evaluate(() =>
    (window.acpRequests ?? [])
      .filter(({ method }) => method === "session/new")
      .filter(({ child }) => child.startsWith("monocode-cursor-probe"))
      .map(({ params }) => (params as { cwd: string }).cwd),
  );
  expect(told.every((cwd) => cwd === NEUTRAL), JSON.stringify(told)).toBe(true);
  const [probe] = await spawns(page, "monocode-cursor-probe");
  expect(probe.cwd, "the probe asked for the home folder").toBe(HOME);
  expect(probe, "the probe is a helper of no session").toMatchObject({
    helper: {},
  });
  expect(
    (probe.helper as { owner?: string }).owner,
    "the probe names no owner",
  ).toBeUndefined();
});

test("a session's skills helper names the session it works for", async ({
  app,
  page,
}) => {
  await open(
    app,
    page,
    session("p-1", "pi", "pi:anthropic/claude-sonnet-4-5", "Pi work"),
    pi,
  );
  await expect
    .poll(
      async () =>
        (await spawns(page, "monocode-pi-skills-")).map(
          (args) => (args.helper as { owner?: string } | undefined)?.owner,
        ),
      "every skills probe of the session's composer names it as owner",
    )
    .toContainEqual("p-1");
  const owners = (await spawns(page, "monocode-pi-skills-")).map(
    (args) => (args.helper as { owner?: string } | undefined)?.owner,
  );
  expect(owners.every((owner) => owner === "p-1"), JSON.stringify(owners)).toBe(
    true,
  );
});
