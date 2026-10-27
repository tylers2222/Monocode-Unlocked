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
    /** mono_message_assign_slot refuses while set, as a locked store does. */
    refuseSlots?: boolean;
    /** mono_message_drop answers this instead of dropping the row. */
    dropAnswer?: "closed";
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
  mono_outbox_pending: () => [],
  mono_message_assign_slot: (_store, { id, turnId, kind }) => {
    if (window.refuseSlots) throw "database is locked";
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
  // mono_messages.rs drop_message on a waiting row: the user removed it, so
  // its card reads undelivered "removed" (decide.rs UNDELIVERED).
  mono_message_drop: (_store, { id }) => {
    if (window.dropAnswer) return window.dropAnswer;
    const row = window.chatLine!.rows.find((row) => row.message.id === id)!;
    const status = window.chatLine!.cards[row.message.requestId].status;
    if (status.state === "decided") status.undelivered = "removed";
    return "dropped";
  },
  approvals_get: (_store, { id }) => {
    const found = window.chatLine!.cards[id as string];
    if (!found) throw "unknown";
    return found;
  },
};

async function mountGiga(page: Page, app: AppShell, cards = CARDS) {
  await page.addInitScript(
    (chatLine) => {
      window.chatLine = chatLine;
    },
    { cards, rows: [...ROWS, ORDINARY] },
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

/** WebKit prints more digits than Chromium; three places name the colour. */
const color = (locator: Locator) =>
  locator.evaluate((element) =>
    getComputedStyle(element).color.replace(/\d+\.\d+/g, (number) =>
      String(Number(Number(number).toFixed(3))),
    ),
  );

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
    { state: "decided", verdict: "approved", note: null, chosen: [] },
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

  await expect(queueRows(page)).toHaveCount(ROWS.length);
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
    await expect(queueRows(page)).toHaveCount(ROWS.length - 1 - index);
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
  await expect(lines(page)).toHaveCount(ROWS.length);
  await expect(queueRows(page)).toHaveCount(0);
  for (const row of ROWS)
    await expect(
      lines(page).filter({ hasText: `“${card(row).heading}”` }),
    ).toHaveCount(1);
  await expect(
    page
      .locator(".user-message-bubble")
      .filter({ hasText: 'via="approvals board"' }),
  ).toHaveCount(0);
});

const FIRST = ROWS[0];
const UNDELIVERABLE = "Delivery is unavailable. Retry when the agent is ready.";

/** The Giga's chat gets decision `row`; an idle chat starts its hand-off at once. */
async function decisionArrives(page: Page, app: AppShell, row = FIRST) {
  await page.evaluate((row) => {
    window.unqueued = [row];
  }, row);
  await app.emit("mono-message-notices", { sessions: [fixture.chat] });
}

// Frame A·7b, review r1 M1. Defects: a decision whose send failed loses its
// queue row or shows the failure twice, or with another element, colour or
// size than the queue's own failure line; Retry does not send it.
test("a decision whose send failed shows the queue's failure line once, then sends on Retry", async ({
  page,
  app,
}) => {
  await mountGiga(page, app);
  await page.evaluate(() => {
    window.refuseSlots = true;
  });
  await decisionArrives(page, app);
  const failed = queueRows(page).getByRole("alert");
  await expect(failed).toHaveText(UNDELIVERABLE);
  await expect(page.getByText(UNDELIVERABLE)).toHaveCount(1);
  expect(
    await failed.evaluate((element) => ({
      tag: element.tagName,
      className: element.className,
      color: getComputedStyle(element).color,
      size: getComputedStyle(element).fontSize,
    })),
  ).toEqual({
    tag: "P",
    className: "text-red-400",
    color: "oklch(0.704 0.191 22.216)",
    size: "12px",
  });
  await expect(queueRows(page)).toHaveCount(1);
  await expect(lines(page)).toHaveCount(0);
  await page.screenshot({ path: test.info().outputPath("a7b-failed.png") });

  await page.evaluate(() => {
    window.refuseSlots = false;
  });
  await page.getByRole("button", { name: "Retry" }).click();
  await acceptPrompt(page, app, 1);
  await expect(lines(page)).toHaveCount(1);
  await expect(queueRows(page)).toHaveCount(0);
  await expect(page.getByText(UNDELIVERABLE)).toHaveCount(0);
});

