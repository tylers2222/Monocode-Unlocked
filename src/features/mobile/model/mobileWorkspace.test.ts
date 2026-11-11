import { describe, expect, it } from "vitest";
import { queuedAppDelivery } from "../../sessions/model/messageQueue";
import { newSession, type Block } from "../../sessions/model/session";
import { mobileMessages, mobileQuestionReply } from "./mobileWorkspace";

const prompt = {
  requestId: 1,
  questions: [
    {
      id: "colour",
      prompt: "Which colour?",
      multiSelect: false,
      allowCustom: true,
      options: [
        { id: "red", label: "Red" },
        { id: "blue", label: "Blue" },
      ],
    },
    {
      id: "size",
      prompt: "Which sizes?",
      multiSelect: true,
      allowCustom: false,
      options: [
        { id: "s", label: "Small" },
        { id: "l", label: "Large" },
      ],
    },
  ],
};

describe("mobile question replies", () => {
  it("keeps the questions and options asked, and a deliberate partial answer", () => {
    expect(
      mobileQuestionReply(
        { kind: "answered", answers: { colour: ["red"], size: ["s", "xl"], other: ["x"] } },
        prompt,
      ),
    ).toEqual({ kind: "answered", answers: { colour: ["red"], size: ["s"] } });
    expect(
      mobileQuestionReply(
        { kind: "answered", answers: { colour: ["__custom__"] }, custom: { colour: " Teal " } },
        prompt,
      ),
    ).toEqual({ kind: "answered", answers: { colour: ["__custom__"] }, custom: { colour: "Teal" } });
  });

  it("skips only when asked to, rejecting answers that leave nothing answered", () => {
    expect(mobileQuestionReply({ kind: "skipped" }, prompt)).toEqual({ kind: "skipped" });
    expect(mobileQuestionReply({ kind: "answered", answers: { colour: ["green"] } }, prompt)).toBeUndefined();
    expect(
      mobileQuestionReply(
        { kind: "answered", answers: { colour: ["__custom__"] }, custom: { colour: "x".repeat(10_001) } },
        prompt,
      ),
    ).toBeUndefined();
    expect(mobileQuestionReply({ kind: "maybe" }, prompt)).toBeUndefined();
    expect(mobileQuestionReply(null, prompt)).toBeUndefined();
  });
});

// Defect: a delivered <mono_alarm> is no longer internal, so the phone's
// message list sent its raw tag as a message from the user.
it("leaves a delivered alarm and a nudge out of the phone's messages", () => {
  const delivered = (id: string, key: string, text: string): Block => ({
    id,
    role: "user",
    text,
    ...queuedAppDelivery({
      id: key,
      appRequestId: key,
      text,
      attachments: [],
      monoReminder: true,
    }),
  });
  const session = {
    ...newSession("claude", "/repo"),
    blocks: [
      delivered(
        "alarm",
        "alarm-al_7Q2-1",
        '<mono_alarm alarm="al_7Q2" level="1" cause="dialog">\nS1 waits on a dialog.\n</mono_alarm>',
      ),
      delivered("nudge", "nudge-al_7Q2-2", "Still on the dialog?"),
      { id: "a", role: "assistant", text: "Nudging it." } as Block,
    ],
  };
  expect(mobileMessages(session)).toEqual([
    { id: "a", role: "assistant", text: "Nudging it." },
  ]);
});
