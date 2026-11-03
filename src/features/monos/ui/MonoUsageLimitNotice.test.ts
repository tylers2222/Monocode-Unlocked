// @vitest-environment happy-dom
import { act, createElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MonoUsageLimitNotice } from "./MonoUsageLimitNotice";
import { newSession, type Session } from "../../sessions/model/session";
import {
  resetHarnessModelOverlays,
  setHarnessModels,
} from "../../sessions/model/models";
import { saveProviderAccount } from "../../providers/model/providerAccounts";
import { useProviderAccountUsage } from "../../providers/model/accountUsage";
import type {
  ProviderRateLimits,
  RateLimitWindow,
} from "../../providers/model/rateLimits";
import { saveRouteGrantDefault } from "../../settings/model/settings";
import { routeGrantOutUntil } from "../model/monoUsageLimit";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
let usage: Record<string, ProviderRateLimits> = {};
// A fixed midday (vitest runs on Los Angeles time): a reset 2h 14m on is
// the same day, whenever the suite runs. The card adds the date once the
// reset is another day.
const NOW = new Date(2026, 10, 3, 12, 0).getTime();
vi.mock("../../providers/model/accountUsage", async (importOriginal) => ({
  ...(await importOriginal<
    typeof import("../../providers/model/accountUsage")
  >()),
  useProviderAccountUsage: vi.fn(() => ({
    usage,
    now: NOW,
    refreshing: false,
    refresh: () => {},
  })),
}));

vi.mock("../../../integrations/harness/core/availability", () => ({
  getHarnessAvailabilitySnapshot: () => 0,
  hasProbedHarnessAvailability: () => true,
  isHarnessAvailable: () => true,
  probeHarnessAvailability: () => Promise.resolve(),
  subscribeHarnessAvailability: () => () => {},
}));
vi.mock("../../../integrations/harness/core/registry", () => ({
  refreshHarnessCatalogs: () => Promise.resolve(),
}));
vi.mock("../../../shared/ui/Popover", () => ({
  Popover: ({
    children,
    role,
    "aria-label": label,
    onKeyDown,
  }: {
    children: ReactNode;
    role?: string;
    "aria-label"?: string;
    onKeyDown?: React.KeyboardEventHandler<HTMLDivElement>;
  }) =>
    createElement("div", { role, "aria-label": label, onKeyDown }, children),
}));

