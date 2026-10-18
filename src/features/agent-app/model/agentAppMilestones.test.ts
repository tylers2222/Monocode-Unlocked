import { expect, it } from "vitest";
import { newSession } from "../../sessions/model/session";
import type {
  Milestone,
  MilestoneSettings,
  Writer,
} from "../../milestones/model/milestones";
import { monoContext, type MonoFiles } from "../../monos/model/monoFiles";
import { handleAgentApp, type AgentAppHost } from "./agentApp";
import { appErrorResponse } from "./appActionError";

const APP = "/code/app";
const SITE = "/code/site";

/**
 * The Rust commands as `milestones.rs` answers them: a bare code or
 * `invalid_input: <reason>` on refusal, and agent writers refused while off.
 */
function rustMilestones(off: string[] = []) {
  const rows: Milestone[] = [];
  const settings = (project: string): MilestoneSettings => ({
    enabled: !off.includes(project),
    hiddenStatuses: [],
  });
  const writable = (project: string, by: Writer) => {
    if (by !== "user" && !settings(project).enabled) throw "milestones_off";
  };
  const find = (project: string, id: string) => {
    const row = rows.find((r) => r.id === id && r.projectPath === project);
    if (!row) throw "unknown_milestone";
    return row;
  };
  const inProject = (project: string) =>
    rows
      .filter((row) => row.projectPath === project)
      .sort((a, b) => a.order - b.order);
  const milestones: NonNullable<AgentAppHost["milestones"]> = {
    list: async (project) => ({
      settings: settings(project),
      milestones: inProject(project),
    }),
    add: async (project, milestone, by) => {
      writable(project, by);
      if (milestone.title.length > 200)
        throw "invalid_input: title must be 1 to 200 characters";
      const row: Milestone = {
        id: `m${rows.length + 1}`,
        projectPath: project,
        title: milestone.title,
        note: milestone.note ?? "",
        status: milestone.status ?? "idea",
        target: milestone.target ?? null,
        order: inProject(project).length,
        createdBy: by,
        createdAt: 1,
        updatedAt: 1,
      };
      rows.push(row);
      return row;
    },
    update: async (project, id, patch, by) => {
      writable(project, by);
      return Object.assign(find(project, id), patch, { updatedAt: 2 });
    },
    remove: async (project, id, by) => {
      writable(project, by);
      rows.splice(rows.indexOf(find(project, id)), 1);
    },
    reorder: async (project, ids, by) => {
      writable(project, by);
      const current = inProject(project).map((row) => row.id);
      if ([...ids].sort().join() !== [...current].sort().join())
        throw "stale_order";
      ids.forEach((id, order) => (find(project, id).order = order));
      return inProject(project);
    },
  };
  return { rows, milestones };
}

/** A plain session in APP, or the Mono "mono-1" with APP and SITE. */
function fixture(kind: "session" | "mono", off: string[] = []) {
  const source = newSession(
    "codex",
    kind === "session" ? APP : "/Users/me",
    "codex:test",
  );
  source.id = "caller";
  const rust = rustMilestones(off);
  const host = {
    isMono: (id: string) => kind === "mono" && id === source.id,
    monoOf: (id: string) =>
      kind === "mono" && id === source.id
        ? { id: "mono-1", name: "Alpha", projects: [APP, SITE] }
        : undefined,
    milestones: rust.milestones,
  } as unknown as AgentAppHost;
  const run = (action: string, input: Record<string, unknown> = {}) =>
    handleAgentApp(source, "req-1", action, input, host);
  return { run, rows: rust.rows };
}

/** What the CLI prints for a call that fails. */
async function failure(call: Promise<unknown>) {
  return appErrorResponse(
    await call.then(
      () => {
        throw new Error("the call succeeded");
      },
      (error) => error,
    ),
  );
}

it("a session adds to its own project as session:<id>", async () => {
  const { run, rows } = fixture("session");
  const added = await run("milestones.add", {
    title: "Ship the harness",
    note: "After M-5",
  });
  expect(added).toEqual({
    id: "m1",
    title: "Ship the harness",
    note: "After M-5",
    status: "idea",
    target: null,
    order: 0,
    createdBy: "session:caller",
    createdAt: 1,
    updatedAt: 1,
  });
  expect(rows.map((row) => [row.projectPath, row.createdBy])).toEqual([
    [APP, "session:caller"],
  ]);
  expect(await run("milestones.list")).toEqual({
    project: APP,
    settings: { enabled: true, hiddenStatuses: [] },
    milestones: [added],
  });
});

