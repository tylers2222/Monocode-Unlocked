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

/** Why a run ended with its question still open. */
export type HabitQuestionStop = Extract<
  HabitQuestionEnd,
  { kind: "stopped" | "timed-out" }
>;

/** The card once the user answered it, from either card. */
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

/** The card once its run ended with nobody having answered it. */
export function endHabitQuestion(block: Block, end: HabitQuestionStop): Block {
  const asked = block.habitQuestion;
  return asked && !asked.end
    ? { ...block, habitQuestion: { ...asked, end } }
    : block;
}
