/**
 * Carries out the Giga workspace rules through the Rust commands. Nothing
 * here throws into a turn: every problem becomes one `Giga workspace:` line,
 * posted once per app run.
 */
import { invoke } from "@tauri-apps/api/core";
import {
  ensureProblem,
  gigaFolder,
  gigaWorkspaceRoot,
  receiptLine,
  recordTurnCall,
  turnGuardRequestId,
  turnGuardStep,
  workspaceStatus,
  TURN_GUARD_LIMIT_MS,
  TURN_GUARD_RETRY_MS,
  TURN_GUARD_STEER,
  type EnsureReport,
  type EnsureTrigger,
  type LastEnsure,
  type TurnCalls,
  type TurnOutcome,
  type TurnStart,
  type WorkspaceStatus,
} from "./gigaWorkspace";
import { addProjectOnce } from "../../projects/model/recents";
import type { Block } from "../../sessions/model/session";
import {
  findMono,
  listMonos,
  monoForSession,
  monoLook,
  saveMonoGiga,
  type Mono,
} from "./mono";
import { habitRunMono, isHabitRun } from "./monoHabits";
import type { MonoRole, TurnId } from "./monoMessages";

export const GIGA_WORKSPACE_PREFIX = "Giga workspace:";

let home: string | undefined;
let lastReport: EnsureReport | undefined;
let lastEnsure: LastEnsure | undefined;
const reported = new Set<string>();
const turnCalls = new Map<string, TurnCalls>();
const turnStarts = new Map<string, TurnStart>();
const turnGuards = new Map<string, TurnGuard>();
let post: (monoId: string, text: string) => void = () => {};

/** How a status line reaches a Giga's chat. The app sets it once. */
export function setGigaWorkspacePoster(
  poster: (monoId: string, text: string) => void,
): void {
  post = poster;
}

/** A running Giga chat turn's guard. Whether the turn was steered is read
 *  from its blocks, so it survives a reload. */
type TurnGuard = {
  turnId: TurnId;
  startedAt: number;
  checks: number;
  cancel?: () => void;
};

/** What the turn guard needs from the app. */
export type TurnGuardHost = {
  now: () => number;
  /** Runs `run` after `ms`; returns its cancel. */
  schedule: (run: () => void, ms: number) => () => void;
  /** Turn `turnId` while it still runs in `sessionId`, else undefined. */
  turn: (
    sessionId: string,
    turnId: TurnId,
  ) =>
    | { blocks: readonly Pick<Block, "appRequestId">[]; canSteer: boolean }
    | undefined;
  /** Steers `text` into turn `turnId` as an app steer tagged `requestId`. */
  steer: (
    sessionId: string,
    turnId: TurnId,
    text: string,
    requestId: string,
  ) => Promise<void>;
};
let guardHost: TurnGuardHost | undefined;

export function setTurnGuardHost(host: TurnGuardHost): void {
  guardHost = host;
}

async function knownHome(): Promise<string> {
  home ??= await invoke<string>("home_dir");
  return home;
}

/** The Giga a chat or habit run works for, once it has its slug. */
export function gigaOfSession(
  sessionId: string,
): (Mono & { gigaSlug: string }) | undefined {
  const mono =
    monoForSession(sessionId) ?? findMono(habitRunMono(sessionId) ?? "");
  return mono?.giga && mono.gigaSlug
    ? (mono as Mono & { gigaSlug: string })
    : undefined;
}

/** Posts `Giga workspace: <problem>` the first time this run sees it. */
function reportOnce(problem: string | undefined, monoIds: readonly string[]) {
  if (!problem || reported.has(problem)) return;
  reported.add(problem);
  for (const monoId of monoIds)
    post(monoId, `${GIGA_WORKSPACE_PREFIX} ${problem}`);
}

/**
 * Creates whatever is missing of the workspace and every Giga's folder. A
 * problem goes to the chats of `notify`, by default every Giga's.
 */
