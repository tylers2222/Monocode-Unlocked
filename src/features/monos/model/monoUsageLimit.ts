import { CONTINUE_PROMPT } from "../../sessions/model/inFlight";
import type { Session } from "../../sessions/model/session";
import {
  DEFAULT_PROVIDER_ACCOUNT_ID,
  sameProviderAccountId,
  supportsProviderAccounts,
  type ProviderAccountProvider,
} from "../../providers/model/providerAccounts";
import { resumeUsageLimitedSession } from "../../sessions/model/usageLimit";
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

/** Account-owned threads need a fresh connection with a transcript handoff. */
export function switchMonoUsageLimitAccount(
  session: Session,
  accountId: string,
): Session {
  if (
    !session.usageLimit ||
    session.busy ||
    sameProviderAccountId(session.providerAccountId, accountId)
  )
    return session;
  return resumeMonoUsageLimit({
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
  });
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

/**
 * Yes on the usage-limit card. "Until the first login resets" waits for the
 * reset the provider reported for this stop, which the backend requires.
 * Asked again after `ended`, the first login stays the one the session left.
 */
export function routeGrantRequest(
  session: Session,
  toAccountId: string,
  length: RouteGrantLength,
  ended?: Pick<RouteGrant, "fromAccountId">,
): RouteGrantRequest | undefined {
  if (!supportsProviderAccounts(session.harness)) return undefined;
  return {
    sessionId: session.id,
    provider: session.harness,
    fromAccountId:
      ended?.fromAccountId ??
      session.providerAccountId ??
      DEFAULT_PROVIDER_ACCOUNT_ID,
    toAccountId,
    length,
    firstLoginResetsAt: session.usageLimit?.resetsAt,
  };
}

/** Once the grant is stored: its login, or the one the session is already on. */
export function continueOnRouteGrant(
  session: Session,
  toAccountId: string,
): Session {
  return sameProviderAccountId(session.providerAccountId, toAccountId)
    ? resumeMonoUsageLimit(session)
    : switchMonoUsageLimitAccount(session, toAccountId);
}

/**
 * Whether a turn at `now` runs under the grant or is refused because it has
 * ended. The backend's `grants::check_turn` gives the same answer before
 * every turn (`routeGrant.fixtures.json` pins both).
 */
export function routeGrantDecision(
  grant: Pick<RouteGrant, "expiresAt">,
  now: number,
): "route" | "expired" {
  return now >= grant.expiresAt ? "expired" : "route";
}

const EXPIRED_PREFIX = "route_grant_expired: the grant from ";

/**
 * The grant a turn was refused on, from the backend's `route_grant_expired`
 * error (`grants.rs` `expired`); the backend has removed it by then. "Go
 * back" is `switchMonoUsageLimitAccount(session, fromAccountId)`.
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
