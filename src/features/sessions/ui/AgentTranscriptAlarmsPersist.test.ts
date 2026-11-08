import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { newSession, type Block } from "../model/session";
import type { AlarmCardView } from "../../alarms/model/alarmViews";
import { createViewStore } from "../../alarms/model/viewStore";
import type { SessionRecord } from "../data/sessionStore";
import { AgentTranscript } from "./AgentTranscript";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

const { readSession, sanitizeSessionForPersist } =
  await import("../data/sessionStore");

const EPISODE = "al_7f3a9c2e1b04";
/** Every alarms key that reaches the front end (design sections 2 and 6). */
const KEYS = [`alarm-${EPISODE}-1`, `nudge-${EPISODE}-2`, "hold-sh_4be1"];

const CARD: AlarmCardView = {
  id: EPISODE,
  cause: "idle_open_work",
  stalled: { name: "PR S1 builder", actor: "session:9f3c", role: "worker" },
  stalledSince: "14:05",
  minutes: 17,
  openWork: [],
  sends: [],
  level: 1,
  audience: "provisioner",
  next: { kind: "provisioner", name: "Fork Orchestrator", minutes: 15 },
};

/** Saves the session the way the app does, then reads it back. */
async function saveAndLoad(blocks: Block[]) {
  const session = newSession("claude", "/repo");
  session.blocks = blocks;
  session.queuedMessages = KEYS.map((id) => ({ id, text: `queued ${id}` }));
  const saved = sanitizeSessionForPersist(session);
  invoke.mockImplementation((cmd: string) =>
    Promise.resolve(
      cmd === "session_get" ? (saved as unknown as SessionRecord) : null,
    ),
  );
  return readSession(session.id);
}

describe("alarm request ids through a save and a load", () => {
  it("keeps every alarms key, so a delivered card still binds after reload", async () => {
    // Defect: a ':' key is dropped on save, and the card comes back as a raw <mono_alarm> tag.
    const blocks: Block[] = KEYS.map((key) => ({
      id: `block-${key}`,
      role: "user",
      text: `<mono_alarm alarm="${EPISODE}" level="1" cause="idle_open_work">\nPR S1 builder is stalled.\n</mono_alarm>`,
      appRequestId: key,
    }));
    const loaded = await saveAndLoad(blocks);

    expect(loaded?.blocks.map((block) => block.appRequestId)).toEqual(KEYS);
    expect(loaded?.queuedMessages?.map((message) => message.id)).toEqual(KEYS);
    const shown = renderToStaticMarkup(
      createElement(AgentTranscript, {
        blocks: loaded?.blocks ?? [],
        alarms: {
          cards: createViewStore(new Map([[CARD.id, CARD]])),
          holds: createViewStore(new Map()),
        },
      }),
    );
    expect(shown.match(/data-alarm="/g)).toHaveLength(1);
    expect(shown).toContain(`data-alarm="${EPISODE}"`);
  });
});
