import { test as base, expect, type Page } from "@playwright/test";
import type {
  BackendReport,
  FakeSeed,
  Handler,
  SteerlessHarness,
} from "./backend";

export { expect };
export type { BackendReport, FakeSeed, Handler };

/** `navigator.platform`, which the app reads to pick its window chrome. */
const PLATFORMS = { macos: "MacIntel", linux: "Linux x86_64" };

export type MountOptions = {
  /** Defaults to macOS, the app the design frames show. */
  platform?: keyof typeof PLATFORMS;
  /**
   * Handlers that replace or add to the defaults for this test. They run in
   * the page, so each must be an arrow or function expression that uses only
   * its arguments and page globals, not variables from the test.
   */
  handlers?: Record<string, Handler>;
  /**
   * Harnesses whose adapter the app sees with `canSteer: false`. No shipped
   * adapter both reports a ready turn and refuses steers, so this is how a
   * test reaches the app's own can-steer checks on a running turn.
   */
  cannotSteer?: SteerlessHarness[];
};

export type AppShell = {
  /** Boots the real app (src/main.tsx) on a fake backend seeded with `seed`. */
  mount: (seed?: FakeSeed, options?: MountOptions) => Promise<void>;
  /** Emits a Tauri event to the app, as the Rust side would. */
  emit: (event: string, payload?: unknown) => Promise<void>;
  /** Returns and clears the harness reports, for a test that expects some. */
  takeReports: () => Promise<BackendReport[]>;
  /**
   * Quits the app and opens it again on the same backend store, as a restart
   * finds the Rust database as the quit left it. `beforeExit` runs once the
   * quit has saved: Rust stops the harnesses before it exits, and the window
   * still runs until then.
   */
  restart: (beforeExit?: () => Promise<void>) => Promise<void>;
};
/**
 * A handler crosses into the page as source text. A method (`name() {…}`)
 * does not parse as an expression there, which would drop every override.
 */
function handlerSource(command: string, handler: Handler) {
  const source = handler.toString();
  try {
    new Function(`return (${source})`);
  } catch {
    throw new Error(
      `handler "${command}" must be an arrow or function expression, not a method`,
    );
  }
  return source;
}

/** Boots the app on `page`; the `app` fixture's `mount` is this on its own page. */
export async function mount(
  page: Page,
  seed: FakeSeed = {},
  { platform = "macos", handlers = {}, cannotSteer = [] }: MountOptions = {},
) {
  await page.addInitScript(
    ({ seed, platform, handlers, cannotSteer }) => {
      Object.defineProperty(Navigator.prototype, "platform", {
        configurable: true,
        get: () => platform,
      });
      window.appBackendSeed = seed;
      window.appBackendCannotSteer = cannotSteer;
      window.appBackendHandlers = Object.fromEntries(
        Object.entries(handlers).map(([command, source]) => [
          command,
          (0, eval)(`(${source})`),
        ]),
      );
    },
    {
      seed,
      platform: PLATFORMS[platform],
      cannotSteer,
      handlers: Object.fromEntries(
        Object.entries(handlers).map(([command, handler]) => [
          command,
          handlerSource(command, handler),
        ]),
      ),
    },
  );
  await page.goto("/tests/browser/app.html");
  await booted(page);
}

async function booted(page: Page) {
  // A cold Vite cache optimizes deps on the first load, which takes seconds.
  await expect(page.getByRole("button", { name: "Automations" })).toBeVisible({
    timeout: 30_000,
  });
}

/**
 * Quits the way Rust's quit coordinator does (`quit_commit`, then the app's
 * `quit_ready`), then opens the app again on the same store.
 */
async function restart(page: Page, beforeExit?: () => Promise<void>) {
  await page.evaluate(() => window.appBackend!.emit("quit_commit", 1));
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.appBackend!.calls.find(
            ({ command }) => command === "quit_ready",
          )?.args,
      ),
    )
    .toEqual({ id: 1, persisted: true });
  await beforeExit?.();
  await page.evaluate(() => window.appBackend!.keepStore());
  await page.reload();
  await booted(page);
}

export const test = base.extend<{ app: AppShell }>({
  app: async ({ page }, use) => {
    const reports: BackendReport[] = [];
    await page.exposeFunction("appBackendReport", (report: BackendReport) => {
      reports.push(report);
    });
    // One round trip delivers reports the page sent before it.
    const takeReports = async () => {
      await page.evaluate(() => undefined);
      return reports.splice(0);
    };
    await use({
      mount: (seed, options) => mount(page, seed, options),
      emit: (event, payload) =>
        page.evaluate(
          ({ event, payload }) => window.appBackend!.emit(event, payload),
          { event, payload },
        ),
      takeReports,
      restart: (beforeExit) => restart(page, beforeExit),
    });
    expect(
      await takeReports(),
      "Add unmocked commands to HANDLERS in tests/browser/app/backend.ts and fix handlers that threw",
    ).toEqual([]);
  },
});
