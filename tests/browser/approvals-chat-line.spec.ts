import { readFileSync } from "node:fs";
import type { Locator, Page } from "@playwright/test";
import type { Approval } from "../../src/features/approvals/model/approvals";
import type { SessionSummary } from "../../src/features/sessions/data/sessionStore";
import { type AppShell, expect, type MountOptions, test } from "./app/fixture";
import type { MonoSeed } from "./app/backend";

// Frame A·7, https://doop.design/c/oRuRCkta7A?frame=nk4hGT-EXb. The fixture
// is what the real `decide` left on a copy of the app database (see
// `writes_the_chat_line_fixture_from_a_copy_of_the_app_database`): its Monos,
// the decision rows queued to its first Giga's chat, and their cards.

type Row = { message: { id: string; requestId: string; body: string } };

const fixture: {
  giga: string;
  chat: string;
  project: string;
  monos: MonoSeed[];
  rows: Row[];
  cards: Record<string, Approval>;
  withdrawn: string;
  removedGiga: string;
} = JSON.parse(
  readFileSync(
    new URL("./approvals-chat-line.realdb.json", import.meta.url),
    "utf8",
  ),
);
const ROWS = fixture.rows;
const CARDS = fixture.cards;
const GIGA = fixture.monos.find((mono) => mono.id === fixture.giga)!;
const NAME = GIGA.data.name as string;
const card = (row: Row) => CARDS[row.message.requestId];
const SENDER = fixture.monos.find((mono) => mono.role === "mono")!;
const REPORT = "S2 is ready for review.";
/** A Mono's own message to the Giga, in the same queue: it stays a bubble. */
const ORDINARY: Row = {
  message: {
    ...ROWS[0].message,
    id: "m_orchestrator",
    requestId: "orch-1",
    fromMonoId: SENDER.id,
    fromSessionId: SENDER.data.sessionId as string,
    body: REPORT,
    bodyChars: REPORT.length,
  } as Row["message"],
};

declare global {
  interface Window {
    chatLine?: { cards: Record<string, Approval>; rows: Row[] };
    /** What mono_messages_unqueued answers once. */
    unqueued?: Row[];
  }
}

/** Pi in RPC mode (piFamily.ts): every request but a prompt is answered at once. */
const handlers: MountOptions["handlers"] = {
  harness_resolve_pi: () => ({ path: "/usr/local/bin/pi" }),
  harness_spawn: () => 4242,
  harness_write: (_store, { sessionId, line }) => {
    const request = JSON.parse(line as string) as { id: string; type: string };
    if (request.type === "prompt") return null;
    void window.appBackend!.emit("harness-stdout", {
      sessionId,
      line: JSON.stringify({
        type: "response",
        id: request.id,
        command: request.type,
        success: true,
        data: {},
      }),
    });
    return null;
  },
  control_owns_session: () => true,
  mono_messages_unqueued: () => {
    const rows = window.unqueued ?? [];
    window.unqueued = undefined;
    return rows;
  },
  mono_messages_pending_notices: () => [],
  mono_message_assign_slot: (_store, { id, turnId, kind }) => {
    const row = window.chatLine!.rows.find((row) => row.message.id === id)!;
    return {
      message: {
        ...row.message,
        delivery: kind,
        slot: { turnId, deliveredAt: null },
      },
      target: null,
    };
  },
  mono_message_mark_delivery: () => null,
  approvals_get: (_store, { id }) => {
    const found = window.chatLine!.cards[id as string];
    if (!found) throw "unknown";
    return found;
  },
};

async function mountGiga(page: Page, app: AppShell) {
  await page.addInitScript(
    (chatLine) => {
      window.chatLine = chatLine;
    },
    { cards: CARDS, rows: [...ROWS, ORDINARY] },
  );
  const chat: SessionSummary = {
    id: fixture.chat,
    cwd: `/Users/test/MonoCode/gigas/${GIGA.data.gigaSlug as string}`,
    harness: "pi",
    model: "pi:anthropic/claude-sonnet-4-5",
    runtimeMode: "supervised",
    title: NAME,
    createdAt: 1,
    updatedAt: 1,
  };
  await page.clock.install({ time: new Date("2026-10-19T22:30:00Z") });
  await app.mount(
    {
      projects: [fixture.project],
      sessions: [chat],
      monos: fixture.monos as MonoSeed[],
    },
    { handlers },
  );
  await page.getByRole("button", { name: new RegExp(`^${NAME}, `) }).click();
  await expect(page.getByPlaceholder(`Message ${NAME}`)).toBeVisible();
}

