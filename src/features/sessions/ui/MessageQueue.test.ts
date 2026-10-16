// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { monoSessionCompletionMessage } from "../../monos/model/monoSessionCompletion";
import {
  noticeRequestId,
  renderMonoNotice,
  type MessageId,
} from "../../monos/model/monoMessages";
import { MessageQueue } from "./MessageQueue";

it("labels a pending completion and offers removal without editing or steering it", () => {
  const message = monoSessionCompletionMessage({
    sessionId: "worker",
    requestId: "app-mono-monitor",
    project: "/code/project",
    prompt: "Review",
    session: undefined,
    outcome: { status: "completed", text: "Done" },
  });
  const container = document.createElement("div");
  container.innerHTML = renderToStaticMarkup(
    createElement(MessageQueue, {
      messages: [message],
      onSteer: vi.fn(),
      onEdit: vi.fn(),
      onDelete: vi.fn(),
      variant: "messages",
    }),
  );
  expect(container.textContent).toContain("Session completed: Agent session");
  expect(container.textContent).not.toContain(
    "MonoCode completion notification",
  );
  expect(container.textContent).not.toContain("Steer");
  expect(
    container.querySelector<HTMLButtonElement>(
      '[aria-label="Edit queued message"]',
    )?.disabled,
  ).toBe(true);
  expect(
    container.querySelector('[aria-label="Remove queued message"]'),
  ).not.toBeNull();
});

it("labels a combined report without exposing the app prompt", () => {
  const message = monoSessionCompletionMessage({
    sessionId: "worker",
    requestId: "request",
    project: "/code/project",
    prompt: "Review",
    outcome: { status: "completed", text: "Done" },
  });
  message.monoSessionCompletion!.sessionCount = 3;
  const markup = renderToStaticMarkup(
    createElement(MessageQueue, { messages: [message] }),
  );
  expect(markup).toContain("3 sessions finished");
  expect(markup).not.toContain("MonoCode completion notification");
});

// Design U10. Defects: a notice shows its raw <mono_notice> tag, or offers
// Steer or Edit, which only make sense for a message the user wrote.
it("shows a sender notice in words with only a remove control", () => {
  const id = "m_aaaaaaaaaaaa" as MessageId;
  const notice = {
    id: noticeRequestId(id, "unanswered"),
    appRequestId: noticeRequestId(id, "unanswered"),
    text: renderMonoNotice(
      { id, body: "Nightly check: is CI green?", status: "unanswered" },
      "Monocode General",
    ),
    attachments: [],
    waitForIdle: true,
  };
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const onDelete = vi.fn();
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  act(() =>
    root.render(
      createElement(MessageQueue, {
        messages: [notice],
        onSteer: vi.fn(),
        onEdit: vi.fn(),
        onDelete,
        variant: "messages",
      }),
    ),
  );
  expect(container.textContent).toContain(
    "Unanswered · Monocode General did not reply to “Nightly check: is CI green?”",
  );
  // Frame n-wASP1amp: the header states no order, at a readable 70%.
  const header = [...container.querySelectorAll("div")].find(
    (element) => element.textContent === "Notice from the app · not sent yet",
  );
  expect(header?.className).toContain("text-content/70");
  expect(container.textContent).not.toContain("<mono_notice");
  expect(container.textContent).not.toContain("Steer");
  expect(
    container.querySelector('[aria-label="Edit queued message"]'),
  ).toBeNull();
  act(() =>
    container
      .querySelector<HTMLButtonElement>('[aria-label="Remove notice"]')!
      .click(),
  );
  expect(onDelete).toHaveBeenCalledWith(notice.id);
  act(() => root.unmount());
  container.remove();
});

// Design U10 names both status words. Defects: a dropped notice reads as
// unanswered, or a cut quote shows no sign that it was cut.
it("says a dropped notice never reached the recipient and marks a cut quote", () => {
  const id = "m_aaaaaaaaaaaa" as MessageId;
  const body =
    "Nightly check: 0 UI changes without an approval. Does Change E still pass?";
  const notice = {
    id: noticeRequestId(id, "dropped"),
    appRequestId: noticeRequestId(id, "dropped"),
    text: renderMonoNotice({ id, body, status: "dropped" }, "Monocode General"),
    attachments: [],
    waitForIdle: true,
  };
  const container = document.createElement("div");
  container.innerHTML = renderToStaticMarkup(
    createElement(MessageQueue, { messages: [notice] }),
  );
  expect(container.textContent).toContain(
    `Dropped · Monocode General never got “${Array.from(body).slice(0, 60).join("")}…”`,
  );
});

// Frame wBCOiNb5GF. Defect: a failed notice pauses the queue but never says
// which notice failed or why.
it("shows a failed notice's error under its sentence", () => {
  const id = "m_aaaaaaaaaaaa" as MessageId;
  const notice = {
    id: noticeRequestId(id, "unanswered"),
    appRequestId: noticeRequestId(id, "unanswered"),
    text: renderMonoNotice(
      { id, body: "Is CI green?", status: "unanswered" },
      "MonoGhost",
    ),
    attachments: [],
    waitForIdle: true,
    error: "Delivery is unavailable. Retry when the agent is ready.",
  };
  const container = document.createElement("div");
  container.innerHTML = renderToStaticMarkup(
    createElement(MessageQueue, { messages: [notice], status: "paused" }),
  );
  const alert = container.querySelector('[role="alert"]');
  expect(alert?.textContent).toBe(notice.error);
  expect(alert?.previousElementSibling?.textContent).toContain(
    "Unanswered · MonoGhost did not reply to “Is CI green?”",
  );
  expect(container.textContent).toContain("Retry");
});
