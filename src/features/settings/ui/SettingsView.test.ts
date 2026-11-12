// @vitest-environment happy-dom
import { act, createElement, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import { SettingsView } from "./SettingsView";
import { checkProjectDelete } from "../../projects/model/projectDeleteCheck";
import { setHomeDir } from "../../../shared/lib/paths";
import { rememberNotificationProjects } from "../../notifications/model/notificationProjects";
import {
  loadRouteGrantDefault,
  SETTINGS_INDEX,
  CLASSIC_LAYOUT_HIDDEN_SETTINGS,
  INSET_LAYOUT_HIDDEN_SETTINGS,
  SETTINGS_SECTIONS,
  isSettingsSectionId,
  searchSettings,
  settingsSectionsByGroup,
  type SettingsSectionId,
} from "../model/settings";
import {
  createAlarmsFeed,
  type AlarmsFeed,
} from "../../alarms/model/alarmsFeed";
import { AlarmsProvider } from "../../alarms/ui/AlarmsProvider";
import { snapshotWire } from "../../alarms/test/alarmsFixtures";
import { loadInsetLayout } from "../model/appearance";
import {
  providerAccounts,
  saveProviderAccount,
} from "../../providers/model/providerAccounts";
import {
  clearCachedRateLimits,
  setCachedRateLimits,
} from "../../providers/model/rateLimitsCache";
import {
  HARNESSES,
  HARNESS_TITLE,
} from "../../sessions/model/session";
import { saveMaskEmails, saveShowRemainingUsage } from "../model/displayPrefs";
import {
  createMono,
  findMono,
  MONO_COLORS,
  monoLook,
  updateMono,
} from "../../monos/model/mono";
import { __spacesTestReset } from "../../spaces/model/activeSpace";
import { loginHarness } from "../../../integrations/harness/core/auth";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => undefined),
  convertFileSrc: (path: string) => path,
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isMaximized: async () => false,
    onResized: async () => () => {},
  }),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(async () => true) }));
vi.mock("../../../integrations/harness/core/auth", async (importOriginal) => ({
  ...(await importOriginal<
    typeof import("../../../integrations/harness/core/auth")
  >()),
  loginHarness: vi.fn(async () => {}),
}));
vi.mock("../../../integrations/harness/core/availability", () => ({
  isHarnessAvailable: (id: string) => id === "claude" || id === "cursor",
  hasProbedHarnessAvailability: () => true,
  getHarnessAvailabilitySnapshot: () => 0,
  subscribeHarnessAvailability: () => () => {},
  probeHarnessAvailability: async () => {},
  harnessUnavailableHint: () => "",
}));

vi.mock("../../projects/model/projectDeleteCheck", async (original) => ({
  ...(await original<object>()),
  checkProjectDelete: vi.fn(async () => null),
  readProjectDeletePrompt: vi.fn(async () => ({
    sessions: 0,
    scoped: true,
    spaceName: "Space 1",
  })),
}));

let container: HTMLDivElement;
let root: Root;
let onSelectSection: ReturnType<typeof vi.fn>;

function mockLocalStorage() {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => data.set(key, value),
    removeItem: (key: string) => data.delete(key),
    clear: () => data.clear(),
    key: (index: number) => [...data.keys()][index] ?? null,
    get length() {
      return data.size;
    },
  });
}

async function render(
  section: SettingsSectionId,
  options: Partial<ComponentProps<typeof SettingsView>> = {},
) {
  await act(async () =>
    root.render(
      createElement(SettingsView, {
        section,
        cwd: "/repo",
        sessions: [],
        onClose: vi.fn(),
        onSelectSection,
        onOpenSession: vi.fn(),
        onArchiveSession: vi.fn(),
        onDeleteSession: vi.fn(),
        onOpenWhatsNew: vi.fn(),
        ...options,
      }),
    ),
  );
}

