import { isHarnessAvailable } from "../../../integrations/harness/core/availability";
import { modelsFor, type AgentModel } from "../../sessions/model/models";
import { HARNESSES, type HarnessId } from "../../sessions/model/session";

export type AgentModelChoice = { harness: HarnessId; model: AgentModel };

/**
 * The harness and model for every CLI path that starts an agent. Both are
 * required: an agent never falls back to the last model used in the composer.
 */
export function requireHarnessModel(input: {
  harness?: unknown;
  model?: unknown;
}): AgentModelChoice {
  if (input.harness == null || input.model == null)
    throw new Error(
      "harness and model are required; run models.list for exact IDs",
    );
  if (
    typeof input.harness !== "string" ||
    !HARNESSES.includes(input.harness as HarnessId)
  )
    throw new Error("Unknown harness; run models.list for available providers");
  const harness = input.harness as HarnessId;
  if (!isHarnessAvailable(harness))
    throw new Error(
      `${harness} is not available in MonoCode; run models.list for available providers`,
    );
  const model =
    typeof input.model === "string"
      ? modelsFor(harness).find((entry) => entry.id === input.model)
      : undefined;
  if (!model || model.harness !== harness)
    throw new Error(
      `Unknown model for ${harness}; run models.list for exact model IDs`,
    );
  return { harness, model };
}
