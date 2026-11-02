import { describe, expect, it } from "vitest";
import { applyHarnessEvent } from "../../../integrations/harness/core/apply";
import { canDispatchQueuedHead } from "../../sessions/model/messageQueue";
import { CONTINUE_PROMPT } from "../../sessions/model/inFlight";
import {
  pendingComposerSwitch,
  planComposerSwitch,
  shouldAskOutgoingAgent,
} from "../../sessions/model/handoff";
import { newSession, type Session } from "../../sessions/model/session";
import {
  continueOnRouteGrant,
  endedRouteGrant,
  resumeMonoUsageLimit,
  routeGrantRequest,
  switchMonoUsageLimitAccount,
  type RouteGrant,
} from "./monoUsageLimit";

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
    expect(routeGrantRequest(personal, "account-work", "3h")).toEqual({
      sessionId: personal.id,
      provider: "claude",
      fromAccountId: "personal",
      toAccountId: "account-work",
      length: "3h",
      firstLoginResetsAt: 1000,
    });
    expect(
      routeGrantRequest(limited({ harness: "cursor" }), "account-work", "3h"),
    ).toBeUndefined();
    expect(
      routeGrantRequest(
        limited({ providerAccountId: undefined, usageLimit: undefined }),
        "account-work",
        "1h",
      ),
    ).toMatchObject({
      fromAccountId: "default",
      firstLoginResetsAt: undefined,
    });
    const granted = continueOnRouteGrant(original, "account-work");
    expect(granted.providerAccountId).toBe("account-work");
    expect(granted.providerSessionId).toBeUndefined();
    expect(pendingComposerSwitch(granted)?.fromProviderAccountId).toBe(
      "default",
    );

    const grant: RouteGrant = {
      sessionId: original.id,
      provider: "claude",
      fromAccountId: "default",
      toAccountId: "account-work",
      grantedAt: 0,
      expiresAt: 3 * 60 * 60 * 1000,
    };
    expect(endedRouteGrant(grant, grant.expiresAt - 1)).toBeUndefined();
    expect(endedRouteGrant(undefined, grant.expiresAt)).toBeUndefined();
    const ended = endedRouteGrant(grant, grant.expiresAt);
    expect(ended).toBe(grant);
    const expired = applyHarnessEvent(
      { ...granted, pendingSwitch: undefined },
      {
        type: "session.error",
        message: `route_grant_expired: the grant to the claude login account-work ended at ${grant.expiresAt}`,
      },
    );
    expect(expired.usageLimit).toEqual({});
    expect(expired.queueStatus).toBe("paused");
    expect(canDispatchQueuedHead(expired)).toBe(false);
    expect(routeGrantRequest(expired, "account-work", "1h", ended)).toEqual({
      sessionId: original.id,
      provider: "claude",
      fromAccountId: "default",
      toAccountId: "account-work",
      length: "1h",
      firstLoginResetsAt: undefined,
    });
    const again = continueOnRouteGrant(expired, "account-work");
    expect(again.providerAccountId).toBe("account-work");
    expect(again.usageLimit).toBeUndefined();
    expect(canDispatchQueuedHead(again)).toBe(true);
    const back = switchMonoUsageLimitAccount(expired, grant.fromAccountId);
    expect(back.providerAccountId).toBe("default");
    expect(pendingComposerSwitch(back)?.fromProviderAccountId).toBe(
      "account-work",
    );
    expect(canDispatchQueuedHead(back)).toBe(true);
  });
});
