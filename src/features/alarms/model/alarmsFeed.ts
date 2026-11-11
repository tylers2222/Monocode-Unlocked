/**
 * One window's copy of the alarms snapshot (U2 contract, "Event"): fetched
 * on start and on every `monocode-alarms-changed`, one fetch at a time, and
 * mapped to stable views. A view that did not change keeps its identity, so
 * the rows showing it do not re-render.
 */

import {
  freshRequestId,
  holdAct,
  provisionerAct,
  tylerAct,
  type AlarmActHost,
} from "./alarmActs";
import {
  alarmSurfaces,
  holdsCounting,
  type AlarmSurfaces,
  type SessionStallMinutes,
} from "./alarmsMap";
import {
  parseAlarmsSnapshot,
  type AlarmsSnapshot,
  type EpisodeWire,
} from "./alarmsWire";
import type {
  AlarmActEvent,
  AlarmAgent,
  AlarmSettingsView,
} from "./alarmViews";
import { createViewStore } from "./viewStore";
import type { TranscriptAlarmsView } from "../ui/TranscriptAlarms";
import type { AlarmCardActions } from "../ui/AlarmCard";

export const ALARMS_CHANGED = "monocode-alarms-changed";

export type AlarmsFeedDeps = Omit<AlarmActHost, "requestId"> & {
  listen: (event: string, handler: () => void) => Promise<() => void>;
  /** A loaded session's pending question choices, by its session id. */
  questionOptions?: (sessionId: string) => readonly string[] | undefined;
  requestId?: () => string;
  now?: () => number;
};

export type AlarmSettingsChange = Partial<
  Pick<
    AlarmSettingsView,
    "stallMinutes" | "rootGiga" | "holdForegroundWaits" | "holdTylerSleeps"
  >
>;

export type AlarmsFeed = {
  /** Listens and fetches; returns the stop. */
  start: () => () => void;
  refresh: () => Promise<void>;
  subscribe: (listener: () => void) => () => void;
  surfaces: () => AlarmSurfaces | undefined;
  /** The stable view a session's transcript reads its cards and holds from. */
  transcript: (sessionId: string) => TranscriptAlarmsView;
  /** Tyler's landing acts, for any landing. */
  landingActions: AlarmCardActions;
  changeSettings: (change: AlarmSettingsChange) => void;
  setStallMinutes: (sessionId: string, minutes: number | null) => void;
};

const same = (a: unknown, b: unknown) =>
  JSON.stringify(a) === JSON.stringify(b);

/** `next`, with each view equal to one in `previous` swapped for that one. */
export function reuseViews<V>(
  previous: ReadonlyMap<string, V> | undefined,
  next: ReadonlyMap<string, V>,
): ReadonlyMap<string, V> {
  if (!previous) return next;
  let changed = previous.size !== next.size;
  const kept = new Map<string, V>();
  for (const [id, view] of next) {
    const old = previous.get(id);
    const reused = old !== undefined && same(old, view);
    kept.set(id, reused ? old : view);
    changed ||= !reused;
  }
  return changed ? kept : previous;
}

function reuseNested<V>(
  previous: ReadonlyMap<string, ReadonlyMap<string, V>> | undefined,
  next: ReadonlyMap<string, ReadonlyMap<string, V>>,
): ReadonlyMap<string, ReadonlyMap<string, V>> {
  if (!previous) return next;
  let changed = previous.size !== next.size;
  const kept = new Map<string, ReadonlyMap<string, V>>();
  for (const [id, views] of next) {
    const reused = reuseViews(previous.get(id), views);
    kept.set(id, reused);
    changed ||= reused !== previous.get(id);
  }
  return changed ? kept : previous;
}

function reuseStallMinutes(
  previous: SessionStallMinutes | undefined,
  next: SessionStallMinutes,
): SessionStallMinutes {
  if (!previous) return next;
  const bySession = reuseViews(previous.bySession, next.bySession);
  const fallback = same(previous.fallback, next.fallback)
    ? previous.fallback
    : next.fallback;
  return bySession === previous.bySession && fallback === previous.fallback
    ? previous
    : { bySession, fallback };
}

