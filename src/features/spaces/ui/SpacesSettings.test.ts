// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { MONO_COLORS } from "../../monos/model/mono";
import { __spacesTestReset } from "../model/activeSpace";
import type { Space } from "../model/spaces";
import { NewSpaceDialog, SpacesList } from "./SpacesSettings";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// The roster as monocode.db saves it: each Mono in its Space.
vi.mock("../../monos/model/mono", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../monos/model/mono")>()),
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

const VIOLET = MONO_COLORS[5];

let spaces: Space[];
let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  __spacesTestReset();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  // Space 1 predates colours; Space 2 is violet.
  spaces = [
    { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
    { id: "space-2", name: "Space 2", color: VIOLET, createdAt: 2 },
  ];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = args as Record<string, string>;
    switch (command) {
      case "spaces_list":
        return spaces;
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
      case "spaces_create": {
        const space = {
          id: "space-3",
          name: input.name!,
          color: input.color!,
          createdAt: 3,
        };
        spaces = [...spaces, space];
        return space;
      }
      case "provider_account_identity":
        return IDENTITIES[`${input.provider}:${input.accountId}`] ?? null;
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

async function renderList(onEdit = vi.fn()) {
  await act(async () =>
    root.render(
      createElement(SpacesList, {
        states: new Map(),
        unseenCounts: new Map([
          ["mono-1", 5],
          ["mono-2", 2],
        ]),
        onEdit,
      }),
    ),
  );
  // The Spaces load, then each login's identity.
  await act(async () => {});
  return onEdit;
}

function typeInto(input: HTMLInputElement, value: string) {
  Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )!.set!.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

describe("Settings > Spaces list (frames 3, 13, 14)", () => {
  it("lists each Space with its colour, both logins, this window and its news", async () => {
    const onEdit = await renderList();

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

    // Frame 13: the tile takes the Space colour; no colour reads as the first.
    const tile = (spaceId: string) =>
      row(spaceId).querySelector<HTMLElement>("[data-space-color]")!;
    expect(tile("space-1").dataset.spaceColor).toBe(MONO_COLORS[0]);
    expect(tile("space-2").dataset.spaceColor).toBe(VIOLET);
    expect(tile("space-2").style.background).toContain("16%");
    expect(tile("space-2").style.boxShadow).toContain("32%");

    // Frame 14 A+A: one button per row, Edit; no remove, no counts.
    const buttons = (spaceId: string) =>
      Array.from(row(spaceId).querySelectorAll("button"), (button) => [
        button.getAttribute("aria-label"),
        button.title,
      ]);
    expect(buttons("space-1")).toEqual([["Edit Space 1", "Edit Space"]]);
    expect(buttons("space-2")).toEqual([["Edit Space 2", "Edit Space"]]);

    await act(async () => row("space-2").querySelector("button")!.click());
    expect(onEdit).toHaveBeenCalledWith(spaces[1]);
  });
});

describe("New Space (frame 11)", () => {
  async function openDialog() {
    const onCreated = vi.fn();
    const onCancel = vi.fn();
    await renderList();
    await act(async () =>
      root.render(createElement(NewSpaceDialog, { onCreated, onCancel })),
    );
    const dialog = document.querySelector<HTMLElement>('[role="dialog"]')!;
    const name = dialog.querySelector<HTMLInputElement>(
      'input[aria-label="Space name"]',
    )!;
    const create = Array.from(dialog.querySelectorAll("button")).find(
      (button) => button.textContent === "Create Space",
    )!;
    return { dialog, name, create, onCreated, onCancel };
  }

  it("creates the Space with its name and colour, then hands it to Edit Space", async () => {
    const { dialog, name, create, onCreated } = await openDialog();
    expect(dialog.textContent).toContain("New Space");
    expect(dialog.textContent).toContain(
      "A Space has its own logins, Gigas, Monos, projects and sessions.",
    );
    expect(dialog.textContent).toContain(
      "Next you pick its Claude and Codex logins: Create opens Edit Space for the new Space.",
    );
    // Two Spaces exist, so the next palette colour is preselected.
    expect(
      dialog
        .querySelector('[aria-label="Yellow"]')!
        .getAttribute("aria-pressed"),
    ).toBe("true");

    expect(create.disabled).toBe(true);
    await act(async () => typeInto(name, "  Client work  "));
    expect(create.disabled).toBe(false);
    await act(async () =>
      dialog.querySelector<HTMLButtonElement>('[aria-label="Green"]')!.click(),
    );
    await act(async () => create.click());

    expect(invoke).toHaveBeenCalledWith("spaces_create", {
      name: "Client work",
      color: MONO_COLORS[3],
    });
    expect(onCreated).toHaveBeenCalledWith({
      id: "space-3",
      name: "Client work",
      color: MONO_COLORS[3],
      createdAt: 3,
    });
    // The list reread the Spaces before Edit Space opens.
    expect(
      vi
        .mocked(invoke)
        .mock.calls.filter(([command]) => command === "spaces_list"),
    ).toHaveLength(2);
  });

  it("shows a refused create and stays open; Cancel closes", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "spaces_create") throw "name is taken";
      return command === "spaces_list" ? spaces : [];
    });
    const { dialog, name, create, onCreated, onCancel } = await openDialog();
    await act(async () => typeInto(name, "Space 2"));
    await act(async () => create.click());

    expect(dialog.querySelector('[role="alert"]')?.textContent).toBe(
      "name is taken",
    );
    expect(onCreated).not.toHaveBeenCalled();
    expect(create.disabled).toBe(false);

    await act(async () =>
      Array.from(dialog.querySelectorAll("button"))
        .find((button) => button.textContent === "Cancel")!
        .click(),
    );
    expect(onCancel).toHaveBeenCalled();
  });
});
