import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { sessionDisplayTitle, type Block } from "../../src/features/sessions/model/session";
import { expect, test } from "./app/fixture";
import { OMP, bridgeOmp, ompHandlers, type ModelRequest } from "./app/ompBridge";

const realDb = JSON.parse(
  readFileSync(new URL("./fixtures/ade82-idle-run.json", import.meta.url), "utf8"),
) as {
  record: Pick<SessionSummary, "id" | "harness" | "model" | "runtimeMode" | "title"> & {
    blocks: Block[];
  };
};

const PROJECT = "/Users/test/acme";
const WATCHING = "Watching the build.";
const FINISHED = "The watcher finished.";

test.skip(!OMP, "needs the omp binary on PATH");

test("the reply of a run omp starts on its own is sealed when that run ends", async ({
  app,
  page,
}) => {
  const dir = mkdtempSync(join(tmpdir(), "ade82-"));
  const marker = join(dir, "build.done");
  await bridgeOmp(page, app, {
    dir,
    // A local Ollama would otherwise be picked over the rig's model.
    args: ["--model", "offline/offline-model"],
    model: ({ messages, tools }: ModelRequest) => {
      // A title job runs without tools.
      if (!tools.length) return { text: "Watch the build" };
      const last = messages.at(-1)!;
      if (last.role === "tool") return { text: WATCHING };
      // omp hands a finished background job back as a message of its own.
      if (JSON.stringify(last.content ?? "").includes("Background job"))
        return { text: FINISHED };
      return {
        tool: "bash",
        arguments: { command: `while [ ! -f ${marker} ]; do sleep 0.1; done`, async: true },
      };
    },
  });
  await app.mount(
    {
      projects: [PROJECT],
      sessions: [{ ...realDb.record, cwd: PROJECT, createdAt: 1, updatedAt: 1 }],
      blocks: { [realDb.record.id]: realDb.record.blocks },
    },
    { handlers: ompHandlers },
  );
  const main = page.getByRole("main");
  await expect(async () => {
    await page
      .getByText(sessionDisplayTitle(realDb.record.title, realDb.record.harness), { exact: true })
      .click();
    await expect(main.getByText(realDb.record.blocks[0].text)).toBeVisible({ timeout: 1_000 });
  }).toPass();

  const composer = page.getByPlaceholder("Ask, build, / for commands");
  await composer.fill("Start the watcher");
  await composer.press("Enter");
  await expect(main.getByText(WATCHING)).toBeVisible();
  await expect(main.getByRole("button", { name: "Stop" }), "the turn ends").toHaveCount(0);

  writeFileSync(marker, "");
  const reply = main.locator('[data-chat-message-role="assistant"]', { hasText: FINISHED });
  await expect(reply, "omp answers the job result while the session is idle").toBeVisible();
  await expect(reply, "the reply is sealed like any finished reply").toHaveAttribute(
    "data-selectable-agent-response",
  );
});
