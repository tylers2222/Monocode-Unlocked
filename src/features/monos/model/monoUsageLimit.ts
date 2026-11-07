import { invoke } from "@tauri-apps/api/core";
import { CONTINUE_PROMPT } from "../../sessions/model/inFlight";
import type { Session } from "../../sessions/model/session";
import {
  accountHeadroom,
  accountUsageKey,
} from "../../providers/model/accountUsage";
import {
  DEFAULT_PROVIDER_ACCOUNT_ID,
  providerAccountLabel,
  providerAccounts,
  sameProviderAccountId,
  selectedProviderAccountId,
  supportsProviderAccounts,
  type ProviderAccount,
  type ProviderAccountProvider,
} from "../../providers/model/providerAccounts";
import {
  exhaustedWindowResetAt,
  formatResetDuration,
  formatUsagePercent,
  type ProviderRateLimits,
} from "../../providers/model/rateLimits";
import type { RouteGrantDuration } from "../../settings/model/settings";
import {
  expiredRouteGrant,
  formatUsageLimitReset,
  resumeUsageLimitedSession,
  withUsageLimitAccount,
} from "../../sessions/model/usageLimit";
import { enqueueMonoMessage } from "./monoMessaging";

/** Retry the existing outbox in order, or continue the stopped work once. */
export function resumeMonoUsageLimit(session: Session): Session {
  if (!session.usageLimit || session.busy) return session;
  const resumed = resumeUsageLimitedSession(session);
  return resumed.queuedMessages?.length
    ? resumed
    : enqueueMonoMessage(resumed, {
        id: crypto.randomUUID(),
        text: CONTINUE_PROMPT,
        attachments: [],
      });
}

/** Continue under another account, carrying the native transcript when it
 * was copied there, otherwise through a transcript handoff. */
export function switchMonoUsageLimitAccount(
  session: Session,
  accountId: string,
  carried = false,
): Session {
  if (
    !session.usageLimit ||
    session.busy ||
    sameProviderAccountId(session.providerAccountId, accountId)
  )
    return session;
  return resumeMonoUsageLimit(
    withUsageLimitAccount(session, accountId, carried),
  );
}

/** The usage-limit card's lengths. Settings only picks the one preselected. */
export type RouteGrantLength =
  "1h" | "3h" | "6h" | "12h" | "24h" | "untilReset";

/** A session moved to another login until `expiresAt` (`spaces/grants.rs`). */
export type RouteGrant = {
  sessionId: string;
  provider: ProviderAccountProvider;
  fromAccountId: string;
  toAccountId: string;
  grantedAt: number;
  expiresAt: number;
};

/** What `spaces_grant_save` takes. */
export type RouteGrantRequest = Omit<RouteGrant, "grantedAt" | "expiresAt"> & {
  length: RouteGrantLength;
  firstLoginResetsAt?: number;
};

/** The card's choice as `spaces_grant_save` takes it. */
export function routeGrantLength(duration: RouteGrantDuration): RouteGrantLength {
  return duration === "reset" ? "untilReset" : `${duration}h`;
}

/**
 * The login the session's next turn runs on, as the turn path picks it
 * (`App.tsx`): its own, else its project's. `default` is its Space's login,
 * which `spaces_grant_save` resolves as the spawn hook does.
 */
export function sessionLogin(
  session: Session,
  provider: ProviderAccountProvider,
): string {
  return session.providerAccountId ?? selectedProviderAccountId(provider, session.cwd);
}

/**
 * Yes on the usage-limit card. "Until the first login resets" waits for
 * `firstLoginResetsAt`, the reset of the login the session leaves, which the
 * backend requires. After an expired grant the session is back on its first
 * login, so asking again starts from there.
 */
export function routeGrantRequest(
  session: Session,
  toAccountId: string,
  duration: RouteGrantDuration,
  firstLoginResetsAt: number | undefined,
): RouteGrantRequest | undefined {
  if (!supportsProviderAccounts(session.harness)) return undefined;
  return {
    sessionId: session.id,
    provider: session.harness,
    fromAccountId: sessionLogin(session, session.harness),
    toAccountId,
    length: routeGrantLength(duration),
    firstLoginResetsAt,
  };
}

/** `spaces_grant_save`: the backend checks the logins and stores the grant. */
export function saveRouteGrant(request: RouteGrantRequest): Promise<RouteGrant> {
  return invoke<RouteGrant>("spaces_grant_save", { request });
}

/** A Space's login for one CLI (`spaces_logins_list`). */
export type SpaceLogin = {
  spaceId: string;
  provider: ProviderAccountProvider;
  accountId: string;
};

