// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import { applyHarnessEvent } from "../../../integrations/harness/core/apply";
import {
  canDispatchQueuedHead,
  wakesQueue,
} from "../../sessions/model/messageQueue";
import { CONTINUE_PROMPT } from "../../sessions/model/inFlight";
import {
  pendingComposerSwitch,
  planComposerSwitch,
  shouldAskOutgoingAgent,
} from "../../sessions/model/handoff";
import { newSession, type Session } from "../../sessions/model/session";
import {
  loadSpaceLogins,
  resumeMonoUsageLimit,
  routeGrantAgainCard,
  routeGrantCard,
  routeGrantEnded,
  routeGrantExpiry,
  routeGrantOutUntil,
  routeGrantRequest,
  routeGrantYes,
  saveRouteGrant,
  sessionLogin,
  switchMonoUsageLimitAccount,
  type SpaceLogins,
} from "./monoUsageLimit";
import {
  saveProviderAccount,
  selectProviderAccount,
} from "../../providers/model/providerAccounts";
import type {
  ProviderRateLimits,
  RateLimitWindow,
} from "../../providers/model/rateLimits";
import { expiredRouteGrant } from "../../sessions/model/usageLimit";
import ROUTE_GRANT from "./routeGrant.fixtures.json";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  localStorage.clear();
  vi.mocked(invoke).mockReset();
});

function limited(patch: Partial<Session> = {}): Session {
  return {
    ...newSession("claude", "/tmp/project"),
    providerSessionId: "old-thread",
    providerAccountId: "default",
    blocks: [
      { id: "request", role: "user", text: "Fix the tests" },
      {
        id: "edit",
        role: "tool",
        text: "",
        tool: { kind: "edit", title: "Edit tests.ts" },
      },
    ],
    busy: false,
    usageLimit: { resetsAt: 1000, resumeAtReset: true },
    ...patch,
  };
}

