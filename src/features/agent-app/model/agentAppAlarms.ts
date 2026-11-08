import {
  harnessWireRequestId,
  respondHarnessApproval,
  respondHarnessQuestion,
} from "../../../integrations/harness/core/registry";
import type { DialogKind } from "../../../integrations/harness/core/types";
import { isRemoteProjectPath } from "../../projects/model/recents";
import type { Session } from "../../sessions/model/session";
import {
  buildQuestionReply,
  type UserQuestion,
} from "../../sessions/model/userQuestion";
import { AppActionError, monoError } from "./appActionError";

type Answered = { answered: "permission" | "question" };

/** The pending dialog of `kind` and UI id `uiId` is the one the alarm named. */
function isNamed(
  source: Session,
  kind: DialogKind,
  uiId: number,
  input: Record<string, unknown>,
): boolean {
  const named = input.dialogRequestId;
  return (
    typeof named === "string" &&
    harnessWireRequestId(source.harness, source.id, kind, uiId) === named
  );
}

function questionOption(question: UserQuestion, choice: string) {
  const option = question.options.find(
    (item) =>
      item.id === choice ||
      item.label.trim().toLowerCase() === choice.toLowerCase(),
  );
  const labels = question.options.map((item) => item.label).join(", ");
  if (choice && !option)
    throw new AppActionError(
      "invalid_input",
      `choice must be one of: ${labels}`,
    );
  return { option, labels };
}

function answerQuestion(
  source: Session,
  input: Record<string, unknown>,
): Answered {
  const prompt = source.pendingQuestion;
  const question = prompt?.questions[0];
  if (
    !prompt ||
    !question ||
    !isNamed(source, "question", prompt.requestId, input)
  )
    throw monoError("dialog_gone");
  const choice = typeof input.choice === "string" ? input.choice.trim() : "";
  const text = typeof input.text === "string" ? input.text.trim() : "";
  const { option, labels } = questionOption(question, choice);
  if (!option && !(text && question.allowCustom))
    throw new AppActionError(
      "invalid_input",
      `This question takes a choice: ${labels}`,
    );
  const reply = option
    ? buildQuestionReply(prompt.questions, { [question.id]: [option.id] })
    : buildQuestionReply(prompt.questions, {}, { [question.id]: text });
  respondHarnessQuestion(source.harness, source.id, prompt.requestId, reply);
  return { answered: "question" };
}

function answerPermission(
  source: Session,
  input: Record<string, unknown>,
): Answered {
  const pending = source.blocks
    .map((block) => block.approval)
    .find(
      (approval) =>
        approval &&
        !approval.decided &&
        isNamed(source, "approval", approval.requestId, input),
    );
  if (!pending) throw monoError("dialog_gone");
  if (input.decision !== "allow" && input.decision !== "deny")
    throw new AppActionError("invalid_input", "decision must be allow or deny");
  respondHarnessApproval(
    source.harness,
    source.id,
    pending.requestId,
    input.decision,
  );
  return { answered: "permission" };
}

/**
 * alarms.answer (alarms design section 7). Only `control.rs` sends it, once
 * it has checked that the caller may answer: to the window that owns the
 * stalled agent's session, with that session as `source`. Only the dialog
 * the alarm named (`dialogRequestId`, the harness's own id) is answered,
 * as the user answers it, so it closes the same way.
 */
export function answerAlarmDialog(
  source: Session,
  input: Record<string, unknown>,
): Answered {
  if (source.worktreeRemoved || isRemoteProjectPath(source.cwd))
    throw monoError("dialog_gone");
  return input.kind === "question"
    ? answerQuestion(source, input)
    : answerPermission(source, input);
}
