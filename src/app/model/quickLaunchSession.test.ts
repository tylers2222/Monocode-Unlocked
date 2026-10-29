// @vitest-environment happy-dom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { rememberProject } from "../../features/projects/model/recents";
import { ProjectNotFoundError } from "../../features/projects/model/projectLocationError";
import { launchReceiver } from "../../features/quick-composer/model/launchDelivery";
import type { QuickLaunch } from "../../features/quick-composer/model/quickComposer";
import {
  newSession,
  type Attachment,
  type Session,
} from "../../features/sessions/model/session";
import {
  leafIds,
  newTab,
  splitPane,
  type WorkspaceTab,
} from "../../features/workspace/model/layout";
import { filterTabsForProject } from "../../features/workspace/model/workspaceTabGroups";
import { acceptQuickLaunch, appLaunchRepeat } from "./quickLaunchSession";
import {
  submitAfterProjectSync,
  type SubmissionAcceptance,
} from "./submissionAcceptance";

const storedValues = new Map<string, string>();
vi.stubGlobal("localStorage", {
  clear: () => storedValues.clear(),
  getItem: (key: string) => storedValues.get(key) ?? null,
  setItem: (key: string, value: string) => storedValues.set(key, value),
  removeItem: (key: string) => storedValues.delete(key),
});

beforeEach(() => {
  localStorage.clear();
  vi.useFakeTimers();
});
afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

function setup(reveal = false) {
  const oldSession = newSession("codex", "/old-project");
  const oldTab = newTab(oldSession.id);
  const state = {
    sessions: [oldSession],
    tabs: [oldTab],
    projectCwd: oldSession.cwd,
    recents: rememberProject(oldSession.cwd),
    activeTabId: oldTab.id,
  };
  const request: QuickLaunch = {
    cwd: "/new-project",
    harness: "codex",
    prompt: "hello from the floating composer",
    reveal,
  };
  const commit = vi.fn(
    (id: string, text: string, _attachments: Attachment[]) => {
      state.sessions = state.sessions.map((session) =>
        session.id === id
          ? {
              ...session,
              blocks: [
                ...session.blocks,
                { id: "user-turn", role: "user" as const, text },
              ],
            }
          : session,
      );
      return true;
    },
  );
  const submit = vi.fn(
    (...args: Parameters<typeof commit>): SubmissionAcceptance =>
      commit(...args),
  );
  const workspace = {
    getSessions: () => state.sessions,
    updateSessions: (update: (sessions: Session[]) => Session[]) => {
      state.sessions = update(state.sessions);
    },
    keepLoaded: new Set<string>(),
    appendTab: vi.fn((tab: WorkspaceTab) => {
      state.tabs.push(tab);
    }),
    setProjectCwd: vi.fn((cwd: string) => {
      state.projectCwd = cwd;
    }),
    setRecents: vi.fn((recents: typeof state.recents) => {
      state.recents = recents;
    }),
    revealTab: vi.fn((id: string, cwd: string) => {
      // Assert the title bar and recents already point at the revealed project.
      expect(
        filterTabsForProject(state.tabs, state.sessions, state.projectCwd).some(
          (tab) => tab.id === id,
        ),
      ).toBe(true);
      expect(state.recents[0].path).toBe(request.cwd);
      expect(cwd).toBe(request.cwd);
      state.activeTabId = id;
    }),
    submit,
    saveDraft: vi.fn(
      (
        id: string,
        text: string,
        _attachments: Attachment[],
        requestId: string,
      ) => {
        state.sessions = state.sessions.map((session) =>
          session.id === id
            ? {
                ...session,
                blocks: [
                  ...session.blocks,
                  {
                    id: "draft-turn",
                    role: "user" as const,
                    text,
                    draft: true,
                    appRequestId: requestId,
                  },
                ],
              }
            : session,
        );
        return true;
      },
    ),
    saveSession: vi.fn(async (_session: Session) => true),
  };
  const queue = [{ id: "quick-session", request }];
  const ack = vi.fn(async () => {
    queue.shift();
  });
  const receive = launchReceiver({
    take: async () => queue[0] ?? null,
    accept: (launch, id) => acceptQuickLaunch(launch, id, workspace),
    ack,
    accepted: new Set(),
    accepting: new Map(),
    disposed: () => false,
  });
  return { state, oldTab, request, commit, workspace, queue, ack, receive };
}

