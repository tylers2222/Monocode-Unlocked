import { invoke } from "@tauri-apps/api/core";
import type { Approval } from "./approvals";

type Decided = Extract<Approval["status"], { state: "decided" }>;
export type Verdict = Decided["verdict"];

/**
 * What the filing Giga's chat shows of a decision (frame A·7). It comes from
 * the card the decision row names, so nothing is read back out of the text
 * the agent got.
 */
export type BoardDecision = Pick<Approval, "number" | "heading"> &
  Pick<Decided, "verdict" | "note" | "chosen">;

const VERDICTS: readonly Verdict[] = [
  "approved",
  "rejected",
  "changes_requested",
];

/** The decision of card `cardId`; none while the card is open or withdrawn. */
export async function boardDecision(
  cardId: string,
): Promise<BoardDecision | undefined> {
  const card = await invoke<Approval>("approvals_get", { id: cardId });
  if (card.status.state !== "decided") return undefined;
  const { verdict, note, chosen } = card.status;
  return { number: card.number, heading: card.heading, verdict, note, chosen };
}

/** A saved decision line, or nothing when any field is missing or malformed. */
export function parseBoardDecision(value: unknown): BoardDecision | undefined {
  if (!value || typeof value !== "object") return undefined;
  const { number, heading, verdict, note, chosen } = value as BoardDecision;
  if (
    !Number.isInteger(number) ||
    typeof heading !== "string" ||
    !VERDICTS.includes(verdict) ||
    (note !== null && typeof note !== "string") ||
    !Array.isArray(chosen) ||
    !chosen.every((label) => typeof label === "string")
  )
    return undefined;
  return { number, heading, verdict, note, chosen: [...chosen] };
}