/** Every prompt the app wrote to Pi for the Giga's chat, in order. */
async function prompts(page: Page) {
  const lines = await page.evaluate(
    (chat) =>
      window
        .appBackend!.calls.filter(
          ({ command, args }) =>
            command === "harness_write" &&
            (args as { sessionId: string }).sessionId === chat,
        )
        .map(({ args }) => (args as { line: string }).line),
    fixture.chat,
  );
  return lines
    .map(
      (line) =>
        JSON.parse(line) as { id: string; type: string; message?: string },
    )
    .filter((request) => request.type === "prompt");
}

/** Pi accepts the newest prompt: the turn runs. */
async function acceptPrompt(page: Page, app: AppShell, count: number) {
  await expect.poll(async () => (await prompts(page)).length).toBe(count);
  const last = (await prompts(page)).at(-1)!;
  await app.emit("harness-stdout", {
    sessionId: fixture.chat,
    line: JSON.stringify({
      type: "response",
      id: last.id,
      command: "prompt",
      success: true,
    }),
  });
}

/** Pi says `text`, then ends the turn. */
async function endTurn(app: AppShell, text: string) {
  for (const line of [
    { type: "agent_start" },
    {
      type: "message_update",
      assistantMessageEvent: { type: "text_delta", delta: text },
    },
    { type: "agent_end", willRetry: false },
  ])
    await app.emit("harness-stdout", {
      sessionId: fixture.chat,
      line: JSON.stringify(line),
    });
}

const queueRows = (page: Page) => page.locator("[data-queued-decision]");
const lines = (page: Page) => page.locator("[data-mono-notice]");

const TONES = {
  approved: { label: "Approved", color: "oklch(0.765 0.177 163.223)" },
  rejected: { label: "Rejected", color: "oklch(0.808 0.114 19.571)" },
  changes_requested: {
    label: "Changes requested",
    color: "oklch(0.828 0.189 84.429)",
  },
};

const color = (locator: Locator) =>
  locator.evaluate((element) => getComputedStyle(element).color);

/** The decision's verdict, card id and heading, as the queue row and the line say it. */
function headline(approval: Approval) {
  if (approval.status.state !== "decided") throw new Error("not decided");
  return `${TONES[approval.status.verdict].label} · APR-${approval.number} “${approval.heading}”`;
}

/** Frame A·7 "After": one sent line for `row`, in the decision's colour. */
async function expectLine(line: Locator, row: Row, prompt: string) {
  const approval = card(row);
  if (approval.status.state !== "decided") throw new Error("not decided");
  const { verdict, chosen, note } = approval.status;
  const sentence = line.locator("p");
  await expect(
    line.getByText(`Decision from the approvals board · sent to ${NAME}`),
  ).toBeVisible();
  const chose = chosen.length
    ? ` · chose ${chosen.map((label) => `“${label}”`).join(", ")}`
    : "";
  const comment = note ? `Your comment: “${note}”` : "";
  await expect(sentence).toHaveText(`${headline(approval)}${chose}${comment}`);
  if (note) await expect(sentence.getByText(comment)).toBeVisible();
  const tone = TONES[verdict].color;
  expect(
    await color(sentence.getByText(TONES[verdict].label, { exact: true })),
  ).toBe(tone);
  expect(await color(line.locator("svg").first())).toBe(tone);
  const got = line.locator("pre");
  await expect(got).toBeHidden();
  await line.getByText("What the agent got").click();
  await expect(got).toBeVisible();
  expect(await got.textContent()).toContain(row.message.body);
  expect(prompt).toContain((await got.textContent())!);
}

