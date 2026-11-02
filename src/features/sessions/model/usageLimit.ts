import type { RouteGrant } from "../../monos/model/monoUsageLimit";
import {
  providerAccountExists,
  supportsProviderAccounts,
} from "../../providers/model/providerAccounts";
import { formatResetDuration } from "../../providers/model/rateLimits";
import { resumeQueuedMessages } from "./messageQueue";
import type { Session, UsageLimit } from "./session";

/**
 * A fallback for adapters that report exhausted accounts as plain errors. A
 * turn the backend refused because its routing grant ended
 * (`route_grant_expired`, before the turn or at the spawn) waits the same way.
 */
export function usageLimitFromError(message: string): UsageLimit | undefined {
  if (expiredRouteGrant(message)) return {};
  return /(?:usage|spending|monthly|weekly|daily) limit (?:reached|exceeded)|(?:quota|credits?) (?:exceeded|exhausted|depleted)|insufficient[_ ](?:quota|credits)|hit your (?:usage )?limit|credit balance is too low/i.test(
    message,
  )
    ? {}
    : undefined;
}

const EXPIRED_PREFIX = "route_grant_expired: the grant from ";

/**
 * The grant a turn was refused on, from the backend's `route_grant_expired`
 * error (`grants.rs` `expired`); the backend has removed it by then.
 */
export function expiredRouteGrant(
  message: string,
): Omit<RouteGrant, "sessionId" | "provider"> | undefined {
  if (!message.startsWith(EXPIRED_PREFIX)) return undefined;
  const [fromAccountId, , toAccountId, , , grantedAt, , , expiresAt] = message
    .slice(EXPIRED_PREFIX.length)
    .split(" ");
  return {
    fromAccountId,
    toAccountId,
    grantedAt: Number(grantedAt),
    expiresAt: Number(expiresAt),
  };
}

/**
 * The session on login `accountId` (none: the Space's), in a fresh thread
 * that starts from a transcript of the old one.
 */
export function onProviderAccount(
  session: Session,
  accountId: string | undefined,
): Session {
  return {
    ...session,
    providerAccountId: accountId,
    providerSessionId: undefined,
    pendingSwitch: {
      ...(session.pendingSwitch ?? {
        from: session.harness,
        fromModel: session.model,
        fromSettings: session.modelSettings,
        fromProviderSessionId: session.providerSessionId,
        fromProviderAccountId: session.providerAccountId,
      }),
      skipOutgoingRecap: true,
    },
  };
}

/**
 * A turn refused with `route_grant_expired` (`apply.ts`) sends the session
 * back to the grant's first login, or to none when that login was removed,
 * so the Space's login applies, never the granted one. It stays stopped:
 * nothing sends by itself.
 */
export function leaveExpiredRouteGrant(
  session: Session,
  message: string,
): Session {
  const ended = expiredRouteGrant(message);
  if (!ended) return session;
  const first =
    supportsProviderAccounts(session.harness) &&
    providerAccountExists(session.harness, ended.fromAccountId);
  return onProviderAccount(session, first ? ended.fromAccountId : undefined);
}

/** Release the stopped outbox only when the user chooses to recover. */
export function resumeUsageLimitedSession(session: Session): Session {
  if (!session.usageLimit || session.busy) return session;
  return {
    ...session,
    usageLimit: undefined,
    queueStatus: session.queuedMessages?.length ? "active" : undefined,
    queuePause: undefined,
    queuedMessages: resumeQueuedMessages(session.queuedMessages),
  };
}

/** Providers can still refuse right at the reset; give them a moment. */
export const USAGE_LIMIT_RESUME_GRACE_MS = 30_000;

/** "3:16 AM · in 4h 42m" today, "Sep 26, 3:16 AM · in 1d 4h" later. */
export function formatUsageLimitReset(resetsAt: number, now: number): string {
  const reset = new Date(resetsAt);
  const sameDay = reset.toDateString() === new Date(now).toDateString();
  const when = reset.toLocaleString(undefined, {
    ...(sameDay ? {} : { month: "short", day: "numeric" }),
    hour: "numeric",
    minute: "2-digit",
  });
  return `${when} · in ${formatResetDuration(resetsAt - now)}`;
}

/** Idle, armed, and past its reset: time to send the continue turn. */
export function usageLimitResumeDue(session: Session, now: number): boolean {
  const limit = session.usageLimit;
  if (!limit?.resumeAtReset || limit.resetsAt == null || session.busy) {
    return false;
  }
  return now >= limit.resetsAt + USAGE_LIMIT_RESUME_GRACE_MS;
}