export async function ensureGigaWorkspace(
  trigger: EnsureTrigger,
  notify?: readonly string[],
): Promise<void> {
  let report: EnsureReport;
  try {
    const root = gigaWorkspaceRoot(await knownHome());
    // A Giga turned on before this app knew its home folder gets it now.
    for (const mono of listMonos())
      if (mono.giga && !mono.gigaSlug) saveMonoGiga(mono.id, true, root);
    const gigas = listMonos().flatMap((mono) =>
      mono.giga && mono.gigaSlug
        ? [{ slug: mono.gigaSlug, name: monoLook(mono).name }]
        : [],
    );
    report = await invoke<EnsureReport>("giga_workspace_ensure", { gigas });
    // The workspace is every Giga's first project, so the rail shows it the
    // way "Open folder…" would, unless the user archived it.
    if (report.state === "ready" && gigas.length > 0) addProjectOnce(root);
  } catch (error) {
    report = {
      root: home ? gigaWorkspaceRoot(home) : "",
      state: "ready",
      created: [],
      error: error instanceof Error ? error.message : String(error),
    };
  }
  lastReport = report;
  lastEnsure = {
    at: Date.now(),
    trigger,
    created: report.created,
    ...(report.error ? { error: report.error } : {}),
    ...(report.warning ? { warning: report.warning } : {}),
  };
  reportOnce(
    ensureProblem(report),
    notify ??
      listMonos()
        .filter((mono) => mono.giga)
        .map((mono) => mono.id),
  );
}

/** Turns Giga on: its slug, the workspace as its first project, its folder. */
export async function turnOnGiga(monoId: string): Promise<void> {
  const root = await knownHome()
    .then(gigaWorkspaceRoot)
    .catch(() => undefined);
  saveMonoGiga(monoId, true, root);
  await ensureGigaWorkspace("giga-on", [monoId]);
}

/**
 * Before a Giga's turn: the workspace, and the slug its agent process gets
 * as `MONOCODE_GIGA_SLUG`. Gives up waiting after `waitMs`, so a slow disk
 * never holds the turn; the run carries on in the background.
 */
export async function prepareGigaTurn(
  sessionId: string,
  waitMs = 3_000,
): Promise<void> {
  const giga = gigaOfSession(sessionId);
  if (!giga) return;
  turnCalls.delete(sessionId);
  turnStarts.delete(sessionId);
  const ready = Promise.all([
    ensureGigaWorkspace(isHabitRun(sessionId) ? "habit" : "turn", [giga.id])
      .then(() =>
        invoke<TurnStart | null>("giga_workspace_turn_start", {
          slug: giga.gigaSlug,
        }),
      )
      .then((start) => {
        if (start) turnStarts.set(sessionId, start);
      })
      .catch(() => undefined),
    invoke("giga_workspace_bind", { sessionId, slug: giga.gigaSlug }).catch(
      () => undefined,
    ),
  ]);
  await Promise.race([
    ready,
    new Promise((resolve) => window.setTimeout(resolve, waitMs)),
  ]);
}

/** Records a finished app CLI call for the receipt of its session's turn. */
export function noteGigaTurnCall(
  sessionId: string,
  action: string,
  requestId: string,
  input: Record<string, unknown>,
  result: unknown,
): void {
  if (!gigaOfSession(sessionId)) return;
  const calls = turnCalls.get(sessionId) ?? { sent: [], started: [] };
  turnCalls.set(
    sessionId,
    recordTurnCall(
      calls,
      action,
      requestId,
      input,
      result,
      (id) => !!findMono(id),
    ),
  );
}

/**
 * After a Giga's turn has settled: its receipt with the files the turn
 * wrote, the commit of its folder when it changed more than the log, and the
 * index rebuild, in the background.
 */
export function finishGigaTurn(
  sessionId: string,
  asked: string,
  outcome: TurnOutcome,
): void {
  const giga = gigaOfSession(sessionId);
  const calls = turnCalls.get(sessionId) ?? { sent: [], started: [] };
  const before = turnStarts.get(sessionId) ?? null;
  turnCalls.delete(sessionId);
  turnStarts.delete(sessionId);
  if (!giga) return;
  const at = Date.now();
  void invoke<string[]>("giga_workspace_turn_files", {
    slug: giga.gigaSlug,
    before,
  })
    .catch(() => [])
    .then((files) =>
      invoke<{ commit?: string; error?: string }>("giga_workspace_turn_end", {
        turn: {
          slug: giga.gigaSlug,
          receipt: receiptLine({
            at,
            monoId: giga.id,
            slug: giga.gigaSlug,
            sessionId,
            kind: isHabitRun(sessionId) ? "habit" : "chat",
            asked,
            outcome,
            calls,
            files,
          }),
          outcome,
          sessionId,
        },
      }),
    )
    .then((result) => result.error)
    .catch((error: unknown) =>
      error instanceof Error ? error.message : String(error),
    )
    .then((problem) => reportOnce(problem, [giga.id]));
}

