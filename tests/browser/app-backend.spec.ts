import { expect, test } from "./app/fixture";

const PROJECT = "/Users/test/acme";

test("a command with no handler is still reported after a reload", async ({
  app,
  page,
}) => {
  await app.mount();
  await page.evaluate(() =>
    window.__TAURI_INTERNALS__.invoke("probe_unmocked").catch(() => {}),
  );
  await page.reload();
  await expect(page.getByRole("button", { name: "Automations" })).toBeVisible();

  expect(await app.takeReports()).toEqual([
    { command: "probe_unmocked", problem: "no handler" },
  ]);
});

test("a handler that throws or answers with uncloneable data is reported", async ({
  app,
  page,
}) => {
  await app.mount(
    {},
    {
      handlers: {
        probe_throws: () => {
          throw new TypeError("bad args");
        },
        probe_function: () => () => 1,
      },
    },
  );
  const answers = await page.evaluate(async () => {
    const settle = (command: string) =>
      window.__TAURI_INTERNALS__.invoke(command, {}).then(
        () => "resolved",
        () => "rejected",
      );
    return [await settle("probe_throws"), await settle("probe_function")];
  });

  expect(answers).toEqual(["rejected", "rejected"]);
  expect(await app.takeReports()).toEqual([
    { command: "probe_throws", problem: "TypeError: bad args" },
    {
      command: "probe_function",
      problem: expect.stringMatching(/^DataCloneError/),
    },
  ]);
});

test("a test's own async handlers answer once they settle", async ({
  app,
  page,
}) => {
  await app.mount(
    {
      monos: [
        {
          id: "mono-1",
          role: "mono",
          data: { name: "Pip", mascot: "ghost", projects: [PROJECT] },
        },
      ],
    },
    {
      handlers: {
        monos_list: async (store) => ({
          revision: 1,
          monos: store.monos.map((mono) => ({
            ...mono,
            data: { ...mono.data, name: "Async Pip" },
          })),
        }),
        probe_rejects: async () => {
          throw "Claude Code CLI not found";
        },
      },
    },
  );
  await expect(page.getByText("Async Pip")).toBeVisible();

  const rejection = await page.evaluate(() =>
    window.__TAURI_INTERNALS__.invoke("probe_rejects").catch(String),
  );
  expect(
    rejection,
    "a string rejection is Rust's Err, not a harness error",
  ).toBe("Claude Code CLI not found");
});

