import { configDefaults, defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "tests/**/*.test.ts"],
    // The mobile protocol tests need the Expo app's dependencies; the CI
    // mobile job runs them with mobile/vitest.config.ts.
    exclude: [...configDefaults.exclude, "tests/mobile/**"],
    // West of UTC, so a date-only string parsed as UTC lands a day early.
    env: { TZ: "America/Los_Angeles" },
  },
});
