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
  workspaceStatus,
  type EnsureReport,
  type EnsureTrigger,
  type LastEnsure,
  type TurnCalls,
  type TurnOutcome,
  type WorkspaceStatus,
} from "./gigaWorkspace";
import {
  findMono,
  listMonos,
  monoForSession,
  monoLook,
  saveMonoGiga,
  type Mono,
} from "./mono";
import { habitRunMono, isHabitRun } from "./monoHabits";

export const GIGA_WORKSPACE_PREFIX = "Giga workspace:";

let home: string | undefined;
let lastReport: EnsureReport | undefined;
let lastEnsure: LastEnsure | undefined;
const reported = new Set<string>();
const turnCalls = new Map<string, TurnCalls>();
let post: (monoId: string, text: string) => void = () => {};

/** How a status line reaches a Giga's chat. The app sets it once. */
export function setGigaWorkspacePoster(
  poster: (monoId: string, text: string) => void,
): void {
  post = poster;
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
  const ready = Promise.all([
    ensureGigaWorkspace(isHabitRun(sessionId) ? "habit" : "turn", [giga.id]),
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
  result: unknown,
): void {
  if (!gigaOfSession(sessionId)) return;
  const calls = turnCalls.get(sessionId) ?? { sent: [], started: [] };
  turnCalls.set(sessionId, recordTurnCall(calls, action, requestId, result));
}

/**
 * After a Giga's turn has settled: its receipt, the commit of its folder and
 * the index rebuild, in the background.
 */
export function finishGigaTurn(
  sessionId: string,
  asked: string,
  outcome: TurnOutcome,
): void {
  const giga = gigaOfSession(sessionId);
  const calls = turnCalls.get(sessionId) ?? { sent: [], started: [] };
  turnCalls.delete(sessionId);
  if (!giga) return;
  const receipt = receiptLine({
    at: Date.now(),
    monoId: giga.id,
    slug: giga.gigaSlug,
    sessionId,
    kind: isHabitRun(sessionId) ? "habit" : "chat",
    asked,
    outcome,
    calls,
  });
  void invoke<{ commit?: string; error?: string }>("giga_workspace_turn_end", {
    turn: { slug: giga.gigaSlug, receipt, outcome, sessionId },
  })
    .then((result) => result.error)
    .catch((error: unknown) =>
      error instanceof Error ? error.message : String(error),
    )
    .then((problem) => reportOnce(problem, [giga.id]));
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