function renderedSettingIds(): string[] {
  return Array.from(
    container.querySelectorAll<HTMLElement>("[data-setting-id]"),
    (node) => node.dataset.settingId!,
  );
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mockLocalStorage();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  onSelectSection = vi.fn();
  vi.mocked(invoke).mockReset().mockImplementation(async command => command === "mobile_status" ? {
    enabled: false, listening: false, name: "Desktop", fingerprint: null,
    openRouter: true, remoteAddress: "", routerStatus: "Off", addresses: [], devices: [],
  } : undefined);
  __spacesTestReset();
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  clearCachedRateLimits();
  localStorage.clear();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("settings pages", () => {
  it("saves each Mono's session visibility and restores it when settings reopen", async () => {
    const mono = createMono();
    const other = createMono();
    const toggle = (id: string) =>
      container.querySelector<HTMLButtonElement>(
        `[role="switch"][aria-label="Show sessions started by ${monoLook(findMono(id)!).name} in sidebar"]`,
      )!;
    await render("monos");
    // Off until turned on: a Mono's sessions stay in its overview.
    expect(toggle(mono.id).getAttribute("aria-checked")).toBe("false");
    expect(toggle(other.id).getAttribute("aria-checked")).toBe("false");
    await act(async () => toggle(mono.id).click());
    expect(toggle(mono.id).getAttribute("aria-checked")).toBe("true");
    expect(findMono(mono.id)?.showStartedSessionsInSidebar).toBe(true);
    expect(toggle(other.id).getAttribute("aria-checked")).toBe("false");
    await render("general");
    await render("monos");
    expect(toggle(mono.id).getAttribute("aria-checked")).toBe("true");
    await act(async () => updateMono(mono.id, (entry) => ({
      ...entry,
      showStartedSessionsInSidebar: false,
    })));
    expect(toggle(mono.id).getAttribute("aria-checked")).toBe("false");
  });

  it("blurs account emails by default and hides them when settings reopen", async () => {
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "provider_account_identity") {
        const { provider } = args as { provider: string };
        return { email: `${provider}@example.com`, plan: "Pro" };
      }
      return undefined;
    });
    await render("providers");

    const emails = container.querySelectorAll<HTMLButtonElement>(
      '[aria-label="Reveal email"]',
    );
    expect(emails).toHaveLength(2);
    expect(
      [...emails].every((email) =>
        email.querySelector("span")?.className.includes("blur-[5px]"),
      ),
    ).toBe(true);
    expect(container.textContent).toContain("Pro");
    await act(async () => emails[0].click());
    expect(emails[0].getAttribute("aria-label")).toBe("Hide email");
    expect(emails[0].querySelector("span")?.className).not.toContain("blur");
    expect(emails[1].getAttribute("aria-label")).toBe("Reveal email");
    await act(async () => emails[0].click());
    expect(emails[0].getAttribute("aria-label")).toBe("Reveal email");

    await act(async () => emails[0].click());
    await render("general");
    await render("providers");
    expect(container.querySelector('[aria-label="Hide email"]')).toBeNull();
    expect(
      container.querySelectorAll('[aria-label="Reveal email"]'),
    ).toHaveLength(2);
  });

  it("shows used usage and plain emails until the options are turned on", async () => {
    saveMaskEmails(false);
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "provider_account_identity"
        ? { email: "user@example.com", plan: "Pro" }
        : undefined,
    );
    setCachedRateLimits("claude", "default", {
      provider: "claude",
      session: {
        usedPercent: 23,
        windowMinutes: 300,
        resetsAt: Date.now() + 3_600_000,
      },
      weekly: null,
      monthly: null,
      resetCredits: null,
      updatedAt: Date.now(),
      error: null,
      status: "ok",
    });

    await render("providers");

    const used = container.querySelector('[aria-label="5h limit used"]');
    expect(used?.getAttribute("aria-valuenow")).toBe("23");
    expect(used?.querySelector("span")?.getAttribute("style")).toBe(
      "width: 23%;",
    );
    expect(container.textContent).toContain("user@example.com");
    expect(container.querySelector('[aria-label="Reveal email"]')).toBeNull();

    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[aria-label="Show remaining usage"]',
        )!
        .click(),
    );
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[aria-label="Mask account emails"]')!
        .click(),
    );

    const remaining = container.querySelector(
      '[aria-label="5h limit remaining"]',
    );
    expect(remaining?.getAttribute("aria-valuenow")).toBe("77");
    expect(
      container.querySelectorAll('[aria-label="Reveal email"]').length,
    ).toBeGreaterThan(0);
  });

  it("shows account usage bars as remaining capacity", async () => {
    saveShowRemainingUsage(true);
    setCachedRateLimits("claude", "default", {
      provider: "claude",
      session: {
        usedPercent: 23,
        windowMinutes: 300,
        resetsAt: Date.now() + 3_600_000,
      },
      weekly: null,
      monthly: null,
      resetCredits: null,
      updatedAt: Date.now(),
      error: null,
      status: "ok",
    });

    await render("providers");

    const bar = container.querySelector('[aria-label="5h limit remaining"]');
    expect(bar?.getAttribute("aria-valuenow")).toBe("77");
    expect(bar?.querySelector("span")?.getAttribute("style")).toBe(
      "width: 77%;",
    );
    expect(bar?.parentElement?.textContent).toContain("77% left");
  });

  it("shows background effect choices above scope when artwork is available", async () => {
    localStorage.setItem(
      "monocode.chatBackgroundPath",
      "/app-data/backgrounds/chat-background.png",
    );
    await render("appearance");

    const effect = container.querySelector(
      "#new-thread-background-effect-dither",
    )!;
    const effectRow = effect.closest(".settings-row")!;
    const scopeRow = container
      .querySelector('[aria-label="Show background on"]')!
      .closest(".settings-row")!;
    expect(effect).not.toBeNull();
    expect(
      effectRow.compareDocumentPosition(scopeRow) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).not.toBe(0);

    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: false })),
    );
    await act(async () => (effect as HTMLButtonElement).click());
    expect(localStorage.getItem("monocode.newThreadBackgroundEffect")).toBe(
      "dither",
    );
    expect(effect.getAttribute("aria-checked")).toBe("true");
    expect(container.textContent).toContain(
      "Rebuilds the artwork with a dithered color palette.",
    );
  });

  it("previews and restores Haze with the existing empty-chat visibility", async () => {
    localStorage.setItem("monocode.chatBackgroundPath", "/background.png");
    localStorage.setItem("monocode.chatBackgroundEmptyOpacity", "0.4");
    await render("appearance");

    const option = container.querySelector<HTMLButtonElement>(
      "#new-thread-background-effect-gradient-blur",
    )!;
    expect(option.textContent).toBe("Haze");
    await act(async () => option.click());

    const preview = container.querySelector<HTMLElement>(
      ".gradient-blur-background",
    )!;
    expect(option.getAttribute("aria-checked")).toBe("true");
    expect(preview.style.opacity).toBe("0.4");
    expect(preview.querySelectorAll("span")).toHaveLength(2);
    expect(localStorage.getItem("monocode.newThreadBackgroundEffect")).toBe(
      "gradient-blur",
    );

    await render("providers");
    await render("appearance");
    expect(
      container
        .querySelector("#new-thread-background-effect-gradient-blur")
        ?.getAttribute("aria-checked"),
    ).toBe("true");
    expect(container.querySelector(".gradient-blur-background")).not.toBeNull();
  });

  it("manages named accounts independently for each supported provider", async () => {
    saveProviderAccount({
      id: "account-work",
      provider: "codex",
      label: "Wrk",
    });
    await render("providers");

    expect(container.textContent).toContain("Claude Code");
    expect(container.textContent).toContain("Codex");
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[aria-label="Rename Default account"]',
        )!
        .click(),
    );
    const defaultInput = container.querySelector<HTMLInputElement>(
      '[aria-label="Rename Claude Code account"]',
    )!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(defaultInput, "Primary");
      defaultInput.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('button[type="submit"]')!
        .click(),
    );
    expect(providerAccounts("claude")[0]?.label).toBe("Primary");

    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[aria-label="Rename Wrk"]')!
        .click(),
    );
    const input = container.querySelector<HTMLInputElement>(
      '[aria-label="Rename Codex account"]',
    )!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(input, "Work");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('button[type="submit"]')!
        .click(),
    );
    expect(providerAccounts("codex")[1]?.label).toBe("Work");

    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[aria-label="Remove Work"]')!
        .click(),
    );
    expect(ask).toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith("provider_account_remove", {
      provider: "codex",
      accountId: "account-work",
    });
    expect(providerAccounts("codex")).toHaveLength(1);
  });

  it("validates and stores Codex and OpenCode binary overrides", async () => {
    let failAutoCodex = false;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      const payload = args as { binaryPath?: string } | undefined;
      if (command === "harness_resolve_configured") {
        return { path: payload?.binaryPath };
      }
      if (command === "harness_resolve_codex") {
        if (failAutoCodex && payload?.binaryPath == null) {
          throw new Error("Codex auto-detection failed");
        }
        return { path: payload?.binaryPath ?? "/auto/codex" };
      }
      if (command === "harness_resolve_opencode") {
        return { path: payload?.binaryPath ?? "/auto/opencode" };
      }
      if (command === "harness_exec") {
        if (payload?.binaryPath === "/bad/codex") {
          throw new Error("Codex failed to start");
        }
        return {
          stdout: payload?.binaryPath?.includes("opencode")
            ? "opencode 1.18.32"
            : "codex-cli 0.156.1",
          cwd: "/helper-cwd",
        };
      }
      return undefined;
    });
    await render("providers");

    const details = container.querySelector<HTMLButtonElement>(
      '[aria-label="Show Codex CLI details"]',
    )!;
    await act(async () => details.click());
    await act(async () =>
      document
        .querySelector<HTMLButtonElement>(
          '[aria-label="Open Codex CLI location"]',
        )!
        .click(),
    );
    expect(invoke).toHaveBeenCalledWith("reveal_path", {
      path: "/auto/codex",
    });

    const save = async (provider: "Codex" | "OpenCode", path: string) => {
      const id = `${provider.toLowerCase()}-binary-path`;
      if (!document.querySelector(`#${id}`)) {
        if (!document.querySelector(`[aria-label="Edit ${provider} CLI path"]`)) {
          await act(async () =>
            container
              .querySelector<HTMLButtonElement>(
                `[aria-label^="Show ${provider} CLI details"]`,
              )!
              .click(),
          );
        }
        await act(async () =>
          document
            .querySelector<HTMLButtonElement>(
              `[aria-label="Edit ${provider} CLI path"]`,
            )!
            .click(),
        );
      }
      const input = document.querySelector<HTMLInputElement>(`#${id}`)!;
      await act(async () => {
        Object.getOwnPropertyDescriptor(
          HTMLInputElement.prototype,
          "value",
        )!.set!.call(input, path);
        input.dispatchEvent(new Event("input", { bubbles: true }));
      });
      await act(async () => input.closest("form")!.requestSubmit());
    };

    await save("Codex", "/opt/codex/bin/codex");
    await save("OpenCode", "/opt/opencode/bin/opencode");
    expect(
      JSON.parse(
        localStorage.getItem("monocode.providerBinaryPaths.v1") ?? "{}",
      ),
    ).toEqual({
      codex: "/opt/codex/bin/codex",
      opencode: "/opt/opencode/bin/opencode",
    });
    await act(async () => details.click());
    expect(document.body.textContent).toContain("/opt/codex/bin/codex");
    expect(document.body.textContent).toContain("codex-cli 0.156.1");
    expect(document.body.textContent).toContain("Restart required");
    await act(async () => details.click());
    const openCodeDetails = container.querySelector<HTMLButtonElement>(
      '[aria-label^="Show OpenCode CLI details"]',
    )!;
    await act(async () => openCodeDetails.click());
    expect(document.body.textContent).toContain("/opt/opencode/bin/opencode");
    expect(document.body.textContent).toContain("opencode 1.18.32");
    await act(async () => openCodeDetails.click());

    await save("Codex", "/bad/codex");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain(
      "Codex failed to start",
    );
    expect(container.textContent).not.toContain("/opt/codex/bin/codex");
    expect(
      JSON.parse(
        localStorage.getItem("monocode.providerBinaryPaths.v1") ?? "{}",
      ).codex,
    ).toBe("/opt/codex/bin/codex");
    await act(async () =>
      Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
        (button) => button.textContent === "Cancel",
      )!.click(),
    );
    expect(document.querySelector('[aria-label="Retry Codex configured path"]')).not.toBeNull();

    failAutoCodex = true;
    await save("Codex", "");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain(
      "Codex auto-detection failed",
    );
    expect(
      JSON.parse(
        localStorage.getItem("monocode.providerBinaryPaths.v1") ?? "{}",
      ).codex,
    ).toBe("/opt/codex/bin/codex");

    failAutoCodex = false;
    await save("Codex", "");
    expect(
      JSON.parse(
        localStorage.getItem("monocode.providerBinaryPaths.v1") ?? "{}",
      ).codex,
    ).toBeUndefined();
    await act(async () => details.click());
    expect(document.body.textContent).toContain("/auto/codex");
    expect(document.body.textContent).toContain("Auto-detected");
  });

  it("offers manual auto-detect retry when a CLI is missing", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "harness_resolve_codex") {
        throw new Error("Codex CLI not found");
      }
      return undefined;
    });
    await render("providers");
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[aria-label="Show Codex CLI details"]',
        )!
        .click(),
    );
    const retry = document.querySelector<HTMLButtonElement>(
      '[aria-label="Retry Codex auto-detect"]',
    )!;
    expect(retry).not.toBeNull();
    await act(async () => retry.click());
    expect(invoke).toHaveBeenCalledWith("harness_resolve_codex", undefined);
  });

  it("reopens, scrolls to, focuses and highlights the same project on a repeated notification settings request", async () => {
    vi.useFakeTimers();
    const scroll = vi.spyOn(HTMLElement.prototype, "scrollIntoView");
    rememberNotificationProjects([
      {
        id: "repository:github.com/work/app",
        name: "work/app",
        detail: "github.com",
        kind: "repository",
        paths: ["/repo"],
      },
    ]);
    const shortcut = {
      anchor: "project-notifications" as const,
      notificationProjectPath: "/repo",
      notificationSettingsRequest: 1,
    };
    await render("inbox", shortcut);
    const project = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Notification categories for work/app"]',
    )!;
    const card = project.closest("fieldset")!;
    const section = container.querySelector(
      '[data-setting-id="project-notifications"]',
    )!;
    const highlight = () => section.querySelector(".border-accent\\/60");
    expect(highlight()).not.toBeNull();
    expect(project.getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(card);

    await act(async () => vi.advanceTimersByTimeAsync(1800));
    expect(highlight()).toBeNull();
    await act(async () => project.click());
    expect(project.getAttribute("aria-expanded")).toBe("false");
    const search = container.querySelector<HTMLInputElement>(
      '[aria-label="Search settings"]',
    )!;
    search.focus();
    expect(document.activeElement).toBe(search);
    scroll.mockClear();

    await render("inbox", { ...shortcut, notificationSettingsRequest: 2 });

    expect.soft(project.getAttribute("aria-expanded")).toBe("true");
    expect.soft(scroll).toHaveBeenCalledWith({ block: "nearest" });
    expect.soft(scroll.mock.contexts).toContain(card);
    expect.soft(document.activeElement === card).toBe(true);
    expect.soft(highlight()).not.toBeNull();

    await act(async () => vi.advanceTimersByTimeAsync(1800));
    expect(highlight()).toBeNull();
    expect(project.getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(card);
  });

  it("clears the project shortcut highlight after 1.8 seconds while keeping its project open and focused", async () => {
    vi.useFakeTimers();
    rememberNotificationProjects([
      {
        id: "repository:github.com/work/app",
        name: "work/app",
        detail: "github.com",
        kind: "repository",
        paths: ["/repo"],
      },
    ]);
    await render("inbox", {
      anchor: "project-notifications",
      notificationProjectPath: "/repo",
    });
    const section = container.querySelector(
      '[data-setting-id="project-notifications"]',
    )!;
    const project = section.querySelector(
      'button[aria-label="Notification categories for work/app"]',
    )!;
    expect(section.querySelector(".border-accent\\/60")).not.toBeNull();
    expect(project.getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(project.closest("fieldset"));

    await act(async () => vi.advanceTimersByTimeAsync(1800));

    expect(section.querySelector(".border-accent\\/60")).toBeNull();
    expect(project.getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(project.closest("fieldset"));
  });

  it("gives every section a rail group", () => {
    const groups = new Set(SETTINGS_SECTIONS.map((section) => section.group));
    expect([...groups]).toEqual(["app", "agents", "workspace"]);
  });

  it("indexes each setting once", () => {
    const ids = SETTINGS_INDEX.map((entry) => entry.id);
    expect(ids).toEqual([...new Set(ids)]);
  });

  it("lets files open as normal top-bar tabs", async () => {
    await render("general");
    const control = container.querySelector<HTMLElement>(
      '[role="radiogroup"][aria-label="File tabs"]',
    )!;
    const [besideChat, topBar] = Array.from(
      control.querySelectorAll<HTMLButtonElement>('[role="radio"]'),
    );

    expect(besideChat?.getAttribute("aria-checked")).toBe("true");
    expect(topBar?.getAttribute("aria-checked")).toBe("false");

    await act(async () => topBar?.click());

    expect(topBar?.getAttribute("aria-checked")).toBe("true");
    expect(localStorage.getItem("monocode.fileTabMode")).toBe("workspace");
  });

  it("offers tab animations as an opt-in", async () => {
    await render("general");
    const control = container.querySelector<HTMLButtonElement>(
      '[role="switch"][aria-label="Tab animations"]',
    )!;

    expect(control.getAttribute("aria-checked")).toBe("false");
    await act(async () => control.click());
    expect(control.getAttribute("aria-checked")).toBe("true");
    expect(localStorage.getItem("monocode.tabAnimationsEnabled")).toBe("1");
  });

  it("defaults to the icon rail and lets users hide it", async () => {
    await render("appearance");
    let control = container.querySelector<HTMLElement>(
      '[role="radiogroup"][aria-label="Collapsed project rail"]',
    )!;
    let [iconRail, hidden] = Array.from(
      control.querySelectorAll<HTMLButtonElement>('[role="radio"]'),
    );

    expect(iconRail?.getAttribute("aria-checked")).toBe("true");
    expect(hidden?.getAttribute("aria-checked")).toBe("false");

    await act(async () => hidden?.click());

    expect(hidden?.getAttribute("aria-checked")).toBe("true");
    expect(localStorage.getItem("monocode.collapsedProjectRailMode")).toBe(
      "hidden",
    );

    await act(async () => root.unmount());
    root = createRoot(container);
    await render("appearance");

    control = container.querySelector<HTMLElement>(
      '[role="radiogroup"][aria-label="Collapsed project rail"]',
    )!;
    [iconRail, hidden] = Array.from(
      control.querySelectorAll<HTMLButtonElement>('[role="radio"]'),
    );
    expect(iconRail?.getAttribute("aria-checked")).toBe("false");
    expect(hidden?.getAttribute("aria-checked")).toBe("true");
  });

  it("sets interface scale from a menu instead of a live slider", async () => {
    await render("appearance");
    const row = container.querySelector('[data-setting-id="interface-scale"]')!;
    expect(row.querySelector('input[type="range"]')).toBeNull();
    const trigger = row.querySelector<HTMLButtonElement>(
      '[aria-haspopup="listbox"]',
    )!;
    expect(trigger.getAttribute("aria-label")).toBe("Interface scale: 100%");

    await act(async () => trigger.click());
    const option = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="option"]'),
    ).find((node) => node.textContent?.includes("150%"));
    expect(option).toBeTruthy();
    await act(async () => option!.click());

    expect(localStorage.getItem("monocode.uiScale")).toBe("1.5");
    expect(trigger.getAttribute("aria-label")).toBe("Interface scale: 150%");
    document.documentElement.style.removeProperty("zoom");
  });

  // Frames 3 and 5 (ADE-170): the default the usage-limit card starts on.
  it("sets the usage-limit card's default time on the Spaces page", async () => {
    await render("spaces");
    expect(container.querySelector("h1")?.textContent).toBe("Spaces");
    const text = container.textContent ?? "";
    for (const line of [
      "One window, several Spaces. Each one is like a separate copy of MonoCode.",
      "Each Space has its own logins, Gigas, Monos, projects and sessions. Every Space keeps running while you look at another one.",
      "New Space",
      "When a login runs out",
      "Use the other login for",
      "Default time for the usage-limit card. When a login hits its limit, the card starts on this time; you can pick another one there. After that time the session stops and asks again.",
      "Between Spaces",
      "Messages",
      "A regular Mono can message a regular Mono in another Space. Gigas, sessions, asks and steers stay in their own Space.",
      "Regular Monos only",
      "Same project",
      "Automatic",
    ]) {
      expect(text).toContain(line);
    }
    expect(text).toContain(
      "the second Space works in its own worktree (monocode-worktrees/space-2/…) on space-2/ branches.",
    );

    const row = container.querySelector(
      '[data-setting-id="route-grant-default"]',
    )!;
    const trigger = row.querySelector<HTMLButtonElement>(
      '[aria-haspopup="listbox"]',
    )!;
    expect(trigger.getAttribute("aria-label")).toBe(
      "Use the other login for: 6 hours",
    );

    await act(async () => trigger.click());
    const options = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="option"]'),
    );
    expect(options.map((node) => node.textContent)).toEqual([
      "1 hour",
      "3 hours",
      "6 hoursdefault",
      "12 hours",
      "24 hours",
      "Until the limit resets",
    ]);
    // Label only; 6 hours adds the "default" hint and the check.
    expect(options.map((node) => node.childElementCount)).toEqual([
      1, 1, 3, 1, 1, 1,
    ]);
    await act(async () => options[5]!.click());

    expect(loadRouteGrantDefault()).toBe("reset");
    expect(trigger.getAttribute("aria-label")).toBe(
      "Use the other login for: Until the limit resets",
    );

    // A fresh mount reads the saved choice back.
    await act(async () => root.unmount());
    root = createRoot(container);
    await render("spaces");
    expect(
      container
        .querySelector('[data-setting-id="route-grant-default"]')!
        .querySelector('[aria-haspopup="listbox"]')!
        .getAttribute("aria-label"),
    ).toBe("Use the other login for: Until the limit resets");
  });

  it("reports collapsed project rail changes to the app shell", async () => {
    const onCollapsedProjectRailModeChange = vi.fn();
    await render("appearance", {
      collapsedProjectRailMode: "compact",
      onCollapsedProjectRailModeChange,
    });
    const control = container.querySelector<HTMLElement>(
      '[role="radiogroup"][aria-label="Collapsed project rail"]',
    )!;
    const hidden =
      control.querySelectorAll<HTMLButtonElement>('[role="radio"]')[1];

    await act(async () => hidden?.click());

    expect(onCollapsedProjectRailModeChange).toHaveBeenCalledWith("hidden");
  });

  // The search index is hand-maintained; this is what keeps it honest.
  it.each(
    [...new Set(SETTINGS_INDEX.map((entry) => entry.section))].map(
      (section) => ({ section }),
    ),
  )(
    "renders every indexed setting on the $section page",
    async ({ section }) => {
      await render(section);
      const hidden = loadInsetLayout()
        ? INSET_LAYOUT_HIDDEN_SETTINGS
        : CLASSIC_LAYOUT_HIDDEN_SETTINGS;
      const expected = SETTINGS_INDEX.filter(
        (entry) => entry.section === section && !hidden.has(entry.id),
      ).map((entry) => entry.id);
      expect(renderedSettingIds().sort()).toEqual(expected.sort());
    },
  );

  it("shows path details for every Agent CLI", async () => {
    await render("providers");
    expect(
      vi.mocked(invoke).mock.calls.some(([command]) => command === "harness_exec"),
    ).toBe(false);
    for (const harness of HARNESSES) {
      expect(
        container.querySelector(
          `[aria-label="Show ${HARNESS_TITLE[harness]} CLI details"]`,
        ),
      ).not.toBeNull();
    }
  });

  it("returns focus to the CLI trigger when the details popover closes", async () => {
    await render("providers");
    const trigger = container.querySelector<HTMLButtonElement>(
      '[aria-label="Show Codex CLI details"]',
    )!;
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    await act(async () => trigger.click());
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    await act(async () => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(trigger);
  });

  it("keeps location failures separate from CLI check failures", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "harness_resolve_codex") return { path: "/auto/codex" };
      if (command === "harness_exec")
        return { stdout: "codex-cli 0.156.1", cwd: "/helper-cwd" };
      if (command === "reveal_path") throw new Error("File manager unavailable");
      return undefined;
    });
    await render("providers");
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[aria-label="Show Codex CLI details"]',
        )!
        .click(),
    );
    await act(async () =>
      document
        .querySelector<HTMLButtonElement>(
          '[aria-label="Open Codex CLI location"]',
        )!
        .click(),
    );
    expect(document.body.textContent).toContain(
      "Could not open the CLI location: File manager unavailable",
    );
  });

  it("only tags rows that search can find", async () => {
    for (const section of SETTINGS_SECTIONS.map((item) => item.id)) {
      if (section === "skills") continue;
      await render(section);
      for (const id of renderedSettingIds()) {
        expect(
          SETTINGS_INDEX.some((entry) => entry.id === id),
          `${section}: ${id}`,
        ).toBe(true);
      }
    }
  });
});

