// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { StallMarkerView } from "../model/alarmViews";
import { StallChip, StallDetails, StallRailMarker } from "./StallMarker";

/** Frame 1: PR S1 builder, stalled 17 min. */
const STALLED: StallMarkerView = {
  id: "al_7Q2",
  cause: "idle_open_work",
  stalled: { name: "PR S1 builder", actor: "session:9f3c", role: "worker" },
  stalledSince: "14:05",
  minutes: 17,
  openWork: ['ask m_tngc "Build S1 stall detector"'],
  lastActivity: {
    at: "13:48",
    tool: "Bash",
    command: "cargo test",
    state: "completed",
  },
  sends: [{ recipient: "PR-S1", at: "14:20", level: 1 }],
};

const DIALOG: StallMarkerView = {
  ...STALLED,
  cause: "dialog",
  stalled: { name: "PR S1 reviewer", actor: "session:7c21", role: "worker" },
  minutes: 4,
  dialog: {
    kind: "permission",
    minutes: 4,
    text: 'Allow Bash "rm -rf target"?',
    options: ["Allow", "Deny"],
  },
};

function text(markup: string): string {
  const host = document.createElement("div");
  host.innerHTML = markup;
  return (host.textContent ?? "").replace(/\s+/g, " ");
}

describe("stall markers, frame 1", () => {
  it("labels a stall and a dialog wait with their minutes", () => {
    // Defect: a dialog wait reads as a plain stall, or the minutes are missing.
    expect(
      text(renderToStaticMarkup(createElement(StallChip, { marker: STALLED }))),
    ).toBe("Stalled 17m");
    expect(
      text(renderToStaticMarkup(createElement(StallChip, { marker: DIALOG }))),
    ).toBe("Waiting on dialog 4m");
  });

  it("shows only the minutes on the Mono rail, named in full for screen readers", () => {
    // Defect: the rail marker crowds the Mono's name with words.
    const markup = renderToStaticMarkup(
      createElement(StallRailMarker, { marker: { ...STALLED, minutes: 22 } }),
    );
    expect(text(markup)).toBe("22m");
    expect(markup).toContain('aria-label="Stalled 22m"');
  });

  it("says why and who was told in the hover card", () => {
    // Defect: the hover hides the cause, the open work, or who was told.
    expect(
      text(
        renderToStaticMarkup(createElement(StallDetails, { marker: STALLED })),
      ),
    ).toBe(
      "PR S1 builder is stalled since 14:05" +
        "CauseNo turn for 17 min, open work" +
        'Open workask m_tngc "Build S1 stall detector"' +
        "Last activity13:48 Bash cargo test (completed)" +
        "ToldPR-S1 at 14:20 · no act yet",
    );
    expect(
      text(
        renderToStaticMarkup(createElement(StallDetails, { marker: DIALOG })),
      ),
    ).toContain("CausePermission dialog open 4 min");
  });
});

describe("stall marker hover", () => {
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

  it("opens the details on hover and closes them on leave", async () => {
    // Defect: the hover card never opens, or stays after the pointer leaves.
    await act(async () =>
      root.render(createElement(StallChip, { marker: STALLED })),
    );
    const chip = container.querySelector<HTMLElement>('[role="status"]')!;
    expect(document.querySelector('[role="tooltip"]')).toBeNull();

    await act(async () =>
      chip.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })),
    );
    expect(document.querySelector('[role="tooltip"]')?.textContent).toContain(
      "PR S1 builder is stalled since 14:05",
    );

    await act(async () =>
      chip.dispatchEvent(
        new MouseEvent("mouseout", {
          bubbles: true,
          relatedTarget: document.body,
        }),
      ),
    );
    expect(document.querySelector('[role="tooltip"]')).toBeNull();
  });
});
