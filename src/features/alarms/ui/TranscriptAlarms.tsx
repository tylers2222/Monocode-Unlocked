import { createContext, useContext } from "react";
import {
  monoAlarmId,
  type AlarmCardView,
  type SleepHoldView,
} from "../model/alarmViews";
import { AlarmCard, type AlarmCardActions } from "./AlarmCard";
import { SleepHoldCard } from "./SleepHoldCard";

/** What a transcript knows about alarms; absent, it renders as it always has. */
export type TranscriptAlarmsView = AlarmCardActions & {
  /** Delivered `<mono_alarm>` cards, by alarm id. */
  cards: ReadonlyMap<string, AlarmCardView>;
  /** Sleep holds, by the tool call they hold. */
  holds: ReadonlyMap<string, SleepHoldView>;
  onAllowHold?: (holdId: string) => void;
  onDenyHold?: (holdId: string) => void;
};

export const TranscriptAlarms = createContext<TranscriptAlarmsView | undefined>(
  undefined,
);

/** The card for a delivered `<mono_alarm>` message, once it has a view. */
export function useDeliveredAlarm(text: string): AlarmCardView | undefined {
  const alarms = useContext(TranscriptAlarms);
  if (!alarms?.cards.size) return undefined;
  const id = monoAlarmId(text);
  return id ? alarms.cards.get(id) : undefined;
}

export function DeliveredAlarm({ view }: { view: AlarmCardView }) {
  const alarms = useContext(TranscriptAlarms);
  return (
    <AlarmCard
      view={view}
      onAct={alarms?.onAct}
      onAnswer={alarms?.onAnswer}
      onOpenLead={alarms?.onOpenLead}
    />
  );
}

/** Frame 3's hold card under the tool row it holds. */
export function ToolSleepHold({ callId }: { callId?: string }) {
  const alarms = useContext(TranscriptAlarms);
  const view = callId ? alarms?.holds.get(callId) : undefined;
  if (!view) return null;
  return (
    <SleepHoldCard
      view={view}
      onAllow={alarms?.onAllowHold}
      onDeny={alarms?.onDenyHold}
    />
  );
}
