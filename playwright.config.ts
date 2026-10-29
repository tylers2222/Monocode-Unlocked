import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/browser",
  // tests/**/*.test.ts are vitest unit tests of the browser rigs.
  testMatch: "**/*.spec.ts",
  // Specs share only the read-only dev server: the fake backend lives in each
  // page, and the real-harness rigs use their own temp dir and a port-0 model
  // server (ADE-155). Not 4: on CI's 4-core runner each test ran 2.8x slower
  // and 12 timed out (run 40834966222).
  workers: 2,
  use: {
    baseURL: "http://127.0.0.1:1435",
    viewport: { width: 1000, height: 800 },
    trace: "retain-on-failure",
  },
  projects: [
    { name: "chromium", use: { browserName: "chromium" } },
    { name: "webkit", use: { browserName: "webkit" } },
  ],
  webServer: {
    command: "npm run dev -- --host 127.0.0.1 --port 1435",
    url: "http://127.0.0.1:1435/tests/browser/transcript.html",
    reuseExistingServer: false,
  },
});
