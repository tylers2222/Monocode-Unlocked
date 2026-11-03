// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { NewSpaceButton, SpacesList } from "./SpacesSettings";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// The roster as monocode.db saves it: each Mono in its Space.
vi.mock("../../monos/model/mono", () => ({
  listMonos: () => [
    { id: "mono-1", spaceId: "space-1" },
    { id: "mono-2", spaceId: "space-2" },
  ],
  monosSnapshot: () => "roster",
  subscribeMonos: () => () => {},
}));

const IDENTITIES: Record<string, { email: string; plan: string }> = {
  "claude:default": { email: "tyler.main@example.com", plan: "Max 20x" },
  "codex:account-dev": { email: "tyler.dev@example.com", plan: "ChatGPT Pro" },
  "claude:account-studio": { email: "studio.ts@example.com", plan: "Max 5x" },
};

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    switch (command) {
      case "spaces_list":
        return [
          { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
          { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
        ];
      case "spaces_settings_get":
        return { activeSpaceId: "space-1", routeGrantTtlMs: 21_600_000 };
      case "spaces_logins_list":
        return [
          { spaceId: "space-1", provider: "claude", accountId: "default" },
          { spaceId: "space-1", provider: "codex", accountId: "account-dev" },
          {
            spaceId: "space-2",
            provider: "claude",
            accountId: "account-studio",
          },
        ];
      case "provider_account_identity": {
        const { provider, accountId } = args as Record<string, string>;
        return IDENTITIES[`${provider}:${accountId}`] ?? null;
      }
      default:
        return undefined;
    }
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

function row(spaceId: string): HTMLElement {
  return container.querySelector<HTMLElement>(`[data-space-id="${spaceId}"]`)!;
}

describe("Settings > Spaces list (frame 3)", () => {
  it("lists each Space with both logins, this window, its news and its buttons", async () => {
    await act(async () =>
      root.render(
        createElement(SpacesList, {
          states: new Map(),
          unseenCounts: new Map([
            ["mono-1", 5],
            ["mono-2", 2],
          ]),
        }),
      ),
    );
    // The Spaces load, then each login's identity.
    await act(async () => {});

    expect(
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-space-id]"),
        (node) => node.dataset.spaceId,
      ),
    ).toEqual(["space-1", "space-2"]);

    // Space 1 is this window: its chip, both logins with their plans, and
    // no badge for its own replies.
    expect(row("space-1").textContent).toBe(
      "Space 1This window" +
        "tyler.main@example.com · Max 20x" +
        "tyler.dev@example.com · ChatGPT Pro",
    );
    // Space 2: its replies as a badge, and no Codex login, muted.
    expect(row("space-2").textContent).toBe(
      "Space 22studio.ts@example.com · Max 5xNo Codex login",
    );
    expect(
      row("space-2").querySelector("[data-space-unread]")?.textContent,
    ).toBe("2");
    expect(row("space-1").querySelector("[data-space-unread]")).toBeNull();
    expect(
      row("space-2")
        .querySelector('[data-space-login="codex"]')!
        .hasAttribute("data-missing"),
    ).toBe(true);

    // Frame 3: every Space can be renamed; only another Space can be removed.
    const buttons = (spaceId: string) =>
      Array.from(row(spaceId).querySelectorAll("button"), (button) => [
        button.getAttribute("aria-label"),
        button.title,
      ]);
    expect(buttons("space-1")).toEqual([["Rename Space 1", "Rename Space"]]);
    expect(buttons("space-2")).toEqual([
      ["Rename Space 2", "Rename Space"],
      ["Remove Space 2", "Remove Space"],
    ]);
  });

  it("draws the New Space button", async () => {
    await act(async () => root.render(createElement(NewSpaceButton)));
    expect(container.querySelector("button")?.textContent).toBe("New Space");
  });
});