// Frame 12 (ADE-170 card #47): Edit Space from a Space row's pencil.
describe("Settings > Spaces: Edit Space", () => {
  const VIOLET = MONO_COLORS[5];
  const IDENTITIES: Record<string, { email: string; plan: string }> = {
    "claude:default": { email: "tyler.main@example.com", plan: "Max 20x" },
    "claude:account-second": { email: "second.ts@example.com", plan: "Max 5x" },
    "claude:account-studio": { email: "studio.ts@example.com", plan: "Max 5x" },
    "codex:account-dev": {
      email: "tyler.dev@example.com",
      plan: "ChatGPT Pro",
    },
  };

  beforeEach(() => {
    // Frame 12 draws the emails; masking is the default.
    saveMaskEmails(false);
    saveProviderAccount({
      id: "account-second",
      provider: "claude",
      label: "Claude account 2",
    });
    saveProviderAccount({
      id: "account-studio",
      provider: "claude",
      label: "Studio",
    });
    saveProviderAccount({ id: "account-dev", provider: "codex", label: "Dev" });
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      const input = args as Record<string, string>;
      switch (command) {
        case "spaces_list":
          return [
            { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
            { id: "space-2", name: "Space 2", color: VIOLET, createdAt: 2 },
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
        case "provider_account_identity":
          return IDENTITIES[`${input.provider}:${input.accountId}`] ?? null;
        default:
          return undefined;
      }
    });
  });

  const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
  const buttonIn = (scope: ParentNode, text: string) =>
    Array.from(scope.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent === text,
    )!;
  const picker = (provider: string) =>
    dialog()
      .querySelector(`[data-space-login-row="${provider}"]`)!
      .querySelector<HTMLButtonElement>('[aria-haspopup="listbox"]')!;
  const writes = () =>
    vi
      .mocked(invoke)
      .mock.calls.filter(
        ([command]) =>
          command === "spaces_update" || command === "spaces_login_set",
      );

  async function openEdit(spaceId: string) {
    await render("spaces");
    await act(async () => {});
    await act(async () =>
      container
        .querySelector(`[data-space-id="${spaceId}"]`)!
        .querySelector("button")!
        .click(),
    );
    await act(async () => {});
  }

  async function pick(provider: string, label: string) {
    await act(async () => picker(provider).click());
    const options = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="option"]'),
    );
    await act(async () =>
      options.find((option) => option.textContent?.startsWith(label))!.click(),
    );
  }

  it("renames, recolours and picks each login for that Space only", async () => {
    await openEdit("space-2");
    expect(dialog().textContent).toContain("Edit Space");
    expect(
      dialog().querySelector<HTMLInputElement>(
        'input[aria-label="Space name"]',
      )!.value,
    ).toBe("Space 2");
    expect(
      dialog()
        .querySelector('[aria-label="Purple"]')!
        .getAttribute("aria-pressed"),
    ).toBe("true");
    expect(dialog().textContent).toContain(
      "One per provider. New sessions in Space 2 sign in with these.",
    );

    // Claude: Space 2's login, shown with its email; every Claude login to
    // pick from, each with email and plan. It has a login, so no "No login".
    expect(picker("claude").getAttribute("aria-label")).toBe(
      "Claude login: Studio",
    );
    expect(picker("claude").textContent).toBe("Studio · studio.ts@example.com");
    await act(async () => picker("claude").click());
    expect(
      Array.from(
        document.querySelectorAll('[role="option"]'),
        (option) => option.textContent,
      ),
    ).toEqual([
      "Default accounttyler.main@example.com · Max 20x",
      "Claude account 2second.ts@example.com · Max 5x",
      "Studiostudio.ts@example.com · Max 5x",
    ]);
    await act(async () => picker("claude").click());

    // Codex: none yet, muted, and "No Codex login" is a choice.
    expect(picker("codex").getAttribute("aria-label")).toBe(
      "Codex login: No Codex login",
    );
    expect(picker("codex").querySelector(".text-content\\/45")).not.toBeNull();
    await act(async () => picker("codex").click());
    expect(
      Array.from(
        document.querySelectorAll('[role="option"]'),
        (option) => option.textContent,
      ),
    ).toEqual([
      "Default account",
      "Devtyler.dev@example.com · ChatGPT Pro",
      "No Codex loginSessions in this Space cannot use Codex",
    ]);
    await act(async () => picker("codex").click());

    await pick("claude", "Claude account 2");
    await pick("codex", "Dev");
    const name = dialog().querySelector<HTMLInputElement>(
      'input[aria-label="Space name"]',
    )!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(name, "  Studio space  ");
      name.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () =>
      dialog().querySelector<HTMLButtonElement>('[aria-label="Teal"]')!.click(),
    );
    await act(async () => buttonIn(dialog(), "Save").click());

    expect(writes()).toEqual([
      [
        "spaces_update",
        { id: "space-2", name: "Studio space", color: MONO_COLORS[6] },
      ],
      [
        "spaces_login_set",
        {
          login: {
            spaceId: "space-2",
            provider: "claude",
            accountId: "account-second",
          },
        },
      ],
      [
        "spaces_login_set",
        {
          login: {
            spaceId: "space-2",
            provider: "codex",
            accountId: "account-dev",
          },
        },
      ],
    ]);
    expect(document.querySelector('[role="dialog"]')).toBeNull();
  });

  it("leaves emails out of the picker while they are masked", async () => {
    saveMaskEmails(true);
    await openEdit("space-2");
    expect(picker("claude").textContent).toBe("Studio");
    await act(async () => picker("claude").click());
    expect(
      Array.from(
        document.querySelectorAll('[role="option"]'),
        (option) => option.textContent,
      ),
    ).toEqual([
      "Default accountMax 20x",
      "Claude account 2Max 5x",
      "StudioMax 5x",
    ]);
  });

  it("saves only the name and colour when no login changed", async () => {
    await openEdit("space-1");
    // Space 1 predates colours: it opens on the first one.
    expect(
      dialog()
        .querySelector('[aria-label="Blue"]')!
        .getAttribute("aria-pressed"),
    ).toBe("true");
    await act(async () => buttonIn(dialog(), "Save").click());
    expect(writes()).toEqual([
      [
        "spaces_update",
        { id: "space-1", name: "Space 1", color: MONO_COLORS[0] },
      ],
    ]);
  });

  it("keeps the dialog open with the error when a save is refused", async () => {
    const answer = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "spaces_update") throw "database is locked";
      return answer(command, args);
    });
    await openEdit("space-2");
    await act(async () => buttonIn(dialog(), "Save").click());
    expect(dialog().querySelector('[role="alert"]')?.textContent).toBe(
      "database is locked",
    );
    await act(async () => buttonIn(dialog(), "Cancel").click());
    expect(document.querySelector('[role="dialog"]')).toBeNull();
  });

  it("adds a login with Providers' sign-in and picks it for the Space", async () => {
    await openEdit("space-2");
    const codexRow = () =>
      dialog().querySelector('[data-space-login-row="codex"]')!;
    await act(async () => buttonIn(codexRow(), "Add a login").click());

    expect(buttonIn(codexRow(), "Add a login")).toBeUndefined();
    expect(codexRow().textContent).toContain(
      "Same sign-in as Settings › Providers › Add account. When it finishes, the new login is saved there and picked here for this Space.",
    );
    const label = codexRow().querySelector<HTMLInputElement>(
      'input[aria-label="New Codex account"]',
    )!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(label, "Studio Codex");
      label.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => buttonIn(codexRow(), "Sign in and add").click());

    const added = providerAccounts("codex").find(
      (account) => account.label === "Studio Codex",
    )!;
    expect(loginHarness).toHaveBeenCalledWith("codex", added.id);
    expect(picker("codex").getAttribute("aria-label")).toBe(
      "Codex login: Studio Codex",
    );
    expect(buttonIn(codexRow(), "Add a login")).toBeDefined();
    // Picked, not saved, until Save.
    expect(writes()).toEqual([]);

    await act(async () => buttonIn(dialog(), "Save").click());
    expect(writes()[1]).toEqual([
      "spaces_login_set",
      { login: { spaceId: "space-2", provider: "codex", accountId: added.id } },
    ]);
  });

  it("shows a failed sign-in under the editor", async () => {
    vi.mocked(loginHarness).mockRejectedValueOnce(new Error("Browser closed"));
    await openEdit("space-2");
    const claudeRow = () =>
      dialog().querySelector('[data-space-login-row="claude"]')!;
    await act(async () => buttonIn(claudeRow(), "Add a login").click());
    const label = claudeRow().querySelector<HTMLInputElement>(
      'input[aria-label="New Claude Code account"]',
    )!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(label, "Work");
      label.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => buttonIn(claudeRow(), "Sign in and add").click());
    expect(claudeRow().querySelector('[role="alert"]')?.textContent).toBe(
      "Browser closed",
    );
    expect(picker("claude").getAttribute("aria-label")).toBe(
      "Claude login: Studio",
    );
  });
});