it("switches project and recents before revealing a cross-project quick session", async () => {
  const { state, workspace, receive } = setup(true);
  await receive();
  expect(state.projectCwd).toBe("/new-project");
  expect(state.recents.map((entry) => entry.path)).toEqual([
    "/new-project",
    "/old-project",
  ]);
  expect(workspace.revealTab).toHaveBeenCalledOnce();
  expect(
    filterTabsForProject(state.tabs, state.sessions, state.projectCwd).map(
      (tab) => tab.id,
    ),
  ).toContain(state.activeTabId);
});

it("leaves the selected project, recents, and active tab unchanged for background launches", async () => {
  const { state, oldTab, workspace, receive } = setup();
  await receive();
  expect(state.projectCwd).toBe("/old-project");
  expect(state.activeTabId).toBe(oldTab.id);
  expect(state.recents.map((entry) => entry.path)).toEqual(["/old-project"]);
  expect(workspace.setProjectCwd).not.toHaveBeenCalled();
  expect(workspace.setRecents).not.toHaveBeenCalled();
  expect(workspace.revealTab).not.toHaveBeenCalled();
});

it("starts a session without a workspace tab when asked, even if revealed", async () => {
  const { state, oldTab, request, commit, workspace } = setup(true);
  await acceptQuickLaunch(request, "mono-launch", workspace, undefined, false);
  expect(state.sessions.map((session) => session.id)).toContain("mono-launch");
  expect(commit).toHaveBeenCalledOnce();
  expect(workspace.appendTab).not.toHaveBeenCalled();
  expect(state.tabs).toEqual([oldTab]);
  expect(workspace.revealTab).not.toHaveBeenCalled();
  expect(workspace.setProjectCwd).not.toHaveBeenCalled();
});

it("starts the first turn in the mode picked in the floating composer", async () => {
  const { request, workspace } = setup();
  request.intent = "orchestrate";
  await acceptQuickLaunch(request, "quick-session", workspace);
  expect(workspace.submit).toHaveBeenCalledWith(
    "quick-session",
    request.prompt,
    [],
    { intent: "orchestrate" },
  );
});

it.each([false, true])(
  "keeps a hidden launch hidden before submission (draft: %s)",
  async (draft) => {
    const { state, request, workspace } = setup();
    request.sidebarHidden = true;
    request.draft = draft;
    const save = draft ? workspace.saveDraft : workspace.submit;
    const original = save.getMockImplementation()!;
    save.mockImplementation((...args) => {
      expect(
        state.sessions.find((session) => session.id === args[0])?.sidebarHidden,
      ).toBe(true);
      return original(...args);
    });
    await acceptQuickLaunch(request, "quick-session", workspace);
    expect(
      state.sessions.find((session) => session.id === "quick-session"),
    ).toMatchObject({ sidebarHidden: true, quickLaunchAccepted: true });
    request.sidebarHidden = false;
    await acceptQuickLaunch(request, "quick-session", workspace);
    expect(
      state.sessions.find((session) => session.id === "quick-session")
        ?.sidebarHidden,
    ).toBe(true);
    expect(save).toHaveBeenCalledOnce();
  },
);