let container: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(NOW);
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  setHarnessModels("claude", [
    { id: "claude:current", harness: "claude", name: "Current model" },
  ]);
  setHarnessModels("codex", [
    { id: "codex:alternate", harness: "codex", name: "Alternate model" },
  ]);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(() => {
  usage = {};
  vi.mocked(invoke).mockReset();
  act(() => root.unmount());
  container.remove();
  resetHarnessModelOverlays();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
const noop = () => {};
function render(
  patch: Partial<Session> = {},
  callbacks: Record<string, unknown> = {},
) {
  const session = {
    ...newSession("claude", "/tmp", "claude:current"),
    busy: false,
    usageLimit: { resetsAt: Date.now() + 3600_000 },
    ...patch,
  };
  act(() =>
    root.render(
      createElement(MonoUsageLimitNotice, {
        session,
        onModelChange: noop,
        onResume: noop,
        onResumeAtReset: noop,
        ...callbacks,
      }),
    ),
  );
}

it("opens the actual model picker and routes a different provider choice", () => {
  const onModelChange = vi.fn();
  render({}, { onModelChange });
  act(() =>
    container
      .querySelector<HTMLButtonElement>('[aria-label="Choose another model"]')!
      .click(),
  );
  act(() =>
    container
      .querySelector<HTMLButtonElement>('[role="tab"][aria-label="Codex"]')!
      .click(),
  );
  act(() =>
    container
      .querySelector<HTMLButtonElement>(
        '[role="option"][aria-label="Alternate model, Codex"]',
      )!
      .click(),
  );
  expect(onModelChange).toHaveBeenCalledWith("codex", "codex:alternate");
  expect(container.querySelector('[role="dialog"]')).toBeNull();
});

it("offers other saved accounts and routes a selected profile", () => {
  saveProviderAccount({
    id: "account-work",
    provider: "claude",
    label: "Work account",
  });
  const onAccountChange = vi.fn();
  render({ providerAccountId: "default" }, { onAccountChange });
  const picker = container.querySelector<HTMLButtonElement>(
    '[aria-label="Choose another account"]',
  )!;
  expect(container.querySelector("select")).toBeNull();
  act(() => picker.click());
  expect(
    container.querySelector(
      '[role="menu"][aria-label="Choose another account"]',
    ),
  ).not.toBeNull();
  act(() =>
    container
      .querySelector<HTMLButtonElement>(
        '[role="menuitem"][aria-label="Work account"]',
      )!
      .click(),
  );
  expect(onAccountChange).toHaveBeenCalledWith("account-work");
  expect(container.querySelector('[role="menu"]')).toBeNull();
});

it("hides account switching when no alternate profile is saved", () => {
  render({}, { onAccountChange: vi.fn() });
  expect(
    container.querySelector('[aria-label="Choose another account"]'),
  ).toBeNull();
});

it("supports cancelling and selecting accounts with the keyboard", () => {
  saveProviderAccount({
    id: "account-work",
    provider: "claude",
    label: "Work account",
  });
  saveProviderAccount({
    id: "account-other",
    provider: "claude",
    label: "Other account",
  });
  const onAccountChange = vi.fn();
  render({}, { onAccountChange });
  const trigger = container.querySelector<HTMLButtonElement>(
    '[aria-label="Choose another account"]',
  )!;
  act(() => trigger.click());
  let menu = container.querySelector('[role="menu"]')!;
  act(() =>
    menu.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  expect(container.querySelector('[role="menu"]')).toBeNull();
  expect(onAccountChange).not.toHaveBeenCalled();
  expect(document.activeElement).toBe(trigger);
  act(() => trigger.click());
  menu = container.querySelector('[role="menu"]')!;
  act(() =>
    menu.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
    ),
  );
  act(() =>
    menu.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
    ),
  );
  expect(onAccountChange).toHaveBeenCalledWith("account-other");
  expect(container.querySelector('[role="menu"]')).toBeNull();
});

it("supports arming and cancelling reset recovery, and resuming without a known reset", () => {
  const onResumeAtReset = vi.fn();
  const onResume = vi.fn();
  render({}, { onResumeAtReset });
  const reset = [
    ...container.querySelectorAll<HTMLButtonElement>("button"),
  ].find((button) => button.textContent?.trim() === "Resume at reset")!;
  act(() => reset.click());
  expect(onResumeAtReset).toHaveBeenCalledWith(true);
  render(
    { usageLimit: { resetsAt: Date.now() + 3600_000, resumeAtReset: true } },
    { onResumeAtReset },
  );
  const cancel = [
    ...container.querySelectorAll<HTMLButtonElement>("button"),
  ].find((button) => button.textContent?.trim() === "Resuming at reset")!;
  act(() => cancel.click());
  expect(onResumeAtReset).toHaveBeenLastCalledWith(false);
  render({ usageLimit: {} }, { onResume });
  const resume = [
    ...container.querySelectorAll<HTMLButtonElement>("button"),
  ].find((button) => button.textContent?.trim() === "Resume")!;
  act(() => resume.click());
  expect(onResume).toHaveBeenCalledOnce();
  render({ usageLimit: undefined });
  expect(container.textContent).toBe("");
});

const HOUR = 60 * 60 * 1000;

function windowOf(usedPercent: number, resetsAt: number | null): RateLimitWindow {
  return { usedPercent, windowMinutes: 300, resetsAt };
}

function limits(
  session: RateLimitWindow,
  weekly: RateLimitWindow | null = null,
): ProviderRateLimits {
  return {
    provider: "claude",
    session,
    weekly,
    monthly: null,
    resetCredits: null,
    updatedAt: NOW,
    error: null,
    status: "ok",
  };
}

/**
 * Space 1 (on screen) uses Claude account 1, which is out; Space 2 uses
 * account 2 and Space 3 account 3, both with room. `grant` answers
 * `spaces_grant_save`.
 */
function spacesWorld(grant: () => Promise<unknown> = () => Promise.resolve({})) {
  for (const n of [1, 2, 3])
    saveProviderAccount({
      id: `account-${n}`,
      provider: "claude",
      label: `Claude account ${n}`,
    });
  usage = {
    "claude:account-1": limits(windowOf(100, NOW + 3 * HOUR)),
    "claude:account-2": limits(windowOf(18, NOW + HOUR), {
      usedPercent: 34,
      windowMinutes: 10_080,
      resetsAt: null,
    }),
    "claude:account-3": limits(windowOf(50, null)),
  };
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === "spaces_settings_get") return { activeSpaceId: "space-1" };
    if (command === "spaces_logins_list")
      return [1, 2, 3].map((n) => ({
        spaceId: `space-${n}`,
        provider: "claude",
        accountId: `account-${n}`,
      }));
    if (command === "provider_account_identity")
      return { email: "second.ts@example.com", plan: "Max 5x" };
    if (command === "spaces_grant_save") return grant();
    throw new Error(command);
  }) as typeof invoke);
}

