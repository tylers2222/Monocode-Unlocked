import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  createAlarmsFeed,
  type AlarmSettingsChange,
  type AlarmsFeed,
  type AlarmsFeedDeps,
} from "../model/alarmsFeed";
import type { AlarmSurfaces, SessionStallMinutes } from "../model/alarmsMap";
import type {
  AlarmCardView,
  AlarmSettingsView,
  StallMarkerView,
} from "../model/alarmViews";
import type { AlarmCardActions } from "./AlarmCard";
import type { TranscriptAlarmsView } from "./TranscriptAlarms";

/** What a window does for an act; the feed brings its own invoke and listen. */
export type AlarmsWindowHost = Omit<
  AlarmsFeedDeps,
  "invoke" | "listen" | "requestId" | "now"
>;

const AlarmsFeedContext = createContext<AlarmsFeed | undefined>(undefined);

/**
 * One feed per window, made once. `host` may change every render: the feed
 * reaches the current one, so App's callbacks need no memo.
 */
export function useAlarmsFeed(host: AlarmsWindowHost): AlarmsFeed {
  const current = useRef(host);
  current.current = host;
  const [feed] = useState(() =>
    createAlarmsFeed({
      invoke: (command, args) => invoke(command, args),
      listen: (event, handler) => listen(event, handler),
      openAgent: (actor) => current.current.openAgent(actor),
      answerDialog: (sessionId, input) =>
        current.current.answerDialog(sessionId, input),
      sendAsUser: (sessionId, text) =>
        current.current.sendAsUser(sessionId, text),
      refused: (what, detail) => current.current.refused(what, detail),
      questionOptions: (sessionId) =>
        current.current.questionOptions?.(sessionId),
    }),
  );
  return feed;
}

export function AlarmsProvider({
  feed,
  children,
}: {
  feed: AlarmsFeed;
  children: ReactNode;
}) {
  useEffect(() => feed.start(), [feed]);
  return (
    <AlarmsFeedContext.Provider value={feed}>
      {children}
    </AlarmsFeedContext.Provider>
  );
}

const NOTHING = () => () => {};

/** One part of the surfaces; parts keep their identity until they change. */
function useSurface<T>(pick: (surfaces: AlarmSurfaces) => T): T | undefined {
  const feed = useContext(AlarmsFeedContext);
  const read = () => {
    const surfaces = feed?.surfaces();
    return surfaces && pick(surfaces);
  };
  return useSyncExternalStore(feed?.subscribe ?? NOTHING, read, read);
}

/** Frame 1: open episodes of stalled sessions, by session id. */
export function useSessionStalls():
  ReadonlyMap<string, StallMarkerView> | undefined {
  return useSurface((surfaces) => surfaces.sessionStalls);
}

/** Frame 1 on the Mono rail: open episodes of stalled Monos, by Mono id. */
export function useMonoStalls():
  ReadonlyMap<string, StallMarkerView> | undefined {
  return useSurface((surfaces) => surfaces.monoStalls);
}

/** Frame 5's session menu: its values, and the setter. */
export function useSessionStallMinutes():
  | (SessionStallMinutes & {
      set: (sessionId: string, minutes: number | null) => void;
    })
  | undefined {
  const feed = useContext(AlarmsFeedContext);
  const minutes = useSurface((surfaces) => surfaces.stallMinutes);
  return useMemo(
    () => feed && minutes && { ...minutes, set: feed.setStallMinutes },
    [feed, minutes],
  );
}

/** Frame 5: Settings > Alarms, read from the snapshot, written through Rust. */
export function useAlarmSettings():
  | {
      settings: AlarmSettingsView;
      onChange: (change: AlarmSettingsChange) => void;
    }
  | undefined {
  const feed = useContext(AlarmsFeedContext);
  const settings = useSurface((surfaces) => surfaces.settings);
  return useMemo(
    () => feed && settings && { settings, onChange: feed.changeSettings },
    [feed, settings],
  );
}

/** Frames 2 and 3 in `sessionId`'s transcript: one stable object per session. */
export function useTranscriptAlarms(
  sessionId: string | undefined,
): TranscriptAlarmsView | undefined {
  const feed = useContext(AlarmsFeedContext);
  return useMemo(
    () => (feed && sessionId ? feed.transcript(sessionId) : undefined),
    [feed, sessionId],
  );
}

/** Frame 4: Tyler's landing in the stalled agent's own composer. */
export function useTylerLanding(
  sessionId: string | undefined,
): (AlarmCardActions & { view: AlarmCardView }) | undefined {
  const feed = useContext(AlarmsFeedContext);
  const view = useSurface((surfaces) =>
    sessionId ? surfaces.landings.get(sessionId) : undefined,
  );
  return useMemo(
    () => feed && view && { ...feed.landingActions, view },
    [feed, view],
  );
}
