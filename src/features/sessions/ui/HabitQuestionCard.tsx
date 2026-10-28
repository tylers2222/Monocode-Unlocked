import {
  ArrowUpRight,
  Check,
  Clock,
  MessageSquare,
  Square,
} from "../../../shared/ui/icons";
import type {
  HabitQuestion,
  HabitQuestionEnd,
} from "../../monos/model/habitQuestion";
import type { UserQuestionReply } from "../model/userQuestion";
import { QuestionForm } from "./QuestionForm";

type Reason = { Icon: typeof Square; text: string };

/** Each way a question ends: the card folds to one line, or, if the run
 * ended before anyone answered, it stays, dimmed, with the reason. */
const END_LOOK: Record<HabitQuestionEnd["kind"], "fold" | Reason> = {
  answered: "fold",
  skipped: "fold",
  stopped: { Icon: Square, text: "You stopped the run. Nothing to answer." },
  "timed-out": { Icon: Clock, text: "No answer in 12 h. The run was stopped." },
};

type CardStatus =
  | { kind: "open"; look: "open" }
  /** `answer` is null when the question was skipped. */
  | { kind: HabitQuestionEnd["kind"]; look: "folded"; answer: string | null }
  | { kind: HabitQuestionEnd["kind"]; look: "ended"; reason: Reason };

function cardStatus(end: HabitQuestionEnd | undefined): CardStatus {
  if (!end) return { kind: "open", look: "open" };
  const look = END_LOOK[end.kind];
  if (look !== "fold") return { kind: end.kind, look: "ended", reason: look };
  return {
    kind: end.kind,
    look: "folded",
    answer: end.kind === "answered" ? end.answer : null,
  };
}

/** The card once answered or skipped: the question, the answer, and the run. */
function FoldedCard({
  kind,
  question,
  answer,
  habit,
  runFrom,
}: {
  kind: HabitQuestionEnd["kind"];
  question: string | undefined;
  answer: string | null;
  habit: string;
  runFrom: string;
}) {
  return (
    <div
      data-habit-question={kind}
      className="flex min-w-0 items-center gap-1.5 px-4 pt-1 pb-2 font-sans text-[14px] text-content/50"
    >
      <MessageSquare
        className="size-3.5 shrink-0 text-content/45"
        strokeWidth={1.75}
      />
      <span className="min-w-0 truncate">{question}</span>
      {answer !== null ? (
        <span className="inline-flex shrink-0 items-center gap-1 text-content/85">
          <Check className="size-3 text-emerald-400" strokeWidth={1.75} />
          {answer}
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
}

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
  onReply: (requestId: number, reply: UserQuestionReply) => void;
  onOpenRun?: (runId: string) => void;
}) {
  const { prompt } = asked;
  const status = cardStatus(asked.end);
  if (status.look === "folded")
    return (
      <FoldedCard
        kind={status.kind}
        question={prompt.questions[0]?.prompt}
        answer={status.answer}
        habit={habit}
        runFrom={runFrom}
      />
    );
  return (
    <div
      data-habit-question={status.kind}
      className="px-2.5 pt-1.5 pb-1 font-sans"
    >
      <QuestionForm
        key={status.look}
        prompt={prompt}
        onReply={onReply}
        disabled={status.look === "ended"}
        header={
          <>
            <span className="font-medium text-content/72">{habit}</span> · run
            from {runFrom}
          </>
        }
        footer={
          <span className="mr-auto flex min-w-0 items-center gap-1.5 text-[11px] text-content/45">
            {status.look === "ended" ? (
              <>
                <status.reason.Icon
                  className="size-3 shrink-0"
                  strokeWidth={1.75}
                />
                {status.reason.text}
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