/** Each part of `next` that equals `previous`'s is `previous`'s. */
export function reuseSurfaces(
  previous: AlarmSurfaces | undefined,
  next: AlarmSurfaces,
): AlarmSurfaces {
  if (!previous) return next;
  const kept: AlarmSurfaces = {
    sessionStalls: reuseViews(previous.sessionStalls, next.sessionStalls),
    monoStalls: reuseViews(previous.monoStalls, next.monoStalls),
    cards: reuseNested(previous.cards, next.cards),
    landings: reuseViews(previous.landings, next.landings),
    holds: reuseNested(previous.holds, next.holds),
    settings: same(previous.settings, next.settings)
      ? previous.settings
      : next.settings,
    stallMinutes: reuseStallMinutes(previous.stallMinutes, next.stallMinutes),
  };
  const parts = Object.keys(kept) as (keyof AlarmSurfaces)[];
  return parts.every((part) => kept[part] === previous[part]) ? previous : kept;
}

const onOff = (on: boolean) => (on ? "on" : "off");

/** The `alarms_settings_set` calls a page change makes, in order. */
export function settingWrites(
  change: AlarmSettingsChange,
): { key: string; value: string }[] {
  const writes: { key: string; value: string }[] = [];
  if (change.stallMinutes !== undefined)
    writes.push({ key: "stall_minutes", value: String(change.stallMinutes) });
  if (change.rootGiga !== undefined)
    writes.push({ key: "root_giga", value: change.rootGiga });
  if (change.holdForegroundWaits !== undefined)
    writes.push({
      key: "sleep_hold",
      value: onOff(change.holdForegroundWaits),
    });
  if (change.holdTylerSleeps !== undefined)
    writes.push({
      key: "tyler_sleep_hold",
      value: onOff(change.holdTylerSleeps),
    });
  return writes;
}

const NO_VIEWS: ReadonlyMap<string, never> = new Map<string, never>();

type Transcript = {
  view: TranscriptAlarmsView;
  set: (surfaces: AlarmSurfaces) => void;
};

/** One fetch at a time: an event during a fetch schedules exactly one more. */
function coalesced(fetch: () => Promise<void>): () => Promise<void> {
  let running: Promise<void> | undefined;
  let again = false;
  return () => {
    if (running) {
      again = true;
      return running;
    }
    running = (async () => {
      try {
        do {
          again = false;
          await fetch();
        } while (again);
      } finally {
        running = undefined;
      }
    })();
    return running;
  };
}

/**
 * A session's card and hold stores, set from each new set of surfaces. A row
 * that reads a card names its alarm, so the read can serve it once closed.
 */
function transcriptStores(
  sessionId: string,
  surfaces: AlarmSurfaces | undefined,
  {
    name,
    ...actions
  }: Required<AlarmCardActions> & { name: (alarmId: string) => void },
): Transcript {
  const cards = createViewStore(surfaces?.cards.get(sessionId) ?? NO_VIEWS);
  const holds = createViewStore(surfaces?.holds.get(sessionId) ?? NO_VIEWS);
  return {
    view: {
      cards: {
        get: cards.get,
        subscribe(id, listener) {
          name(id);
          return cards.subscribe(id, listener);
        },
      },
      holds,
      ...actions,
    },
    set: (next) => {
      cards.set(next.cards.get(sessionId) ?? NO_VIEWS);
      holds.set(next.holds.get(sessionId) ?? NO_VIEWS);
    },
  };
}

/** Calls `tick` each second while `run(true)`; `run(false)` stops it. */
function everySecondWhile(tick: () => void): { run: (on: boolean) => void } {
  let timer: ReturnType<typeof setInterval> | undefined;
  return {
    run(on) {
      if (on && !timer) timer = setInterval(tick, 1000);
      if (!on && timer) {
        clearInterval(timer);
        timer = undefined;
      }
    },
  };
}

/** Fetches now and on every change event; returns the stop. */
function listenAndFetch(
  deps: Pick<AlarmsFeedDeps, "listen">,
  refresh: () => Promise<void>,
  stopTick: () => void,
): () => void {
  let stopped = false;
  let unlisten: (() => void) | undefined;
  void deps
    .listen(ALARMS_CHANGED, () => void refresh())
    .then((stop) => (stopped ? stop() : (unlisten = stop)))
    .catch((error: unknown) => console.error(ALARMS_CHANGED, error));
  void refresh();
  return () => {
    stopped = true;
    unlisten?.();
    stopTick();
  };
}

