import {
  ArrowUpRight,
  Check,
  Clock,
  MessageSquare,
  Square,
} from "../../../shared/ui/icons";
import type { HabitQuestion } from "../../monos/model/habitQuestion";
import { HABIT_USER_WAIT_MS } from "../../monos/model/monoHabits";
import {
  questionPromptTitle,
  type UserQuestionReply,
} from "../model/userQuestion";
import { QuestionForm } from "./QuestionForm";

/** Why a card is dead: the run ended before anyone answered. */
const ENDED = {
  stopped: { Icon: Square, text: "You stopped the run. Nothing to answer." },
  "timed-out": {
    Icon: Clock,
    text: `No answer in ${HABIT_USER_WAIT_MS / 3_600_000} h. The run was stopped.`,
  },
};

/**
 * A habit run's question in its Mono's chat: the run's own question card,
 * named for the habit and the run. Answered, it folds to one line; if the run
 * ended first, it stays, dimmed, with the reason.
 */
export function HabitQuestionCard({
  asked,
  habit,
  runFrom,
  onReply,
  onOpenRun,
}: {
  asked: HabitQuestion;
  habit: string;
  /** The run's start, as a clock time. */
  runFrom: string;
  onReply?: (requestId: number, reply: UserQuestionReply) => void;
  onOpenRun?: (runId: string) => void;
}) {
  const { end, prompt } = asked;
  if (end?.kind === "answered" || end?.kind === "skipped")
    return (
      <div
        data-habit-question={end.kind}
        className="flex min-w-0 items-center gap-1.5 px-4 pt-1 pb-2 font-sans text-[14px] text-content/50"
      >
        <MessageSquare
          className="size-3.5 shrink-0 text-content/45"
          strokeWidth={1.75}
        />
        <span className="min-w-0 truncate">
          {questionPromptTitle(prompt.questions)}
        </span>
        {end.kind === "answered" ? (
          <span className="inline-flex shrink-0 items-center gap-1 text-content/85">
            <Check className="size-3 text-emerald-400" strokeWidth={1.75} />
            {end.answer}
          </span>
        ) : (
          <span className="shrink-0 text-content/60">Skipped</span>
        )}
        <span className="shrink-0 text-content/25">·</span>
        <span className="shrink-0 text-[12px] text-content/35">
          {habit}, {runFrom} run
        </span>
      </div>
    );
  const ended = end && ENDED[end.kind];
  return (
    <div
      data-habit-question={end?.kind ?? "open"}
      className="px-2.5 pt-1.5 pb-1 font-sans"
    >
      <QuestionForm
        prompt={prompt}
        onReply={(requestId, reply) => onReply?.(requestId, reply)}
        disabled={!!end || !onReply}
        header={
          <>
            <span className="font-medium text-content/72">{habit}</span> · run
            from {runFrom}
          </>
        }
        footer={
          <span className="mr-auto flex min-w-0 items-center gap-1.5 text-[11px] text-content/45">
            {ended ? (
              <>
                <ended.Icon className="size-3 shrink-0" strokeWidth={1.75} />
                {ended.text}
              </>
            ) : onOpenRun ? (
              <button
                type="button"
                className="-ml-px flex items-center gap-1 rounded-md text-content/55 hover:text-content"
                onClick={() => onOpenRun(asked.runId)}
              >
                <ArrowUpRight className="size-3" strokeWidth={1.75} />
                Open run
              </button>
            ) : null}
          </span>
        }
      />
    </div>
  );
}
