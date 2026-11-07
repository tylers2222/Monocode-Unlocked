import { configDefaults, defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "tests/**/*.test.ts"],
    // tests/mobile needs mobile/node_modules; mobile/vitest.config.ts runs it.
    exclude: [...configDefaults.exclude, "tests/mobile/**"],
    // West of UTC, so a date-only string parsed as UTC lands a day early.
    env: { TZ: "America/Los_Angeles" },
  },
});
