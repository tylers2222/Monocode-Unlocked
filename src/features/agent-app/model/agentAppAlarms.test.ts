import { beforeEach, expect, it, vi } from "vitest";
import {
  respondHarnessApproval,
  respondHarnessQuestion,
} from "../../../integrations/harness/core/registry";
import { newSession, type Session } from "../../sessions/model/session";
import { handleAgentApp, type AgentAppHost } from "./agentApp";
import { AppActionError } from "./appActionError";

vi.mock("../../../integrations/harness/core/registry", () => ({
  respondHarnessApproval: vi.fn(),
  respondHarnessQuestion: vi.fn(),
}));

beforeEach(() => {
  vi.mocked(respondHarnessApproval).mockClear();
  vi.mocked(respondHarnessQuestion).mockClear();
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

// Defect: an alarm's answer reaches a dialog the agent already closed, or
// another request than the pending one, or a question with an option it
// does not offer.
it("answers the agent's pending dialog as the user does, or says it is gone", async () => {
  const pending = worker({
    blocks: [
      { id: "old", role: "tool", text: "", approval: { requestId: 3, decided: "allow" } },
      { id: "now", role: "tool", text: "", approval: { requestId: 7 } },
    ],
  });
  await expect(
    answer(pending, { alarm: "al_1", kind: "permission", decision: "deny" }),
  ).resolves.toEqual({ answered: "permission" });
  expect(respondHarnessApproval).toHaveBeenCalledWith("claude", "worker", 7, "deny");

  expect(await refusal(worker(), { alarm: "al_1", kind: "permission", decision: "allow" })).toBe(
    "dialog_gone",
  );
  expect(
    await refusal(worker({ cwd: "remote://box/code" }), {
      alarm: "al_1",
      kind: "permission",
      decision: "allow",
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
  await answer(asked, { alarm: "al_1", kind: "question", choice: "Dev" });
  expect(respondHarnessQuestion).toHaveBeenCalledWith("claude", "worker", 9, {
    kind: "answered",
    answers: { q1: ["o2"] },
  });
  expect(await refusal(asked, { alarm: "al_1", kind: "question", choice: "prod" })).toBe(
    "invalid_input",
  );
  expect(await refusal(asked, { alarm: "al_1", kind: "question", text: "prod" })).toBe(
    "invalid_input",
  );
  expect(await refusal(worker(), { alarm: "al_1", kind: "question", text: "x" })).toBe(
    "dialog_gone",
  );
});
