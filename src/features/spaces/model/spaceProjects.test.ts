import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setHomeDir } from "../../../shared/lib/paths";
import {
  archiveProject,
  loadArchivedProjects,
  loadRecents,
  projectRailItems,
  rememberProject,
  type RecentProject,
} from "../../projects/model/recents";
import { pickerProjects } from "../../projects/ui/SearchableProjectPicker";
import {
  archiveProjectInSpace,
  listProjectInSpace,
  openListedProjects,
  projectAfterLeaving,
  restoreProjectInSpace,
  shownProjects,
  spaceProjectKey,
  type SpaceProjectState,
} from "./spaceProjects";
import type { SpaceProjectList } from "./spaces";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => path,
  invoke: vi.fn(async () => "linked"),
}));

const HOME = "/Users/me";
const ROOT = `${HOME}/MonoCode`;
const UNDER = `${ROOT}/gigas/monocode-general-2`;

function recents(...paths: string[]): RecentProject[] {
  return paths.map((path, index) => ({ path, openedAt: 100 - index }));
}

function reply(fields: Partial<SpaceProjectList> = {}): SpaceProjectList {
  return {
    spaceId: "space-2",
    projects: [],
    monoHeld: [],
    noRowIsHere: false,
    assignedElsewhere: [],
    ...fields,
  };
}

const listed = (...paths: string[]) =>
  paths.map((path) => ({ path, assigned: false, linked: true, monos: [] }));

const held = (path: string, ...monos: string[]) => ({
  path,
  assigned: false,
  linked: false,
  monos,
});

const paths = (
  saved: RecentProject[],
  open: string,
  list: SpaceProjectState,
  archived: string[] = [],
) =>
  shownProjects(
    saved,
    open,
    list,
    archived.map((path) => ({ path, archivedAt: 1 })),
    HOME,
  ).map((item) => item.path);

function mockBrowserStorage() {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => data.set(key, value),
    removeItem: (key: string) => data.delete(key),
  });
}

beforeEach(mockBrowserStorage);

describe("shown(S), one row per rule", () => {
  it.each<[string, () => string[], string[]]>([
    [
      "recents in Space keep their order, then other projects, then the open project",
      () =>
        paths(
          recents("/b", "/a", "/out"),
          "/open",
          reply({ projects: listed("/a", "/z", "/b"), assignedElsewhere: ["/out"] }),
        ),
      ["/b", "/a", "/z", "/open"],
    ],
    [
      "P2: a project in Space outside recents, as past the 20 cap, shows",
      () => paths(recents(), "", reply({ projects: listed("/vibtra") })),
      ["/vibtra"],
    ],
    [
      "P1: a shared project shows in Space 2 (Vibtra's goal proof)",
      () =>
        paths(
          recents("/Users/me/Desktop/vibtra"),
          "",
          reply({
            projects: [held("/Users/me/Desktop/vibtra", "Vibtra Orchestrator")],
            monoHeld: ["/Users/me/Desktop/vibtra"],
          }),
        ),
      ["/Users/me/Desktop/vibtra"],
    ],
    [
      "Space 2: a recent with no stored fact is out",
      () => paths(recents("/plain"), "", reply()),
      [],
    ],
    [
      "Space 1: a recent with no row is in",
      () => paths(recents("/plain"), "", reply({ spaceId: "space-1", noRowIsHere: true })),
      ["/plain"],
    ],
    [
      "Space 1: a recent assigned elsewhere is out",
      () =>
        paths(
          recents("/two"),
          "",
          reply({ spaceId: "space-1", noRowIsHere: true, assignedElsewhere: ["/two"] }),
        ),
      [],
    ],
    [
      "Space 1: a never-seen path is on no list",
      () => paths(recents(), "", reply({ spaceId: "space-1", noRowIsHere: true })),
      [],
    ],
    [
      "P7: the open project already in Space shows only once",
      () => paths(recents("/in"), "/in/", reply({ projects: listed("/in") })),
      ["/in"],
    ],
    [
      "T13: until the first answer only the open project shows",
      () => paths(recents("/a", "/b"), "/b", "loading"),
      ["/b"],
    ],
    [
      "T13: with nothing open, nothing shows until the first answer",
      () => paths(recents("/a"), "", "loading"),
      [],
    ],
    [
      "T14, I8: a failed read shows every project",
      () => paths(recents("/a", "/b"), "/open", "failed"),
      ["/a", "/b", "/open"],
    ],
    [
      "I10: /a, /a/ and ~/a are one project",
      () =>
        paths(
          recents(`${HOME}/a/`),
          "~/a",
          reply({ projects: listed(`${HOME}/a`) }),
        ),
      [`${HOME}/a/`],
    ],
    [
      "I10: a Windows drive path in another case is the same Space member",
      () => paths(recents("C:/Work/App"), "", reply({ projects: listed("c:/work/app/") })),
      ["C:/Work/App"],
    ],
    [
      "I10: a UNC path in another case is the same Space member",
      () =>
        paths(
          recents("//Server/Share/App"),
          "",
          reply({ projects: listed("//server/share/app") }),
        ),
      ["//Server/Share/App"],
    ],
    [
      "F6: an archived project with a stored fact stays hidden",
      () => paths(recents(), "", reply({ projects: listed("/a") }), ["/a"]),
      [],
    ],
    [
      "F6, I1: an archived project a Mono in S lists shows",
      () =>
        paths(
          recents(),
          "",
          reply({ projects: [held("/a", "Site Builder")], monoHeld: ["/a"] }),
          ["/a"],
        ),
      ["/a"],
    ],
    [
      "F1: the Giga root needs a Mono in S, whatever else lists it",
      () => paths(recents(ROOT), "", reply({ projects: listed(ROOT) })),
      [],
    ],
    [
      "F1: the Giga root is out of Space 1 with no Mono, before the no-row rule",
      () => paths(recents(ROOT), "", reply({ noRowIsHere: true })),
      [],
    ],
    [
      "R1: the Giga root shows with a Mono in S",
      () =>
        paths(
          recents(),
          "",
          reply({ projects: [held(ROOT, "General")], monoHeld: [ROOT] }),
        ),
      [ROOT],
    ],
    [
      "F1, R1: a path under the root is never listed, even with a Mono or a row",
      () =>
        paths(
          recents(UNDER),
          "",
          reply({ projects: [held(UNDER, "General")], monoHeld: [UNDER] }),
        ),
      [],
    ],
    [
      "F1: a path under the root stays out on a failed read",
      () => paths(recents(UNDER, "/a"), "", "failed"),
      ["/a"],
    ],
    [
      "A window on a path under the root shows that row while open (General's chat)",
      () => paths(recents(), UNDER, reply()),
      [UNDER],
    ],
    [
      "A remote project has a key and follows V: in Space it shows",
      () =>
        paths(recents("remote://box/a"), "", reply({ projects: listed("remote://box/a") })),
      ["remote://box/a"],
    ],
    [
      "A remote project with no fact in S is out",
      () => paths(recents("remote://box/a"), "", reply()),
      [],
    ],
  ])("%s", (_name, run, expected) => {
    expect(run()).toEqual(expected);
  });
});

