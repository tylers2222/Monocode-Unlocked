import { test as base, expect, type Page } from "@playwright/test";
import type { FakeStore } from "./backend";

export { expect };
export type { FakeStore };

export type AppShell = {
  /** Boots the real app (src/main.tsx) on a fake backend seeded with `seed`. */
  mount: (seed?: Partial<FakeStore>) => Promise<void>;
  /** Emits a Tauri event to the app, as the Rust side would. */
  emit: (event: string, payload?: unknown) => Promise<void>;
};

async function mount(page: Page, seed: Partial<FakeStore> = {}) {
  await page.addInitScript((value) => {
    window.appBackendSeed = value;
  }, seed);
  await page.goto("/tests/browser/app.html");
  // A cold Vite cache optimizes deps on the first load, which takes seconds.
  await expect(page.getByRole("button", { name: "Automations" })).toBeVisible({
    timeout: 30_000,
  });
}

export const test = base.extend<{ app: AppShell }>({
  app: async ({ page }, use) => {
    await use({
      mount: (seed) => mount(page, seed),
      emit: (event, payload) =>
        page.evaluate(
          ({ event, payload }) => window.appBackend!.emit(event, payload),
          { event, payload },
        ),
    });
    const unmocked = await page.evaluate(
      () => window.appBackend?.unmocked ?? [],
    );
    expect(
      unmocked,
      "Add these commands to HANDLERS in tests/browser/app/backend.ts",
    ).toEqual([]);
  },
});
