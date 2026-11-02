// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_PROVIDER_ACCOUNT_ID,
  newProviderAccount,
  providerAccountLabel,
  providerAccountExists,
  providerAccounts,
  removeProviderAccount,
  renameProviderAccount,
  saveProviderAccount,
  selectedProviderAccountId,
  selectProviderAccount,
  supportsAccountProfiles,
} from "./providerAccounts";

type Login = { provider: string; id: string; label: string };
type Row = Login & { createdAt: number };
type SpaceLogin = { spaceId: string; provider: string; accountId: string };

/** `monocode.db` behind the Spaces storage commands, as Rust answers them. */
const db = vi.hoisted(() => ({
  accounts: [] as Row[],
  spaceLogins: [] as SpaceLogin[],
  calls: [] as [string, unknown][],
  refuse: new Set<string>(),
  /** `spaces_account_save` of these login ids is refused. */
  refuseSave: new Set<string>(),
  /** The fake clock `created_at` comes from: every save in a test is one ms. */
  now: 1,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (command: string, args: Record<string, never> = {}) => {
    db.calls.push([command, args]);
    if (db.refuse.has(command)) throw `${command} refused`;
    const same = (row: Login, provider: string, id: string) =>
      row.provider === provider && row.id === id;
    switch (command) {
      case "spaces_logins_list":
        return db.spaceLogins;
      // account.rs: ORDER BY provider, created_at, id
      case "spaces_accounts_list":
        return [...db.accounts]
          .sort(
            (a, b) =>
              a.provider.localeCompare(b.provider) ||
              a.createdAt - b.createdAt ||
              (a.id < b.id ? -1 : 1),
          )
          .map(({ provider, id, label }) => ({ provider, id, label }));
      case "spaces_account_save": {
        const { provider, id, label } = args.account as Login;
        if (db.refuseSave.has(id)) throw `spaces_account_save refused ${id}`;
        // serde refuses a lone surrogate; clean_label cuts 48 code points.
        if (
          /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/.test(
            label,
          )
        ) {
          throw "lone leading surrogate in hex escape";
        }
        const cut = Array.from(label.split(/\s+/).filter(Boolean).join(" "))
          .slice(0, 48)
          .join("");
        if (!cut) throw "A provider account needs a label";
        const row = db.accounts.find((entry) => same(entry, provider, id));
        if (row) row.label = cut;
        else db.accounts.push({ provider, id, label: cut, createdAt: db.now });
        return null;
      }
      case "spaces_account_remove": {
        const { provider, id } = args as { provider: string; id: string };
        if (id === "default")
          throw "The default provider account cannot be removed";
        db.accounts = db.accounts.filter((row) => !same(row, provider, id));
        return null;
      }
      case "provider_account_remove":
        return null;
      case "spaces_login_set": {
        const login = args.login as SpaceLogin;
        db.spaceLogins = [
          ...db.spaceLogins.filter(
            (row) =>
              row.spaceId !== login.spaceId || row.provider !== login.provider,
          ),
          login,
        ];
        return null;
      }
    }
    throw `no command ${command}`;
  },
}));

/**
 * The windows' BroadcastChannel: a message reaches every other open channel of
 * the same name. A boot closes the earlier ones, as a restart does, unless it
 * opens another window beside them.
 */
const windows = vi.hoisted(() => {
  type Listener = ((event: { data: unknown }) => void) | null;
  const open: { name: string; onmessage: Listener }[] = [];
  class Channel {
    onmessage: Listener = null;
    constructor(readonly name: string) {
      open.push(this);
    }
    postMessage(data: unknown) {
      for (const other of open) {
        if (other !== this && other.name === this.name) {
          setTimeout(() => other.onmessage?.({ data }));
        }
      }
    }
  }
  Object.assign(globalThis, { BroadcastChannel: Channel });
  return { open };
});

/** A fresh copy of the module, as a new boot of the app (or a new window) loads it. */
async function boot(anotherWindow = false) {
  if (!anotherWindow) windows.open.length = 0;
  vi.resetModules();
  const accounts = await import("./providerAccounts");
  await accounts.loadProviderAccounts();
  return accounts;
}

