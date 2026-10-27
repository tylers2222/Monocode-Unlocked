// @vitest-environment happy-dom
import { act, createElement, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  flushSessionWrites,
  shouldPersistSession,
  upsertSession,
} from "../../features/sessions/data/sessionStore";
import {
  newSession,
  type Session,
} from "../../features/sessions/model/session";
import { newTab } from "../../features/workspace/model/layout";
import { applyHarnessEvents } from "../../integrations/harness/core/apply";
import { useIdleSessionDetach } from "../hooks/useIdleSessionDetach";
import { acceptQuickLaunch } from "./quickLaunchSession";
import { updateSessionState } from "./sessionState";
import { submitAfterProjectSync } from "./submissionAcceptance";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  forgetHarnessSession: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("../../integrations/harness/core/registry", () => ({
  forgetHarnessSession: mocks.forgetHarnessSession,
}));

const PROJECT = "/projects/alpha";
/** A project sync is one backend round trip. */
const SYNC_MS = 20;

let root: Root;
let container: HTMLDivElement;
let saved: Set<string>;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"],
  });
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
  mocks.forgetHarnessSession.mockReset().mockResolvedValue(undefined);
  saved = new Set();
  mocks.invoke.mockReset().mockImplementation(async (command, args) => {
    if (command !== "session_upsert")
      throw new Error(`Unexpected command: ${command}`);
    saved.add(args.session.id);
    return { ...args.session, createdAt: 1, updatedAt: 2 };
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  act(() => root.unmount());
  container.remove();
  await flushSessionWrites();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

type LaunchWorkspace = Parameters<typeof acceptQuickLaunch>[2];

/**
 * App's session state as App renders it: React state, the render-time
 * `sessionsRef`, the real idle detach, and App's project-session submit. That
 * submit waits for the project sync, then reads the session from
 * `sessionsRef` again (submitSession's `!storedCurrent` guard) and commits.
 */
function mountApp() {
  const home = newSession("fx", PROJECT);
  const parent: Session = {
    ...newSession("fx", PROJECT),
    id: "parent",
    busy: true,
  };
  let app!: {
    sessions: Session[];
    workspace: LaunchWorkspace;
    stream: () => void;
  };

  function App() {
    const [sessions, setSessions] = useState([home, parent]);
    const sessionsRef = useRef(sessions);
    sessionsRef.current = sessions;
    const tabsRef = useRef([newTab(home.id)]);
    const openingSessionIds = useRef(new Set<string>());
    const loadedSessionCache = useRef(new Map<string, Session>());
    const skipForgetSessionIds = useRef(new Set<string>());
    useIdleSessionDetach({
      sessions,
      sessionsRef,
      tabs: tabsRef.current,
      tabsRef,
      orchestrationRuns: [],
      liveAgentsEnabled: false,
      unseenFinishedIds: new Set(),
      openingSessionIds,
      loadedSessionCache,
      skipForgetSessionIds,
      persistSession: () => undefined,
      setSessions,
    });
    const [workspace] = useState((): LaunchWorkspace => ({
      getSessions: () => sessionsRef.current,
      updateSessions: (update) => {
        sessionsRef.current = update(sessionsRef.current);
        setSessions(update);
      },
      appendTab: () => undefined,
      setProjectCwd: () => undefined,
      setRecents: () => undefined,
      revealTab: () => undefined,
      submit: (id, text) =>
        submitAfterProjectSync({
          cwd: PROJECT,
          sync: new Promise((resolve) =>
            setTimeout(
              () => resolve({ path: PROJECT, identity: "alpha", moved: false }),
              SYNC_MS,
            ),
          ),
          applyLocationChange: async () => undefined,
          submit: () => {
            if (!sessionsRef.current.some((session) => session.id === id))
              return false;
            updateSessionState(sessionsRef, setSessions, (current) =>
              current.map((session) =>
                session.id === id
                  ? {
                      ...session,
                      busy: true,
                      blocks: [{ id: `${id}-turn`, role: "user", text }],
                    }
                  : session,
              ),
            );
            return true;
          },
          onError: (error) => {
            throw error;
          },
        }),
      saveSession: async (session) =>
        !shouldPersistSession(session) || !!(await upsertSession(session)),
    }));
    app = {
      sessions,
      workspace,
      stream: () =>
        updateSessionState(sessionsRef, setSessions, (current) =>
          current.map((session) =>
            session.id === parent.id
              ? applyHarnessEvents(session, [
                  { type: "message.delta", text: "Starting the leads. " },
                ])
              : session,
          ),
        ),
    };
    return null;
  }

  act(() => root.render(createElement(App)));
  return () => app;
}

it("accepts and saves three lead starts made within 300 ms while the parent's turn streams", async () => {
  const app = mountApp();
  setInterval(() => app().stream(), 40);
  const leads = ["lead-a", "lead-b", "lead-c"];
  const answers: Promise<string>[] = [];
  leads.forEach((id, index) =>
    setTimeout(
      () =>
        answers.push(
          acceptQuickLaunch(
            {
              cwd: PROJECT,
              harness: "fx",
              prompt: `${id} You lead a task.`,
              startedBy: "parent",
            },
            id,
            app().workspace,
            undefined,
            false,
          ).then(
            () => "accepted",
            (error: Error) => error.message,
          ),
        ),
      index * 120,
    ),
  );

  // React renders between tasks in the app, so it renders after each step.
  for (let elapsed = 0; elapsed < 1_000; elapsed += 5)
    await act(() => vi.advanceTimersByTimeAsync(5));

  expect(await Promise.all(answers)).toEqual([
    "accepted",
    "accepted",
    "accepted",
  ]);
  expect(saved).toEqual(new Set(leads));
  expect(
    app()
      .sessions.filter((session) => leads.includes(session.id))
      .map(({ id, busy, quickLaunchAccepted }) => ({
        id,
        busy,
        quickLaunchAccepted,
      })),
  ).toEqual(leads.map((id) => ({ id, busy: true, quickLaunchAccepted: true })));
});