it("creates a draft-only session without submitting an agent turn", async () => {
  const { state, request, workspace } = setup();
  request.draft = true;
  await acceptQuickLaunch(request, "quick-session", workspace);
  expect(workspace.submit).not.toHaveBeenCalled();
  expect(workspace.saveDraft).toHaveBeenCalledWith(
    "quick-session",
    request.prompt,
    [],
    "quick-session",
  );
  expect(
    state.sessions.find((session) => session.id === "quick-session"),
  ).toMatchObject({
    quickLaunchAccepted: true,
    blocks: [
      {
        role: "user",
        text: request.prompt,
        draft: true,
        appRequestId: "quick-session",
      },
    ],
  });
  await acceptQuickLaunch(request, "quick-session", workspace);
  expect(workspace.saveDraft).toHaveBeenCalledOnce();
  state.sessions = state.sessions.map((session) =>
    session.id === "quick-session"
      ? {
          ...session,
          quickLaunchAccepted: undefined,
          blocks: session.blocks.map((block) => ({ ...block, draft: false })),
        }
      : session,
  );
  await acceptQuickLaunch(request, "quick-session", workspace);
  expect(workspace.saveDraft).toHaveBeenCalledOnce();
});

it("places successive sessions in right and down splits of the same tab", async () => {
  const { state, oldTab, request, workspace } = setup(true);
  request.cwd = state.sessions[0].cwd;
  request.draft = true;
  const placeSession = vi.fn(
    (
      sessionId: string,
      placement: { direction: "right" | "down"; besideSessionId: string },
    ) => {
      const tab = state.tabs.find((entry) =>
        leafIds(entry.layout).includes(placement.besideSessionId),
      );
      if (!tab) throw new Error("Target pane unavailable");
      state.tabs = state.tabs.map((entry) =>
        entry.id === tab.id
          ? {
              ...entry,
              layout: splitPane(
                entry.layout,
                placement.besideSessionId,
                placement.direction,
                sessionId,
              ),
              focusedId: sessionId,
            }
          : entry,
      );
      return tab.id;
    },
  );
  await acceptQuickLaunch(
    request,
    "right-session",
    { ...workspace, placeSession },
    { direction: "right", besideSessionId: state.sessions[0].id },
  );
  await acceptQuickLaunch(
    request,
    "down-session",
    { ...workspace, placeSession },
    { direction: "down", besideSessionId: "right-session" },
  );
  expect(state.tabs).toHaveLength(1);
  expect(state.tabs[0].id).toBe(oldTab.id);
  expect(state.tabs[0].layout).toMatchObject({
    type: "split",
    dir: "right",
    children: [
      { type: "leaf", id: state.sessions[0].id },
      {
        type: "split",
        dir: "down",
        children: [
          { type: "leaf", id: "right-session" },
          { type: "leaf", id: "down-session" },
        ],
      },
    ],
  });
  expect(workspace.appendTab).not.toHaveBeenCalled();
  expect(workspace.revealTab).toHaveBeenLastCalledWith(oldTab.id, request.cwd);
  expect(workspace.submit).not.toHaveBeenCalled();
});

it("rejects a missing pane before adding or submitting a session", async () => {
  const { state, request, workspace } = setup();
  await expect(
    acceptQuickLaunch(
      request,
      "lost-session",
      {
        ...workspace,
        placeSession: () => {
          throw new Error("Target pane unavailable");
        },
      },
      { direction: "right", besideSessionId: "missing" },
    ),
  ).rejects.toThrow("Target pane unavailable");
  expect(state.sessions.some((session) => session.id === "lost-session")).toBe(
    false,
  );
  expect(workspace.submit).not.toHaveBeenCalled();
});

it("does not acknowledge or mark accepted while project synchronization is pending", async () => {
  const { state, workspace, commit, ack, receive } = setup();
  let resolveSync!: (location: {
    path: string;
    identity: string;
    moved: boolean;
  }) => void;
  const sync = new Promise<{ path: string; identity: string; moved: boolean }>(
    (resolve) => {
      resolveSync = resolve;
    },
  );
  workspace.submit.mockImplementationOnce((...args) =>
    submitAfterProjectSync({
      cwd: "/new-project",
      sync,
      applyLocationChange: vi.fn(),
      submit: () => commit(...args),
      onError: vi.fn(),
    }),
  );
  const pending = receive();
  await vi.waitFor(() => expect(workspace.submit).toHaveBeenCalledOnce());
  expect(ack).not.toHaveBeenCalled();
  expect(commit).not.toHaveBeenCalled();
  expect(
    state.sessions.find((s) => s.id === "quick-session")?.quickLaunchAccepted,
  ).toBeUndefined();
  resolveSync({ path: "/new-project", identity: "repo", moved: false });
  await pending;
  expect(commit).toHaveBeenCalledOnce();
  expect(ack).toHaveBeenCalledOnce();
  expect(
    state.sessions.find((s) => s.id === "quick-session")?.quickLaunchAccepted,
  ).toBe(true);
});