describe("settings search", () => {
  async function type(value: string) {
    const input = container.querySelector<HTMLInputElement>(
      '[aria-label="Search settings"]',
    )!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    return input;
  }

  function options(): HTMLButtonElement[] {
    return Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="option"]'),
    );
  }

  it("finds a setting that lives on another page", async () => {
    await render("general");
    await type("pacman");
    expect(options().map((item) => item.textContent)).toEqual([
      "Empty session gamesChat",
    ]);

    await act(async () => options()[0]!.click());
    expect(onSelectSection).toHaveBeenCalledWith("chat");
  });

  it("finds and reveals project notifications separately from global notifications", async () => {
    await render("general");
    await type("project notifications");
    expect(options().map((item) => item.textContent)).toEqual([
      "Project notificationsInbox",
    ]);

    await act(async () => options()[0]!.click());
    expect(onSelectSection).toHaveBeenCalledWith("inbox");
    await render("inbox");
    const section = container.querySelector(
      '[data-setting-id="project-notifications"]',
    );
    expect(
      section?.querySelector('[aria-label="Project notifications"]'),
    ).not.toBeNull();
    expect(section?.querySelector(".border-accent\\/60")).not.toBeNull();
  });

  // A page whose name starts with the query beats a setting that merely
  // mentions it; anything weaker loses to the settings themselves.
  it("ranks a page against the settings that mention it", async () => {
    await render("general");
    await type("archive");
    expect(
      options().map((item) => item.querySelector("span")!.textContent),
    ).toEqual(["Archive", "Show archived in the sidebar"]);

    await type("notification");
    expect(
      options().map((item) => item.querySelector("span")!.textContent),
    ).toEqual([
      "Notifications",
      "Project notifications",
      "Claude Code hooks",
      "General",
      "Inbox",
    ]);
  });

  it("closes the results without touching the page when cleared", async () => {
    await render("general");
    const input = await type("sounds");
    expect(options().length).toBeGreaterThan(0);
    await act(async () => {
      container
        .querySelector<HTMLButtonElement>(
          '[aria-label="Clear settings search"]',
        )!
        .click();
    });
    expect(options()).toHaveLength(0);
    expect(input.value).toBe("");
    expect(onSelectSection).not.toHaveBeenCalled();
  });

  it("lists Escape stopping a session without a recorder", async () => {
    await render("keybindings");
    const row = [...container.querySelectorAll("span")].find(
      (span) => span.textContent === "Session: Stop",
    )?.parentElement;
    expect(row?.textContent).toContain("Esc");
    expect(
      container.querySelector('[aria-label="Change Session: Stop shortcut"]'),
    ).toBeNull();
  });

  it("records a custom keybinding from the key cell", async () => {
    await render("keybindings");
    const input = container.querySelector<HTMLInputElement>(
      '[aria-label="Change App: Search shortcut"]',
    )!;
    await act(async () => input.click());
    await act(async () =>
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", {
          code: "KeyM",
          key: "m",
          ctrlKey: true,
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );

    expect(localStorage.getItem("monocode.keybindingOverrides")).toBe(
      '{"App: Search":{"shortcut":"Control+Shift+KeyM"}}',
    );
    expect(input.value).toBe("Ctrl+Shift+M");
  });

  it("lets Tab leave the recorder and keeps Cmd+Delete recordable", async () => {
    await render("keybindings");
    const input = container.querySelector<HTMLInputElement>(
      '[aria-label="Change App: Search shortcut"]',
    )!;

    await act(async () => input.click());
    await act(async () =>
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", {
          code: "Tab",
          key: "Tab",
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
    expect(container.textContent).not.toContain("Del disables");

    await act(async () => input.click());
    await act(async () =>
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", {
          code: "Delete",
          key: "Delete",
          metaKey: true,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
    expect(localStorage.getItem("monocode.keybindingOverrides")).toBe(
      '{"App: Search":{"shortcut":"Command+Delete"}}',
    );
  });

  it("surfaces a storage failure instead of silently dropping the change", async () => {
    await render("keybindings");
    (
      localStorage as unknown as {
        setItem: (key: string, value: string) => void;
      }
    ).setItem = () => {
      throw new Error("quota exceeded");
    };
    const input = container.querySelector<HTMLInputElement>(
      '[aria-label="Change App: Search shortcut"]',
    )!;
    await act(async () => input.click());
    await act(async () =>
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", {
          code: "KeyY",
          key: "y",
          ctrlKey: true,
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );

    expect(container.textContent).toContain("Could not save shortcuts");
  });

  it("records an Alt shortcut on a keybinding row", async () => {
    await render("keybindings");
    const input = container.querySelector<HTMLInputElement>(
      '[aria-label="Change App: Search shortcut"]',
    )!;
    await act(async () => input.click());
    await act(async () =>
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", {
          code: "KeyM",
          key: "m",
          altKey: true,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );

    expect(localStorage.getItem("monocode.keybindingOverrides")).toBe(
      '{"App: Search":{"shortcut":"Option+KeyM"}}',
    );
    expect(input.value).toBe("Alt+M");
  });

  it("disables and restores an individual keybinding", async () => {
    await render("keybindings");
    const input = container.querySelector<HTMLInputElement>(
      '[aria-label="Change App: Search shortcut"]',
    )!;
    await act(async () => input.click());
    await act(async () =>
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", {
          code: "Backspace",
          key: "Backspace",
          bubbles: true,
          cancelable: true,
        }),
      ),
    );

    expect(localStorage.getItem("monocode.keybindingOverrides")).toBe(
      '{"App: Search":{"disabled":true}}',
    );
    expect(input.value).toBe("Disabled");

    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(
          '[aria-label="Reset App: Search shortcut"]',
        )!
        .click(),
    );
    expect(localStorage.getItem("monocode.keybindingOverrides")).toBeNull();
  });

  it("reveals a setting on the current page", async () => {
    await render("general");
    await type("sounds");
    await act(async () => options()[0]!.click());
    expect(onSelectSection).not.toHaveBeenCalled();
    const row = container.querySelector('[data-setting-id="sounds"]')!;
    expect(row.className).toContain("bg-accent/10");
  });
});

