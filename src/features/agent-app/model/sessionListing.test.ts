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
// ADE-175: or a lead lists its folder after that project moved to another
// Space. ADE-194: or it lists a session the scope does not name: another
// Space's, or one no Space saved.
it("lists the app that answered and each session's branch, null when it has none", async () => {
  const mono = open("mono-chat");
  const scope = {
    space: "space-1",
    projects: [PROJECT],
    monos: [],
    sessions: ["stored", "loose", "live", "fresh"],
  };
  const host = {
    levels: { check: async () => ({ leadSessionId: null, ask: null, scope }) },
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
  scope.sessions = ["loose", "fresh"];
  expect(await handleAgentApp(mono, "r2", "sessions.list", {}, host)).toMatchObject({
    sessions: [{ id: "loose" }, { id: "fresh" }],
  });
  // A lead whose own folder is a project of another Space lists nothing.
  let listed = 0;
  const leadHost = {
    ...host,
    sessions: async () => {
      listed += 1;
      return [];
    },
  } as unknown as AgentAppHost;
  scope.projects = [];
  await expect(handleAgentApp(open("lead"), "r3", "sessions.list", {}, leadHost)).rejects.toThrow(
    "Not one of your projects",
  );
  expect(listed).toBe(0);
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

// ADE-194 #520 r1 M1. The app never saves a session in a remote project
// (`canPersistSession`), so no remote session is ever in the scope's saved
// ids. Defect: sessions.list in a remote project of the caller's Space
// lists nothing, not even the lead the Mono just started there; or a local
// project lists an open session its Space never saved; or a call's own
// `space` field reaches the level check.
it("lists a remote project's open sessions, and a local project's saved ones only", async () => {
  const REMOTE = "remote://host/srv/app";
  const mono = open("mono-chat");
  const scope = {
    space: "space-2",
    projects: [REMOTE, PROJECT],
    monos: [],
    sessions: [] as string[],
  };
  const checked: unknown[] = [];
  const host = {
    levels: {
      check: async (_id: string, call: unknown) => {
        checked.push(call);
        return { leadSessionId: null, ask: null, scope };
      },
    },
    isMono: (id: string) => id === mono.id,
    monoOf: (id: string) =>
      id === mono.id ? { id: "mono", projects: [REMOTE, PROJECT] } : undefined,
    sessions: async (cwd: string) =>
      appSessionListings(cwd, [], [open("remote-lead", { cwd: REMOTE }), open("local-new")]),
    appIdentifier: async () => "com.monocode.second",
  } as unknown as AgentAppHost;
  const ids = async (project: string) => {
    const reply = (await handleAgentApp(mono, `r-${project}`, "sessions.list", { project }, host)) as {
      sessions: { id: string }[];
    };
    return reply.sessions.map(({ id }) => id);
  };
  expect(await ids(REMOTE)).toStrictEqual(["remote-lead"]);
  expect(await ids(PROJECT)).toStrictEqual([]);
  scope.sessions = ["local-new"];
  expect(await ids(PROJECT)).toStrictEqual(["local-new"]);
  checked.length = 0;
  await expect(
    handleAgentApp(mono, "r-spoof", "sessions.list", { project: PROJECT, space: "space-1" }, host),
  ).rejects.toMatchObject({ code: "invalid_input" });
  expect(checked).toStrictEqual([]);
});