// ADE-135. Defect: idle detach drops a tabless session while its submit waits
// for the project sync, or the hold outlives the acceptance and the session
// is never detached.
it("holds a session only while its launch is being accepted", async () => {
  const { request, workspace } = setup();
  let resolveSubmit!: (accepted: boolean) => void;
  workspace.submit.mockImplementationOnce(
    () => new Promise<boolean>((resolve) => (resolveSubmit = resolve)),
  );
  const pending = acceptQuickLaunch(request, "quick-session", workspace);
  await vi.waitFor(() => expect(workspace.submit).toHaveBeenCalledOnce());
  expect(workspace.keepLoaded.has("quick-session")).toBe(true);
  resolveSubmit(true);
  await pending;
  expect(workspace.keepLoaded.has("quick-session")).toBe(false);

  workspace.submit.mockImplementationOnce(() => false);
  await expect(
    acceptQuickLaunch(request, "refused-session", workspace),
  ).rejects.toThrow("could not accept");
  expect(workspace.keepLoaded.has("refused-session")).toBe(false);
});

// ADE-137. Defect: an acceptance that finds its id already held (an open in
// flight, or another acceptance of the same id) releases that hold when it
// ends, so idle detach drops the session while its owner still needs it.
it("leaves a hold it did not add in place", async () => {
  const { request, workspace } = setup();
  workspace.keepLoaded.add("opening-session");
  await acceptQuickLaunch(request, "opening-session", workspace);
  expect(workspace.keepLoaded.has("opening-session")).toBe(true);

  let resolveFirst!: (accepted: boolean) => void;
  workspace.submit.mockImplementationOnce(
    () => new Promise<boolean>((resolve) => (resolveFirst = resolve)),
  );
  const first = acceptQuickLaunch(request, "quick-session", workspace);
  await vi.waitFor(() => expect(workspace.submit).toHaveBeenCalledTimes(2));
  await acceptQuickLaunch(request, "quick-session", workspace);
  expect(workspace.submit).toHaveBeenCalledTimes(3);
  expect(workspace.keepLoaded.has("quick-session")).toBe(true);
  resolveFirst(true);
  await first;
  expect(workspace.keepLoaded.has("quick-session")).toBe(false);
});

// ADE-135. Defect: a refused launch logs no guard, or names the wrong one.
it.each([
  ["still loaded", false, "a submitSession guard refused the loaded session"],
  [
    "gone from the workspace",
    true,
    "submitSession's !storedCurrent guard refused it; the session left the workspace",
  ],
])(
  "names the guard when a submit refuses a session %s",
  async (_case, removed, guard) => {
    const { request, workspace } = setup();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    workspace.submit.mockImplementationOnce((id) => {
      if (removed)
        workspace.updateSessions((sessions) =>
          sessions.filter((session) => session.id !== id),
        );
      return false;
    });
    await expect(
      acceptQuickLaunch(request, "quick-session", workspace),
    ).rejects.toThrow("could not accept");
    expect(warn).toHaveBeenCalledExactlyOnceWith(
      `Quick launch quick-session: ${guard}`,
    );
    warn.mockRestore();
  },
);

