import {
  ArrowUpRight,
  Check,
  Clock,
  Info,
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
type Open = { kind: "open" };
/** The card's state: open until the question ends, then its end. */
type CardState = Open | HabitQuestionEnd;
/** The ends that fold the card to one line. */
type Folded = Extract<HabitQuestionEnd, { kind: "answered" | "skipped" }>;

const OPEN: Open = { kind: "open" };

/** How each state looks: open, folded to one line, or, if the run ended
 * before anyone answered, dimmed with the reason. Total, and only a `Folded`
 * kind may fold: a new end kind does not compile until it has a reason. */
const LOOK: {
  [K in CardState["kind"]]: K extends "open"
    ? null
    : K extends Folded["kind"]
      ? "fold"
      : Reason;
} = {
  open: null,
  answered: "fold",
  skipped: "fold",
  stopped: { Icon: Square, text: "You stopped the run. Nothing to answer." },
  "timed-out": { Icon: Clock, text: "No answer in 12 h. The run was stopped." },
};

/** Why an open card has nothing to press: this chat has no reply path. */
const READ_ONLY: Reason = {
  Icon: Info,
  text: "This chat can't answer. Answer in the run.",
};

/** The one decision, pure: the card's state and the look `LOOK` gives it. */
function cardStatus(end: HabitQuestionEnd | undefined) {
  const state: CardState = end ?? OPEN;
  return { state, look: LOOK[state.kind] };
}

/** Why the card has nothing to press, in the footer. */
function ReasonNote({ reason: { Icon, text } }: { reason: Reason }) {
  return (
    <span className="flex items-center gap-1.5">
      <Icon className="size-3 shrink-0" strokeWidth={1.75} />
      {text}
    </span>
  );
}

/** The card once answered or skipped: the question, the answer, and the run. */
function FoldedCard({
  end,
  question,
  habit,
  runFrom,
}: {
  end: CardState;
  question: string | undefined;
  habit: string;
  runFrom: string;
}) {
  return (
    <div
      data-habit-question={end.kind}
      className="flex min-w-0 items-center gap-1.5 px-4 pt-1 pb-2 font-sans text-[14px] text-content/50"
    >
      <MessageSquare
        className="size-3.5 shrink-0 text-content/45"
        strokeWidth={1.75}
      />
      <span className="min-w-0 truncate">{question}</span>
      {"answer" in end ? (
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
  /** Absent when the chat cannot answer: the open card shows, disabled,
   * with the reason. */
  onReply?: (requestId: number, reply: UserQuestionReply) => void;
  onOpenRun?: (runId: string) => void;
}) {
  const { prompt } = asked;
  const { state, look } = cardStatus(asked.end);
  if (look === "fold")
    return (
      <FoldedCard
        end={state}
        question={prompt.questions[0]?.prompt}
        habit={habit}
        runFrom={runFrom}
      />
    );
  const reason = look ?? (onReply ? null : READ_ONLY);
  return (
    <div
      data-habit-question={state.kind}
      className="px-2.5 pt-1.5 pb-1 font-sans"
    >
      <QuestionForm
        key={state.kind}
        prompt={prompt}
        onReply={onReply}
        disabled={!!look}
        header={
          <>
            <span className="font-medium text-content/72">{habit}</span> · run
            from {runFrom}
          </>
        }
        footer={
          <span className="mr-auto flex min-w-0 items-center gap-2.5 text-[11px] text-content/45">
            {!look && onOpenRun && (
              <button
                type="button"
                className="-ml-px flex items-center gap-1 rounded-md text-content/55 hover:text-content"
                onClick={() => onOpenRun(asked.runId)}
              >
                <ArrowUpRight className="size-3" strokeWidth={1.75} />
                Open run
              </button>
            )}
            {reason && <ReasonNote reason={reason} />}
          </span>
        }
      />
    </div>
  );
}