describe("the window's invariants", () => {
  it("I1: a Mono's project is in rail and picker, whatever recents holds", () => {
    const list = reply({
      projects: [held("/vibtra", "Vibtra Orchestrator")],
      monoHeld: ["/vibtra"],
    });
    for (const saved of [recents(), recents("/other"), recents("/vibtra")]) {
      const shown = shownProjects(saved, "/open", list, [], HOME);
      expect(projectRailItems(shown, "/open").map((item) => item.path)).toContain("/vibtra");
      expect(pickerProjects(shown, "/open").map((item) => item.path)).toContain("/vibtra");
    }
  });

  it("P7: rail and picker list the same projects, open one included", () => {
    const list = reply({ projects: listed("/a") });
    const shown = shownProjects(recents("/a", "/out"), "/open", list, [], HOME);
    const set = (items: RecentProject[]) => items.map((item) => item.path).sort();
    expect(set(projectRailItems(shown, "/open"))).toEqual(set(pickerProjects(shown, "/open")));
    expect(set(pickerProjects(shown, "/open"))).toEqual(["/a", "/open"]);
  });

  it("I3: the window's list says V exactly for A, L or M, in every combination", () => {
    for (const assigned of [false, true]) {
      for (const linked of [false, true]) {
        for (const mono of [false, true]) {
          const v = assigned || linked || mono;
          const list = reply({
            projects: v
              ? [{ path: "/p", assigned, linked, monos: mono ? ["M"] : [] }]
              : [],
            monoHeld: mono ? ["/p"] : [],
            assignedElsewhere: v ? [] : ["/p"],
          });
          expect(paths(recents("/p"), "", list)).toEqual(v ? ["/p"] : []);
          expect(paths(recents(), "", list)).toEqual(v ? ["/p"] : []);
        }
      }
    }
  });

  it("gives a project outside recents the oldest place, after every recent", () => {
    const shown = shownProjects(recents("/a"), "/open", reply({ projects: listed("/a", "/z") }), [], HOME);
    expect(shown.find((item) => item.path === "/z")?.openedAt).toBe(0);
    expect(projectRailItems(shown, "/open").map((item) => item.path)).toEqual(["/a", "/z", "/open"]);
  });

  it("leaves ~/a alone before the home folder is known", () => {
    expect(spaceProjectKey("~/a", undefined)).toBe("~/a");
    expect(spaceProjectKey("~", HOME)).toBe("~");
    expect(spaceProjectKey("~/a", HOME)).toBe(`${HOME}/a`);
    expect(spaceProjectKey("/a/", HOME)).toBe("/a");
  });

  it("I10: keys a Windows drive or UNC path without its case", () => {
    expect(spaceProjectKey("C:/A/", HOME)).toBe("c:/a");
    expect(spaceProjectKey("//Server/Share", HOME)).toBe("//server/share");
  });
});