it.each(["sync failure", "deferred rejection", "deferred exception"])(
  "retains the prompt after %s and retries the same session",
  async (failure) => {
    const { state, workspace, commit, queue, ack, receive } = setup();
    const onError = vi.fn();
    const deferredSubmit = vi.fn(() => {
      if (failure === "deferred exception")
        throw new Error("submission failed");
      return false;
    });
    workspace.submit.mockImplementationOnce(() =>
      submitAfterProjectSync({
        cwd: "/new-project",
        sync:
          failure === "sync failure"
            ? Promise.reject(new Error("disk unavailable"))
            : Promise.resolve({
                path: "/new-project",
                identity: "repo",
                moved: false,
              }),
        applyLocationChange: vi.fn(),
        submit: deferredSubmit,
        onError,
      }),
    );
    await expect(receive()).rejects.toThrow("could not accept");
    expect(ack).not.toHaveBeenCalled();
    expect(queue).toHaveLength(1);
    const session = state.sessions.find((s) => s.id === "quick-session")!;
    expect(session.quickLaunchAccepted).toBeUndefined();
    expect(session.blocks.some((block) => block.role === "user")).toBe(false);
    expect(deferredSubmit).toHaveBeenCalledTimes(
      failure.startsWith("deferred") ? 1 : 0,
    );
    expect(onError).toHaveBeenCalledTimes(
      failure === "deferred rejection" ? 0 : 1,
    );

    await vi.advanceTimersByTimeAsync(250);
    expect(queue).toHaveLength(0);
    expect(ack).toHaveBeenCalledOnce();
    expect(commit).toHaveBeenCalledOnce();
    expect(state.sessions.filter((s) => s.id === "quick-session")).toHaveLength(
      1,
    );
    expect(workspace.appendTab).toHaveBeenCalledOnce();
    expect(
      state.sessions.find((s) => s.id === "quick-session")?.quickLaunchAccepted,
    ).toBe(true);
  },
);

it("retains a missing project's prompt without automatic retries, then accepts an explicit retry after reconnection", async () => {
  const { state, workspace, commit, queue, ack, receive } = setup();
  const onError = vi.fn();
  let connected = false;
  workspace.submit.mockImplementation((...args) =>
    submitAfterProjectSync({
      cwd: "/new-project",
      sync: Promise.resolve(
        connected
          ? { path: "/new-project", identity: "repo", moved: false }
          : null,
      ),
      applyLocationChange: vi.fn(),
      submit: () => commit(...args),
      onError,
    }),
  );
  await expect(receive()).rejects.toBeInstanceOf(ProjectNotFoundError);
  await vi.advanceTimersByTimeAsync(300_000);
  expect(vi.getTimerCount()).toBe(0);
  expect(workspace.submit).toHaveBeenCalledOnce();
  expect(onError).toHaveBeenCalledOnce();
  expect(commit).not.toHaveBeenCalled();
  expect(ack).not.toHaveBeenCalled();
  expect(queue).toHaveLength(1);
  expect(
    state.sessions.find((s) => s.id === "quick-session")?.quickLaunchAccepted,
  ).toBeUndefined();

  connected = true;
  await receive();
  expect(queue).toHaveLength(0);
  expect(commit).toHaveBeenCalledOnce();
  expect(ack).toHaveBeenCalledOnce();
  expect(workspace.appendTab).toHaveBeenCalledOnce();
});

it("does not submit an accepted prompt again after a lost ACK", async () => {
  const { workspace, ack, receive, queue } = setup();
  ack.mockRejectedValueOnce(new Error("IPC interrupted"));
  await expect(receive()).rejects.toThrow("IPC interrupted");
  await vi.advanceTimersByTimeAsync(250);
  expect(workspace.submit).toHaveBeenCalledOnce();
  expect(queue).toHaveLength(0);
});