it("a session that names a project is refused and writes nothing", async () => {
  const { run, rows } = fixture("session");
  expect(
    await failure(run("milestones.add", { title: "X", project: SITE })),
  ).toMatchObject({
    code: "invalid_input",
    hint: "project is only for a Mono",
  });
  expect(rows).toEqual([]);
});

it("a Mono adds to a roster project by name as mono:<id>", async () => {
  const { run, rows } = fixture("mono");
  await run("milestones.add", { title: "Dark mode", project: "site" });
  expect(rows.map((row) => [row.projectPath, row.createdBy])).toEqual([
    [SITE, "mono:mono-1"],
  ]);
});

it("a Mono lists and removes in a project it names", async () => {
  const { run, rows } = fixture("mono");
  await run("milestones.add", { title: "On site", project: SITE });
  await run("milestones.add", { title: "On app", project: APP });
  expect(await run("milestones.list", { project: SITE })).toMatchObject({
    project: SITE,
    milestones: [{ id: "m1", title: "On site" }],
  });
  expect(await run("milestones.remove", { id: "m1", project: SITE })).toEqual({
    removed: "m1",
  });
  expect(rows.map((row) => row.title)).toEqual(["On app"]);
});

it("the Mono prompt's milestones commands work for a Mono with two projects", async () => {
  const look = {
    name: "Alpha",
    mascot: "crab",
    color: "#fff",
    projects: [
      { path: APP, name: "app" },
      { path: SITE, name: "site" },
    ],
  };
  const files = { soul: "", memory: "", topics: [] } as unknown as MonoFiles;
  const prompt = monoContext(look, files, { soul: true, memory: false });
  const commands = [
    ...prompt.matchAll(/app (milestones\.\w+) --json '([^']*)'/g),
  ].map(([, action, json]) => ({
    action,
    input: JSON.parse(
      json.replace("<path or name>", "site").replace("...", "Dark mode"),
    ) as Record<string, unknown>,
  }));
  expect(commands.map((command) => command.action)).toEqual([
    "milestones.add",
    "milestones.list",
  ]);
  const { run } = fixture("mono");
  for (const { action, input } of commands) await run(action, input);
  expect(await run("milestones.list", { project: SITE })).toMatchObject({
    milestones: [{ title: "Dark mode", createdBy: "mono:mono-1" }],
  });
});

it("a Mono naming a project outside its roster is refused", async () => {
  const { run, rows } = fixture("mono");
  expect(
    await failure(
      run("milestones.add", { title: "X", project: "/code/other" }),
    ),
  ).toMatchObject({
    code: "invalid_input",
    hint: "Not one of your projects. Yours: app (/code/app), site (/code/site)",
  });
  expect(rows).toEqual([]);
});

it.each([
  "2026-13-01",
  "2026-02-30",
  "26-01-01",
  "later",
  20261201,
  " ",
  0,
  false,
])("target %s is invalid_input and writes nothing", async (target) => {
  const { run, rows } = fixture("session");
  expect(
    await failure(run("milestones.add", { title: "X", target })),
  ).toMatchObject({
    code: "invalid_input",
    hint: 'target must be a real YYYY-MM-DD date, or "" or null for later',
  });
  expect(rows).toEqual([]);
});

// M-9 R1. Defect: an agent sending target "" (Later, as the page means it)
// is refused, stores "", or "" on update keeps the old date.
it('target "" and null both mean Later on add and clear the date on update', async () => {
  const { run, rows } = fixture("session");
  await run("milestones.add", { title: "Empty", target: "" });
  await run("milestones.add", { title: "Null", target: null });
  await run("milestones.add", { title: "Dated", target: "2026-12-12" });
  expect(rows.map((row) => [row.title, row.target])).toEqual([
    ["Empty", null],
    ["Null", null],
    ["Dated", "2026-12-12"],
  ]);
  expect(
    await run("milestones.update", { id: "m3", target: "" }),
  ).toMatchObject({ id: "m3", target: null });
  await run("milestones.update", { id: "m3", target: "2026-12-12" });
  expect(
    await run("milestones.update", { id: "m3", target: null }),
  ).toMatchObject({ id: "m3", target: null });
});

it.each([
  [{}, "title is required"],
  [{ title: 5 }, "title must be a string"],
  [{ title: "X", note: ["a"] }, "note must be a string"],
  [
    { title: "X", status: "doing" },
    "status must be one of idea, planned, in_progress, done",
  ],
])("add %j is invalid_input before it reaches Rust", async (input, hint) => {
  const { run, rows } = fixture("session");
  expect(await failure(run("milestones.add", input))).toMatchObject({
    code: "invalid_input",
    hint,
  });
  expect(rows).toEqual([]);
});

