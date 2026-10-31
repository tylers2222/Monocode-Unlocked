import type { Block } from "../../src/features/sessions/model/session";
import { expect, test } from "./app/fixture";

// ADE-140/141 frames A1-A3: this window is window 2. Rust's answer to
// `control_loaded_sessions` names the chats window 1 ("main") holds; the
// scenarios in src-tauri/src/tests/scenario/window_holds.rs prove that answer
// against the real commands.

const PROJECT = "/Users/test/acme";
const TAB_CHAT = "tab-chat-0001";
const MONO_CHAT = "mono-chat-0001";

const said = (id: string, text: string): Block[] => [
  { id: `${id}-u`, role: "user", text: "Rebase lane d and rerun the store tests." },
  { id: `${id}-a`, role: "assistant", text },
];

const session = (id: string, title: string) => ({
  id,
  cwd: PROJECT,
  harness: "claude" as const,
  model: "sonnet",
  runtimeMode: "supervised" as const,
  title,
  createdAt: 1,
  updatedAt: 1,
});

test("a chat another window holds is read-only here until that window lets go of it", async ({
  app,
  page,
}) => {
  await page.addInitScript(() => {
    (globalThis as { heldBy?: Record<string, string> }).heldBy = {
      "tab-chat-0001": "main",
      "mono-chat-0001": "main",
    };
    // The Mono open when the app last closed, which every window reopens.
    localStorage.setItem("monocode:open-mono", "mono-1");
  });
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [
        session(TAB_CHAT, "Process fixes, lane d"),
        session(MONO_CHAT, "Pip"),
      ],
      blocks: {
        [TAB_CHAT]: said("t", "Rebased cleanly. Running the store suite now."),
        [MONO_CHAT]: said("m", "Starting round 2 on the new head."),
      },
      monos: [
        {
          id: "mono-1",
          role: "mono",
          data: {
            name: "Pip",
            mascot: "mushroom",
            sessionId: MONO_CHAT,
            projects: [PROJECT],
          },
        },
      ],
    },
    {
      handlers: {
        control_loaded_sessions: (_store, { sessions }) => {
          const held =
            (globalThis as { heldBy?: Record<string, string> }).heldBy ?? {};
          return Object.fromEntries(
            (sessions as string[])
              .filter((id) => held[id])
              .map((id) => [id, held[id]]),
          );
        },
      },
    },
  );
  const main = page.getByRole("main");
  const notice = main.getByRole("status").filter({ hasText: "Open in another window" });

  // A1: the reopened Mono's chat. The title says where it runs; a notice
  // stands in for the composer.
  await expect(main.getByText("Starting round 2 on the new head.")).toBeVisible();
  await expect(page.locator("[data-mono-title] [data-mono-status]")).toHaveText("In another window");
  await expect(notice).toBeVisible();
  await expect(notice).toContainText("Go there to see it live or send.");
  await expect(page.getByPlaceholder("Message Pip")).toHaveCount(0);
  await page.screenshot({ path: test.info().outputPath("a1.png") });

  // A2: a copied tab chat. The row says where it runs; the same notice.
  await page.getByText("acme", { exact: true }).click();
  await page.getByText("Process fixes, lane d", { exact: true }).click();
  await expect(main.getByText("Running the store suite now.")).toBeVisible();
  await expect(notice).toBeVisible();
  await expect(page.getByPlaceholder("Ask, build, / for commands")).toHaveCount(0);
  await expect(
    page.locator("[data-session-card]", { hasText: "Process fixes, lane d" }),
  ).toContainText("In another window");
  await page.screenshot({ path: test.info().outputPath("a2.png") });

  // "Go to that window" asks the holding window to open the chat, as a
  // notification click does there; this window does not act on it.
  await page.evaluate(() => {
    const tauri = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke(command: string, args?: unknown, options?: unknown): Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const sent: unknown[] = [];
    (window as unknown as { goTo: unknown[] }).goTo = sent;
    const invoke = tauri.invoke.bind(tauri);
    tauri.invoke = (command, args, options) => {
      if (command === "plugin:event|emit_to") sent.push(args);
      return invoke(command, args, options);
    };
  });
  await notice.getByRole("button", { name: "Go to that window" }).click();
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { goTo: unknown[] }).goTo))
    .toEqual([
      expect.objectContaining({
        target: expect.objectContaining({ label: "main" }),
        event: "monocode:notification-click",
        payload: TAB_CHAT,
      }),
    ]);
  await expect(notice, "this window stays on the chat, read-only").toBeVisible();

  await page.getByRole("button", { name: /^Pip, / }).click();
  await expect(main.getByText("Starting round 2 on the new head.")).toBeVisible();

  // A3: window 1 closes. Window 2 holds the chat, which comes back as window 1
  // saved it, and the composer returns. No button to press.
  await page.evaluate(
    ({ id, blocks }) => {
      window.appBackend!.store.blocks[id] = blocks;
      (globalThis as { heldBy?: Record<string, string> }).heldBy = {};
    },
    { id: MONO_CHAT, blocks: said("m", "Round 2 is clean. Merging next.") },
  );
  await app.emit("control-holds-changed");
  await expect(main.getByText("Round 2 is clean. Merging next.")).toBeVisible();
  await expect(notice).toHaveCount(0);
  await expect(page.getByPlaceholder("Message Pip")).toBeVisible();
  await expect(page.locator("[data-mono-title] [data-mono-status]")).not.toHaveText("In another window");
  await page.screenshot({ path: test.info().outputPath("a3.png") });
});