// Design 13.3. Defect: a worker's first save carries no lead id, so it is
// stored as a lead for good. ADE-117: the same holds for its mcp choice, or
// the first turn starts omp with MCP servers; a reused session keeps its own.
it("writes a worker's lead id before its first save", async () => {
  const { state, request, workspace } = setup();
  const lead = state.sessions[0].id;
  request.leadSessionId = lead;
  request.mcp = "none";
  const original = workspace.submit.getMockImplementation()!;
  workspace.submit.mockImplementation((...args) => {
    const worker = state.sessions.find((session) => session.id === args[0]);
    expect(worker?.leadSessionId).toBe(lead);
    expect(worker?.mcp).toBe("none");
    return original(...args);
  });
  await acceptQuickLaunch(request, "worker", workspace);
  expect(workspace.submit).toHaveBeenCalledOnce();
  const worker = state.sessions.find((session) => session.id === "worker");
  expect(worker?.leadSessionId).toBe(lead);
  expect(worker?.mcp).toBe("none");

  const reuse = setup();
  const reused = reuse.state.sessions[0].id;
  await acceptQuickLaunch(
    { ...reuse.request, mcp: "none" },
    reused,
    reuse.workspace,
  );
  expect(reuse.workspace.submit).toHaveBeenCalledOnce();
  expect(
    reuse.state.sessions.find((session) => session.id === reused)?.mcp,
  ).toBeUndefined();
});

// Design 13.2. Defect: the worker's first save runs before its lead's row
// exists, so the lead id is stored as NULL and the worker becomes a lead.
it("saves the lead before the worker exists", async () => {
  const { state, request, workspace } = setup();
  const lead = state.sessions[0];
  request.leadSessionId = lead.id;
  let saved!: (ok: boolean) => void;
  workspace.saveSession.mockImplementation(
    () => new Promise<boolean>((resolve) => (saved = resolve)),
  );
  const accepting = acceptQuickLaunch(request, "worker", workspace);
  await vi.waitFor(() =>
    expect(workspace.saveSession).toHaveBeenCalledExactlyOnceWith(lead),
  );
  expect(state.sessions.some((session) => session.id === "worker")).toBe(false);
  saved(true);
  await accepting;
  expect(
    state.sessions.find((session) => session.id === "worker")?.leadSessionId,
  ).toBe(lead.id);
});

// Design 13.2. Defect: a worker starts although its lead was not saved, and
// its first save stores it as a lead.
it.each([
  ["the save fails", false, "The lead session could not be saved"],
  ["the lead is not open", true, "The lead session is not open"],
])("gives no worker when %s", async (_case, saves, message) => {
  const { state, request, workspace } = setup();
  request.leadSessionId = saves ? "closed-lead" : state.sessions[0].id;
  workspace.saveSession.mockResolvedValue(saves);
  await expect(acceptQuickLaunch(request, "worker", workspace)).rejects.toThrow(
    message,
  );
  expect(state.sessions.some((session) => session.id === "worker")).toBe(false);
  expect(workspace.appendTab).not.toHaveBeenCalled();
  expect(workspace.submit).not.toHaveBeenCalled();
});

// R4 review r1 M1. Defect: an app launch answers its caller before the new
// session's row exists, so the caller's next call can name it as a target
// with no stored level, and its first CLI call can find no row; or a failed
// save still answers that the session started.
it("answers an app launch only once the new session is saved", async () => {
  const { request, workspace } = setup();
  let saved!: (ok: boolean) => void;
  workspace.saveSession.mockImplementation(
    () => new Promise<boolean>((resolve) => (saved = resolve)),
  );
  let answered = false;
  const accepting = acceptQuickLaunch(
    { ...request, startedBy: "lead" },
    "worker",
    workspace,
  ).then(() => (answered = true));
  await vi.waitFor(() =>
    expect(workspace.saveSession).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({
        id: "worker",
        blocks: [expect.objectContaining({ role: "user" })],
      }),
    ),
  );
  expect(answered).toBe(false);
  saved(true);
  await accepting;
  workspace.saveSession.mockResolvedValue(false);
  await expect(
    acceptQuickLaunch({ ...request, startedBy: "lead" }, "worker-2", workspace),
  ).rejects.toThrow("The workspace could not save the session yet.");
});

/** A session `app-lead-r1` holding `blocks`, as an app launch left it. */
function launched(blocks: Session["blocks"], extra: Partial<Session> = {}) {
  return { ...newSession("codex", "/p"), id: "app-lead-r1", blocks, ...extra };
}
const FLOATING: QuickLaunch = { cwd: "/p", harness: "codex", prompt: "Build" };
const APP_LAUNCH: QuickLaunch = { ...FLOATING, startedBy: "lead" };
const reused = { code: "request_id_reused" };

