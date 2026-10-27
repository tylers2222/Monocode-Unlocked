import { buildOutgoingHandoffPrompt } from "./handoff";
import {
  cancelHarnessTurn,
  respondHarnessApproval,
  respondHarnessQuestion,
  sendHarnessTurn,
} from "../../../integrations/harness/core/registry";
import { mergeStream } from "../../../integrations/harness/core/streamText";
import type { AgentCwd, HarnessId, Session } from "./session";

const HANDOFF_TIMEOUT_MS = 45_000;

export async function requestOutgoingHandoff(input: {
  harness: HarnessId;
  sessionId: string;
  cwd: AgentCwd;
  model: string;
  modelSettings?: Record<string, string>;
  providerAccountId?: string;
  mcp?: Session["mcp"];
  userRequest: string;
}): Promise<string> {
  let brief = "";
  const timer = setTimeout(() => {
    void cancelHarnessTurn(input.harness, input.sessionId);
  }, HANDOFF_TIMEOUT_MS);
  try {
    await sendHarnessTurn({
      harness: input.harness,
      sessionId: input.sessionId,
      cwd: input.cwd,
      model: input.model,
      modelSettings: input.modelSettings,
      providerAccountId: input.providerAccountId,
      mcp: input.mcp,
      runtimeMode: "supervised",
      text: buildOutgoingHandoffPrompt(input.userRequest),
      // Not a window's current turn: a Mono steer for this session waits
      // for the turn end instead of steering this hand-off brief.
      turnId: crypto.randomUUID(),
      onEvent: (event) => {
        if (event.type === "message.delta") {
          brief = mergeStream(brief, event.text);
        }
        if (event.type === "approval.requested") {
          respondHarnessApproval(
            input.harness,
            input.sessionId,
            event.requestId,
            "deny",
          );
        }
        if (event.type === "question.asked") {
          respondHarnessQuestion(
            input.harness,
            input.sessionId,
            event.requestId,
            { kind: "skipped" },
          );
        }
      },
    });
  } catch {
    // Caller falls back to the deterministic packet.
  } finally {
    clearTimeout(timer);
  }
  return brief.trim();
}
