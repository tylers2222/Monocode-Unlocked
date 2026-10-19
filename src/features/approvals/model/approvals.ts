import { invoke } from "@tauri-apps/api/core";
import { normalizeProjectPath } from "../../projects/model/recents";
import { countBadge } from "../../../shared/lib/numbers";
import type { Verdict } from "./boardDecision";

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

export type Closed = Verdict | "withdrawn";

export type ClosedStatus = Exclude<ApprovalStatus, { state: "open" }>;

export type ClosedApproval = Approval & { status: ClosedStatus };

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

/** Its Giga was deleted after it filed: `requested_by` is null (ON DELETE SET NULL). */
export function filerRemoved(card: Approval): boolean {
  return card.requestedBy.id === null;
}

export function closedAs(status: ClosedStatus): Closed {
  return status.state === "decided" ? status.verdict : "withdrawn";
}

export function closedAt(status: ClosedStatus): number {
  return status.state === "decided" ? status.decidedAt : status.withdrawnAt;
}

export type Board = {
  open: Approval[];
  closed: ClosedApproval[];
  summary: string;
};

/** `approvals_list` sends open cards first (blocking, then newest) and the rest after; the board keeps that order. */
export function board(cards: readonly Approval[]): Board {
  const open = cards.filter((card) => card.status.state === "open");
  const closed = cards.filter(
    (card): card is ClosedApproval => card.status.state !== "open",
  );
  const blocking = open.filter((card) => card.urgency === "blocking").length;
  const summary = `${open.length} open · ${blocking} blocking · ${closed.length} decided`;
  return { open, closed, summary };
}

/**
 * Where the decision went (ADE-52 `undelivered`): `waiting` is sent again to
 * the Giga's next chat (A·5b), `unqueued` was removed from the Giga's queue by
 * the user and is never sent (A·5c).
 */
export type Delivery =
  "sent" | "waiting" | "unqueued" | "filer_removed" | "withdrawn";

export function delivery(card: ClosedApproval): Delivery {
  if (card.status.state === "withdrawn") return "withdrawn";
  if (filerRemoved(card)) return "filer_removed";
  if (card.status.undelivered === "removed") return "unqueued";
  return card.status.undelivered ?? "sent";
}

const clock = new Intl.DateTimeFormat(undefined, {
  hour: "numeric",
  minute: "2-digit",
});
const date = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
});

const time = (at: number) => clock.format(at).toLowerCase();

function daysBefore(at: number, now: number): number {
  const midnight = (ms: number) => new Date(ms).setHours(0, 0, 0, 0);
  return Math.round((midnight(now) - midnight(at)) / 86_400_000);
}

export const stamp = (at: number) => `${date.format(at)}, ${time(at)}`;

export function dayOrTime(at: number, now: number): string {
  const days = daysBefore(at, now);
  if (days === 0) return time(at);
  return days === 1 ? "yesterday" : date.format(at);
}

export function ago(at: number, now: number): string {
  const minutes = Math.floor((now - at) / 60_000);
  if (minutes < 60) return `${Math.max(1, minutes)}m ago`;
  return daysBefore(at, now) === 0
    ? `${Math.floor(minutes / 60)}h ago`
    : dayOrTime(at, now);
}

export const deliveryTime = (at: number, now: number) =>
  daysBefore(at, now) === 0 ? time(at) : stamp(at);

export function decide(
  card: Approval,
  verdict: Verdict,
  comment: string,
  chosen: string[],
): Promise<Approval> {
  return invoke("approvals_decide", {
    id: card.id,
    verdict,
    note: comment.trim() ? comment : null,
    chosen,
  });
}

export type ApproveButton = { enabled: boolean; label: string };

export function approveButton(options: number, ticked: number): ApproveButton {
  if (options === 0) return { enabled: true, label: "Approve" };
  if (ticked === 0) return { enabled: false, label: "Approve" };
  return {
    enabled: true,
    label: `Approve ${ticked} option${ticked === 1 ? "" : "s"}`,
  };
}

/**
 * Ticks and the cursor by option label: labels are unique per card and are
 * its only option id, so a Giga update never moves a tick to another option.
 */
export type Picker = { cursor: string | null; ticked: ReadonlySet<string> };

export const OPEN_PICKER: Picker = { cursor: null, ticked: new Set() };

/** The ticked labels the card still has, in its order. */
export function chosen(
  picker: Picker,
  options: readonly ApprovalOption[],
): string[] {
  return options
    .map((option) => option.label)
    .filter((label) => picker.ticked.has(label));
}

/** The cursor's row; before any key or tick, the Giga's recommended row. */
export function cursorRow(
  picker: Picker,
  options: readonly ApprovalOption[],
): number {
  const at = options.findIndex((option) => option.label === picker.cursor);
  if (at >= 0) return at;
  return Math.max(
    0,
    options.findIndex((option) => option.recommended),
  );
}

export function tick(picker: Picker, label: string): Picker {
  const ticked = new Set(picker.ticked);
  if (!ticked.delete(label)) ticked.add(label);
  return { cursor: label, ticked };
}

export function pickerKey(
  picker: Picker,
  key: string,
  options: readonly ApprovalOption[],
): Picker | null {
  if (options.length === 0) return null;
  const row = cursorRow(picker, options);
  const to = (at: number) => ({ ...picker, cursor: options[at].label });
  if (key === "ArrowDown") return to(Math.min(options.length - 1, row + 1));
  if (key === "ArrowUp") return to(Math.max(0, row - 1));
  if (key === " ") return tick(picker, options[row].label);
  const option = /^[1-9]$/.test(key) ? options[Number(key) - 1] : undefined;
  return option ? tick(picker, option.label) : null;
}
