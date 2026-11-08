import { beforeEach, expect, it, vi } from "vitest";
import {
  harnessWireRequestId,
  respondHarnessApproval,
  respondHarnessQuestion,
} from "../../../integrations/harness/core/registry";
import { newSession, type Session } from "../../sessions/model/session";
import { handleAgentApp, type AgentAppHost } from "./agentApp";
import { AppActionError } from "./appActionError";

vi.mock("../../../integrations/harness/core/registry", () => ({
  harnessWireRequestId: vi.fn(),
  respondHarnessApproval: vi.fn(),
  respondHarnessQuestion: vi.fn(),
}));

beforeEach(() => {
  vi.mocked(respondHarnessApproval).mockClear();
  vi.mocked(respondHarnessQuestion).mockClear();
  // The harness names UI dialog N `wire-<kind>-N` in its own frames.
  vi.mocked(harnessWireRequestId).mockImplementation(
    (_harness, _session, kind, uiId) => `wire-${kind}-${uiId}`,
  );
});

/** control.rs checked the caller; the host is never reached. */
const host = {} as AgentAppHost;

function worker(patch: Partial<Session> = {}): Session {
  return { ...newSession("claude", "/code/app"), id: "worker", ...patch };
}

async function answer(source: Session, input: Record<string, unknown>) {
  return handleAgentApp(source, "req-1", "alarms.answer", input, host);
}

async function refusal(source: Session, input: Record<string, unknown>) {
  try {
    await answer(source, input);
  } catch (error) {
    if (error instanceof AppActionError) return error.code;
    throw error;
  }
  return "answered";
}

// Defect: an alarm's answer reaches a dialog the agent already closed, a
// newer dialog than the one the alarm named, or a question with an option
// it does not offer.
it("answers the dialog the alarm named as the user does, or says it is gone", async () => {
  const pending = worker({
    blocks: [
      {
        id: "old",
        role: "tool",
        text: "",
        approval: { requestId: 3, decided: "allow" },
      },
      { id: "named", role: "tool", text: "", approval: { requestId: 7 } },
      { id: "newer", role: "tool", text: "", approval: { requestId: 8 } },
    ],
  });
  const permission = { alarm: "al_1", kind: "permission", decision: "deny" };
  await expect(
    answer(pending, { ...permission, dialogRequestId: "wire-approval-7" }),
  ).resolves.toEqual({ answered: "permission" });
  expect(respondHarnessApproval).toHaveBeenCalledWith(
    "claude",
    "worker",
    7,
    "deny",
  );
  expect(respondHarnessApproval).toHaveBeenCalledTimes(1);

  for (const stale of ["wire-approval-3", "wire-approval-9", undefined])
    expect(
      await refusal(pending, { ...permission, dialogRequestId: stale }),
    ).toBe("dialog_gone");
  expect(respondHarnessApproval).toHaveBeenCalledTimes(1);
  expect(
    await refusal(worker({ cwd: "remote://box/code" }), {
      ...permission,
      dialogRequestId: "wire-approval-7",
    }),
  ).toBe("dialog_gone");

  const asked = worker({
    pendingQuestion: {
      requestId: 9,
      questions: [
        {
          id: "q1",
          prompt: "Which branch?",
          multiSelect: false,
          allowCustom: false,
          options: [
            { id: "o1", label: "main" },
            { id: "o2", label: "dev" },
          ],
        },
      ],
    },
  });
  const question = {
    alarm: "al_1",
    kind: "question",
    dialogRequestId: "wire-question-9",
  };
  expect(
    await refusal(asked, {
      ...question,
      dialogRequestId: "wire-question-8",
      choice: "dev",
    }),
  ).toBe("dialog_gone");
  await answer(asked, { ...question, choice: "Dev" });
  expect(respondHarnessQuestion).toHaveBeenCalledWith("claude", "worker", 9, {
    kind: "answered",
    answers: { q1: ["o2"] },
  });
  expect(respondHarnessQuestion).toHaveBeenCalledTimes(1);
  expect(await refusal(asked, { ...question, choice: "prod" })).toBe(
    "invalid_input",
  );
  expect(await refusal(asked, { ...question, text: "prod" })).toBe(
    "invalid_input",
  );
  expect(await refusal(worker(), { ...question, text: "x" })).toBe(
    "dialog_gone",
  );
});