function chatRole(sessionId: string): MonoRole | undefined {
  const mono = monoForSession(sessionId);
  return mono ? (mono.giga ? "giga" : "mono") : undefined;
}

/**
 * Starts the guard of turn `turnId`: a Giga's own chat still running it at
 * the limit is steered once to move the work to a session.
 */
export function armTurnGuard(sessionId: string, turnId: TurnId): void {
  const host = guardHost;
  if (!host) return;
  disarmTurnGuard(sessionId);
  const first = turnGuardStep({
    role: chatRole(sessionId),
    elapsedMs: 0,
    steered: false,
    canSteer: false,
    checks: 0,
  });
  if (first.kind !== "wait") return;
  const guard: TurnGuard = { turnId, startedAt: host.now(), checks: 0 };
  turnGuards.set(sessionId, guard);
  guard.cancel = host.schedule(
    () => checkTurnGuard(sessionId, guard, host),
    first.ms,
  );
}

/** Clears the guard of `sessionId`, or only the one of turn `turnId`. */
export function disarmTurnGuard(sessionId: string, turnId?: TurnId): void {
  const guard = turnGuards.get(sessionId);
  if (!guard || (turnId !== undefined && guard.turnId !== turnId)) return;
  guard.cancel?.();
  turnGuards.delete(sessionId);
}

function checkTurnGuard(
  sessionId: string,
  guard: TurnGuard,
  host: TurnGuardHost,
): void {
  guard.cancel = undefined;
  if (turnGuards.get(sessionId) !== guard) return;
  const turn = host.turn(sessionId, guard.turnId);
  if (!turn) return disarmTurnGuard(sessionId);
  const elapsedMs = host.now() - guard.startedAt;
  const requestId = turnGuardRequestId(guard.turnId);
  const step = turnGuardStep({
    role: chatRole(sessionId),
    elapsedMs,
    steered: turn.blocks.some((block) => block.appRequestId === requestId),
    canSteer: turn.canSteer,
    checks: guard.checks,
  });
  const recheck = (ms: number) => {
    if (turnGuards.get(sessionId) === guard)
      guard.cancel = host.schedule(
        () => checkTurnGuard(sessionId, guard, host),
        ms,
      );
  };
  if (step.kind === "skip") return disarmTurnGuard(sessionId);
  if (step.kind === "wait") {
    if (elapsedMs >= TURN_GUARD_LIMIT_MS) guard.checks += 1;
    return recheck(step.ms);
  }
  guard.checks += 1;
  // The steer's block is recorded before the harness call, so the turn reads
  // as steered from here on; a rejected steer removes it again.
  host
    .steer(sessionId, guard.turnId, TURN_GUARD_STEER, requestId)
    .then(() => disarmTurnGuard(sessionId, guard.turnId))
    .catch(() => recheck(TURN_GUARD_RETRY_MS));
}

/** Where a Giga's chat and habit runs work, once the home folder is known. */
export function gigaCwd(
  mono: Pick<Mono, "giga" | "gigaSlug">,
): string | undefined {
  return mono.giga && mono.gigaSlug && home
    ? gigaFolder(home, mono.gigaSlug)
    : undefined;
}

export async function gigaWorkspaceStatus(
  monoId: string,
): Promise<WorkspaceStatus> {
  if (!lastReport) await ensureGigaWorkspace("start");
  const mono = findMono(monoId);
  return workspaceStatus(
    lastReport ?? {
      root: gigaWorkspaceRoot(await knownHome()),
      state: "ready",
    },
    lastEnsure,
    mono?.giga ? mono.gigaSlug : undefined,
  );
}