function freshDatabase() {
  db.accounts = ["claude", "codex", "devin"].map((provider) => ({
    provider,
    id: "default",
    label: "Default account",
    createdAt: 0,
  }));
  db.now = 1;
  db.refuseSave.clear();
  windows.open.length = 0;
  db.spaceLogins = [];
  db.calls = [];
  db.refuse.clear();
}

const sent = (command: string) =>
  db.calls.filter(([name]) => name === command).map(([, args]) => args);

beforeEach(() => {
  localStorage.clear();
  freshDatabase();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("provider accounts", () => {
  it("always exposes the provider-owned default account", () => {
    expect(providerAccounts("claude")).toEqual([
      {
        id: DEFAULT_PROVIDER_ACCOUNT_ID,
        provider: "claude",
        label: "Default account",
        isDefault: true,
      },
    ]);
  });

  it("lists Devin with only its CLI login and refuses named profiles", () => {
    expect(supportsAccountProfiles("devin")).toBe(false);
    expect(supportsAccountProfiles("codex")).toBe(true);
    saveProviderAccount({
      id: "account-work",
      provider: "devin",
      label: "Work",
    });
    // Stale storage from elsewhere is ignored too.
    localStorage.setItem(
      "monocode.providerAccounts.v1",
      JSON.stringify({
        devin: [{ id: "account-old", provider: "devin", label: "Old" }],
      }),
    );
    expect(providerAccounts("devin").map((account) => account.id)).toEqual([
      DEFAULT_PROVIDER_ACCOUNT_ID,
    ]);
    expect(renameProviderAccount("devin", "default", "Personal")?.label).toBe(
      "Personal",
    );
    expect(providerAccountLabel("devin", "default")).toBe("Personal");
  });

  it("stores named profiles separately per provider", () => {
    vi.spyOn(crypto, "randomUUID").mockReturnValue(
      "00000000-0000-4000-8000-000000000001",
    );
    const work = newProviderAccount("claude", "  Work   account  ");
    saveProviderAccount(work);

    expect(providerAccounts("claude").map((account) => account.label)).toEqual([
      "Default account",
      "Work account",
    ]);
    expect(providerAccounts("codex")).toHaveLength(1);
    expect(providerAccountLabel("claude", work.id)).toBe("Work account");
  });

  it("falls back to the default account for malformed stored profiles", () => {
    localStorage.setItem(
      "monocode.providerAccounts.v1",
      JSON.stringify({ claude: { id: "not-an-array" } }),
    );

    expect(providerAccounts("claude").map((account) => account.id)).toEqual([
      DEFAULT_PROVIDER_ACCOUNT_ID,
    ]);
  });

  it("falls back when the stored root is not a record", () => {
    for (const malformed of ["null", "[]", "42"]) {
      localStorage.setItem("monocode.providerAccounts.v1", malformed);
      expect(providerAccounts("claude").map((account) => account.id)).toEqual([
        DEFAULT_PROVIDER_ACCOUNT_ID,
      ]);
    }

    localStorage.setItem("monocode.providerAccountSelections.v1", "null");
    expect(selectedProviderAccountId("claude", "/repo")).toBe(
      DEFAULT_PROVIDER_ACCOUNT_ID,
    );
  });

  it("replaces malformed provider storage when saving an account", () => {
    localStorage.setItem(
      "monocode.providerAccounts.v1",
      JSON.stringify({ codex: "not-an-array" }),
    );

    expect(() =>
      saveProviderAccount({
        id: "account-work",
        provider: "codex",
        label: "Work",
      }),
    ).not.toThrow();
    expect(providerAccounts("codex").map((account) => account.label)).toEqual([
      "Default account",
      "Work",
    ]);
  });

  it("discards malformed account entries when saving an account", () => {
    localStorage.setItem(
      "monocode.providerAccounts.v1",
      JSON.stringify({
        codex: [
          null,
          42,
          { id: "account-missing-label", provider: "codex" },
          { id: "account-keep", provider: "codex", label: "Keep" },
        ],
      }),
    );

    expect(() =>
      saveProviderAccount({
        id: "account-work",
        provider: "codex",
        label: "Work",
      }),
    ).not.toThrow();
    expect(providerAccounts("codex").map((account) => account.label)).toEqual([
      "Default account",
      "Keep",
      "Work",
    ]);
  });

  it("remembers a selection per project and ignores unknown ids", () => {
    const work = {
      id: "account-work",
      provider: "codex" as const,
      label: "Work",
    };
    saveProviderAccount(work);
    selectProviderAccount("codex", "/repo/one", work.id);

    expect(selectedProviderAccountId("codex", "/repo/one")).toBe(work.id);
    expect(selectedProviderAccountId("codex", "/repo/two")).toBe(
      DEFAULT_PROVIDER_ACCOUNT_ID,
    );
    selectProviderAccount("codex", "/repo/one", "missing");
    expect(selectedProviderAccountId("codex", "/repo/one")).toBe(work.id);
  });

  it("renames a named account without changing its identity or order", () => {
    saveProviderAccount({
      id: "account-work",
      provider: "codex",
      label: "Wrk",
    });
    saveProviderAccount({
      id: "account-personal",
      provider: "codex",
      label: "Personal",
    });

    expect(
      renameProviderAccount("codex", "account-work", "  Work   account  "),
    ).toEqual({
      id: "account-work",
      provider: "codex",
      label: "Work account",
    });
    expect(providerAccounts("codex").map((account) => account.id)).toEqual([
      DEFAULT_PROVIDER_ACCOUNT_ID,
      "account-work",
      "account-personal",
    ]);
  });

  it("removes named account metadata and every project selection", () => {
    const work = {
      id: "account-work",
      provider: "claude" as const,
      label: "Work",
    };
    saveProviderAccount(work);
    selectProviderAccount("claude", "/repo/one", work.id);
    selectProviderAccount("claude", "/repo/two", work.id);

    expect(removeProviderAccount("claude", work.id)).toBe(true);
    expect(providerAccountExists("claude", work.id)).toBe(false);
    expect(selectedProviderAccountId("claude", "/repo/one")).toBe(
      DEFAULT_PROVIDER_ACCOUNT_ID,
    );
    expect(selectedProviderAccountId("claude", "/repo/two")).toBe(
      DEFAULT_PROVIDER_ACCOUNT_ID,
    );
  });

  it("renames but does not remove the provider-owned default account", () => {
    expect(renameProviderAccount("codex", "default", "  Personal  ")).toEqual({
      id: DEFAULT_PROVIDER_ACCOUNT_ID,
      provider: "codex",
      label: "Personal",
      isDefault: true,
    });
    expect(removeProviderAccount("codex", "default")).toBe(false);
    expect(providerAccounts("codex")[0]?.label).toBe("Personal");
  });

  it("preserves a custom default name when named profiles change", () => {
    renameProviderAccount("claude", "default", "Primary");
    saveProviderAccount({
      id: "account-work",
      provider: "claude",
      label: "Work",
    });
    removeProviderAccount("claude", "account-work");

    expect(providerAccounts("claude")[0]?.label).toBe("Primary");
  });
});

describe("provider accounts in monocode.db", () => {
  const BROWSER_LOGINS = JSON.stringify({
    claude: [
      { id: "default", provider: "claude", label: "Primary" },
      { id: "account-work", provider: "claude", label: "Work" },
      { id: "account-alpha", provider: "claude", label: "Alpha" },
      { id: "bad id!", provider: "claude", label: "Bad" },
    ],
    codex: [{ id: "account-x", provider: "codex", label: "X" }],
    devin: [{ id: "account-old", provider: "devin", label: "Old" }],
  });
  const labels = (
    accounts: typeof import("./providerAccounts"),
    provider: "claude" | "codex" | "devin",
  ) =>
    accounts
      .providerAccounts(provider)
      .map(({ id, label }) => `${id} ${label}`);
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
  const marker = () =>
    JSON.parse(
      localStorage.getItem("monocode.providerAccounts.v1.import") ?? "[]",
    );

  it("imports browser storage once, in its order, then reads and writes the database", async () => {
    localStorage.setItem("monocode.providerAccounts.v1", BROWSER_LOGINS);
    let accounts = await boot();

    const claude = [
      "default Primary",
      "account-work Work",
      "account-alpha Alpha",
    ];
    expect(labels(accounts, "claude")).toEqual(claude);
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-x X",
    ]);
    expect(labels(accounts, "devin")).toEqual(["default Default account"]);
    expect(db.accounts.filter(({ id }) => id === "default")).toHaveLength(3);
    expect(db.spaceLogins).toEqual(
      ["claude", "codex", "devin"].map((provider) => ({
        spaceId: "space-1",
        provider,
        accountId: "default",
      })),
    );
    expect(localStorage.getItem("monocode.providerAccounts.v1")).toBe(
      BROWSER_LOGINS,
    );

    db.calls = [];
    accounts.saveProviderAccount({
      id: "account-new",
      provider: "codex",
      label: " New ",
    });
    expect(
      accounts.renameProviderAccount("claude", "account-work", "Office")?.label,
    ).toBe("Office");
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-x X",
      "account-new New",
    ]);
    await vi.waitFor(() => expect(db.calls).toHaveLength(2));
    await settle();
    expect(db.calls).toEqual([
      [
        "spaces_account_save",
        { account: { id: "account-new", provider: "codex", label: "New" } },
      ],
      [
        "spaces_account_save",
        {
          account: { id: "account-work", provider: "claude", label: "Office" },
        },
      ],
    ]);
    expect(localStorage.getItem("monocode.providerAccounts.v1")).toBe(
      BROWSER_LOGINS,
    );

    // Same-ms saves list by id in the database (alpha before work); the
    // snapshot keeps the order browser storage had.
    db.calls = [];
    accounts = await boot();
    expect(db.calls.map(([command]) => command)).toEqual([
      "spaces_accounts_list",
    ]);
    expect(labels(accounts, "claude")).toEqual([
      "default Primary",
      "account-work Office",
      "account-alpha Alpha",
    ]);
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-x X",
      "account-new New",
    ]);
  });

  it("gives Space 1 each CLI's own login when browser storage holds none", async () => {
    const accounts = await boot();

    expect(db.accounts.map(({ provider, id }) => `${provider} ${id}`)).toEqual([
      "claude default",
      "codex default",
      "devin default",
    ]);
    expect(sent("spaces_login_set")).toHaveLength(3);
    expect(db.spaceLogins.map(({ accountId }) => accountId)).toEqual([
      "default",
      "default",
      "default",
    ]);
    expect(accounts.providerAccountExists("claude", "default")).toBe(true);
  });

  it("cuts a long label to 48 code points before the database sees it", async () => {
    // The fake database cuts too, as Rust does; this reads what TS sends.
    const long = `${"\u{1F600}".repeat(10)}${"a".repeat(50)}`;
    const cut = `${"\u{1F600}".repeat(10)}${"a".repeat(38)}`;
    const accounts = await boot();
    db.calls = [];
    accounts.saveProviderAccount({
      id: "account-long",
      provider: "claude",
      label: long,
    });
    expect(labels(accounts, "claude")).toEqual([
      "default Default account",
      `account-long ${cut}`,
    ]);
    await vi.waitFor(() =>
      expect(sent("spaces_account_save")).toEqual([
        { account: { id: "account-long", provider: "claude", label: cut } },
      ]),
    );
  });

  it("imports an emoji label at the limit whole, and retries a refused import save", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const emoji = `${"a".repeat(47)}\u{1F600}`;
    localStorage.setItem(
      "monocode.providerAccounts.v1",
      JSON.stringify({
        claude: [
          { id: "account-emoji", provider: "claude", label: emoji },
          { id: "account-long", provider: "claude", label: "b".repeat(50) },
          { id: "account-late", provider: "claude", label: "Late" },
        ],
      }),
    );
    db.refuseSave.add("account-late");
    let accounts = await boot();

    expect(
      accounts
        .providerAccounts("claude")
        .find(({ id }) => id === "account-long")?.label,
    ).toBe("b".repeat(48));
    expect(db.accounts.find(({ id }) => id === "account-emoji")?.label).toBe(
      emoji,
    );
    expect(db.accounts.some(({ id }) => id === "account-late")).toBe(false);
    expect(console.error).toHaveBeenCalledWith(
      "spaces_account_save refused account-late",
    );
    expect(marker()).toContain("claude/account-emoji");
    expect(marker()).not.toContain("claude/account-late");

    db.refuseSave.clear();
    accounts = await boot();
    expect(labels(accounts, "claude")).toEqual([
      "default Default account",
      `account-emoji ${emoji}`,
      `account-long ${"b".repeat(48)}`,
      "account-late Late",
    ]);
    expect(marker()).toContain("claude/account-late");
    db.calls = [];
    await boot();
    expect(db.calls.map(([command]) => command)).toEqual([
      "spaces_accounts_list",
    ]);
  });

  it("retries only the refused import entry, so later changes survive the next boot", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    localStorage.setItem(
      "monocode.providerAccounts.v1",
      JSON.stringify({
        claude: [
          { id: "account-work", provider: "claude", label: "Work" },
          { id: "account-gone", provider: "claude", label: "Gone" },
          { id: "account-late", provider: "claude", label: "Late" },
        ],
      }),
    );
    db.refuseSave.add("account-late");
    let accounts = await boot();
    accounts.renameProviderAccount("claude", "account-work", "Office");
    accounts.removeProviderAccount("claude", "account-gone");
    await settle();

    db.refuseSave.clear();
    db.calls = [];
    accounts = await boot();
    expect(sent("spaces_account_save")).toEqual([
      { account: { id: "account-late", provider: "claude", label: "Late" } },
      { account: { id: "account-late", provider: "claude", label: "Late" } },
    ]);
    expect(labels(accounts, "claude")).toEqual([
      "default Default account",
      "account-work Office",
      "account-late Late",
    ]);
    expect(marker()).toContain("claude/account-late");
  });

  it("imports a legacy label cut through an emoji once, without its lone half", async () => {
    const legacy = `${"a".repeat(47)}\u{1F600}xyz`.slice(0, 48);
    localStorage.setItem(
      "monocode.providerAccounts.v1",
      JSON.stringify({
        claude: [{ id: "account-old", provider: "claude", label: legacy }],
      }),
    );
    let accounts = await boot();
    expect(labels(accounts, "claude")).toEqual([
      "default Default account",
      `account-old ${"a".repeat(47)}`,
    ]);
    expect(db.accounts.find(({ id }) => id === "account-old")?.label).toBe(
      "a".repeat(47),
    );
    expect(marker()).toContain("claude/account-old");

    db.calls = [];
    accounts = await boot();
    expect(db.calls.map(([command]) => command)).toEqual([
      "spaces_accounts_list",
    ]);
    expect(labels(accounts, "claude")).toEqual([
      "default Default account",
      `account-old ${"a".repeat(47)}`,
    ]);
  });

  it("keeps browser storage when the database cannot load, and tries again on the next write", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    localStorage.setItem("monocode.providerAccounts.v1", BROWSER_LOGINS);
    db.refuse.add("spaces_accounts_list");
    const accounts = await boot();

    expect(console.error).toHaveBeenCalledWith("spaces_accounts_list refused");
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-x X",
    ]);
    db.calls = [];
    accounts.saveProviderAccount({
      id: "account-new",
      provider: "codex",
      label: "New",
    });
    expect(localStorage.getItem("monocode.providerAccounts.v1")).toContain(
      "account-new",
    );
    await vi.waitFor(() => expect(console.error).toHaveBeenCalledTimes(3));
    expect(db.calls.map(([command]) => command)).toEqual([
      "spaces_accounts_list",
      "spaces_accounts_list",
    ]);

    db.refuse.delete("spaces_accounts_list");
    db.calls = [];
    accounts.saveProviderAccount({
      id: "account-next",
      provider: "codex",
      label: "Next",
    });
    await vi.waitFor(() =>
      expect(db.accounts.some(({ id }) => id === "account-next")).toBe(true),
    );
    await vi.waitFor(() =>
      expect(labels(accounts, "codex")).toContain("account-next Next"),
    );

    db.calls = [];
    accounts.saveProviderAccount({
      id: "account-last",
      provider: "codex",
      label: "Last",
    });
    await vi.waitFor(() => expect(db.calls).toHaveLength(1));
    await settle();
    expect(db.calls.map(([command]) => command)).toEqual([
      "spaces_account_save",
    ]);
  });

  it("keeps the saved writes around a refused one, and a refused remove keeps the folder", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const accounts = await boot();
    const credentials = await import("./providerAccountCredentials");
    db.refuseSave.add("account-b");
    db.calls = [];

    accounts.saveProviderAccount({
      id: "account-a",
      provider: "codex",
      label: "A",
    });
    accounts.saveProviderAccount({
      id: "account-b",
      provider: "codex",
      label: "B",
    });
    accounts.saveProviderAccount({
      id: "account-c",
      provider: "codex",
      label: "C",
    });
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-a A",
      "account-b B",
      "account-c C",
    ]);
    await vi.waitFor(() =>
      expect(sent("spaces_accounts_list")).toHaveLength(1),
    );
    await vi.waitFor(() =>
      expect(labels(accounts, "codex")).toEqual([
        "default Default account",
        "account-a A",
        "account-c C",
      ]),
    );
    expect(console.error).toHaveBeenCalledWith(
      "spaces_account_save refused account-b",
    );

    db.calls = [];
    accounts.saveProviderAccount({
      id: "account-d",
      provider: "codex",
      label: "D",
    });
    await vi.waitFor(() => expect(db.calls).toHaveLength(1));
    await settle();
    expect(db.calls.map(([command]) => command)).toEqual([
      "spaces_account_save",
    ]);

    db.refuse.add("spaces_account_remove");
    db.calls = [];
    await expect(
      credentials.removeProviderAccountCredentials("codex", "account-a"),
    ).rejects.toBe("spaces_account_remove refused");
    expect(sent("provider_account_remove")).toEqual([]);
    db.refuse.delete("spaces_account_remove");
    await credentials.removeProviderAccountCredentials("codex", "account-c");
    expect(accounts.removeProviderAccount("codex", "account-c")).toBe(true);
    expect(sent("provider_account_remove")).toEqual([
      { provider: "codex", accountId: "account-c" },
    ]);
    expect(db.accounts.map(({ id }) => id)).toContain("account-a");
    expect(db.accounts.map(({ id }) => id)).not.toContain("account-c");
  });

  it("tells the other windows, which reload and do not bring a removed login back", async () => {
    localStorage.setItem("monocode.providerAccounts.v1", BROWSER_LOGINS);
    const windowA = await boot();
    const credentials = await import("./providerAccountCredentials");
    const windowB = await boot(true);

    // The channel name is the contract between windows: one opened by it
    // hears a window's write, and its post makes a window reload.
    const raw = new BroadcastChannel("monocode-provider-accounts");
    const heard: unknown[] = [];
    raw.onmessage = ({ data }) => heard.push(data);
    windowA.saveProviderAccount({
      id: "account-new",
      provider: "codex",
      label: "New",
    });
    await vi.waitFor(() =>
      expect(windowB.providerAccountExists("codex", "account-new")).toBe(true),
    );
    expect(heard).toEqual(["spaces_account_save"]);
    db.accounts.push({
      provider: "codex",
      id: "account-cli",
      label: "CLI",
      createdAt: 1,
    });
    raw.postMessage("spaces_account_save");
    await vi.waitFor(() =>
      expect(windowB.providerAccountExists("codex", "account-cli")).toBe(true),
    );

    await credentials.removeProviderAccountCredentials("codex", "account-x");
    expect(windowA.removeProviderAccount("codex", "account-x")).toBe(true);
    await vi.waitFor(() =>
      expect(windowB.providerAccountExists("codex", "account-x")).toBe(false),
    );
    db.calls = [];
    expect(
      windowB.renameProviderAccount("codex", "account-x", "Back"),
    ).toBeNull();
    await settle();
    expect(sent("spaces_account_save")).toEqual([]);
    expect(db.accounts.some(({ id }) => id === "account-x")).toBe(false);
  });
});