describe("providers scope inheritance", () => {
  async function selectScope(label: string) {
    const trigger = container.querySelector<HTMLButtonElement>(
      '[aria-label^="Provider defaults scope"]',
    )!;
    await act(async () => trigger.click());
    const option = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="option"]'),
    ).find((node) => node.textContent?.trim() === label);
    expect(option).toBeTruthy();
    await act(async () => option!.click());
  }

  it("inherits the global default provider and picker visibility in project scope", async () => {
    localStorage.setItem(
      "monocode.lastModel",
      JSON.stringify({ harness: "claude", model: "claude:opus-5" }),
    );
    localStorage.setItem(
      "monocode.hiddenPickerProviders",
      JSON.stringify(["cursor"]),
    );
    await render("providers");

    await selectScope("repo");

    // A project with no overrides shows the inherited global default provider.
    const claudeRow = container
      .querySelector('[aria-label^="Claude Code model"]')!
      .closest(".settings-row")!;
    const claudeDefault = Array.from(
      claudeRow.querySelectorAll<HTMLButtonElement>("button"),
    ).find((node) => node.textContent?.trim() === "Default");
    expect(claudeDefault).toBeTruthy();

    // Picker visibility also inherits the global setting.
    expect(
      container
        .querySelector<HTMLButtonElement>(
          '[aria-label="Show Claude Code in the model picker"]',
        )!
        .getAttribute("aria-checked"),
    ).toBe("true");
    const cursorToggle = container.querySelector<HTMLButtonElement>(
      '[aria-label="Show Cursor in the model picker"]',
    )!;
    expect(cursorToggle.getAttribute("aria-checked")).toBe("false");
    // Global precedence: the project toggle cannot turn a globally hidden
    // provider back on, so it is locked and explained.
    expect(cursorToggle.hasAttribute("disabled")).toBe(true);
    expect(cursorToggle.closest(".settings-row")?.textContent).toContain(
      "Hidden globally",
    );
  });
});