/** Every Space's logins, and the Space on screen. */
export type SpaceLogins = { activeSpaceId: string; logins: SpaceLogin[] };

export async function loadSpaceLogins(): Promise<SpaceLogins> {
  const [settings, logins] = await Promise.all([
    invoke<{ activeSpaceId: string }>("spaces_settings_get"),
    invoke<SpaceLogin[]>("spaces_logins_list"),
  ]);
  return { activeSpaceId: settings.activeSpaceId, logins };
}

/**
 * What the usage-limit card asks about. Only the Space on screen shows its
 * sessions, so the session is in the active Space; the backend checks the
 * Space it is saved in. Without another Space's login that still has usage,
 * or a reset to wait for, the card stays as it was before Spaces.
 */
export type RouteGrantCard = {
  provider: ProviderAccountProvider;
  /** The session's login, `default` resolved to its Space's. */
  fromAccountId: string;
  fromLabel: string;
  resetsAt: number;
  offers: { account: ProviderAccount; limits: ProviderRateLimits }[];
  /**
   * Ask again after a grant ended (frame 15): what ended and the question,
   * in place of frame 4's header and line; it offers no "until reset".
   */
  ended?: { title: string; line: string };
};

/**
 * `usage` is the account usage cache (`useProviderAccountUsage`). A CLI no
 * Space has a login for (one that keeps no logins) gets no card.
 */
export function routeGrantCard(
  session: Session,
  spaces: SpaceLogins,
  usage: Record<string, ProviderRateLimits>,
  now: number,
): RouteGrantCard | undefined {
  const logins = spaces.logins.filter(
    (login) => login.provider === session.harness,
  );
  return (
    logins[0] &&
    cardFor(session, logins[0].provider, logins, spaces.activeSpaceId, usage, now)
  );
}

function cardFor(
  session: Session,
  provider: ProviderAccountProvider,
  logins: SpaceLogin[],
  activeSpaceId: string,
  usage: Record<string, ProviderRateLimits>,
  now: number,
): RouteGrantCard | undefined {
  const own = sessionLogin(session, provider);
  const fromAccountId =
    own === DEFAULT_PROVIDER_ACCOUNT_ID
      ? (logins.find((login) => login.spaceId === activeSpaceId)?.accountId ??
        own)
      : own;
  const fromUsage = usage[accountUsageKey({ provider, id: fromAccountId })];
  // No reset known reads as one long past.
  const resetsAt =
    session.usageLimit?.resetsAt ??
    (fromUsage && exhaustedWindowResetAt(fromUsage)) ??
    0;
  const offers = offersFor(provider, logins, activeSpaceId, fromAccountId, usage, now);
  if (resetsAt <= now || offers.length === 0) return undefined;
  return {
    provider,
    fromAccountId,
    fromLabel: providerAccountLabel(provider, fromAccountId),
    resetsAt,
    offers,
  };
}

/** The other Spaces' logins with usage left, never the one the session leaves. */
function offersFor(
  provider: ProviderAccountProvider,
  logins: SpaceLogin[],
  activeSpaceId: string,
  fromAccountId: string,
  usage: Record<string, ProviderRateLimits>,
  now: number,
): RouteGrantCard["offers"] {
  return providerAccounts(provider).flatMap((account) => {
    const limits = usage[accountUsageKey(account)];
    return account.id !== fromAccountId &&
      logins.some(
        (login) =>
          login.spaceId !== activeSpaceId && login.accountId === account.id,
      ) &&
      hasRoom(limits, now)
      ? [{ account, limits }]
      : [];
  });
}

/**
 * Ask again after `route_grant_expired` (frame 15, Tyler card #47 B): once
 * the first login has room again, the card offers the login the grant ended
 * on, for a new length, or one click back. While the first login is still
 * out, frame 4's card says so instead.
 */
export function routeGrantAgainCard(
  session: Session,
  spaces: SpaceLogins,
  usage: Record<string, ProviderRateLimits>,
  now: number,
): RouteGrantCard | undefined {
  const grant = lastExpiredRouteGrant(session);
  const logins = spaces.logins.filter(
    (login) => login.provider === session.harness,
  );
  if (!grant || !logins[0]) return undefined;
  const provider = logins[0].provider;
  const fromAccountId = grant.fromAccountId;
  const offers = offersFor(
    provider,
    logins,
    spaces.activeSpaceId,
    fromAccountId,
    usage,
    now,
  ).filter((offer) => offer.account.id === grant.toAccountId);
  const fromUsage = usage[accountUsageKey({ provider, id: fromAccountId })];
  if (!offers[0] || !hasRoom(fromUsage, now)) return undefined;
  const fromLabel = providerAccountLabel(provider, fromAccountId);
  const toLabel = offers[0].account.label;
  return {
    provider,
    fromAccountId,
    fromLabel,
    resetsAt: grant.expiresAt,
    offers,
    ended: {
      title: `Your ${grantSpan(grant)} on ${toLabel} ended at ${clockAt(grant.expiresAt)}`,
      line: `${fromLabel} has reset. Use ${toLabel} again, or go back? Your conversation is saved.`,
    },
  };
}