// Design 9, the one repeat rule. Defect: a repeat with other text, from
// another caller whose ids join to the same session id, or a submitted retry
// of a draft launch answers as the first launch did; or the same launch
// again launches twice.
it("repeats an app launch only with its own stamp, caller and text", () => {
  const stamped = (draft: boolean) =>
    launched([
      {
        id: "u1",
        role: "user",
        text: "Build",
        appRequestId: "app-lead-r1",
        startedBy: "lead",
        ...(draft ? { draft } : {}),
      },
    ]);
  expect(appLaunchRepeat(stamped(false), APP_LAUNCH, "app-lead-r1")).toBe(true);
  expect(
    appLaunchRepeat(
      stamped(true),
      { ...APP_LAUNCH, draft: true },
      "app-lead-r1",
    ),
  ).toBe(true);
  for (const other of [
    { ...APP_LAUNCH, prompt: "Test" },
    { ...APP_LAUNCH, startedBy: "app-lead" },
  ])
    expect(() => appLaunchRepeat(stamped(false), other, "app-lead-r1")).toThrow(
      expect.objectContaining(reused),
    );
  expect(() =>
    appLaunchRepeat(stamped(true), APP_LAUNCH, "app-lead-r1"),
  ).toThrow(expect.objectContaining(reused));
});

// Design 9. Defect: an app launch into a session whose turn the user sent or
// wrote (no stamp), or that was accepted, counts as a repeat, so a start row
// names the user's own work; or a floating-composer launch, which stamps
// only drafts, launches its accepted session again.
it("refuses an app launch into a session the user worked in, and repeats a floating one", () => {
  for (const existing of [
    launched([{ id: "u1", role: "user", text: "Build" }]),
    launched([], { quickLaunchAccepted: true }),
  ]) {
    expect(() => appLaunchRepeat(existing, APP_LAUNCH, "app-lead-r1")).toThrow(
      expect.objectContaining(reused),
    );
    expect(appLaunchRepeat(existing, FLOATING, "app-lead-r1")).toBe(true);
  }
});

// Design 9. Defect: an app launch lands in a session id that holds the
// user's other draft, or refuses a fresh id.
it("refuses an app launch into another draft and launches a fresh id", () => {
  const draft = launched([
    { id: "d1", role: "user", text: "Mine", draft: true },
  ]);
  expect(() => appLaunchRepeat(draft, APP_LAUNCH, "app-lead-r1")).toThrow(
    "Session ID already has a different draft",
  );
  expect(appLaunchRepeat(undefined, APP_LAUNCH, "app-lead-r1")).toBe(false);
  expect(appLaunchRepeat(launched([]), APP_LAUNCH, "app-lead-r1")).toBe(false);
});

// Design 9 and 13.4: every app launch stamps its first block with its
// request id and its caller. Defect: a submitted or drafted app launch is
// not stamped, so its repeat is refused as the user's work or the session
// gets no app access; or a floating launch is stamped as an app launch.
it("stamps an app launch with its request id and caller, and leaves a floating one unstamped", async () => {
  const { request, workspace } = setup();
  await acceptQuickLaunch(
    { ...request, startedBy: "lead" },
    "app-lead-r1",
    workspace,
  );
  expect(workspace.submit).toHaveBeenLastCalledWith(
    "app-lead-r1",
    request.prompt,
    [],
    { appRequestId: "app-lead-r1", startedBy: "lead" },
  );
  await acceptQuickLaunch(
    { ...request, draft: true, startedBy: "lead" },
    "app-lead-r2",
    workspace,
  );
  expect(workspace.saveDraft).toHaveBeenLastCalledWith(
    "app-lead-r2",
    request.prompt,
    [],
    "app-lead-r2",
    "lead",
  );
  await acceptQuickLaunch(request, "quick-session", workspace);
  expect(workspace.submit).toHaveBeenLastCalledWith(
    "quick-session",
    request.prompt,
    [],
  );
});