/** The Spaces read, then the identities the card reads once it shows. */
async function settle() {
  await act(async () => {});
  await act(async () => {});
}

const byText = (text: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (button) => button.textContent?.trim() === text,
  );

const radios = (group: string) =>
  [
    ...container.querySelectorAll<HTMLButtonElement>(
      `[role="radiogroup"][aria-label="${group}"] [role="radio"]`,
    ),
  ].map((radio) => [radio.textContent, radio.getAttribute("aria-checked")]);

function grantRequests() {
  return vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === "spaces_grant_save")
    .map(([, args]) => (args as { request: unknown }).request);
}

it("asks for another Space's login and how long, and grants it before moving", async () => {
  let release = () => {};
  spacesWorld(() => new Promise((resolve) => (release = () => resolve({}))));
  saveRouteGrantDefault(12);
  const onAccountChange = vi.fn();
  const onResumeAtReset = vi.fn();
  const resetsAt = NOW + 134 * 60_000;
  const errors = vi.spyOn(console, "error");
  render(
    { providerAccountId: undefined, usageLimit: { resetsAt } },
    { onAccountChange, onResumeAtReset },
  );
  await settle();
  expect(useProviderAccountUsage).toHaveBeenLastCalledWith(expect.any(String), {
    provider: "claude",
    enabled: true,
  });
  const card = container.querySelector('[data-usage-limit][role="status"]')!;
  // The reset's own format (with the date once it is another day) is pinned
  // on a fixed clock in monoUsageLimit.test.ts; this box clock may cross
  // midnight.
  expect(card.textContent).toContain(
    `Claude account 1 is out until ${routeGrantOutUntil(resetsAt, NOW)}`,
  );
  expect(card.textContent).toContain("(in 2h 14m)");
  expect(card.textContent).toContain(
    "Use another login for this session? Your conversation is saved and moves with it.",
  );
  expect(radios("Use another login").map(([, checked]) => checked)).toEqual([
    "true",
    "false",
  ]);
  // The radio is a sibling target: the masked email's Reveal button sits in
  // its row, never inside the radio.
  const login = container.querySelector('[role="radio"][aria-label="Claude account 2"]')!;
  const row = login.parentElement!;
  expect(login.querySelector("button")).toBeNull();
  expect(row.querySelector('button[aria-label="Reveal email"]')).not.toBeNull();
  expect(
    errors.mock.calls.filter(([message]) =>
      String(message).includes("cannot be a descendant"),
    ),
  ).toEqual([]);
  expect(row.textContent).toContain("Max 5x");
  expect(
    [...row.querySelectorAll('[role="progressbar"]')].map((bar) =>
      bar.getAttribute("aria-label"),
    ),
  ).toEqual(["5h limit used", "Weekly limit used"]);
  expect(radios("For")).toEqual([
    ["1 h", "false"],
    ["3 h", "false"],
    ["6 h", "false"],
    ["12 hdefault", "true"],
    ["24 h", "false"],
    ["Until Claude account 1 resets", "false"],
  ]);
  expect(card.textContent).toContain(
    "Only logins that still have usage are listed. Default from Settings › Spaces.",
  );
  expect(byText("Use Claude account 2 for 12 hours")).toBeDefined();
  act(() => byText("3 h")!.click());
  expect(radios("For")[1]).toEqual(["3 h", "true"]);
  const yes = byText("Use Claude account 2 for 3 hours")!;
  expect(container.querySelector('[aria-label="Choose another model"]')).not.toBeNull();
  expect(container.querySelector('[aria-label="Choose another account"]')).toBeNull();

  act(() => yes.click());
  expect(grantRequests()).toEqual([
    {
      sessionId: expect.any(String),
      provider: "claude",
      fromAccountId: "default",
      toAccountId: "account-2",
      length: "3h",
      firstLoginResetsAt: resetsAt,
    },
  ]);
  expect(yes.disabled).toBe(true);
  expect(onAccountChange).not.toHaveBeenCalled();
  await act(async () => release());
  expect(onAccountChange).toHaveBeenCalledWith("account-2");

  const wait = byText("Wait for the reset")!;
  expect(wait.getAttribute("aria-pressed")).toBe("false");
  act(() => wait.click());
  expect(onResumeAtReset).toHaveBeenLastCalledWith(true);
  render(
    { providerAccountId: undefined, usageLimit: { resetsAt, resumeAtReset: true } },
    { onAccountChange, onResumeAtReset },
  );
  const armed = byText("Resuming at reset")!;
  expect(armed.getAttribute("aria-pressed")).toBe("true");
  act(() => armed.click());
  expect(onResumeAtReset).toHaveBeenLastCalledWith(false);
  errors.mockRestore();
});