// Review r1 m1. Defect: a bidi control in Giga text reorders the rest of the
// line, so " · chose …" reads before the heading, or the control itself is
// kept in Tyler's record.
test("a bidi override in a heading cannot reorder the decision line", async ({
  page,
  app,
}) => {
  const heading = "עדכון הלוח";
  const decided = card(FIRST);
  if (decided.status.state !== "decided") throw new Error("not decided");
  await mountGiga(page, app, {
    ...CARDS,
    [decided.id]: {
      ...decided,
      heading: `\u202e${heading}`,
      status: { ...decided.status, chosen: ["Ship it"], note: null },
    },
  });
  await decisionArrives(page, app);
  await acceptPrompt(page, app, 1);
  await expect(lines(page)).toHaveCount(1);
  const shown = lines(page).getByText(heading, { exact: true });
  const chose = lines(page).getByText("Ship it", { exact: true });
  expect(await shown.textContent()).toBe(heading);
  expect(
    await shown.evaluate((element) => getComputedStyle(element).direction),
  ).toBe("rtl");
  const [headingBox, choseBox] = [
    await shown.boundingBox(),
    await chose.boundingBox(),
  ];
  expect(Math.abs(choseBox!.y - headingBox!.y)).toBeLessThan(2);
  expect(choseBox!.x).toBeGreaterThanOrEqual(headingBox!.x + headingBox!.width);
  await expect(lines(page).locator("p")).toHaveText(
    `${TONES.approved.label} · APR-${decided.number} “${heading}” · chose “Ship it”`,
  );
});

const busyTurn = async (page: Page, app: AppShell, count: number) => {
  const composer = page.getByPlaceholder(`Message ${NAME}`);
  await composer.fill("Run the S1 migration tests");
  await composer.press("Enter");
  await acceptPrompt(page, app, count);
};

/** Removes `row`, the only queued row, with one drop; answers its card's status. */
async function removeQueuedDecision(page: Page, row: Row) {
  await queueRows(page)
    .getByRole("button", { name: "Remove queued message" })
    .click();
  await expect(queueRows(page)).toHaveCount(0);
  expect(
    await page.evaluate(() =>
      window
        .appBackend!.calls.filter(
          ({ command }) => command === "mono_message_drop",
        )
        .map(({ args }) => args),
    ),
  ).toEqual([{ id: row.message.id }]);
  return page.evaluate(
    async (id) =>
      (
        (await window.__TAURI_INTERNALS__.invoke("approvals_get", {
          id,
        })) as Approval
      ).status,
    row.message.requestId,
  );
}

// Frame A·7b, lead finding on review r1 and review r1 m1. Defects: a waiting
// decision has no remove control, or Remove skips the drop or drops twice;
// the removed decision keeps its row, gets a line that says it was sent,
// reaches the Giga at the turn end, or comes back after a restart; Remove on
// one decision takes the sent lines of other decisions with it.
test("Remove on a waiting decision drops it once, and no line says it was sent", async ({
  page,
  app,
}) => {
  const sent = ROWS.slice(0, 2);
  const removed = ROWS[2];
  const sentLinesStay = async () => {
    await expect(lines(page)).toHaveCount(sent.length);
    for (const row of sent)
      await expect(
        lines(page).filter({ hasText: `“${card(row).heading}”` }),
      ).toHaveCount(1);
    await expect(page.getByText(card(removed).heading)).toHaveCount(0);
  };
  await mountGiga(page, app);
  for (const [index, row] of sent.entries()) {
    await decisionArrives(page, app, row);
    await acceptPrompt(page, app, index + 1);
    await expect(lines(page)).toHaveCount(index + 1);
    await endTurn(app, `Noted APR-${card(row).number}.`);
  }
  await busyTurn(page, app, sent.length + 1);
  await decisionArrives(page, app, removed);
  await expect(queueRows(page)).toHaveCount(1);

  expect(await removeQueuedDecision(page, removed)).toMatchObject({
    state: "decided",
    undelivered: "removed",
  });
  await sentLinesStay();

  await endTurn(app, "The S1 migration tests pass.");
  await expect(page.getByText("The S1 migration tests pass.")).toBeVisible();
  expect(await prompts(page)).toHaveLength(sent.length + 1);
  await sentLinesStay();

  await app.restart();
  await page.getByRole("button", { name: new RegExp(`^${NAME}, `) }).click();
  await expect(page.getByText("The S1 migration tests pass.")).toBeVisible();
  await sentLinesStay();
  await expect(queueRows(page)).toHaveCount(0);
});

// Review r1 n1. Rust answers `closed` when the row was already sent or is
// gone. Defect: Remove then deletes the decision's bubble, so a decision the
// Giga may have got loses its line, or its card reads "removed".
test("Remove on a decision Rust already closed keeps its line", async ({
  page,
  app,
}) => {
  await mountGiga(page, app);
  await busyTurn(page, app, 1);
  await decisionArrives(page, app);
  await expect(queueRows(page)).toHaveCount(1);
  await page.evaluate(() => {
    window.dropAnswer = "closed";
  });

  expect(await removeQueuedDecision(page, FIRST)).toMatchObject({
    state: "decided",
    undelivered: null,
  });
  await expect(lines(page)).toHaveCount(1);
  await expect(
    lines(page).filter({ hasText: `“${card(FIRST).heading}”` }),
  ).toHaveCount(1);
});
