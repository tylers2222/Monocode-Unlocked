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