describe("Mono usage recovery", () => {
  it("continues the stopped work once when there are no queued messages", () => {
    const original = limited();
    const resumed = resumeMonoUsageLimit(original);
    expect(resumed.usageLimit).toBeUndefined();
    expect(resumed.queuedMessages).toHaveLength(1);
    expect(resumed.queuedMessages?.[0].text).toBe(CONTINUE_PROMPT);
    expect(resumed.blocks.slice(0, 2)).toEqual(original.blocks);
    expect(canDispatchQueuedHead(resumed)).toBe(true);
    expect(resumeMonoUsageLimit(resumed)).toBe(resumed);
  });

  it("resumes the existing outbox without adding a duplicate continuation", () => {
    const attachments = [
      {
        id: "file",
        name: "tests.ts",
        kind: "file" as const,
        path: "/tmp/tests.ts",
        size: 10,
        mimeType: "text/plain",
      },
    ];
    const original = limited({
      queueStatus: "paused",
      queuedMessages: [
        {
          id: "first",
          blockId: "first",
          text: "Check this file",
          attachments,
          error: "Quota exceeded",
        },
        { id: "second", text: "Then run tests", attachments: [] },
      ],
    });
    const resumed = resumeMonoUsageLimit(original);
    expect(resumed.blocks).toBe(original.blocks);
    expect(resumed.queuedMessages?.map((message) => message.id)).toEqual([
      "first",
      "second",
    ]);
    expect(resumed.queuedMessages?.[0].attachments).toBe(attachments);
    expect(resumed.queuedMessages?.[0].error).toBeUndefined();
    expect(canDispatchQueuedHead(resumed)).toBe(true);
  });

  it("keeps busy or ordinary conversations unchanged", () => {
    const busy = limited({ busy: true });
    const ordinary = limited({ usageLimit: undefined });
    expect(resumeMonoUsageLimit(busy)).toBe(busy);
    expect(resumeMonoUsageLimit(ordinary)).toBe(ordinary);
  });

  it("switches providers with a transcript recap instead of contacting the exhausted provider", () => {
    const original = limited();
    const plan = planComposerSwitch(original, "codex");
    expect(plan.kind).toBe("arm");
    if (plan.kind !== "arm") throw new Error("Expected a handoff");
    const resumed = resumeMonoUsageLimit({
      ...original,
      harness: "codex",
      providerAccountId: undefined,
      providerSessionId: undefined,
      pendingSwitch: plan.pending,
    });
    expect(pendingComposerSwitch(resumed)?.from).toBe("claude");
    expect(shouldAskOutgoingAgent(resumed)).toBe(false);
    expect(canDispatchQueuedHead(resumed)).toBe(true);
  });

  it("uses a fresh account-owned thread while preserving the conversation", () => {
    const original = limited();
    const resumed = switchMonoUsageLimitAccount(original, "account-work");
    expect(resumed.providerAccountId).toBe("account-work");
    expect(resumed.providerSessionId).toBeUndefined();
    expect(resumed.blocks.slice(0, 2)).toEqual(original.blocks);
    expect(pendingComposerSwitch(resumed)).toMatchObject({
      from: "claude",
      fromProviderSessionId: "old-thread",
      fromProviderAccountId: "default",
      skipOutgoingRecap: true,
    });
    expect(shouldAskOutgoingAgent(resumed)).toBe(false);
    expect(canDispatchQueuedHead(resumed)).toBe(true);
    expect(switchMonoUsageLimitAccount(original, "default")).toBe(original);
    const legacy = limited({ providerAccountId: undefined });
    expect(switchMonoUsageLimitAccount(legacy, "default")).toBe(legacy);
  });

  it("resumes the same provider session when its transcript was carried over", () => {
    const original = limited();
    const resumed = switchMonoUsageLimitAccount(original, "account-work", true);
    expect(resumed.providerAccountId).toBe("account-work");
    expect(resumed.providerSessionId).toBe("old-thread");
    expect(pendingComposerSwitch(resumed)).toBeNull();
    expect(resumed.usageLimit).toBeUndefined();
    expect(resumed.queuedMessages?.[0].text).toBe(CONTINUE_PROMPT);
  });

  it("shows recovery again if the replacement account also runs out", () => {
    const resumed = resumeMonoUsageLimit(limited());
    const stopped = applyHarnessEvent(resumed, {
      type: "session.error",
      message: "Quota exceeded",
    });
    expect(stopped.usageLimit).toEqual({});
    expect(stopped.queueStatus).toBe("paused");
    expect(canDispatchQueuedHead(stopped)).toBe(false);

    const original = limited();
    const personal = limited({ providerAccountId: "personal" });
    expect(routeGrantRequest(personal, "account-work", 3, 1000)).toEqual({
      sessionId: personal.id,
      provider: "claude",
      fromAccountId: "personal",
      toAccountId: "account-work",
      length: "3h",
      firstLoginResetsAt: 1000,
    });
    expect(
      routeGrantRequest(limited({ harness: "cursor" }), "account-work", 3, 1),
    ).toBeUndefined();
    expect(
      routeGrantRequest(
        limited({ providerAccountId: undefined, usageLimit: undefined }),
        "account-work",
        "reset",
        undefined,
      ),
    ).toMatchObject({
      fromAccountId: "default",
      length: "untilReset",
      firstLoginResetsAt: undefined,
    });
    const work = ROUTE_GRANT.grant.toAccountId;
    const granted = switchMonoUsageLimitAccount(original, work);
    expect(granted.providerAccountId).toBe(work);
    expect(granted.providerSessionId).toBeUndefined();
    expect(pendingComposerSwitch(granted)?.fromProviderAccountId).toBe(
      "default",
    );

    for (const [now, decision] of ROUTE_GRANT.turns)
      expect(routeGrantEnded(ROUTE_GRANT.grant, Number(now))).toBe(
        decision === "expired",
      );
    const ended = expiredRouteGrant(ROUTE_GRANT.expired);
    expect(ended).toEqual(ROUTE_GRANT.grant);
    expect(expiredRouteGrant("Quota exceeded")).toBeUndefined();
    const expired = applyHarnessEvent(
      { ...granted, pendingSwitch: undefined },
      {
        type: "session.error",
        message: ROUTE_GRANT.expired,
      },
    );
    expect(expired.usageLimit).toEqual({});
    expect(expired.queueStatus).toBe("paused");
    expect(canDispatchQueuedHead(expired)).toBe(false);
    expect(wakesQueue(expired, true)).toBe(false);
    expect(expired.providerAccountId).toBe(ROUTE_GRANT.grant.fromAccountId);
    expect(expired.providerSessionId).toBeUndefined();
    expect(pendingComposerSwitch(expired)?.fromProviderAccountId).toBe(work);
    const next = resumeMonoUsageLimit(expired);
    expect(next.providerAccountId).toBe(ROUTE_GRANT.grant.fromAccountId);
    expect(canDispatchQueuedHead(next)).toBe(true);
    expect(routeGrantRequest(expired, work, 1, undefined)).toEqual({
      sessionId: original.id,
      provider: "claude",
      fromAccountId: ROUTE_GRANT.grant.fromAccountId,
      toAccountId: work,
      length: "1h",
      firstLoginResetsAt: undefined,
    });
    const removed = applyHarnessEvent(
      { ...granted, pendingSwitch: undefined },
      {
        type: "session.error",
        message: ROUTE_GRANT.expired.replace("from default", "from removed"),
      },
    );
    expect(removed.providerAccountId).toBeUndefined();
    expect(removed.usageLimit).toEqual({});
  });
});

