// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as approvals from "../../approvals/model/approvals";
import {
  handleAgentApp,
  type AgentAppHost,
} from "../../agent-app/model/agentApp";
import { rememberProject } from "../../projects/model/recents";
import {
  openPickedFolders,
  RESERVED_PROJECT_NAME_ERROR,
  reservedProjectLabelError,
} from "../../projects/model/reservedProjectName";
import { newSession } from "../../sessions/model/session";
import { setHomeDir } from "../../../shared/lib/paths";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, args: Record<string, unknown>) =>
    command === "approvals_file"
      ? { id: "card-1", projectPath: args.projectPath }
      : null,
  ),
}));

const HOME = "/home/u";
const FORK = `${HOME}/Desktop/monocode`;
const CARD = {
  heading: "Ship the fork",
  summary: "Swap the installed app for the new build.",
  tradeoffs: { pros: ["New fixes"], cons: ["One restart"] },
  urgency: "blocking",
};

function fileAsGiga(project: string) {
  const source = { ...newSession("codex", HOME, "codex:test"), id: "giga-chat" };
  const host = {
    levels: {
      check: async () => ({ leadSessionId: null }),
      recordStart: async () => "m_aaaaaaaaaaaa",
    },
    isMono: (id: string) => id === source.id,
    monoOf: (id: string) =>
      id === source.id ? { id: "giga", projects: [], giga: true } : undefined,
    approvals,
  } as unknown as AgentAppHost;
  return handleAgentApp(source, "req-1", "approvals.file", { project, ...CARD }, host);
}

beforeEach(() => {
  localStorage.clear();
  vi.mocked(invoke).mockClear();
  setHomeDir(HOME);
});

afterEach(() => setHomeDir(undefined));

// Defect (ADE-127): a Giga's card about an existing project whose folder is
// named monocode is refused as reserved, so it cannot reach that board.
it("files a Giga's card on an existing project whose folder is named monocode", async () => {
  rememberProject(FORK);

  await expect(fileAsGiga(FORK)).resolves.toEqual({
    id: "card-1",
    projectPath: FORK,
  });
  expect(vi.mocked(invoke)).toHaveBeenCalledExactlyOnceWith(
    "approvals_file",
    expect.objectContaining({ projectPath: FORK, by: "giga" }),
  );
});

// Defect: accepting existing projects lets a new folder named monocode in.
it("still refuses to add a new folder named monocode", async () => {
  const open = vi.fn();
  const refuse = vi.fn();

  await openPickedFolders(async () => [FORK], open, refuse);

  expect(open).toHaveBeenCalledExactlyOnceWith([]);
  expect(refuse).toHaveBeenCalledExactlyOnceWith(RESERVED_PROJECT_NAME_ERROR);
  await expect(fileAsGiga(FORK)).rejects.toThrow(RESERVED_PROJECT_NAME_ERROR);
  expect(vi.mocked(invoke)).not.toHaveBeenCalled();
});

// Defect: an existing project named monocode may be labelled MonoCode.
it("still refuses the label MonoCode on an existing project", () => {
  rememberProject(FORK);

  expect(reservedProjectLabelError(FORK, "MonoCode", HOME)).toBe(
    RESERVED_PROJECT_NAME_ERROR,
  );
});