test("answers have the shapes the Rust commands send", async ({
  app,
  page,
}) => {
  await app.mount({
    projects: [PROJECT],
    sessions: [
      {
        id: "s-old",
        cwd: PROJECT,
        harness: "claude",
        model: "sonnet",
        runtimeMode: "supervised",
        title: "Old",
        createdAt: 1,
        updatedAt: 1,
      },
      {
        id: "s-b",
        cwd: PROJECT,
        harness: "claude",
        model: "sonnet",
        runtimeMode: "supervised",
        title: "New B",
        createdAt: 1,
        updatedAt: 5,
        pinned: true,
        branch: "feat/b",
      },
      {
        id: "s-a",
        cwd: PROJECT,
        harness: "claude",
        model: "sonnet",
        runtimeMode: "supervised",
        title: "New A",
        createdAt: 1,
        updatedAt: 5,
      },
      {
        id: "s-removed",
        cwd: PROJECT,
        harness: "claude",
        model: "sonnet",
        runtimeMode: "supervised",
        title: "Removed",
        createdAt: 1,
        updatedAt: 3,
        branch: "feat/gone",
        worktreeRemoved: true,
      },
      {
        id: "s-sub",
        cwd: `${PROJECT}/packages/web`,
        harness: "claude",
        model: "sonnet",
        runtimeMode: "supervised",
        title: "Sub",
        createdAt: 1,
        updatedAt: 1,
      },
    ],
    monos: [
      { id: "mono-1", role: "mono", data: { name: " Pip ", mascot: "ghost" } },
      { id: "mono-2", role: "mono", data: { mascot: "mushroom" } },
    ],
  });
  const answers = await page.evaluate(async (cwd) => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    return {
      monos: await invoke("monos_list", {}),
      sessions: await invoke("session_list_by_project", { cwd }),
      branches: await invoke("git_branches", { cwd }),
      worktrees: await invoke("git_worktrees", { cwd }),
    };
  }, PROJECT);

  // mono_roster.rs:101-110 MonoRow; `name` from display_name.
  expect(answers.monos).toEqual({
    revision: 1,
    monos: [
      {
        id: "mono-1",
        name: "Pip",
        role: "mono",
        data: { name: " Pip ", mascot: "ghost" },
        sortOrder: 0,
        createdAt: 1,
        updatedAt: 1,
      },
      {
        id: "mono-2",
        name: "MonoShroom",
        role: "mono",
        data: { mascot: "mushroom" },
        sortOrder: 1,
        createdAt: 1,
        updatedAt: 1,
      },
    ],
  });
  // session_store.rs list_by_project: ORDER BY updated_at DESC, id ASC, the
  // SessionSummary fields without skip_serializing_if, and `branch` from the
  // row or the repo unless the worktree was removed (1714-1718).
  const always = {
    sidebarHidden: false,
    worktreeRemoved: false,
    additions: 0,
    deletions: 0,
    archived: false,
    pinned: false,
    draft: false,
  };
  expect(answers.sessions).toEqual([
    {
      ...always,
      id: "s-a",
      cwd: PROJECT,
      harness: "claude",
      model: "sonnet",
      runtimeMode: "supervised",
      title: "New A",
      createdAt: 1,
      updatedAt: 5,
      branch: "main",
    },
    {
      ...always,
      id: "s-b",
      cwd: PROJECT,
      harness: "claude",
      model: "sonnet",
      runtimeMode: "supervised",
      title: "New B",
      createdAt: 1,
      updatedAt: 5,
      pinned: true,
      branch: "feat/b",
    },
    {
      ...always,
      id: "s-removed",
      cwd: PROJECT,
      harness: "claude",
      model: "sonnet",
      runtimeMode: "supervised",
      title: "Removed",
      createdAt: 1,
      updatedAt: 3,
      worktreeRemoved: true,
    },
    {
      ...always,
      id: "s-old",
      cwd: PROJECT,
      harness: "claude",
      model: "sonnet",
      runtimeMode: "supervised",
      title: "Old",
      createdAt: 1,
      updatedAt: 1,
      branch: "main",
    },
  ]);
  // fs.rs git_branches_for: the current branch is in the list.
  expect(answers.branches).toEqual({
    current: "main",
    detached: false,
    branches: [{ name: "main", current: true, remote: null }],
  });
  // worktrees.rs git_worktrees: the main worktree, default_root beside it;
  // sessionIds holds live sessions inside it, subfolders too (103-121).
  expect(answers.worktrees).toEqual({
    worktrees: [
      expect.objectContaining({
        path: PROJECT,
        branch: "main",
        isMain: true,
        sessionIds: ["s-old", "s-b", "s-a", "s-sub"],
      }),
    ],
    defaultRoot: `${PROJECT}-worktrees`,
  });
});

test("a handler written as a method fails mount and names the command", async ({
  app,
}) => {
  await expect(
    app.mount(
      {},
      {
        handlers: {
          probe_arrow: () => "kept",
          monos_list() {
            return { revision: 99, monos: [] };
          },
        },
      },
    ),
  ).rejects.toThrow(
    'handler "monos_list" must be an arrow or function expression, not a method',
  );
});

test("mount renders the macOS app unless a test asks for Linux", async ({
  app,
  page,
}) => {
  await app.mount();
  await expect(page.getByText("⌘K")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Minimize window" }),
  ).toHaveCount(0);

  await app.mount({}, { platform: "linux" });
  await expect(
    page.getByRole("button", { name: "Minimize window" }),
  ).toBeVisible();
});

test.fail(
  "the fixture fails a test that sent a command with no handler",
  async ({ app, page }) => {
    await app.mount();
    await page.evaluate(() =>
      window.__TAURI_INTERNALS__.invoke("probe_unmocked").catch(() => {}),
    );
  },
);