const HOUR = 60 * 60 * 1000;
const NOON = new Date(2026, 10, 3, 12, 0).getTime();

function windowOf(
  usedPercent: number,
  resetsAt: number | null,
  windowMinutes = 300,
): RateLimitWindow {
  return { usedPercent, windowMinutes, resetsAt };
}

function limits(
  session: RateLimitWindow | null,
  weekly: RateLimitWindow | null = null,
): ProviderRateLimits {
  return {
    provider: "claude",
    session,
    weekly,
    monthly: null,
    resetCredits: null,
    updatedAt: NOON,
    error: null,
    status: "ok",
  };
}

function claudeLogins() {
  for (const n of [1, 2, 3, 4])
    saveProviderAccount({
      id: `account-${n}`,
      provider: "claude",
      label: `Claude account ${n}`,
    });
}

describe("usage-limit card (ADE-181)", () => {
  // Space 1 is on screen with Claude account 1; Space 2 uses account 2,
  // Space 3 account 3 (used up); account 4 is Space 2's Codex login.
  const spaces: SpaceLogins = {
    activeSpaceId: "space-1",
    logins: [
      { spaceId: "space-1", provider: "codex", accountId: "account-4" },
      { spaceId: "space-1", provider: "claude", accountId: "account-1" },
      { spaceId: "space-2", provider: "claude", accountId: "account-2" },
      { spaceId: "space-3", provider: "claude", accountId: "account-3" },
      { spaceId: "space-2", provider: "codex", accountId: "account-4" },
    ],
  };
  const two = limits(windowOf(18, NOON + HOUR), windowOf(34, null, 10_080));
  const usage = {
    "claude:account-1": limits(windowOf(100, NOON + 3 * HOUR)),
    "claude:account-2": two,
    "claude:account-3": limits(windowOf(100, NOON + HOUR)),
    "claude:account-4": limits(windowOf(5, null)),
  };
  const out = (patch: Partial<Session> = {}) =>
    limited({ providerAccountId: undefined, usageLimit: {}, ...patch });

  it("offers the other Spaces' logins with usage, leaving the resolved login", () => {
    claudeLogins();
    expect(routeGrantCard(out(), spaces, usage, NOON)).toEqual({
      provider: "claude",
      fromAccountId: "account-1",
      fromLabel: "Claude account 1",
      resetsAt: NOON + 3 * HOUR,
      offers: [
        {
          account: {
            id: "account-2",
            provider: "claude",
            label: "Claude account 2",
          },
          limits: two,
        },
      ],
    });
    // The project's own login (Space 3's, with room) is the one the session
    // leaves, so it is not offered; Space 1's login is on screen.
    selectProviderAccount("claude", "/tmp/project", "account-3");
    const fromProject = routeGrantCard(
      out({ usageLimit: { resetsAt: NOON + HOUR } }),
      spaces,
      {
        ...usage,
        "claude:account-1": limits(windowOf(10, null)),
        "claude:account-3": limits(windowOf(10, null)),
      },
      NOON,
    );
    expect(fromProject).toMatchObject({
      fromAccountId: "account-3",
      fromLabel: "Claude account 3",
      resetsAt: NOON + HOUR,
    });
    expect(fromProject?.offers.map((offer) => offer.account.id)).toEqual([
      "account-2",
    ]);
    localStorage.removeItem("monocode.providerAccountSelections.v1");
    // From Space 2 (account 2), Space 1's used-up login is not offered, and
    // the session's own login never is.
    const onTwo = { ...spaces, activeSpaceId: "space-2" };
    expect(
      routeGrantCard(
        out({ usageLimit: { resetsAt: NOON + HOUR } }),
        onTwo,
        usage,
        NOON,
      ),
    ).toBeUndefined();
    expect(
      routeGrantCard(
        out({ usageLimit: { resetsAt: NOON + HOUR } }),
        onTwo,
        { ...usage, "claude:account-1": limits(windowOf(10, null)) },
        NOON,
      )?.offers.map((offer) => offer.account.id),
    ).toEqual(["account-1"]);
    // A Space without a Claude login leaves the CLI's own login.
    expect(
      routeGrantCard(
        out({ usageLimit: { resetsAt: NOON + HOUR } }),
        { ...spaces, activeSpaceId: "space-9" },
        usage,
        NOON,
      ),
    ).toMatchObject({ fromAccountId: "default", fromLabel: "Default account" });
  });

  it("keeps today's card without a reset ahead, an offer, or a login CLI", () => {
    claudeLogins();
    expect(
      routeGrantCard(out({ usageLimit: undefined }), spaces, usage, NOON),
    ).toMatchObject({ resetsAt: NOON + 3 * HOUR });
    const unknown = { ...usage, "claude:account-1": limits(windowOf(40, null)) };
    expect(routeGrantCard(out(), spaces, unknown, NOON)).toBeUndefined();
    const unread: Record<string, ProviderRateLimits> = { ...usage };
    delete unread["claude:account-1"];
    expect(routeGrantCard(out(), spaces, unread, NOON)).toBeUndefined();
    for (const resetsAt of [NOON, NOON - 1])
      expect(
        routeGrantCard(out({ usageLimit: { resetsAt } }), spaces, usage, NOON),
      ).toBeUndefined();
    expect(
      routeGrantCard(out({ usageLimit: { resetsAt: NOON + 1 } }), spaces, usage, NOON),
    ).toBeDefined();
    const noRoom = { ...usage, "claude:account-2": limits(windowOf(100, null)) };
    expect(routeGrantCard(out(), spaces, noRoom, NOON)).toBeUndefined();
    expect(
      routeGrantCard(out({ harness: "cursor" }), spaces, usage, NOON),
    ).toBeUndefined();
  });

  it("names the choice on Yes and the reset in the title", () => {
    expect(routeGrantYes("Claude account 2", "Claude account 1", 3)).toBe(
      "Use Claude account 2 for 3 hours",
    );
    expect(routeGrantYes("Claude account 2", "Claude account 1", 1)).toBe(
      "Use Claude account 2 for 1 hour",
    );
    expect(routeGrantYes("Claude account 2", "Claude account 1", "reset")).toBe(
      "Use Claude account 2 until Claude account 1 resets",
    );
    const at = new Date(NOON + 134 * 60_000).toLocaleString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
    expect(routeGrantOutUntil(NOON + 134 * 60_000, NOON)).toBe(
      `${at} (in 2h 14m)`,
    );
  });

  it("names the ended grant, and the first login once it has room", () => {
    claudeLogins();
    const ended = (grantedAt: number, expiresAt: number, ...later: string[]) =>
      out({
        blocks: [
          {
            id: "stop",
            role: "system",
            notice: "error",
            text: `route_grant_expired: the grant from account-1 to account-2 granted at ${grantedAt} ended at ${expiresAt}`,
          },
          { id: "note", role: "system", text: "Compacted" },
          ...later.map((text, index) => ({
            id: `later-${index}`,
            role: "system" as const,
            notice: "error" as const,
            text,
          })),
        ],
      });
    const at = (ms: number) =>
      new Date(ms).toLocaleTimeString(undefined, {
        hour: "numeric",
        minute: "2-digit",
      });
    const back = (session: RateLimitWindow | null, weekly?: RateLimitWindow) =>
      routeGrantExpiry(
        ended(NOON - 3 * HOUR, NOON),
        { "claude:account-1": limits(session, weekly ?? null) },
        NOON,
      );
    expect(back(windowOf(41, NOON + HOUR))).toEqual({
      title: `The 3 hours on Claude account 2 ended at ${at(NOON)}`,
      fromLabel: "Claude account 1",
      firstLoginBack:
        "Claude account 1 has reset · 59% of its 5-hour limit left",
    });
    for (const resetsAt of [NOON, NOON - 1])
      expect(back(windowOf(100, resetsAt))?.firstLoginBack).toBe(
        "Claude account 1 has reset · 100% of its 5-hour limit left",
      );
    expect(back(windowOf(41, null))?.firstLoginBack).toContain("59%");
    expect(back(windowOf(41, null), windowOf(100, null, 10_080))).toMatchObject({
      firstLoginBack: undefined,
    });
    expect(back(null, windowOf(10, null, 10_080))?.firstLoginBack).toBeUndefined();
    expect(
      routeGrantExpiry(ended(NOON - HOUR, NOON), {}, NOON),
    ).toEqual({
      title: `The 1 hour on Claude account 2 ended at ${at(NOON)}`,
      fromLabel: "Claude account 1",
      firstLoginBack: undefined,
    });
    expect(
      routeGrantExpiry(ended(NOON - 60_000, NOON), {}, NOON)?.title,
    ).toBe(`The 1m on Claude account 2 ended at ${at(NOON)}`);
    expect(
      routeGrantExpiry(ended(0, NOON, "Quota exceeded"), {}, NOON),
    ).toBeUndefined();
    expect(routeGrantExpiry(out(), {}, NOON)).toBeUndefined();
    expect(
      routeGrantExpiry({ ...ended(0, NOON), harness: "cursor" }, {}, NOON),
    ).toBeUndefined();
  });

  it("asks again with the login the grant ended on, once the first login has room (frame 15)", () => {
    claudeLogins();
    const ended = (grantedAt: number) =>
      out({
        providerAccountId: "account-1",
        blocks: [
          {
            id: "stop",
            role: "system",
            notice: "error",
            text: `route_grant_expired: the grant from account-1 to account-2 granted at ${grantedAt} ended at ${NOON}`,
          },
        ],
      });
    const at = new Date(NOON).toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
    // Account 1 has room again; account 3 has room too, but the grant ended
    // on account 2, so only it is offered.
    const back = {
      ...usage,
      "claude:account-1": limits(windowOf(41, NOON + HOUR)),
      "claude:account-3": limits(windowOf(10, null)),
    };
    expect(routeGrantAgainCard(ended(NOON - 3 * HOUR), spaces, back, NOON)).toEqual({
      provider: "claude",
      fromAccountId: "account-1",
      fromLabel: "Claude account 1",
      resetsAt: NOON,
      offers: [
        {
          account: {
            id: "account-2",
            provider: "claude",
            label: "Claude account 2",
          },
          limits: two,
        },
      ],
      ended: {
        title: `Your 3 hours on Claude account 2 ended at ${at}`,
        line: "Claude account 1 has reset. Use Claude account 2 again, or go back? Your conversation is saved.",
      },
    });
    expect(
      routeGrantAgainCard(ended(NOON - 60_000), spaces, back, NOON)?.ended?.title,
    ).toBe(`Your 1m on Claude account 2 ended at ${at}`);
    // Account 1 still out: frame 4's card says so instead.
    expect(
      routeGrantAgainCard(ended(NOON - 3 * HOUR), spaces, usage, NOON),
    ).toBeUndefined();
    // Account 2 has no room left: nothing to ask for.
    const twoFull = { ...back, "claude:account-2": limits(windowOf(100, null)) };
    expect(
      routeGrantAgainCard(ended(NOON - 3 * HOUR), spaces, twoFull, NOON),
    ).toBeUndefined();
    expect(routeGrantAgainCard(out(), spaces, back, NOON)).toBeUndefined();
    expect(
      routeGrantAgainCard(
        { ...ended(NOON - 3 * HOUR), harness: "cursor" },
        spaces,
        back,
        NOON,
      ),
    ).toBeUndefined();
  });

  it("grants from the project's login, and the turn after expiry runs on it", async () => {
    claudeLogins();
    selectProviderAccount("claude", "/tmp/project", "account-2");
    const session = out();
    expect(sessionLogin(session, "claude")).toBe("account-2");
    expect(routeGrantRequest(session, "account-3", 6, undefined)).toEqual({
      sessionId: session.id,
      provider: "claude",
      fromAccountId: "account-2",
      toAccountId: "account-3",
      length: "6h",
      firstLoginResetsAt: undefined,
    });
    const granted = {
      ...switchMonoUsageLimitAccount(
        { ...session, usageLimit: { resetsAt: NOON } },
        "account-3",
      ),
      pendingSwitch: undefined,
    };
    const stopped = applyHarnessEvent(granted, {
      type: "session.error",
      message: `route_grant_expired: the grant from account-2 to account-3 granted at 0 ended at ${6 * HOUR}`,
    });
    expect(sessionLogin(stopped, "claude")).toBe("account-2");

    vi.mocked(invoke).mockImplementation((async (command: string) =>
      command === "spaces_settings_get"
        ? { activeSpaceId: "space-2", routeGrantTtlMs: 1 }
        : command === "spaces_logins_list"
          ? spaces.logins
          : { saved: command }) as typeof invoke);
    await expect(loadSpaceLogins()).resolves.toEqual({
      activeSpaceId: "space-2",
      logins: spaces.logins,
    });
    const request = routeGrantRequest(session, "account-3", 1, undefined)!;
    await expect(saveRouteGrant(request)).resolves.toEqual({
      saved: "spaces_grant_save",
    });
    expect(invoke).toHaveBeenLastCalledWith("spaces_grant_save", { request });
  });
});
