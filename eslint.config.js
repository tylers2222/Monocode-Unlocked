import { defineConfig } from "eslint/config";
import tseslint from "typescript-eslint";
import comments from "@eslint-community/eslint-plugin-eslint-comments/configs";

export default defineConfig(
  {
    ignores: [
      "**/dist/**",
      "**/build/**",
      "**/target/**",
      "**/gen/**",
      "vendor/**",
      "coverage/**",
      "playwright-report/**",
      "test-results/**",
    ],
  },
  tseslint.configs.recommended,
  comments.recommended,
  {
    linterOptions: { reportUnusedDisableDirectives: "error" },
    rules: {
      complexity: ["error", 15],
      "max-lines": ["error", { max: 600, skipBlankLines: true, skipComments: true }],
      "max-lines-per-function": ["error", { max: 80, skipBlankLines: true, skipComments: true }],
      "max-depth": ["error", 4],
      "no-warning-comments": ["error", { terms: ["todo", "fixme", "xxx", "hack"] }],
      "@typescript-eslint/ban-ts-comment": [
        "error",
        {
          "ts-ignore": true,
          "ts-nocheck": true,
          "ts-expect-error": "allow-with-description",
          minimumDescriptionLength: 10,
        },
      ],
      "@eslint-community/eslint-comments/no-use": ["error", { allow: [] }],
    },
  },
  {
    files: ["**/*.test.{ts,tsx,js,jsx,mjs,cjs,mts,cts}", "tests/**"],
    rules: { "max-lines": "off", "max-lines-per-function": "off" },
  },
);
