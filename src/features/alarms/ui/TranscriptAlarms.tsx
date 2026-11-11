import {
  createContext,
  useCallback,
  useContext,
  useSyncExternalStore,
} from "react";
import type { AlarmCardView, SleepHoldView } from "../model/alarmViews";
import type { ViewStore } from "../model/viewStore";
import { AlarmCard, type AlarmCardActions } from "./AlarmCard";
import { SleepHoldCard } from "./SleepHoldCard";

/**
 * What a transcript knows about alarms; absent, it renders as it always has.
 * Keep the object stable: each row reads only its own view from the stores,
 * so a ticking countdown re-renders that row and nothing else.
 */
export type TranscriptAlarmsView = AlarmCardActions & {
  /** Delivered `<mono_alarm>` cards, by alarm id. */
  cards: ViewStore<AlarmCardView>;
  /** Sleep holds, by the tool call (`tool_use_id`) they hold. */
  holds: ViewStore<SleepHoldView>;
};

export const TranscriptAlarms = createContext<TranscriptAlarmsView | undefined>(
  undefined,
);

const NONE = () => undefined;

function useView<V>(
  store: ViewStore<V> | undefined,
  id: string | undefined,
): V | undefined {
  const subscribe = useCallback(
    (listener: () => void) =>
      store && id ? store.subscribe(id, listener) : () => {},
    [store, id],
  );
  const read = store && id ? () => store.get(id) : NONE;
  return useSyncExternalStore(subscribe, read, read);
}

/** The card for a delivered `<mono_alarm>`, once the alarm has a view. */
export function useDeliveredAlarm(
  alarmId: string | undefined,
): AlarmCardView | undefined {
  return useView(useContext(TranscriptAlarms)?.cards, alarmId);
}

export function DeliveredAlarm({ view }: { view: AlarmCardView }) {
  const alarms = useContext(TranscriptAlarms);
  return (
    <AlarmCard
      key={view.id}
      view={view}
      onAct={alarms?.onAct}
      onOpenLead={alarms?.onOpenLead}
    />
  );
}

/** Frame 3's hold card under the tool row it holds. */
export function ToolSleepHold({ callId }: { callId?: string }) {
  const alarms = useContext(TranscriptAlarms);
  const view = useView(alarms?.holds, callId);
  if (!view) return null;
  return <SleepHoldCard view={view} onAct={alarms?.onAct} />;
}