describe("the move off a project (H1)", () => {
  const list = reply({ projects: listed("/gone", "/here") });

  afterEach(() => setHomeDir(undefined));

  it("walks shown(S), never a recent out of Space", () => {
    expect(projectAfterLeaving(recents("/out", "/gone", "/here"), "/gone", list)?.path).toBe("/here");
  });

  it("can land on a project outside recents", () => {
    expect(projectAfterLeaving(recents("/gone"), "/gone", list)?.path).toBe("/here");
  });

  it("has nowhere to go when the Space has nothing else", () => {
    expect(projectAfterLeaving(recents("/gone"), "/gone", reply({ projects: listed("/gone") }))).toBeUndefined();
  });

  it("is the same project however the path is written", () => {
    expect(projectAfterLeaving(recents(), "/gone/", reply({ projects: listed("/gone") }))).toBeUndefined();
  });

  it("is the same project when the path left is written ~/a", () => {
    setHomeDir(HOME);
    expect(
      projectAfterLeaving(recents(`${HOME}/a`), "~/a", reply({ projects: listed(`${HOME}/a`) })),
    ).toBeUndefined();
  });

  it("skips a row that is not a project folder, as the home folder", () => {
    expect(projectAfterLeaving(recents(), "/gone", reply({ projects: listed(HOME, "/here") }))?.path).toBe("/here");
  });
});

describe("listing a path the user added (T1, T2, H1)", () => {
  beforeEach(() => {
    vi.stubGlobal("window", new EventTarget());
    vi.mocked(invoke).mockClear();
  });

  it("tells the backend whether the window knew it, from recents and archive", async () => {
    await expect(listProjectInSpace("/new")).resolves.toBe(true);
    rememberProject("/seen");
    await listProjectInSpace("/seen");
    rememberProject("/old");
    archiveProject("/old");
    await listProjectInSpace("/old");
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["spaces_project_add", { projectPath: "/new", knownToWindow: false }],
      ["spaces_project_add", { projectPath: "/seen", knownToWindow: true }],
      ["spaces_project_add", { projectPath: "/old", knownToWindow: true }],
    ]);
  });

  it("answers false on a refusal, so a restore keeps the archive entry", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.mocked(invoke).mockRejectedValueOnce({ code: "workspace", message: "stays" });
    await expect(listProjectInSpace("/a")).resolves.toBe(false);
    expect(warn).toHaveBeenCalledWith("monocode: /a was not listed:", {
      code: "workspace",
      message: "stays",
    });
    warn.mockRestore();
  });
});

describe("the window's project actions, with the Space (T1, T2)", () => {
  const ALPHA = "/code/alpha";
  const BETA = "/code/beta";
  const NOTIFY = ["spaces_project_notify_changed"];

  beforeEach(() => {
    vi.stubGlobal("window", new EventTarget());
    vi.mocked(invoke).mockClear();
  });

  it("Open folder lists each picked path, opens them all, then tells other windows", () => {
    const open = vi.fn();
    openListedProjects(open)([ALPHA, BETA]);
    expect(open).toHaveBeenCalledExactlyOnceWith([ALPHA, BETA]);
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["spaces_project_add", { projectPath: ALPHA, knownToWindow: false }],
      ["spaces_project_add", { projectPath: BETA, knownToWindow: false }],
      NOTIFY,
    ]);
  });

  it("Archive moves the project to the archive and tells other windows", () => {
    rememberProject(BETA);
    rememberProject(ALPHA);
    vi.mocked(invoke).mockClear();
    expect(archiveProjectInSpace(ALPHA).map((item) => item.path)).toEqual([BETA]);
    expect(loadArchivedProjects().map((item) => item.path)).toEqual([ALPHA]);
    expect(vi.mocked(invoke).mock.calls).toEqual([NOTIFY]);
  });

  it("Restore remembers, tells other windows and selects once the Space lists it", async () => {
    const remembered = vi.fn();
    const select = vi.fn();
    await restoreProjectInSpace(ALPHA, remembered, select);
    expect(remembered).toHaveBeenCalledExactlyOnceWith(loadRecents());
    expect(loadRecents().map((item) => item.path)).toEqual([ALPHA]);
    expect(select).toHaveBeenCalledExactlyOnceWith(ALPHA);
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["spaces_project_add", { projectPath: ALPHA, knownToWindow: false }],
      NOTIFY,
    ]);
  });

  it("Restore changes nothing when the Space refuses the project", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.mocked(invoke).mockRejectedValueOnce({ code: "workspace", message: "stays" });
    const remembered = vi.fn();
    const select = vi.fn();
    await restoreProjectInSpace(ALPHA, remembered, select);
    expect(remembered).not.toHaveBeenCalled();
    expect(select).not.toHaveBeenCalled();
    expect(loadRecents()).toEqual([]);
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["spaces_project_add", { projectPath: ALPHA, knownToWindow: false }],
    ]);
  });
});