it("grants the picked login until the first one resets, and a refusal moves nothing", async () => {
  spacesWorld(() => Promise.reject("Space 3 has no Claude login"));
  const onAccountChange = vi.fn();
  const resetsAt = NOW + HOUR;
  render({ providerAccountId: undefined, usageLimit: { resetsAt } }, { onAccountChange });
  await settle();
  expect(radios("For").find(([, checked]) => checked === "true")).toEqual([
    "6 hdefault",
    "true",
  ]);
  act(() =>
    container
      .querySelector<HTMLButtonElement>('[role="radio"][aria-label="Claude account 3"]')!
      .click(),
  );
  expect(radios("Use another login").map(([, checked]) => checked)).toEqual([
    "false",
    "true",
  ]);
  act(() => byText("Until Claude account 1 resets")!.click());
  const yes = byText("Use Claude account 3 until Claude account 1 resets")!;
  act(() => yes.click());
  await settle();
  expect(grantRequests()).toEqual([
    expect.objectContaining({
      toAccountId: "account-3",
      length: "untilReset",
      firstLoginResetsAt: resetsAt,
    }),
  ]);
  expect(onAccountChange).not.toHaveBeenCalled();
  expect(yes.disabled).toBe(false);
  expect(container.textContent).toContain("Space 3 has no Claude login");
  expect(container.textContent).not.toContain("Only logins that still");
});

it("keeps today's card without a way to move, and waits on the meter's reset", async () => {
  spacesWorld();
  render({ providerAccountId: undefined }, {});
  await settle();
  expect(container.textContent).toContain("Claude Code usage limit reached");
  expect(container.querySelector('[aria-label="Choose another account"]')).toBeNull();
  render({ providerAccountId: undefined, usageLimit: {} }, { onAccountChange: vi.fn() });
  await settle();
  expect(container.textContent).toContain("Claude account 1 is out until");
  expect(byText("Wait for the reset")).toBeUndefined();
  render({ harness: "cursor", model: "cursor:auto" }, {});
  expect(useProviderAccountUsage).toHaveBeenLastCalledWith(expect.any(String), {
    provider: undefined,
    enabled: false,
  });
});

it("loads each session's Spaces once", async () => {
  spacesWorld();
  render({ id: "mono-a" });
  render({ id: "mono-a" });
  render({ id: "mono-b" });
  await settle();
  expect(
    vi.mocked(invoke).mock.calls.filter(([command]) => command === "spaces_logins_list"),
  ).toHaveLength(2);
});

it("names the ended grant, goes back to the first login, or asks again", async () => {
  spacesWorld();
  usage["claude:account-1"] = limits(windowOf(41, NOW + HOUR));
  const onResume = vi.fn();
  const expiresAt = NOW - 60_000;
  const session = {
    providerAccountId: "account-1",
    usageLimit: {},
    blocks: [
      {
        id: "stop",
        role: "system" as const,
        notice: "error" as const,
        text: `route_grant_expired: the grant from account-1 to account-2 granted at ${expiresAt - 3 * HOUR} ended at ${expiresAt}`,
      },
    ],
  };
  render(session, { onResume, onAccountChange: vi.fn() });
  await settle();
  const alert = container.querySelector('[data-route-grant-expired][role="alert"]')!;
  expect(alert.textContent).toContain(
    "The 3 hours on Claude account 2 ended at " +
      new Date(expiresAt).toLocaleTimeString(undefined, {
        hour: "numeric",
        minute: "2-digit",
      }),
  );
  expect(alert.textContent).toContain(
    "This session stopped before its next turn. Your conversation is saved, and your message waits below.",
  );
  expect(alert.textContent).toContain(
    "Claude account 1 has reset · 59% of its 5-hour limit left",
  );
  expect(alert.querySelectorAll("p")).toHaveLength(2);
  usage["claude:account-1"] = limits(windowOf(100, NOW + HOUR));
  render(session, { onResume, onAccountChange: vi.fn() });
  expect(
    container.querySelectorAll("[data-route-grant-expired] p"),
  ).toHaveLength(1);
  act(() => byText("Go back to Claude account 1")!.click());
  expect(onResume).toHaveBeenCalledOnce();
  // Account 1 is still out here, so asking again re-opens the card.
  act(() => byText("Ask again")!.click());
  expect(container.querySelector("[data-route-grant-expired]")).toBeNull();
  expect(container.textContent).toContain("Claude account 1 is out until");
});

