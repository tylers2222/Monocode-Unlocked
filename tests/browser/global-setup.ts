import { chromium, type FullConfig } from "@playwright/test";

// Each CI shard starts its own Vite (PR #415). Its first app load pre-bundles
// the deps and transforms the whole app, which took a shard's first test to
// 33 s against the 30 s test timeout. Playwright starts the webServer before
// this runs, so one load here leaves a warm server for the first test. Vite
// caches its transforms per server, not per browser, so chromium warms it for
// webkit tests too.
export default async function globalSetup(config: FullConfig) {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({
      baseURL: config.projects[0].use.baseURL,
    });
    await page.goto("/tests/browser/app.html");
    // A dep found during the load makes Vite reload the page, so wait for the
    // booted app rather than the load event.
    await page
      .getByRole("button", { name: "Automations" })
      .waitFor({ timeout: 120_000 });
  } finally {
    await browser.close();
  }
}
