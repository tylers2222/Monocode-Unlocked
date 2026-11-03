import type { Page } from "@playwright/test";
import type { SessionSummary } from "../../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type Handler, test } from "../app/fixture";

// ADE-175: the app CLI shows a caller only its own Space. Pip is a regular
// Mono in Space 2 whose roster entry still names a Space 1 project (acme).
// Rust's level check answers Pip's Space scope (spaces/scope.rs): Space 2's
// projects and Monos, Space 1's regular Mono Sol marked with its Space's
// name, Space 1's Giga not at all, and the saved sessions of Space 1, one of
// them in Space 2's project folder. The window must list only what that
// scope allows.

const HOME = "/Users/test";
const ACME = `${HOME}/acme`;
const WIDGETS = `${HOME}/widgets`;

const session = (
  id: string,
  cwd: string,
  title: string,
): SessionSummary => ({
  id,
  cwd,
  harness: "pi",
  model: "pi:anthropic/claude-sonnet-4-5",
  runtimeMode: "supervised",
  title,
  createdAt: 1,
  updatedAt: 1,
});

const mono = (
  id: string,
  name: string,
  projects: string[],
  role: "mono" | "giga" = "mono",
) => ({
  id,
  role,
  data: { name, mascot: "ghost", sessionId: `${id}-chat`, projects },
});

type ControlResponse =
  | { ok: true; result: Record<string, unknown> }
  | { ok: false; error?: string; code?: string; hint?: string };

declare global {
  interface Window {
    controlReplies?: { id: string; response: ControlResponse }[];
  }
}

const handlers: Record<string, Handler> = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
  control_owns_session: () => true,
  // mono_messages.rs `mono_messages_unqueued`: no message waits for a chat.
  mono_messages_unqueued: () => [],
  control_reply: (_store, { id, response }) => {
    (window.controlReplies ??= []).push({
      id: id as string,
      response: response as ControlResponse,
    });
    return null;
  },
  // mono_gate.rs `check_action` for Pip (Space 2), with scope.rs's answer.
  mono_app_action_check: () => ({
    outcome: "allowed",
    leadSessionId: null,
    ask: null,
    scope: {
      space: "space-2",
      projects: ["/Users/test/widgets"],
      monos: [
        { monoId: "pip", otherSpace: null, projects: ["/Users/test/widgets"] },
        { monoId: "ray", otherSpace: null, projects: ["/Users/test/widgets"] },
        { monoId: "sol", otherSpace: "Space 1", projects: [] },
      ],
      foreignSessions: ["sol-widgets", "sol-acme"],
    },
  }),
};

let calls = 0;

/** One `monocode app` call from Pip's chat turn, as control.rs delivers it. */
async function fromPip(
  app: AppShell,
  page: Page,
  action: string,
  input: Record<string, unknown>,
): Promise<ControlResponse> {
  const id = `control-${++calls}`;
  await app.emit("monocode-control-request", {
    id,
    namespace: "app",
    sessionId: "pip-chat",
    requestId: `r${calls}`,
    action,
    input,
    turnId: "turn-1",
  });
  await expect
    .poll(() =>
      page.evaluate(
        (id) => window.controlReplies?.some((reply) => reply.id === id),
        id,
      ),
    )
    .toBe(true);
  return page.evaluate(
    (id) => window.controlReplies!.find((reply) => reply.id === id)!.response,
    id,
  );
}

test("a Space 2 Mono's monos.list, get_monos and sessions.list show only what its Space allows", async ({
  app,
  page,
}) => {
  await app.mount(
    {
      projects: [ACME, WIDGETS],
      monos: [
        mono("pip", "Pip", [ACME, WIDGETS]),
        mono("ray", "Ray", [WIDGETS]),
        mono("sol", "Sol", [ACME]),
        mono("atlas", "Atlas", [], "giga"),
      ],
      sessions: [
        session("pip-chat", HOME, "Pip"),
        session("ray-chat", HOME, "Ray"),
        session("sol-chat", HOME, "Sol"),
        session("atlas-chat", HOME, "Atlas"),
        session("pip-widgets", WIDGETS, "Widgets in Space 2"),
        session("sol-widgets", WIDGETS, "Widgets in Space 1"),
        session("sol-acme", ACME, "Acme in Space 1"),
      ],
    },
    { handlers },
  );

  // The app CLI answers a chat the window has open (agentApp.ts appCliSource).
  await page.getByRole("button", { name: /^Pip, / }).click();
  await expect(page.getByPlaceholder("Message Pip")).toBeVisible();

  const listed = await fromPip(app, page, "monos.list", {});
  expect(listed.ok, JSON.stringify(listed)).toBe(true);
  const monos = (
    listed as { result: { monos: { id: string; projects: string[] }[] } }
  ).result.monos;
  expect(
    monos.map(({ id, projects }) => [id, projects]),
    "monos.list: Space 2's Monos with their Space 2 projects only",
  ).toEqual([
    ["pip", [WIDGETS]],
    ["ray", [WIDGETS]],
  ]);

  const reachable = await fromPip(app, page, "messages.get_monos", {});
  expect(reachable.ok, JSON.stringify(reachable)).toBe(true);
  const entries = (
    reachable as {
      result: { monos: { mono_id: string; other_space?: string }[] };
    }
  ).result.monos;
  expect(
    entries.map(({ mono_id, other_space }) => [mono_id, other_space ?? null]),
    "get_monos: Space 1's regular Mono marked with its Space, its Giga not listed",
  ).toEqual([
    ["pip", null],
    ["ray", null],
    ["sol", "Space 1"],
  ]);

  const widgets = await fromPip(app, page, "sessions.list", {
    project: WIDGETS,
  });
  expect(widgets.ok, JSON.stringify(widgets)).toBe(true);
  const ids = (
    widgets as { result: { sessions: { id: string }[] } }
  ).result.sessions.map(({ id }) => id);
  expect(ids, "sessions.list: Space 2's session in the folder").toContain(
    "pip-widgets",
  );
  expect(
    ids,
    "sessions.list: Space 1's session in the same folder is hidden",
  ).not.toContain("sol-widgets");

  const acme = await fromPip(app, page, "sessions.list", { project: ACME });
  expect(
    acme,
    "sessions.list: a project of another Space is not Pip's",
  ).toMatchObject({ ok: false });
  expect(JSON.stringify(acme)).toContain("Not one of your projects");
});