// Frame A·7. Defects: a decision shows in the transcript while its Giga is
// busy, or not in the queue; it reaches the Giga as a steer; it shows twice
// once sent, or its line sits above the turn it waited behind; its verdict,
// colour, card id, heading, options, comment or the text the agent got is
// wrong; a withdrawn card or a removed Giga's card gets a line; the lines
// lose their decision on a restart.
test("a decision waits in the busy Giga's queue, then shows once as its sent line", async ({
  page,
  app,
}) => {
  expect(ROWS.map((row) => card(row).status)).toMatchObject([
    { state: "decided", verdict: "approved" },
    { state: "decided", verdict: "rejected" },
    { state: "decided", verdict: "changes_requested" },
  ]);
  await mountGiga(page, app);
  const composer = page.getByPlaceholder(`Message ${NAME}`);
  await composer.fill("Run the S1 migration tests");
  await composer.press("Enter");
  await acceptPrompt(page, app, 1);

  await page.evaluate(
    (rows) => {
      window.unqueued = rows;
    },
    [...ROWS, ORDINARY],
  );
  await app.emit("mono-message-notices", { sessions: [fixture.chat] });

  await expect(queueRows(page)).toHaveCount(3);
  for (const [index, row] of ROWS.entries()) {
    const queued = queueRows(page).nth(index);
    await expect(queued).toHaveText(
      `Decision from the approvals board · not sent yet${headline(card(row))}`,
    );
    const status = card(row).status;
    if (status.state !== "decided") throw new Error("not decided");
    expect(
      await color(
        queued.getByText(TONES[status.verdict].label, { exact: true }),
      ),
    ).toBe(TONES[status.verdict].color);
  }
  await expect(lines(page)).toHaveCount(0);
  await expect(page.getByText(REPORT)).toBeVisible();
  for (const row of ROWS)
    await expect(page.getByText(row.message.body.split("\n")[0])).toHaveCount(
      0,
    );
  expect(await prompts(page)).toHaveLength(1);
  await page.screenshot({ path: test.info().outputPath("a7-busy.png") });

  await endTurn(app, "The S1 migration tests pass.");
  for (const [index, row] of ROWS.entries()) {
    await acceptPrompt(page, app, index + 2);
    await expect(queueRows(page)).toHaveCount(2 - index);
    await expect(lines(page)).toHaveCount(index + 1);
    const prompt = (await prompts(page)).at(-1)!.message!;
    expect(prompt).toContain(row.message.body);
    await expectLine(lines(page).nth(index), row, prompt);
    await endTurn(app, `Noted APR-${card(row).number}.`);
  }
  await expect(queueRows(page)).toHaveCount(0);
  await acceptPrompt(page, app, ROWS.length + 2);
  expect((await prompts(page)).at(-1)!.message).toContain(REPORT);
  await endTurn(app, "Reviewing S2.");
  await expect(page.getByText("Reviewing S2.")).toBeVisible();
  await expect(page.getByText("The S1 migration tests pass.")).toBeVisible();
  const order = await page
    .locator("[data-mono-notice], [data-chat-message-role='assistant']")
    .evaluateAll((elements) =>
      elements.map((element) =>
        element.hasAttribute("data-mono-notice")
          ? "line"
          : element.textContent!.trim(),
      ),
    );
  expect(order).toEqual([
    "The S1 migration tests pass.",
    ...ROWS.flatMap((row) => ["line", `Noted APR-${card(row).number}.`]),
    "Reviewing S2.",
  ]);
  for (const row of ROWS)
    await expect(page.getByText(`“${card(row).heading}”`)).toHaveCount(1);
  for (const id of [fixture.withdrawn, fixture.removedGiga])
    await expect(page.getByText(CARDS[id].heading)).toHaveCount(0);
  await lines(page).first().scrollIntoViewIfNeeded();
  await page.screenshot({
    path: test.info().outputPath("a7-sent.png"),
    fullPage: true,
  });

  await app.restart();
  await page.getByRole("button", { name: new RegExp(`^${NAME}, `) }).click();
  await expect(lines(page)).toHaveCount(3);
  await expect(queueRows(page)).toHaveCount(0);
  for (const row of ROWS)
    await expect(page.getByText(`“${card(row).heading}”`)).toHaveCount(1);
});
