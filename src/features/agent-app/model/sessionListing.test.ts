import { afterEach, expect, it } from "vitest";
import { clearHabitRun, markHabitRun } from "../../monos/model/monoHabits";
import type { SessionSummary } from "../../sessions/data/sessionStore";
import { newSession, type Session } from "../../sessions/model/session";
import { handleAgentApp, type AgentAppHost } from "./agentApp";
import { appSessionListings } from "./sessionListing";

const PROJECT = "/code/app";

const saved = (id: string, extra: Partial<SessionSummary> = {}): SessionSummary => ({
  id,
  cwd: PROJECT,
  harness: "codex",
  model: "codex:test",
  runtimeMode: "supervised",
  title: id,
  createdAt: 1,
  updatedAt: 1,
  ...extra,
});

const open = (id: string, extra: Partial<Session> = {}): Session =>
  Object.assign(newSession("codex", PROJECT, "codex:test"), { id, title: id, ...extra });

afterEach(() => {
  clearHabitRun("habit-saved");
  clearHabitRun("habit-open");
});

// ADE-84 #268 r1 M1, M2, m1. ready-issues.py refuses a reply from another app
// by `appIdentifier` and finds builders by `branch`. Defect: either field is
// missing from the reply as the CLI prints it, or a session with no branch
// drops the key, so its reply looks like one from an app that sends none.
it("lists the app that answered and each session's branch, null when it has none", async () => {
  const mono = open("mono-chat");
  const host = {
    levels: { check: async () => ({ leadSessionId: null }) },
    isMono: (id: string) => id === mono.id,
    monoOf: (id: string) => (id === mono.id ? { id: "mono", projects: [PROJECT] } : undefined),
    sessions: async (cwd: string) =>
      appSessionListings(
        cwd,
        [
          saved("stored", { branch: "feat/ade-61-saved", archived: true }),
          saved("loose"),
          saved("live", { archived: true }),
        ],
        [open("live", { branch: "fix/ade-84-live", busy: true }), open("fresh")],
      ),
    appIdentifier: async () => "com.monocode.second",
  } as unknown as AgentAppHost;
  const reply = await handleAgentApp(mono, "r1", "sessions.list", {}, host);
  const row = { harness: "codex", model: "codex:test", hasDraft: false };
  expect(JSON.parse(JSON.stringify(reply))).toStrictEqual({
    appIdentifier: "com.monocode.second",
    cwd: PROJECT,
    sessions: [
      { ...row, id: "stored", title: "stored", busy: false, branch: "feat/ade-61-saved", archived: true },
      { ...row, id: "loose", title: "loose", busy: false, branch: null, archived: false },
      { ...row, id: "live", title: "live", busy: true, branch: "fix/ade-84-live", archived: true },
      { ...row, id: "fresh", title: "fresh", busy: false, branch: null, archived: false },
    ],
  });
});

// ADE-84 #268 r2 M1. The app rewrites a saved session's branch from git on
// every save; the open session keeps the branch the app attached. Defect: an
// open session lists that stale branch, so ready-issues misses its builder.
it("lists an open session's saved branch over the one it opened with", () => {
  const listed = appSessionListings(
    PROJECT,
    [saved("switched", { branch: "fix/ade-48-x" })],
    [open("switched", { branch: "mc/x", busy: true })],
  );
  expect(listed.map(({ id, branch, busy }) => ({ id, branch, busy }))).toStrictEqual([
    { id: "switched", branch: "fix/ade-48-x", busy: true },
  ]);
});

// Defect: a worker, a habit run, an Inbox Ask or another project's session
// lists as one of the project's sessions, or a draft goes unreported.
it("lists only the project's own sessions, and an open one's draft", () => {
  markHabitRun("habit-saved", "mono");
  markHabitRun("habit-open", "mono");
  const listed = appSessionListings(
    PROJECT,
    [
      saved("worker-saved", { orchestrationLeadId: "lead" }),
      saved("habit-saved"),
      saved("ask"),
      saved("kept", { draft: true }),
    ],
    [
      open("ask", { inboxAsk: {} as Session["inboxAsk"] }),
      open("worker-open", { orchestrationLeadId: "lead" }),
      open("habit-open"),
      open("elsewhere", { cwd: "/code/site" }),
      open("drafting", { blocks: [{ id: "u1", role: "user", text: "Next step", draft: true }] }),
    ],
  );
  expect(listed.map(({ id, hasDraft }) => ({ id, hasDraft }))).toStrictEqual([
    { id: "kept", hasDraft: true },
    { id: "drafting", hasDraft: true },
  ]);
});