/** A Mono whose grant from account 1 to account 2 ended an hour ago. */
function endedMono(id: string): Partial<Session> {
  return {
    id,
    providerAccountId: "account-1",
    usageLimit: {},
    blocks: [
      {
        id: "stop",
        role: "system",
        notice: "error",
        text: `route_grant_expired: the grant from account-1 to account-2 granted at ${NOW - 4 * HOUR} ended at ${NOW - HOUR}`,
      },
    ],
  };
}

// The Mono view keeps one pane and swaps its session, so what a user does on
// one Mono's card must not carry to the next.
it("shows the next Mono its own ended grant after Ask again on another", async () => {
  spacesWorld();
  usage["claude:account-1"] = limits(windowOf(100, NOW + 3 * HOUR));
  render(endedMono("mono-a"), { onAccountChange: vi.fn() });
  await settle();
  act(() => byText("Ask again")!.click());
  expect(container.querySelector("[data-route-grant-expired]")).toBeNull();
  render(endedMono("mono-b"), { onAccountChange: vi.fn() });
  expect(container.querySelector("[data-route-grant-expired]")).not.toBeNull();
});

it("starts the next Mono's card on the Settings default, not another Mono's pick", async () => {
  spacesWorld();
  const out = (id: string): Partial<Session> => ({
    id,
    providerAccountId: undefined,
    usageLimit: { resetsAt: NOW + HOUR },
  });
  render(out("mono-a"), { onAccountChange: vi.fn() });
  await settle();
  act(() => byText("3 h")!.click());
  expect(radios("For")[1]).toEqual(["3 h", "true"]);
  render(out("mono-b"), { onAccountChange: vi.fn() });
  expect(radios("For").find(([, checked]) => checked === "true")).toEqual([
    "6 hdefault",
    "true",
  ]);
});

// Frame 15 (Tyler card #47, option B): Ask again once account 1 has room
// reopens frame 4's card for the login the grant ended on.
it("asks again with the ended login, the Settings default and one click back", async () => {
  spacesWorld();
  usage["claude:account-1"] = limits(windowOf(41, NOW + HOUR));
  const onResume = vi.fn();
  const onAccountChange = vi.fn();
  render(endedMono("mono-a"), { onResume, onAccountChange });
  await settle();
  act(() => byText("Ask again")!.click());
  const card = container.querySelector('[data-usage-limit][role="status"]')!;
  expect(card.textContent).toContain(
    "Your 3 hours on Claude account 2 ended at " +
      new Date(NOW - HOUR).toLocaleTimeString(undefined, {
        hour: "numeric",
        minute: "2-digit",
      }),
  );
  expect(card.textContent).toContain(
    "Claude account 1 has reset. Use Claude account 2 again, or go back? Your conversation is saved.",
  );
  expect(
    [
      ...container.querySelectorAll(
        '[role="radiogroup"][aria-label="Use another login"] [role="radio"]',
      ),
    ].map((radio) => radio.getAttribute("aria-label")),
  ).toEqual(["Claude account 2"]);
  expect(radios("For")).toEqual([
    ["1 h", "false"],
    ["3 h", "false"],
    ["6 hdefault", "true"],
    ["12 h", "false"],
    ["24 h", "false"],
  ]);
  expect(card.textContent).not.toContain("Only logins that still");
  expect(byText("Wait for the reset")).toBeUndefined();
  expect(container.querySelector('[aria-label="Choose another model"]')).toBeNull();
  act(() => byText("Go back to Claude account 1")!.click());
  expect(onResume).toHaveBeenCalledOnce();
  act(() => byText("Use Claude account 2 for 6 hours")!.click());
  await settle();
  expect(grantRequests()).toEqual([
    {
      sessionId: "mono-a",
      provider: "claude",
      fromAccountId: "account-1",
      toAccountId: "account-2",
      length: "6h",
      firstLoginResetsAt: NOW - HOUR,
    },
  ]);
  expect(onAccountChange).toHaveBeenCalledWith("account-2");
});

it("asks again on six hours when the default waits for a reset, and says why a grant is refused", async () => {
  spacesWorld(() => Promise.reject("Claude account 2 is not another Space's login"));
  saveRouteGrantDefault("reset");
  usage["claude:account-1"] = limits(windowOf(41, NOW + HOUR));
  render(endedMono("mono-a"), { onAccountChange: vi.fn() });
  await settle();
  act(() => byText("Ask again")!.click());
  expect(radios("For").find(([, checked]) => checked === "true")).toEqual([
    "6 h",
    "true",
  ]);
  act(() => byText("Use Claude account 2 for 6 hours")!.click());
  await settle();
  expect(container.textContent).toContain(
    "Claude account 2 is not another Space's login",
  );
});
