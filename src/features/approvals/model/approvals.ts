import { invoke } from "@tauri-apps/api/core";
import { normalizeProjectPath } from "../../projects/model/recents";
import { countBadge } from "../../../shared/lib/numbers";

export type Urgency = "blocking" | "not_blocking";

export type ApprovalOption = {
  label: string;
  description: string | null;
  recommended: boolean;
};

export type ApprovalLink = {
  kind: "pr" | "doop" | "linear";
  title: string | null;
  url: string;
};

export type ApprovalCard = {
  heading: string;
  summary: string;
  tradeoffs: { pros: string[]; cons: string[] };
  options: ApprovalOption[];
  urgency: Urgency;
  links: ApprovalLink[];
  origin: string | null;
};

export type Approval = ApprovalCard & {
  id: string;
  number: number;
  projectPath: string;
  /** `id` is null once the filing Giga is deleted. */
  requestedBy: { id: string | null; name: string };
  /** Unix milliseconds, as are the status times. */
  createdAt: number;
  status: ApprovalStatus;
};

export type ApprovalStatus =
  | { state: "open" }
  | { state: "withdrawn"; withdrawnAt: number }
  | {
      state: "decided";
      verdict: "approved" | "rejected" | "changes_requested";
      decidedAt: number;
      note: string | null;
      chosen: string[];
      /** Dropped: sent again to the Giga's next chat, or removed by the user. */
      undelivered: "waiting" | "removed" | null;
    };

export const CARD_FIELDS = [
  "heading",
  "summary",
  "tradeoffs",
  "options",
  "urgency",
  "links",
  "origin",
] as const;

function onlyFields(
  value: unknown,
  name: string,
  keys: readonly string[],
): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    throw new Error(`${name} must be an object`);
  const unknown = Object.keys(value).filter((key) => !keys.includes(key));
  if (unknown.length)
    throw new Error(`Unknown ${name} fields: ${unknown.join(", ")}`);
  return value as Record<string, unknown>;
}

function cardText(value: unknown, name: string): string {
  if (typeof value !== "string") throw new Error(`${name} must be a string`);
  return value;
}

/** Absent or null is null, the form the card comes back in from Rust. */
function optionalCardText(value: unknown, name: string): string | null {
  return value === undefined || value === null ? null : cardText(value, name);
}

function cardList<T>(
  value: unknown,
  name: string,
  item: (value: unknown, name: string) => T,
): T[] {
  if (!Array.isArray(value)) throw new Error(`${name} must be an array`);
  return value.map((entry, index) => item(entry, `${name}[${index}]`));
}

function approvalOption(value: unknown, name: string): ApprovalOption {
  const option = onlyFields(value, name, [
    "label",
    "description",
    "recommended",
  ]);
  const { recommended = false } = option;
  if (typeof recommended !== "boolean")
    throw new Error(`${name}.recommended must be a boolean`);
  return {
    label: cardText(option.label, `${name}.label`),
    description: optionalCardText(option.description, `${name}.description`),
    recommended,
  };
}

function approvalLink(value: unknown, name: string): ApprovalLink {
  const link = onlyFields(value, name, ["kind", "title", "url"]);
  const { kind } = link;
  if (kind !== "pr" && kind !== "doop" && kind !== "linear")
    throw new Error(`${name}.kind must be pr, doop or linear`);
  return {
    kind,
    title: optionalCardText(link.title, `${name}.title`),
    url: cardText(link.url, `${name}.url`),
  };
}

/**
 * A card from the CLI's input, with an unknown key at any level refused. Text
 * is not trimmed or measured: Rust's `Card::into_columns` owns those rules.
 */
export function parseApprovalCard(
  input: Record<string, unknown>,
): ApprovalCard {
  const { urgency, options = [], links = [] } = input;
  if (urgency !== "blocking" && urgency !== "not_blocking")
    throw new Error('urgency must be "blocking" or "not_blocking"');
  const tradeoffs = onlyFields(input.tradeoffs, "tradeoffs", ["pros", "cons"]);
  return {
    heading: cardText(input.heading, "heading"),
    summary: cardText(input.summary, "summary"),
    tradeoffs: {
      pros: cardList(tradeoffs.pros, "tradeoffs.pros", cardText),
      cons: cardList(tradeoffs.cons, "tradeoffs.cons", cardText),
    },
    options: cardList(options, "options", approvalOption),
    urgency,
    links: cardList(links, "links", approvalLink),
    origin: optionalCardText(input.origin, "origin"),
  };
}

export function file(
  project: string,
  card: ApprovalCard,
  by: string,
): Promise<Approval> {
  return invoke("approvals_file", {
    projectPath: normalizeProjectPath(project),
    by,
    card,
  });
}

export function update(
  id: string,
  card: ApprovalCard,
  by: string,
): Promise<Approval> {
  return invoke("approvals_update", { id, by, card });
}

export function withdraw(id: string, by: string): Promise<Approval> {
  return invoke("approvals_withdraw", { id, by });
}

/** Rust emits it after every card write: file, update, withdraw (ADE-51) and decide (ADE-52). */
export const APPROVALS_CHANGED = "approvals-changed";

export type OpenCounts = { total: number; byProject: Record<string, number> };

export type ApprovalsBadge = { label: string; pill?: string };

export function openCounts(): Promise<OpenCounts> {
  return invoke("approvals_open_counts");
}

export function listApprovals(projectPath: string): Promise<Approval[]> {
  return invoke("approvals_list", { projectPath });
}

export function approvalsBadge(open: number): ApprovalsBadge {
  return open > 0
    ? { label: `Approvals, ${open} open`, pill: countBadge(open) }
    : { label: "Approvals" };
}

export type Board = { open: Approval[]; summary: string };

/** `approvals_list` sends open cards first (blocking, then newest) and the rest after; the board keeps that order. */
export function board(cards: readonly Approval[]): Board {
  const open = cards.filter((card) => card.status.state === "open");
  const blocking = open.filter((card) => card.urgency === "blocking").length;
  const summary = `${open.length} open · ${blocking} blocking · ${cards.length - open.length} decided`;
  return { open, summary };
}

const date = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
});

function daysBefore(at: number, now: number): number {
  const midnight = (ms: number) => new Date(ms).setHours(0, 0, 0, 0);
  return Math.round((midnight(now) - midnight(at)) / 86_400_000);
}

export function ago(at: number, now: number): string {
  const minutes = Math.floor((now - at) / 60_000);
  if (minutes < 60) return `${Math.max(1, minutes)}m ago`;
  const days = daysBefore(at, now);
  if (days === 0) return `${Math.floor(minutes / 60)}h ago`;
  return days === 1 ? "yesterday" : date.format(at);
}
