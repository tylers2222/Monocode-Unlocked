import { describe, expect, it } from "vitest";
import { newSession, type Block, type Session } from "./session";
import { agentReplyTo, turnReplyBegan } from "./turnReply";

// ADE-89: a busy chat is saved when the agent writes its first block of a
// turn, so a Mono chat save shows the agent took the notice that opened it.

const prompt: Block = {
  id: "notice",
  role: "user",
  text: "Notice",
  startedAt: 2,
};
const cut: Block = { id: "cut", role: "system", text: "Turn interrupted." };
const reply = (text: string): Block => ({
  id: "reply",
  role: "assistant",
  text,
});
const chat = (blocks: Block[], busy = true): Session => ({
  ...newSession("fx", "/code/project"),
  busy,
  blocks,
});

describe("the agent's reply to a prompt", () => {
  it("is the first agent block after the prompt, not one before it or the app's", () => {
    const earlier: Block = { id: "earlier", role: "assistant", text: "Done." };
    const tool: Block = { id: "tool", role: "tool", text: "ls" };
    expect(agentReplyTo([earlier, prompt, cut], prompt)).toBeUndefined();
    expect(agentReplyTo([earlier, prompt, cut, tool, reply("x")], prompt)).toBe(
      tool,
    );
  });

  it("begins once per turn, when a busy chat gets its first agent block", () => {
    const waiting = chat([prompt, cut]);
    const replied = chat([prompt, reply("Rev")]);
    expect(turnReplyBegan(chat([prompt]), waiting)).toBe(false);
    expect(turnReplyBegan(waiting, replied)).toBe(true);
    expect(turnReplyBegan(undefined, replied)).toBe(true);
    expect(turnReplyBegan(replied, chat([prompt, reply("Reviewed.")]))).toBe(
      false,
    );
    expect(turnReplyBegan(waiting, chat([prompt, reply("Rev")], false))).toBe(
      false,
    );

    const next: Block = {
      id: "next",
      role: "user",
      text: "Next",
      startedAt: 3,
    };
    const nextReply: Block = {
      id: "next-reply",
      role: "assistant",
      text: "On it.",
    };
    const opened = chat([prompt, reply("Reviewed."), next]);
    expect(turnReplyBegan(replied, opened)).toBe(false);
    expect(turnReplyBegan(opened, chat([...opened.blocks, nextReply]))).toBe(
      true,
    );
  });
});
