// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  getSpaceSettings,
  listSpaceLogins,
  listSpaces,
  type Space,
  type SpaceLogin,
} from "../model/spaces";
import { NewSpaceButton, SpacesList, spaceRows } from "./SpacesSettings";

vi.mock("../model/spaces", () => ({
  SPACE_LOGIN_PROVIDERS: ["claude", "codex"],
  listSpaces: vi.fn(),
  listSpaceLogins: vi.fn(),
  getSpaceSettings: vi.fn(),
}));

// The both-logins line is ADE-179's; this stand-in shows what each row hands it.
vi.mock("./SpaceLogins", () => ({
  SpaceLogins: ({ logins }: { logins: Record<string, string | null> }) =>
    createElement("span", {
      "data-logins": JSON.stringify(logins),
    }),
}));

const SPACES: Space[] = [
  { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
  { id: "space-2", name: "Space 2", color: null, createdAt: 2 },
];

const LOGINS: SpaceLogin[] = [
  { spaceId: "space-1", provider: "claude", accountId: "default" },
  { spaceId: "space-1", provider: "codex", accountId: "account-dev" },
  { spaceId: "space-2", provider: "claude", accountId: "account-studio" },
];

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(listSpaces).mockResolvedValue(SPACES);
  vi.mocked(listSpaceLogins).mockResolvedValue(LOGINS);
  vi.mocked(getSpaceSettings).mockResolvedValue({
    activeSpaceId: "space-1",
    routeGrantTtlMs: 6 * 60 * 60 * 1000,
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

async function renderList() {
  await act(async () => root.render(createElement(SpacesList)));
}

function rows(): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>("[data-space-id]"));
}

describe("Settings > Spaces list (frame 3)", () => {
  it("gives each Space its own login per provider, none when it has none", () => {
    expect(spaceRows(SPACES, LOGINS, "space-2")).toEqual([
      {
        space: SPACES[0],
        current: false,
        logins: { claude: "default", codex: "account-dev" },
      },
      {
        space: SPACES[1],
        current: true,
        logins: { claude: "account-studio", codex: null },
      },
    ]);
  });

  it("lists every Space with its logins, marks this window, and keeps it", async () => {
    await renderList();

    expect(rows().map((row) => row.dataset.spaceId)).toEqual([
      "space-1",
      "space-2",
    ]);
    const [first, second] = rows();

    expect(first!.textContent).toBe("Space 1This window");
    expect(second!.textContent).toBe("Space 2");

    expect(
      JSON.parse(
        first!.querySelector<HTMLElement>("[data-logins]")!.dataset.logins!,
      ),
    ).toEqual({ claude: "default", codex: "account-dev" });
    expect(
      JSON.parse(
        second!.querySelector<HTMLElement>("[data-logins]")!.dataset.logins!,
      ),
    ).toEqual({ claude: "account-studio", codex: null });

    // Frame 3: every Space can be renamed; only another Space can be removed.
    const buttons = (row: HTMLElement) =>
      Array.from(row.querySelectorAll("button"), (button) => [
        button.getAttribute("aria-label"),
        button.title,
      ]);
    expect(buttons(first!)).toEqual([["Rename Space 1", "Rename Space"]]);
    expect(buttons(second!)).toEqual([
      ["Rename Space 2", "Rename Space"],
      ["Remove Space 2", "Remove Space"],
    ]);
  });

  it("leaves the card empty when the Spaces cannot load", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.mocked(listSpaces).mockRejectedValue("database is locked");

    await renderList();

    expect(rows()).toEqual([]);
    expect(warn).toHaveBeenCalledWith(
      "Could not load Spaces",
      "database is locked",
    );
  });

  it("draws the New Space button", async () => {
    await act(async () => root.render(createElement(NewSpaceButton)));
    expect(container.querySelector("button")?.textContent).toBe("New Space");
  });
});
