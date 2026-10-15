/**
 * The shared Giga workspace at `~/MonoCode`, as the app sees it: the rules
 * are pure functions here; `gigaWorkspaceRuntime.ts` carries them out.
 */
import { joinPath } from "../../../shared/lib/paths";

export const GIGA_SLUG_MAX = 48;
export const RECEIPT_ASKED_MAX = 200;

export type EnsureTrigger = "start" | "giga-on" | "turn" | "habit";
export type TurnOutcome = "completed" | "failed" | "cancelled";

/** What `giga_workspace_ensure` reports. */
export type EnsureReport = {
  root: string;
  state: "ready" | "blocked";
  reason?: string;
  created: string[];
  error?: string;
  warning?: string;
};

export type LastEnsure = {
  at: number;
  trigger: EnsureTrigger;
  created: string[];
  error?: string;
  warning?: string;
};

export type WorkspaceStatus = {
  root: string;
  state: "ready" | "blocked";
  reason?: string;
  gigaSlug?: string;
  lastEnsure?: LastEnsure;
};

export type SentRecord = { requestId: string; toMonoId: string };
export type StartedRecord = { requestId: string; sessionId: string };
export type TurnCalls = { sent: SentRecord[]; started: StartedRecord[] };

export function gigaWorkspaceRoot(home: string): string {
  return joinPath(home, "MonoCode");
}

export function gigaFolder(home: string, slug: string): string {
  return joinPath(gigaWorkspaceRoot(home), `gigas/${slug}`);
}

/**
 * The slug a Mono keeps from the first time Giga is turned on: its name in
 * lowercase with every other run of characters as one `-`, at most 48
 * characters, and `-2`, `-3`, ... after a slug another Mono has.
 */
export function gigaSlugFor(name: string, taken: readonly string[]): string {
  const base =
    name
      .normalize("NFKD")
      .replace(/\p{M}+/gu, "")
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "") || "giga";
  const fit = (suffix: string) =>
    `${base.slice(0, GIGA_SLUG_MAX - suffix.length).replace(/-+$/, "")}${suffix}`;
  const used = new Set(taken);
  for (let n = 1; ; n += 1) {
    const slug = fit(n === 1 ? "" : `-${n}`);
    if (!used.has(slug)) return slug;
  }
}

/**
 * Adds one finished app CLI call to the turn's records. Only the calls a
 * receipt names count, and a retried request ID is recorded once.
 */
export function recordTurnCall(
  calls: TurnCalls,
  action: string,
  requestId: string,
  result: unknown,
): TurnCalls {
  const value = (result ?? {}) as Record<string, unknown>;
  if (action === "monos.send" && typeof value.monoId === "string") {
    if (calls.sent.some((sent) => sent.requestId === requestId)) return calls;
    return {
      ...calls,
      sent: [...calls.sent, { requestId, toMonoId: value.monoId }],
    };
  }
  if (action === "sessions.start" && typeof value.id === "string") {
    if (calls.started.some((started) => started.requestId === requestId))
      return calls;
    return {
      ...calls,
      started: [...calls.started, { requestId, sessionId: value.id }],
    };
  }
  return calls;
}

export type Receipt = {
  at: number;
  monoId: string;
  slug: string;
  sessionId: string;
  kind: "chat" | "habit";
  asked: string;
  outcome: TurnOutcome;
  calls: TurnCalls;
  /** Work files the turn added or changed, relative to `~/MonoCode`. */
  files: readonly string[];
};

/** Content hash by path of a Giga's work files, taken as a turn starts. */
export type TurnStart = Record<string, string>;

/** One `log.jsonl` line, in the contract's field order. */
export function receiptLine(receipt: Receipt): string {
  return JSON.stringify({
    v: 1,
    at: receipt.at,
    giga: { monoId: receipt.monoId, slug: receipt.slug },
    sessionId: receipt.sessionId,
    kind: receipt.kind,
    // Code points, so a cut never splits a character in two.
    asked: Array.from(receipt.asked).slice(0, RECEIPT_ASKED_MAX).join(""),
    outcome: receipt.outcome,
    sent: receipt.calls.sent,
    started: receipt.calls.started,
    files: receipt.files,
  });
}

export function workspaceStatus(
  report: Pick<EnsureReport, "root" | "state" | "reason">,
  lastEnsure: LastEnsure | undefined,
  gigaSlug: string | undefined,
): WorkspaceStatus {
  return {
    root: report.root,
    state: report.state,
    ...(report.state === "blocked" && report.reason
      ? { reason: report.reason }
      : {}),
    ...(gigaSlug ? { gigaSlug } : {}),
    ...(lastEnsure ? { lastEnsure } : {}),
  };
}

/** What a Giga's chat should be told about an ensure run, if anything. */
export function ensureProblem(report: EnsureReport): string | undefined {
  if (report.state === "blocked") return report.reason ?? "blocked";
  return report.error ?? report.warning;
}
