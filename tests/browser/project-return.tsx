import { createRoot } from "react-dom/client";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { TitleBar, type Tab } from "../../src/app/shell/TitleBar";
import { ProjectRail } from "../../src/app/shell/ProjectRail";
import { planProjectReturn } from "../../src/features/projects/model/projectReturn";
import {
  newSession,
  type Session,
} from "../../src/features/sessions/model/session";
import { newTab } from "../../src/features/workspace/model/layout";
import "../../src/styles/index.css";

mockWindows("main");
mockIPC(
  (command) => {
    if (command === "plugin:window|is_maximized") return false;
    if (command.includes("list") || command.includes("machines")) return [];
    return null;
  },
  { shouldMockEvents: true },
);

const project = "/Users/tyler/code/monocode";
const elsewhere = "/Users/tyler/code/pulse-api";

type Row = { id: string; title: string; busy: boolean; startedAt: number };
type Scenario = { rows: Row[]; remembered: string; before?: boolean };

const scenarios: Record<string, Scenario> = {
  // The rule before ADE-5 ignored running state. The fixture asks the rule
  // with every session idle, which runs only the steps that rule had.
  before: {
    rows: [
      { id: "a", title: "Fix PR7 review nits", busy: false, startedAt: 1 },
      { id: "b", title: "Execute plan M tasks", busy: true, startedAt: 2 },
      { id: "c", title: "Draft ADE-5 plan", busy: false, startedAt: 3 },
    ],
    remembered: "a",
    before: true,
  },
  one: {
    rows: [
      { id: "a", title: "Fix PR7 review nits", busy: false, startedAt: 1 },
      { id: "b", title: "Execute plan M tasks", busy: true, startedAt: 2 },
      { id: "c", title: "Draft ADE-5 plan", busy: false, startedAt: 3 },
    ],
    remembered: "a",
  },
  idle: {
    rows: [
      { id: "a", title: "Fix PR7 review nits", busy: false, startedAt: 1 },
      { id: "b", title: "Execute plan M tasks", busy: false, startedAt: 2 },
      { id: "c", title: "Draft ADE-5 plan", busy: false, startedAt: 3 },
    ],
    remembered: "a",
  },
  two: {
    rows: [
      { id: "a", title: "Fix PR7 review nits", busy: false, startedAt: 1 },
      { id: "b", title: "Execute plan M tasks", busy: true, startedAt: 2 },
      { id: "c", title: "Design M spec", busy: true, startedAt: 3 },
    ],
    remembered: "a",
  },
};

const scenario =
  scenarios[new URLSearchParams(location.search).get("s") ?? "before"];

const sessions: Session[] = [
  ...scenario.rows.map((row) => ({
    ...newSession("claude", project),
    id: row.id,
    title: row.title,
    busy: row.busy,
    blocks: [
      {
        id: `${row.id}-user`,
        role: "user" as const,
        text: row.title,
        startedAt: row.startedAt,
      },
    ],
  })),
  { ...newSession("claude", elsewhere), id: "elsewhere" },
];
const workspaceTabs = sessions.map((session) => newTab(session.id));
const tabIdBySession = new Map(
  workspaceTabs.map((tab) => [tab.focusedId, tab.id]),
);

const decision = planProjectReturn({
  memory: new Map([[project, scenario.remembered]]),
  tabs: workspaceTabs,
  sessions: scenario.before
    ? sessions.map((session) => ({ ...session, busy: false }))
    : sessions,
  activeTabId: tabIdBySession.get("elsewhere")!,
  excludedIds: new Set(),
  projectPath: project,
});
const landing =
  decision.action === "activate"
    ? workspaceTabs.find((tab) => tab.id === decision.tabId)!.focusedId
    : decision.action;

const titleTabs: Tab[] = scenario.rows.map((row) => ({
  id: row.id,
  project: "monocode",
  title: row.title,
  more: [],
  sessionCount: 1,
  harnesses: ["claude"],
  busyHarnesses: row.busy ? ["claude"] : [],
  files: [],
}));

const now = Date.now();
const recents = [
  { path: project, openedAt: now },
  { path: "/Users/tyler/code/mono-review", openedAt: now - 1000 },
  { path: elsewhere, openedAt: now - 2000 },
];

function Fixture() {
  return (
    <div
      data-landing={landing}
      style={{ display: "flex", height: "100vh", background: "#121212" }}
    >
      <div style={{ width: 260, flexShrink: 0, height: "100%" }}>
        <ProjectRail
          cwd={project}
          recents={recents}
          busyPaths={scenario.rows.some((row) => row.busy) ? [project] : []}
          onSelectProject={() => {}}
          onOpenProject={() => {}}
        />
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        <TitleBar
          tabs={titleTabs}
          activeId={landing}
          cwd={project}
          projectRailOpen
          onToggleSidebar={() => {}}
          onSelect={() => {}}
          onClose={() => {}}
          onCloseMany={() => {}}
          onReorder={() => {}}
          onPlaceOnPane={() => {}}
        />
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<Fixture />);
