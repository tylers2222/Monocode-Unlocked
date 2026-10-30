import { existsSync } from "node:fs";
import { chromium, firefox, webkit, type FullConfig } from "@playwright/test";
import { booted } from "./app/fixture";

const BROWSERS = { chromium, firefox, webkit };

// Each CI shard starts its own Vite (PR #415), and a shard's first test paid
// every cold start: up to 33 s against the 30 s test timeout. Playwright starts
// the webServer before this runs. Here each project's browser loads app.html
// once, in turn, and waits for the booted app.
// Warmed: Vite's dep pre-bundle and transforms, which the first load does once
// for every browser, and each browser's first launch, which leaves its binary
// and libraries in the OS file cache.
// Not warmed: the browser a test runs in. Playwright launches a new process
// for the tests, so that browser still starts with an empty HTTP cache and
// fetches and compiles the app's modules on its first load.
// config.projects lists every project even with --project, so a browser that
// is not installed is skipped here. A test that needs it still fails.
export default async function globalSetup(config: FullConfig) {
  for (const { use } of config.projects) {
    const browserType = BROWSERS[use.browserName ?? "chromium"];
    if (!existsSync(browserType.executablePath())) continue;
    const browser = await browserType.launch();
    try {
      const page = await browser.newPage({ baseURL: use.baseURL });
      await page.goto("/tests/browser/app.html");
      await booted(page, 120_000);
    } finally {
      await browser.close();
    }
  }
}
