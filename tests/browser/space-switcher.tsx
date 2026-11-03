import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import {
  createMono,
  updateMono,
  type MonoState,
} from "../../src/features/monos/model/mono";
import { saveMaskEmails } from "../../src/features/settings/model/displayPrefs";
import { SpaceSwitcher } from "../../src/features/spaces/ui/SpaceSwitcher";
import "../../src/styles/index.css";

// The real switcher on ADE-170 frames 1, 2 and 13's data: Space 1 shown, and
// in Space 2 one Mono working and one with 2 new replies. Space 1 predates
// colours; Space 2 is violet. Emails are made up.
const spaces = [
  { id: "space-1", name: "Space 1", color: null, createdAt: 1 },
  { id: "space-2", name: "Space 2", color: "hsl(280 55% 62%)", createdAt: 2 },
];
const logins = [
  { spaceId: "space-1", provider: "codex", accountId: "dev" },
  { spaceId: "space-2", provider: "claude", accountId: "studio" },
];
const identities: Record<string, { email: string; plan: string }> = {
  "claude:default": { email: "tyler.main@example.com", plan: "Max 20x" },
  "codex:dev": { email: "tyler.dev@example.com", plan: "ChatGPT Pro" },
  "claude:studio": { email: "studio.ts@example.com", plan: "Max 5x" },
};
let active = "space-1";
const settings = () => ({ activeSpaceId: active, routeGrantTtlMs: 21_600_000 });

mockWindows("main");
mockIPC((command, args) => {
  const input = (args ?? {}) as Record<string, never>;
  if (command === "spaces_list") return spaces;
  if (command === "spaces_logins_list") return logins;
  if (command === "spaces_settings_get") return settings();
  if (command === "spaces_settings_set") {
    active = (input.patch as { activeSpaceId: string }).activeSpaceId;
    return settings();
  }
  if (command === "provider_account_identity")
    return identities[`${input.provider}:${input.accountId}`] ?? null;
  throw `Unmocked Tauri command: ${command}`;
});

saveMaskEmails(false);
const inSpace = (spaceId: string) => {
  const { id } = createMono();
  updateMono(id, (mono) => ({ ...mono, spaceId }));
  return id;
};
inSpace("space-1");
const working = inSpace("space-2");
const replied = inSpace("space-2");
const states = new Map<string, MonoState>([[working, { status: "working" }]]);
const unseenCounts = new Map([[replied, 2]]);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <div className="text-content">
      <SpaceSwitcher
        states={states}
        unseenCounts={unseenCounts}
        onOpenSettings={() => document.body.setAttribute("data-settings", "")}
      />
    </div>
  </StrictMode>,
);