// Frame 5: the page reads the window's alarms snapshot and writes through Rust.
describe("Settings > Alarms", () => {
  let feed: AlarmsFeed;
  let answer: Record<string, unknown>;

  const alarmsView = (settings: Record<string, unknown> = {}) => {
    const wire = snapshotWire({ episodes: [], holds: [] });
    return { ...wire, settings: { ...wire.settings, ...settings } };
  };

  /** The page inside a provider whose feed has fetched `answer`. */
  async function renderAlarms() {
    feed = createAlarmsFeed({
      invoke: (command, args) => invoke(command, args),
      listen: async () => () => {},
      openAgent: vi.fn(),
      answerDialog: vi.fn(),
      sendAsUser: vi.fn(),
      refused: vi.fn(),
    });
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "alarms_view" ? answer : undefined,
    );
    await feed.refresh();
    await act(async () =>
      root.render(
        createElement(
          AlarmsProvider,
          { feed },
          createElement(SettingsView, {
            section: "alarms",
            cwd: "/repo",
            sessions: [],
            onClose: vi.fn(),
            onSelectSection,
            onOpenSession: vi.fn(),
            onArchiveSession: vi.fn(),
            onDeleteSession: vi.fn(),
            onOpenWhatsNew: vi.fn(),
          }),
        ),
      ),
    );
  }

  beforeEach(() => {
    answer = alarmsView();
  });

  it("is on the rail and in search", () => {
    // Defect: the Alarms page stays hidden after row U2 wires it.
    expect(
      settingsSectionsByGroup().flatMap((group) =>
        group.sections.map((section) => section.id),
      ),
    ).toContain("alarms");
    expect(
      searchSettings("alarms").some((result) => result.section === "alarms"),
    ).toBe(true);
    expect(
      searchSettings("stall").some((result) => result.section === "alarms"),
    ).toBe(true);
    expect(isSettingsSectionId("alarms")).toBe(true);
  });

  it("stays empty until this window has the alarms snapshot", async () => {
    // Defect: the page shows made-up defaults before Rust has said anything.
    await render("alarms");
    expect(container.textContent).not.toContain("Stall after");
  });

  it("shows the frame's rows with the snapshot's values", async () => {
    // Defect: a default differs from the approved one (Tyler's sleeps held, 10 s skip), or the Giga shows by id.
    await renderAlarms();
    const text = container.textContent ?? "";
    for (const line of [
      "Stall alarm",
      "Stall after",
      "Root Giga",
      "Sleep hold",
      "Hold foreground waits",
      "Hold Tyler's agents' sleeps",
      "Skip short sleeps",
    ])
      expect(text).toContain(line);
    expect(text).toContain("15 min");
    expect(text).toContain("Monocode General");
    expect(text).toContain("10 s");
    expect(text).toContain("Let a sleep under 10 s run without asking.");
    expect(text).not.toContain("Pending Tyler");
    const switches = Array.from(
      container.querySelectorAll<HTMLElement>('[role="switch"]'),
      (node) => [
        node.getAttribute("aria-label"),
        node.getAttribute("aria-checked"),
      ],
    );
    expect(switches).toEqual([
      ["Hold foreground waits", "true"],
      ["Hold Tyler's agents' sleeps", "true"],
    ]);
  });

  it("shows a stall time set between the presets", async () => {
    // Defect: 20 min (alarms.set) shows nothing selected, and cannot be picked back once changed.
    answer = alarmsView({ stallMinutes: 20 });
    await renderAlarms();
    expect(
      container.querySelector('button[aria-label="Stall after: 20 min"]')
        ?.textContent,
    ).toContain("20 min");
  });

  it("shows None, not raw text, for a root Giga that is no real Giga", async () => {
    // Defect (#543): a deleted Giga's id shows in the Root Giga select with an empty menu.
    answer = alarmsView({ rootGiga: { id: "gone", name: "Old Giga" } });
    await renderAlarms();
    const select = container.querySelector('button[aria-label^="Root Giga"]');
    expect(select?.textContent).toContain("None");
    expect(container.textContent).not.toContain("gone");
  });

  it("writes a switch to Rust, and shows it once the next snapshot does", async () => {
    // Defect: a change stays on the page only, or flips before Rust has it.
    await renderAlarms();
    vi.mocked(invoke).mockClear();
    const hold = () =>
      container.querySelector<HTMLElement>(
        '[role="switch"][aria-label="Hold Tyler\'s agents\' sleeps"]',
      )!;
    answer = alarmsView({ tylerSleepHold: false });
    await act(async () => hold().click());
    expect(vi.mocked(invoke).mock.calls[0]).toEqual([
      "alarms_settings_set",
      { key: "tyler_sleep_hold", value: "off" },
    ]);
    await act(async () => feed.refresh());
    expect(hold().getAttribute("aria-checked")).toBe("false");
  });

  it("writes the stall time and the root Giga by its id", async () => {
    // Defect: the root Giga is saved by its name, which Rust refuses.
    await renderAlarms();
    vi.mocked(invoke).mockClear();
    feed.changeSettings({ stallMinutes: 30, rootGiga: "fork-orch" });
    expect(
      vi
        .mocked(invoke)
        .mock.calls.filter(([command]) => command === "alarms_settings_set"),
    ).toEqual([
      ["alarms_settings_set", { key: "stall_minutes", value: "30" }],
      ["alarms_settings_set", { key: "root_giga", value: "fork-orch" }],
    ]);
  });
});