/** The page's and the session menu's writes; each refetches once Rust has it. */
function settingsWriters(
  deps: Pick<AlarmsFeedDeps, "invoke" | "refused">,
  refresh: () => Promise<void>,
): Pick<AlarmsFeed, "changeSettings" | "setStallMinutes"> {
  // Rust answers a refused write with Err(String), so invoke rejects.
  const write = (command: string, args: Record<string, unknown>) =>
    void deps
      .invoke(command, args)
      .then(() => refresh())
      .catch((error: unknown) => deps.refused(command, error));
  return {
    changeSettings(change) {
      for (const args of settingWrites(change))
        write("alarms_settings_set", args);
    },
    setStallMinutes(sessionId, minutes) {
      write("alarms_stall_minutes_set", { sessionId, minutes });
    },
  };
}

/**
 * The alarm ids the transcripts hold, for the read to return even once
 * closed. Rows mount together, so the ids they name share one read.
 */
function alarmNames(
  open: (alarmId: string) => boolean,
  refresh: () => Promise<void>,
): { list: () => string[]; name: (alarmId: string) => void } {
  const named = new Set<string>();
  let pending = false;
  return {
    list: () => [...named],
    name(alarmId) {
      if (named.has(alarmId)) return;
      named.add(alarmId);
      if (open(alarmId) || pending) return;
      pending = true;
      queueMicrotask(() => {
        pending = false;
        void refresh();
      });
    },
  };
}

/** Tyler's landing acts, each on the open episode it names. */
function landingActions(
  host: AlarmActHost,
  episode: (alarmId: string) => EpisodeWire | undefined,
  openLead: (lead: AlarmAgent) => void,
): AlarmCardActions {
  return {
    onAct: (event) => {
      if (event.kind !== "alarm") return;
      const found = episode(event.alarmId);
      if (found) void tylerAct(host, found, event);
      else host.refused("alarms_tyler_act", event);
    },
    onOpenLead: openLead,
  };
}

export function createAlarmsFeed(deps: AlarmsFeedDeps): AlarmsFeed {
  const now = deps.now ?? (() => Date.now());
  const host: AlarmActHost = {
    ...deps,
    requestId: deps.requestId ?? freshRequestId,
  };
  let snapshot: AlarmsSnapshot | undefined;
  let episodes = new Map<string, EpisodeWire>();
  let surfaces: AlarmSurfaces | undefined;
  const ticker = everySecondWhile(() => derive());
  const listeners = new Set<() => void>();
  const transcripts = new Map<string, Transcript>();
  const names = alarmNames(
    (alarmId) => episodes.has(alarmId),
    () => refresh(),
  );

  const derive = () => {
    if (!snapshot) return;
    const next = reuseSurfaces(
      surfaces,
      alarmSurfaces(snapshot, {
        questionOptions: deps.questionOptions,
        nowWallMs: now(),
      }),
    );
    const changed = next !== surfaces;
    surfaces = next;
    for (const transcript of transcripts.values()) transcript.set(next);
    if (changed) for (const listener of [...listeners]) listener();
    // The hold countdown is display only, so it ticks only while one waits.
    ticker.run(holdsCounting(snapshot, now()));
  };

  const fetchOnce = async () => {
    try {
      snapshot = parseAlarmsSnapshot(
        await deps.invoke("alarms_view", { alarms: names.list() }),
      );
    } catch (error) {
      console.error("alarms_view failed", error);
      return;
    }
    episodes = new Map(snapshot.episodes.map((entry) => [entry.id, entry]));
    derive();
  };
  const refresh = coalesced(fetchOnce);

  const openLead = (lead: AlarmAgent) => deps.openAgent(lead.actor);

  const transcriptAct = (sessionId: string, event: AlarmActEvent) =>
    event.kind === "hold"
      ? holdAct(host, event)
      : provisionerAct(host, sessionId, episodes.get(event.alarmId), event);

  const transcript = (sessionId: string): TranscriptAlarmsView => {
    const known = transcripts.get(sessionId);
    if (known) return known.view;
    const created = transcriptStores(sessionId, surfaces, {
      onAct: (event) => void transcriptAct(sessionId, event),
      onOpenLead: openLead,
      name: names.name,
    });
    transcripts.set(sessionId, created);
    return created.view;
  };

  return {
    start: () => listenAndFetch(deps, refresh, () => ticker.run(false)),
    refresh,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    surfaces: () => surfaces,
    transcript,
    landingActions: landingActions(
      host,
      (alarmId) => episodes.get(alarmId),
      openLead,
    ),
    ...settingsWriters(deps, refresh),
  };
}
