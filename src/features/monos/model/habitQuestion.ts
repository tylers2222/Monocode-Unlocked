import type { Block } from "../../sessions/model/session";
import {
  selectedAnswerLabels,
  type UserQuestionPrompt,
  type UserQuestionReply,
} from "../../sessions/model/userQuestion";

/** A habit run's question, put to the user in its Mono's chat. */
export type HabitQuestion = {
  runId: string;
  /** When the run started; the card names the run by it. */
  runAt: number;
  /** The run's question, under the relay's request id as an approval is. */
  prompt: UserQuestionPrompt;
  /** How it ended. Until then the card is open. */
  end?: HabitQuestionEnd;
};

export type HabitQuestionEnd =
  | { kind: "answered"; answer: string }
  | { kind: "skipped" }
  | { kind: "stopped" }
  | { kind: "timed-out" };

/** Why a run ended with something still put to the user. */
export type HabitAskEnd = Extract<
  HabitQuestionEnd,
  { kind: "stopped" | "timed-out" }
>;

/** The habit's card once the user answered, from either card. */
export function answerHabitQuestion(
  block: Block,
  reply: UserQuestionReply,
): Block {
  const asked = block.habitQuestion;
  if (!asked || asked.end) return block;
  const end: HabitQuestionEnd =
    reply.kind === "answered"
      ? {
          kind: "answered",
          answer: asked.prompt.questions
            .flatMap((question) => selectedAnswerLabels(question, reply))
            .join(", "),
        }
      : { kind: "skipped" };
  return { ...block, habitQuestion: { ...asked, end } };
}

/** An ask still open in the Mono's chat when its run ended. */
export function endHabitAsk(block: Block, end: HabitAskEnd): Block {
  if (block.approval && !block.approval.decided)
    return { ...block, approval: { ...block.approval, decided: "cancelled" } };
  if (block.habitQuestion && !block.habitQuestion.end)
    return { ...block, habitQuestion: { ...block.habitQuestion, end } };
  return block;
}