it("the writer comes from the caller, so by and createdBy are refused", async () => {
  const { run, rows } = fixture("session");
  expect(
    await failure(run("milestones.add", { title: "X", createdBy: "user" })),
  ).toMatchObject({
    code: "invalid_input",
    hint: "Unknown milestones.add fields: createdBy",
  });
  await run("milestones.add", { title: "Kept" });
  for (const [action, input] of [
    ["milestones.update", { id: "m1", title: "Changed", by: "user" }],
    ["milestones.remove", { id: "m1", by: "user" }],
    ["milestones.reorder", { ids: ["m1"], by: "user" }],
  ] as const) {
    expect(await failure(run(action, input))).toMatchObject({
      code: "invalid_input",
      hint: `Unknown ${action} fields: by`,
    });
  }
  expect(rows.map((row) => [row.title, row.createdBy])).toEqual([
    ["Kept", "session:caller"],
  ]);
});

it.each([
  ["milestones.add", { title: "X" }],
  ["milestones.update", { id: "m1", status: "done" }],
  ["milestones.remove", { id: "m1" }],
  ["milestones.reorder", { ids: ["m1"] }],
])(
  "%s is refused with milestones_off while the project is off",
  async (action, input) => {
    const off = fixture("session", [APP]);
    const on = fixture("session");
    await on.run("milestones.add", { title: "Before" });
    // The same row exists in the off project, written by the page.
    off.rows.push({ ...on.rows[0] });
    expect(await failure(off.run(action, input))).toEqual({
      ok: false,
      code: "milestones_off",
      hint: "The user turned milestones off for this project, so nothing was changed. Do not retry; milestones.list still reads it",
      error:
        "The user turned milestones off for this project, so nothing was changed. Do not retry; milestones.list still reads it",
    });
    expect(off.rows).toEqual(on.rows);
    expect(await off.run("milestones.list")).toMatchObject({
      settings: { enabled: false },
      milestones: [{ id: "m1", title: "Before" }],
    });
  },
);

it("Rust refusals keep their codes", async () => {
  const { run } = fixture("mono");
  await run("milestones.add", { title: "On site", project: SITE });
  await run("milestones.add", { title: "On app", project: APP });
  expect(
    await failure(
      run("milestones.update", { id: "m1", project: APP, title: "Y" }),
    ),
  ).toMatchObject({ code: "unknown_milestone" });
  expect(
    await failure(
      run("milestones.reorder", { ids: ["m1", "m2"], project: APP }),
    ),
  ).toMatchObject({ code: "stale_order" });
  expect(
    await failure(
      run("milestones.add", { title: "x".repeat(201), project: APP }),
    ),
  ).toMatchObject({
    code: "invalid_input",
    hint: "title must be 1 to 200 characters",
  });
});

it("update, reorder and remove answer with the changed rows", async () => {
  const { run } = fixture("session");
  await run("milestones.add", { title: "First" });
  await run("milestones.add", { title: "Second" });
  expect(
    await run("milestones.update", {
      id: "m1",
      status: "in_progress",
      target: "2026-12-01",
    }),
  ).toMatchObject({ id: "m1", status: "in_progress", target: "2026-12-01" });
  expect(
    await run("milestones.update", { id: "m1", target: null }),
  ).toMatchObject({ id: "m1", target: null });
  const reordered = await run("milestones.reorder", { ids: ["m2", "m1"] });
  expect(reordered).toMatchObject({
    project: APP,
    milestones: [
      { id: "m2", order: 0 },
      { id: "m1", order: 1 },
    ],
  });
  expect(await run("milestones.remove", { id: "m2" })).toEqual({
    removed: "m2",
  });
});

it("add keeps the status and target it was given", async () => {
  const { run, rows } = fixture("session");
  expect(
    await run("milestones.add", {
      title: "Voice notes",
      status: "in_progress",
      target: "2026-12-01",
    }),
  ).toMatchObject({ status: "in_progress", target: "2026-12-01" });
  expect(rows.map((row) => [row.status, row.target])).toEqual([
    ["in_progress", "2026-12-01"],
  ]);
});

it.each([["m1"], [[1]], [null]])(
  "reorder with ids %j is invalid_input and changes nothing",
  async (ids) => {
    const { run, rows } = fixture("session");
    await run("milestones.add", { title: "First" });
    expect(await failure(run("milestones.reorder", { ids }))).toMatchObject({
      code: "invalid_input",
      hint: "ids must be an array of milestone ids",
    });
    expect(rows.map((row) => row.order)).toEqual([0]);
  },
);
