// @vitest-environment happy-dom
import { invoke } from "@tauri-apps/api/core";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  __monosTestReset,
  createMono,
  updateMono,
} from "../../monos/model/mono";
import { saveMaskEmails } from "../../settings/model/displayPrefs";
import { __spacesTestReset, loadSpaces } from "../model/activeSpace";
import { SpaceLogins } from "./SpaceLogins";
import { SpaceSwitcher } from "./SpaceSwitcher";

/**
 * The window's view of `monocode.db`: two Spaces, their logins, and what
 * each login's CLI cached about its account. Emails are made up.
 */
const db = {
  spaces: [] as { id: string; name: string; color: string | null; createdAt: number }[],
  active: "space-1",
  logins: [] as { spaceId: string; provider: string; accountId: string }[],
  identities: new Map<string, { email?: string; plan?: string }>(),
  /** The Space list cannot be read, as with a locked database. */
  failList: false,
  /** Saving the active Space fails. */
  failSet: false,
};

const settings = () => ({ activeSpaceId: db.active, routeGrantTtlMs: 21_600_000 });
type Args = {
  patch: { activeSpaceId: string };
  provider: string;
  accountId: string;
};
const ANSWERS: Record<string, (args: Args) => unknown> = {
  spaces_list: () => {
    if (db.failList) throw new Error("database is locked");
    return structuredClone(db.spaces);
  },
  spaces_settings_get: settings,
  spaces_settings_set: ({ patch }) => {
    if (db.failSet) throw new Error("database is locked");
    db.active = patch.activeSpaceId;
    return settings();
  },
  spaces_logins_list: () => structuredClone(db.logins),
  provider_account_identity: ({ provider, accountId }) =>
    db.identities.get(`${provider}:${accountId}`) ?? null,
};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, raw: Record<string, unknown> = {}) =>
    ANSWERS[command](JSON.parse(JSON.stringify(raw))),
  ),
}));

const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);

let root: Root;
let container: HTMLElement;

