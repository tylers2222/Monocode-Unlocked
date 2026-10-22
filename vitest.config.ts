import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "tests/**/*.test.ts"],
    // West of UTC, so a date-only string parsed as UTC lands a day early.
    env: { TZ: "America/Los_Angeles" },
  },
});
