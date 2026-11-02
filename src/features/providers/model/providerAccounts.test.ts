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
type SpaceLogin = { spaceId: string; provider: string; accountId: string };

/** `monocode.db` behind the Spaces storage commands, as Rust answers them. */
const db = vi.hoisted(() => ({
  accounts: [] as Login[],
  spaceLogins: [] as SpaceLogin[],
  calls: [] as [string, unknown][],
  refuse: new Set<string>(),
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
      case "spaces_accounts_list":
        return db.accounts;
      case "spaces_account_save": {
        const { provider, id, label } = args.account as Login;
        const row = db.accounts.find((entry) => same(entry, provider, id));
        if (row) row.label = label;
        else db.accounts.push({ provider, id, label });
        return null;
      }
      case "spaces_account_remove": {
        const { provider, id } = args as { provider: string; id: string };
        if (id === "default")
          throw "The default provider account cannot be removed";
        db.accounts = db.accounts.filter((row) => !same(row, provider, id));
        return null;
      }
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

/** A fresh copy of the module, as a new boot of the app loads it. */
async function boot() {
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
  }));
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

  it("imports browser storage once, then reads and writes the database", async () => {
    localStorage.setItem("monocode.providerAccounts.v1", BROWSER_LOGINS);
    let accounts = await boot();

    expect(labels(accounts, "claude")).toEqual([
      "default Primary",
      "account-work Work",
    ]);
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-x X",
    ]);
    expect(labels(accounts, "devin")).toEqual(["default Default account"]);
    expect(db.accounts).toEqual([
      { provider: "claude", id: "default", label: "Primary" },
      { provider: "codex", id: "default", label: "Default account" },
      { provider: "devin", id: "default", label: "Default account" },
      { provider: "claude", id: "account-work", label: "Work" },
      { provider: "codex", id: "account-x", label: "X" },
    ]);
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
    expect(accounts.removeProviderAccount("codex", "account-x")).toBe(true);
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-new New",
    ]);
    await vi.waitFor(() => expect(db.calls).toHaveLength(3));
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
      ["spaces_account_remove", { provider: "codex", id: "account-x" }],
    ]);
    expect(localStorage.getItem("monocode.providerAccounts.v1")).toBe(
      BROWSER_LOGINS,
    );

    db.calls = [];
    accounts = await boot();
    expect(db.calls.map(([command]) => command)).toEqual([
      "spaces_logins_list",
      "spaces_accounts_list",
    ]);
    expect(labels(accounts, "claude")).toEqual([
      "default Primary",
      "account-work Office",
    ]);
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
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

  it("keeps browser storage when the database cannot load", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    localStorage.setItem("monocode.providerAccounts.v1", BROWSER_LOGINS);
    db.refuse.add("spaces_logins_list");
    const accounts = await boot();

    expect(console.error).toHaveBeenCalledWith("spaces_logins_list refused");
    expect(labels(accounts, "claude")).toEqual([
      "default Primary",
      "account-work Work",
    ]);
    db.calls = [];
    accounts.saveProviderAccount({
      id: "account-new",
      provider: "codex",
      label: "New",
    });
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-x X",
      "account-new New",
    ]);
    expect(localStorage.getItem("monocode.providerAccounts.v1")).toContain(
      "account-new",
    );
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(db.calls).toEqual([]);
  });

  it("logs a write the database refuses", async () => {
    const accounts = await boot();
    vi.spyOn(console, "error").mockImplementation(() => {});
    db.refuse.add("spaces_account_save");

    accounts.saveProviderAccount({
      id: "account-new",
      provider: "codex",
      label: "New",
    });
    await vi.waitFor(() =>
      expect(console.error).toHaveBeenCalledWith("spaces_account_save refused"),
    );
    expect(labels(accounts, "codex")).toEqual([
      "default Default account",
      "account-new New",
    ]);
  });
});
