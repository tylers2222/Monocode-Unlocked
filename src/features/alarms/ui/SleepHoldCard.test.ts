// @vitest-environment happy-dom
import { act, createElement, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SleepHoldView } from "../model/alarmViews";
import { SleepHoldCard } from "./SleepHoldCard";

/** Frame 3: Watch CI for #912, held with 1:42 left. */
const HELD: SleepHoldView = {
  id: "sh_K1",
  callId: "toolu_912",
  agent: { name: "Watch CI for #912", actor: "session:4d10", role: "session" },
  command: "gh pr checks 912 --watch",
  howLong: "until checks finish (no bound)",
  reason: "Wait for CI",
  remainingMs: 102_000,
  windowMs: 120_000,
};

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

async function render(props: ComponentProps<typeof SleepHoldCard>) {
  await act(async () => root.render(createElement(SleepHoldCard, props)));
}

function text(): string {
  return (container.textContent ?? "").replace(/\s+/g, " ");
}

function button(label: string): HTMLButtonElement | undefined {
  return Array.from(container.querySelectorAll("button")).find(
    (node) => node.textContent?.trim() === label,
  );
}

describe("SleepHoldCard, frame 3 held", () => {
  it("reads the frame's lines with the countdown", async () => {
    // Defect: the card drops the command, how long or the reason, or shows the wrong time left.
    await render({ view: HELD });
    expect(text()).toContain("Sleep hold · waiting for your OK1:42");
    expect(text()).toContain(
      "Watch CI for #912 (session:4d10) wants to wait in the foreground.",
    );
    expect(text()).toContain("Commandgh pr checks 912 --watch");
    expect(text()).toContain("How longuntil checks finish (no bound)");
    expect(text()).toContain('Reason"Wait for CI"');
    expect(text()).toContain("No answer in 2 min = refused.");
  });

  it("draws the 2 px amber bar at the share of time left", async () => {
    // Defect: the bar does not shrink with the time left, or overflows it.
    await render({ view: HELD });
    const bar = container.querySelector<HTMLElement>('[role="progressbar"]')!;
    expect(bar.className).toContain("h-0.5");
    const fill = bar.firstElementChild as HTMLElement;
    expect(fill.className).toContain("bg-amber-400");
    expect(fill.style.width).toBe("85%");

    await render({ view: { ...HELD, remainingMs: 0 } });
    expect(text()).toContain("0:00");
    expect(
      (
        container.querySelector('[role="progressbar"]')!
          .firstElementChild as HTMLElement
      ).style.width,
    ).toBe("0%");
  });

  it("says when the agent gave no reason", async () => {
    // Defect: an omp or Codex hold, which has no reason, shows empty quotes.
    await render({ view: { ...HELD, reason: undefined } });
    expect(text()).toContain("Reason(none given)");
  });

  it("answers the hold by its id", async () => {
    // Defect: Allow or Deny answers some other hold, or nothing.
    const onAllow = vi.fn();
    const onDeny = vi.fn();
    await render({ view: HELD, onAllow, onDeny });
    await act(async () => button("Allow")!.click());
    await act(async () => button("Deny")!.click());
    expect(onAllow).toHaveBeenCalledWith("sh_K1");
    expect(onDeny).toHaveBeenCalledWith("sh_K1");
  });
});

describe("SleepHoldCard, frame 3 folded", () => {
  it("folds an allowed hold to one line with no buttons", async () => {
    // Defect: a decided hold keeps its buttons, so it can be answered twice.
    await render({
      view: {
        ...HELD,
        decision: {
          kind: "allow",
          by: "Tyler",
          at: "14:31",
          via: "notification",
        },
      },
    });
    expect(text()).toBe("Allowedby Tyler at 14:31 · from the notification");
    expect(container.querySelector("button")).toBeNull();
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
  });

  it("folds a hold nobody answered to Refused", async () => {
    // Defect: a timed-out hold reads as if someone denied it.
    await render({ view: { ...HELD, decision: { kind: "timeout" } } });
    expect(text()).toBe(
      "Refusedno answer in 2 min · the agent was told to run it in the background",
    );
  });

  it("folds a denied hold with its reason", async () => {
    // Defect: a deny loses who denied it or why.
    await render({
      view: {
        ...HELD,
        decision: {
          kind: "deny",
          by: "Tyler",
          at: "14:31",
          via: "app",
          reason: "Report what you have",
        },
      },
    });
    expect(text()).toBe('Deniedby Tyler at 14:31 · "Report what you have"');
  });
});