describe("deleting an archived project", () => {
  const OLD = "/code/old";
  const UNDER = "/home/u/MonoCode/gigas/general";
  const deleteButtons = () =>
    [...container.querySelectorAll("button")].filter(
      (button) => button.textContent === "Delete",
    );
  const dialog = () => document.querySelector('[role="dialog"][aria-label^="Delete"]');
  const alert = () => container.querySelector('[role="alert"]');
  const click = (button: HTMLElement) =>
    act(async () => {
      button.click();
      await new Promise((done) => setTimeout(done));
    });
  const onDeleteProject = vi.fn();

  beforeEach(async () => {
    setHomeDir("/home/u");
    onDeleteProject.mockReset();
    vi.mocked(checkProjectDelete).mockReset();
    vi.mocked(checkProjectDelete).mockResolvedValue(null);
    localStorage.setItem(
      "monocode.archivedProjects",
      JSON.stringify([
        { path: OLD, archivedAt: 2 },
        { path: UNDER, archivedAt: 1 },
      ]),
    );
    await render("archive", { onDeleteProject });
  });

  afterEach(() => setHomeDir(undefined));

  it("offers no Delete on a path under the Giga workspace", () => {
    expect(deleteButtons()).toHaveLength(1);
  });

  it("offers no Delete without a handler", async () => {
    await render("archive", { onDeleteProject: undefined });
    expect(deleteButtons()).toHaveLength(0);
  });

  it("shows a refusal in the group and opens no dialog", async () => {
    vi.mocked(checkProjectDelete).mockResolvedValue("Take it off that Mono first.");
    await click(deleteButtons()[0]);
    expect(checkProjectDelete).toHaveBeenCalledWith(OLD);
    expect(alert()?.textContent).toBe("Take it off that Mono first.");
    expect(dialog()).toBeNull();
  });

  it("clears an old refusal and opens the dialog once the check passes", async () => {
    vi.mocked(checkProjectDelete).mockResolvedValueOnce("No.");
    await click(deleteButtons()[0]);
    expect(alert()).not.toBeNull();
    await click(deleteButtons()[0]);
    expect(alert()).toBeNull();
    expect(dialog()).not.toBeNull();
  });

  it("deletes on confirm and shows a refusal that comes after it", async () => {
    onDeleteProject.mockResolvedValue("A session is running.");
    await click(deleteButtons()[0]);
    const confirm = [...dialog()!.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "Delete",
    )!;
    await click(confirm);
    expect(onDeleteProject).toHaveBeenCalledWith(OLD);
    expect(dialog()).toBeNull();
    expect(alert()?.textContent).toBe("A session is running.");
  });

  it("shows no refusal when the delete goes through", async () => {
    onDeleteProject.mockResolvedValue(null);
    await click(deleteButtons()[0]);
    const confirm = [...dialog()!.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "Delete",
    )!;
    await click(confirm);
    expect(alert()).toBeNull();
  });
});