beforeEach(() => {
  __spacesTestReset();
  __monosTestReset();
  db.spaces = [
    { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
    { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
  ];
  db.active = "space-1";
  db.logins = [
    { spaceId: "space-1", provider: "codex", accountId: "dev" },
    { spaceId: "space-2", provider: "claude", accountId: "studio" },
  ];
  db.identities = new Map([
    ["claude:default", { email: "main@example.com", plan: "Max 20x" }],
    ["codex:dev", { email: "dev@example.com", plan: "ChatGPT Pro" }],
    ["claude:studio", { email: "studio@example.com", plan: "Max 5x" }],
  ]);
  db.failList = false;
  db.failSet = false;
  vi.mocked(invoke).mockClear();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => stored.set(key, value),
    removeItem: (key: string) => stored.delete(key),
  });
  saveMaskEmails(false);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

/** Lets every pending read answer and React render what it changed. */
async function settle() {
  for (let i = 0; i < 5; i += 1) await act(async () => {});
}

const line = (scope: ParentNode, provider: string) =>
  scope.querySelector<HTMLElement>(`[data-space-login="${provider}"]`)!;
const LINE = "flex min-w-0 items-center gap-[5px] text-[10.5px] leading-[15px]";

describe("SpaceLogins", () => {
  it("shows both logins, Claude then Codex, and a missing one muted", async () => {
    await act(async () =>
      root.render(
        createElement(SpaceLogins, {
          spaceId: "space-2",
          logins: db.logins,
          withPlan: true,
        }),
      ),
    );
    await settle();
    const lines = [...container.querySelectorAll("[data-space-login]")];
    expect(lines.map((entry) => entry.getAttribute("data-space-login"))).toEqual([
      "claude",
      "codex",
    ]);
    const claude = line(container, "claude");
    expect(claude.textContent).toBe("studio@example.com · Max 5x");
    expect(claude.className).toBe(`${LINE} text-content/45`);
    expect(claude.hasAttribute("data-missing")).toBe(false);
    expect(claude.firstElementChild!.getAttribute("class")).toBe(
      "block object-contain size-[11px] shrink-0",
    );
    expect(claude.lastElementChild!.className).toBe("min-w-0 truncate");
    expect(claude.lastElementChild!.firstElementChild!.hasAttribute("class")).toBe(false);

    const codex = line(container, "codex");
    expect(codex.textContent).toBe("No Codex login");
    expect(codex.className).toBe(`${LINE} text-content/28`);
    expect(codex.getAttribute("data-missing")).toBe("");
    expect(codex.firstElementChild!.getAttribute("class")).toBe(
      "block object-contain size-[11px] shrink-0 opacity-35 grayscale",
    );
  });

  it("shows emails without the plan, blurs them when masked, and falls back to the login's name", async () => {
    db.identities.delete("codex:dev");
    db.logins = [
      { spaceId: "space-1", provider: "codex", accountId: "dev" },
      { spaceId: "space-3", provider: "codex", accountId: "dev" },
    ];
    await act(async () =>
      root.render(
        createElement(SpaceLogins, { spaceId: "space-1", logins: db.logins }),
      ),
    );
    await settle();
    expect(line(container, "claude").textContent).toBe("main@example.com");
    expect(line(container, "codex").textContent).toBe("Default account");
    expect(container.textContent).not.toContain("No Claude login");

    await act(async () => saveMaskEmails(true));
    expect(line(container, "claude").lastElementChild!.firstElementChild!.className).toBe(
      "select-none blur-[5px]",
    );

    // Another Space with no Claude login says so.
    await act(async () =>
      root.render(
        createElement(SpaceLogins, { spaceId: "space-3", logins: db.logins }),
      ),
    );
    await settle();
    expect(line(container, "claude").textContent).toBe("No Claude login");
  });
});

const trigger = () =>
  container.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]')!;
const menu = () => document.querySelector<HTMLElement>('[role="menu"][aria-label="Spaces"]');
const row = (id: string) =>
  document.querySelector<HTMLButtonElement>(`[data-space-row="${id}"]`)!;
const item = (label: string) =>
  [...document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')].find(
    (entry) => entry.textContent === label,
  )!;

/** A Mono saved in `spaceId`. */
function monoIn(spaceId: string): string {
  const { id } = createMono();
  updateMono(id, (mono) => ({ ...mono, spaceId }));
  return id;
}

async function renderSwitcher(
  props: Partial<Parameters<typeof SpaceSwitcher>[0]> = {},
) {
  await act(async () =>
    root.render(
      createElement(SpaceSwitcher, {
        states: new Map(),
        onOpenSettings: () => {},
        ...props,
      }),
    ),
  );
  await settle();
}

describe("SpaceSwitcher", () => {
  it("shows the active Space and both its logins, closed (frame 1)", async () => {
    await renderSwitcher();
    expect(trigger().getAttribute("aria-label")).toBe("Space: Space 1");
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
    expect(trigger().type).toBe("button");
    expect(trigger().className).toBe(
      "flex w-full items-center rounded-md border border-content/8 py-1.5 pl-2.5 pr-2 text-left shadow-sm hover:bg-content/10",
    );
    expect(container.firstElementChild!.className).toBe("shrink-0 px-2 pb-2");
    expect(trigger().firstElementChild!.className).toBe(
      "flex min-w-0 flex-1 flex-col gap-0.5",
    );
    const top = trigger().firstElementChild!.firstElementChild!;
    expect(top.className).toBe("flex items-center gap-1.5");
    expect(top.firstElementChild!.className).toBe(
      "min-w-0 flex-1 truncate text-[13px] font-medium leading-tight text-content",
    );
    expect(top.firstElementChild!.textContent).toBe("Space 1");
    expect(top.lastElementChild!.getAttribute("class")).toContain(
      "size-3.5 shrink-0 text-content/50",
    );
    // The closed switcher shows emails only; the plan is in the open menu.
    expect(line(trigger(), "claude").textContent).toBe("main@example.com");
    expect(line(trigger(), "codex").textContent).toBe("dev@example.com");
    expect(container.querySelector("[data-space-news]")).toBeNull();
    expect(menu()).toBeNull();
  });

  it("shows nothing until the Spaces are read, or when the active one is gone", async () => {
    db.failList = true;
    vi.spyOn(console, "error").mockImplementation(() => {});
    await renderSwitcher();
    expect(container.innerHTML).toBe("");

    db.failList = false;
    db.active = "space-9";
    await loadSpaces();
    await settle();
    expect(container.innerHTML).toBe("");
  });

  it("puts a dot on it when another Space has an unread reply or a Mono waiting", async () => {
    db.spaces.push(
      { id: "space-3", name: "Space 3", color: null, createdAt: 3 },
      { id: "space-4", name: "Space 4", color: null, createdAt: 4 },
    );
    const own = monoIn("space-1");
    const other = monoIn("space-2");
    const third = monoIn("space-3");
    const waiting = monoIn("space-2");
    await renderSwitcher({ unseenCounts: new Map([[own, 3]]) });
    expect(container.querySelector("[data-space-news]")).toBeNull();

    await renderSwitcher({
      unseenCounts: new Map([
        [own, 3],
        [other, 2],
        [third, 1],
      ]),
    });
    const dot = container.querySelector<HTMLElement>("[data-space-news]")!;
    expect(dot.title).toBe("Space 2: 2 new replies, Space 3: 1 new reply");
    expect(dot.className).toBe("size-1.5 shrink-0 rounded-full bg-accent");

    await renderSwitcher({
      states: new Map([[waiting, { status: "needs-you" as const }]]),
    });
    expect(container.querySelector<HTMLElement>("[data-space-news]")!.title).toBe("");

    // A new Mono shows in the badge once the database has saved its Space.
    const fresh = createMono().id;
    await renderSwitcher({ unseenCounts: new Map([[fresh, 1]]) });
    expect(container.querySelector("[data-space-news]")).toBeNull();
    await act(async () => updateMono(fresh, (mono) => ({ ...mono, spaceId: "space-2" })));
    expect(container.querySelector("[data-space-news]")).not.toBeNull();
  });

  it("opens one row per Space with both logins and the other Space's news (frame 2)", async () => {
    const working = monoIn("space-2");
    const replied = monoIn("space-2");
    await renderSwitcher({
      states: new Map([[working, { status: "working" as const }]]),
      unseenCounts: new Map([[replied, 2]]),
    });
    await act(async () => trigger().click());
    await settle();

    expect(trigger().getAttribute("aria-expanded")).toBe("true");
    // Frame 2 draws no dot on the open switcher; the rows carry the news.
    expect(container.querySelector("[data-space-news]")).toBeNull();
    expect(trigger().className).toContain("shadow-sm bg-selection");
    expect(menu()!.className).toContain("p-1");
    const header = [...menu()!.querySelectorAll("div")].find(
      (entry) => entry.textContent === "Spaces",
    )!;
    expect(header.className).toBe("px-2 pb-1 pt-1.5 text-[11px] font-semibold text-content/35");

    const one = row("space-1");
    expect(one.getAttribute("role")).toBe("menuitemradio");
    expect(one.type).toBe("button");
    expect(item("New Space").type).toBe("button");
    expect(one.getAttribute("aria-checked")).toBe("true");
    expect(one.className).toBe(
      "flex w-full items-start gap-2.5 rounded-lg px-2.5 py-2 text-left text-[13px] bg-selection",
    );
    expect(one.firstElementChild!.className).toBe("flex min-w-0 flex-1 flex-col gap-[3px]");
    expect(one.firstElementChild!.firstElementChild!.className).toBe(
      "truncate font-medium leading-4 text-content",
    );
    expect(one.firstElementChild!.firstElementChild!.textContent).toBe("Space 1");
    expect(line(one, "claude").textContent).toBe("main@example.com · Max 20x");
    expect(line(one, "codex").textContent).toBe("dev@example.com · ChatGPT Pro");
    expect(one.lastElementChild!.className).toBe("pt-px");
    expect(one.lastElementChild!.firstElementChild!.getAttribute("class")).toContain(
      "size-3.5 text-content",
    );

    const two = row("space-2");
    expect(two.getAttribute("aria-checked")).toBe("false");
    expect(two.className).toBe(
      "flex w-full items-start gap-2.5 rounded-lg px-2.5 py-2 text-left text-[13px] hover:bg-content/[0.04]",
    );
    expect(line(two, "claude").textContent).toBe("studio@example.com · Max 5x");
    expect(line(two, "codex").textContent).toBe("No Codex login");
    const news = two.lastElementChild!.firstElementChild!;
    expect(news.className).toBe("flex items-center gap-1.5");
    expect(news.firstElementChild!.textContent).toBe("1 working");
    expect(news.firstElementChild!.className).toBe("text-[10.5px] text-accent");
    const pill = two.querySelector<HTMLElement>("[data-space-unread]")!;
    expect(pill.textContent).toBe("2");
    expect(pill.className).toBe(
      "grid h-4 min-w-4 place-items-center rounded-full bg-accent px-1 text-[10px] font-medium tabular-nums leading-none text-white",
    );

    expect(menu()!.querySelector(".my-1")!.className).toBe("my-1 h-px bg-content/10");
    expect(item("New Space").className).toBe(
      "flex h-8 w-full items-center gap-2.5 rounded-lg px-2 text-left text-[13px] text-content/85 hover:bg-content/[0.07]",
    );
    expect(item("Manage Spaces").firstElementChild!.getAttribute("class")).toContain(
      "size-3.5 text-content/55",
    );
  });

  it("shows no news on the row of a Space with no Mono", async () => {
    await renderSwitcher();
    await act(async () => trigger().click());
    const news = row("space-2").lastElementChild!.firstElementChild!;
    expect(news.textContent).toBe("");
  });

  it("picks a Space: the view changes, the menu closes and nothing is stopped", async () => {
    const onOpenSettings = vi.fn();
    await renderSwitcher({ onOpenSettings });
    await act(async () => trigger().click());
    vi.mocked(invoke).mockClear();
    await act(async () => row("space-2").click());
    await settle();

    expect(menu()).toBeNull();
    // Nothing but the saved Space goes out: no stop, no kill, no other write.
    expect(
      commands().filter((command) => command !== "provider_account_identity"),
    ).toEqual(["spaces_settings_set"]);
    expect(trigger().getAttribute("aria-label")).toBe("Space: Space 2");
    expect(line(trigger(), "claude").textContent).toBe("studio@example.com");
    expect(line(trigger(), "codex").textContent).toBe("No Codex login");
    expect(onOpenSettings).not.toHaveBeenCalled();
  });

  it("logs a switch the database refused and keeps the Space shown", async () => {
    await renderSwitcher();
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    db.failSet = true;
    await act(async () => trigger().click());
    await act(async () => row("space-2").click());
    await settle();
    expect(error).toHaveBeenCalledWith(
      "monocode: could not switch Space",
      expect.any(Error),
    );
    expect(trigger().getAttribute("aria-label")).toBe("Space: Space 1");
  });

  it.each(["New Space", "Manage Spaces"])("opens Settings from %s and closes", async (label) => {
    const onOpenSettings = vi.fn();
    await renderSwitcher({ onOpenSettings });
    await act(async () => trigger().click());
    await act(async () => item(label).click());
    expect(onOpenSettings).toHaveBeenCalledOnce();
    expect(menu()).toBeNull();

  });

  it("leaves New Space and Manage Spaces out with no Settings to open", async () => {
    await renderSwitcher({ onOpenSettings: undefined });
    await act(async () => trigger().click());
    expect(row("space-2")).not.toBeNull();
    expect(document.querySelectorAll('[role="menuitem"]')).toHaveLength(0);
    expect(menu()!.querySelector(".my-1")).toBeNull();
  });

  it("closes on Escape, and the trigger toggles it", async () => {
    const replied = monoIn("space-2");
    await renderSwitcher({ unseenCounts: new Map([[replied, 1]]) });
    expect(container.querySelector("[data-space-news]")).not.toBeNull();
    await act(async () => trigger().click());
    expect(menu()).not.toBeNull();
    await act(async () =>
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })),
    );
    expect(menu()).toBeNull();
    // Closed again, the dot is back.
    expect(container.querySelector("[data-space-news]")).not.toBeNull();
    await act(async () => trigger().click());
    await act(async () => trigger().click());
    expect(menu()).toBeNull();
  });
});
