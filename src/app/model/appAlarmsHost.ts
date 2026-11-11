import { answerAlarmDialog } from "../../features/agent-app/model/agentAppAlarms";
import { monoError } from "../../features/agent-app/model/appActionError";
import { actorId } from "../../features/alarms/model/alarmsWire";
import type { AlarmsWindowHost } from "../../features/alarms/ui/AlarmsProvider";
import type { Session } from "../../features/sessions/model/session";

/** What the main window lends the alarms feed. */
export type AppAlarmsDeps = {
  sessions: () => readonly Session[];
  openSession: (sessionId: string) => Promise<unknown>;
  openMono: (monoId: string) => Promise<unknown>;
  /** The composer's submit; `steer` joins a running turn as typed text would. */
  submit: (
    sessionId: string,
    text: string,
    options: { followUpBehavior: "steer" },
  ) => boolean;
};

const said = (what: string) => (error: unknown) =>
  console.error(`${what} failed`, error);

/**
 * The main window's side of an alarm act: open, answer and send exactly as
 * the user would from this window.
 */
export function appAlarmsHost(deps: AppAlarmsDeps): AlarmsWindowHost {
  const session = (sessionId: string) =>
    deps.sessions().find((entry) => entry.id === sessionId);
  return {
    openAgent(actor) {
      const monoId = actorId(actor, "mono");
      if (monoId) {
        deps.openMono(monoId).catch(said(`Opening ${actor}`));
        return;
      }
      const sessionId = actorId(actor, "session");
      if (sessionId)
        deps.openSession(sessionId).catch(said(`Opening ${actor}`));
    },
    answerDialog(sessionId, input) {
      const source = session(sessionId);
      // Not loaded here: the dialog is not one this window can answer.
      if (!source) throw monoError("dialog_gone");
      answerAlarmDialog(source, input);
    },
    sendAsUser(sessionId, text) {
      if (!deps.submit(sessionId, text, { followUpBehavior: "steer" }))
        console.error(`The alarm's message did not reach ${sessionId}`);
    },
    refused: (what, detail) => console.error(`${what} refused`, detail),
    questionOptions: (sessionId) =>
      session(sessionId)?.pendingQuestion?.questions[0]?.options.map(
        (option) => option.label,
      ),
  };
}