/**
 * The expired card (frame 6) while the session's latest error is the
 * `route_grant_expired` stop: the grant it names, the login it went back to.
 */
export function routeGrantExpiry(
  session: Session,
  usage: Record<string, ProviderRateLimits>,
  now: number,
): { title: string; fromLabel: string; firstLoginBack?: string } | undefined {
  const grant = lastExpiredRouteGrant(session);
  const provider = session.harness;
  if (!grant || !supportsProviderAccounts(provider)) return undefined;
  const fromLabel = providerAccountLabel(provider, grant.fromAccountId);
  return {
    title: routeGrantEndedTitle(
      grant,
      providerAccountLabel(provider, grant.toAccountId),
    ),
    fromLabel,
    firstLoginBack: firstLoginBack(
      fromLabel,
      usage[accountUsageKey({ provider, id: grant.fromAccountId })],
      now,
    ),
  };
}

/** "15:40 (in 2h 14m)", with the date when the reset is another day. */
export function routeGrantOutUntil(resetsAt: number, now: number): string {
  return `${formatUsageLimitReset(resetsAt, now).replace(" · in ", " (in ")})`;
}

/** The Yes button: "Use Claude account 2 for 3 hours". */
export function routeGrantYes(
  toLabel: string,
  fromLabel: string,
  duration: RouteGrantDuration,
): string {
  return duration === "reset"
    ? `Use ${toLabel} until ${fromLabel} resets`
    : `Use ${toLabel} for ${hours(duration)}`;
}

function hours(count: number): string {
  return count === 1 ? "1 hour" : `${count} hours`;
}

/**
 * The grant the session's latest error ended (`route_grant_expired`), while
 * that error is still its latest.
 */
function lastExpiredRouteGrant(
  session: Session,
): ReturnType<typeof expiredRouteGrant> {
  const error = [...session.blocks]
    .reverse()
    .find((block) => block.notice === "error");
  return error && expiredRouteGrant(error.text);
}

const HOUR_MS = 60 * 60 * 1000;

/**
 * The expired card's title: "The 3 hours on Claude account 2 ended at
 * 14:00". A grant written by hand (the real-app check's minute) names its
 * own length.
 */
function routeGrantEndedTitle(
  grant: { grantedAt: number; expiresAt: number },
  toLabel: string,
): string {
  return `The ${grantSpan(grant)} on ${toLabel} ended at ${clockAt(grant.expiresAt)}`;
}

/** "3 hours", or the exact length of a grant written by hand ("1m"). */
function grantSpan(grant: { grantedAt: number; expiresAt: number }): string {
  const length = grant.expiresAt - grant.grantedAt;
  return length % HOUR_MS === 0
    ? hours(length / HOUR_MS)
    : formatResetDuration(length);
}

function clockAt(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
}

/**
 * "Claude account 1 has reset · 59% of its 5-hour limit left", once the
 * first login's short window has room again.
 */
function firstLoginBack(
  label: string,
  limits: ProviderRateLimits | undefined,
  now: number,
): string | undefined {
  const window = limits?.session;
  if (!window || !hasRoom(limits, now)) return undefined;
  const left = accountHeadroom({ ...limits, weekly: null, monthly: null }, now);
  return `${label} has reset · ${formatUsagePercent(left!)} of its ${window.windowMinutes / 60}-hour limit left`;
}

/** Some of every window is left (a window past its reset is all left). */
function hasRoom(limits: ProviderRateLimits | undefined, now: number): boolean {
  return (accountHeadroom(limits, now) ?? 0) > 0;
}

/**
 * Whether a turn at `now` is refused because the grant has ended. The
 * backend's `grants::check_turn` gives the same answer before every turn
 * (`routeGrant.fixtures.json` pins both).
 */
export function routeGrantEnded(
  grant: Pick<RouteGrant, "expiresAt">,
  now: number,
): boolean {
  return now >= grant.expiresAt;
}
