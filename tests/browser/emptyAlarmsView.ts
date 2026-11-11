/** `alarms_view` with no alarms, under Rust's default settings (alarms/store.rs). */
export const emptyAlarmsView = () => ({
  readWallMs: Date.now(),
  episodes: [],
  closed: [],
  holds: [],
  settings: {
    stallMinutes: 15,
    escalateMinutes: 15,
    sleepHold: true,
    tylerSleepHold: true,
    shortSleepSkipSeconds: 10,
    rootGiga: null,
    gigas: [],
  },
  stallMinutes: [],
});
