import { describe, expect, it } from "vitest";
import { newSession, type Block } from "../../sessions/model/session";
import { sessionConversationPage } from "./sessionConversation";

function conversation(blocks: Block[]) {
  return { ...newSession("codex", "/repo"), id: "target", blocks };
}

describe("sessionConversationPage", () => {
  it.each([undefined, 3])(
    "keeps an app completion report separate from the preceding user exchange with sessionCount=%s",
    (sessionCount) => {
      const session = conversation([
        { id: "u", role: "user", text: "Explain the design" },
        { id: "a", role: "assistant", text: "It uses a queue." },
        {
          id: "notification",
          role: "user",
          text: "Hidden app instructions",
          internal: true,
          monoSessionCompletion: {
            sessionId: "worker",
            title: "API fix",
            status: "completed",
            ...(sessionCount ? { sessionCount } : {}),
          },
        },
        { id: "report", role: "assistant", text: "The API fix passed tests." },
      ]);
      const page = sessionConversationPage(session);
      expect(page.turns).toHaveLength(2);
      expect(page.turns[0].assistant?.text).toBe("It uses a queue.");
      expect(page.turns[1].user.text).toBe(
        sessionCount
          ? "MonoCode: results from 3 sessions"
          : "MonoCode: session completed: API fix",
      );
      expect(page.turns[1].assistant?.text).toBe("The API fix passed tests.");
      expect(JSON.stringify(page)).not.toContain("Hidden app instructions");
    },
  );

  it("returns the newest three exchanges without tools or reasoning, then older pages", () => {
    const blocks: Block[] = [];
    for (let i = 1; i <= 5; i += 1) {
      blocks.push({ id: `u${i}`, role: "user", text: `Question ${i}` });
      blocks.push({ id: `t${i}`, role: "tool", text: "secret tool output" });
      blocks.push({
        id: `r${i}`,
        role: "reasoning",
        text: "private reasoning",
      });
      blocks.push({ id: `a${i}`, role: "assistant", text: `Answer ${i}` });
    }
    const session = conversation(blocks);
    const recent = sessionConversationPage(session);
    expect(recent.turns.map((turn) => turn.turnId)).toEqual(["u3", "u4", "u5"]);
    expect(recent.nextBefore).toBe("u3");
    expect(JSON.stringify(recent)).not.toMatch(
      /secret tool output|private reasoning/,
    );
    const older = sessionConversationPage(session, {
      before: recent.nextBefore!,
      limit: 2,
    });
    expect(older.turns.map((turn) => turn.turnId)).toEqual(["u1", "u2"]);
    expect(older.nextBefore).toBeNull();
  });

  it("lists each turn's reasoning and tool rows in order only with work: true", () => {
    const command = `gh pr checks 20 --watch ${"-v ".repeat(100)}`;
    const session = conversation([
      { id: "early", role: "tool", text: "before any user message" },
      { id: "u1", role: "user", text: "Check CI" },
      { id: "r1", role: "reasoning", text: "R".repeat(250) },
      {
        id: "t1",
        role: "tool",
        text: command,
        tool: {
          kind: "execute",
          title: command,
          status: "completed",
          detail: "x".repeat(250),
        },
      },
      { id: "t2", role: "tool", text: "Read ci.yml", tool: { kind: "read" } },
      { id: "hidden", role: "tool", text: "app step", internal: true },
      { id: "a1", role: "assistant", text: "CI is green." },
    ]);
    const prose = sessionConversationPage(session);
    expect(prose.turns[0]).not.toHaveProperty("work");
    expect(prose.turns[0]).not.toHaveProperty("omittedWork");
    expect(JSON.stringify(prose)).not.toMatch(/RRR|gh pr|before any/);

    const page = sessionConversationPage(session, {
      work: true,
      maxChars: 200,
    });
    expect(page.turns[0].assistant?.text).toBe("CI is green.");
    expect(page.turns[0].omittedWork).toBe(0);
    expect(page.turns[0].work).toEqual([
      { kind: "reasoning", text: "R".repeat(200), truncated: true },
      {
        kind: "tool",
        tool: "execute",
        status: "completed",
        input: { text: command.slice(0, 200), truncated: true },
        result: { text: "x".repeat(200), truncated: true },
      },
      {
        kind: "tool",
        tool: "read",
        status: null,
        input: { text: "Read ci.yml", truncated: false },
        result: null,
      },
    ]);
    const strings = JSON.stringify(page.turns[0].work).match(/"[^"]*"/g) ?? [];
    expect(Math.max(...strings.map((s) => s.length - 2))).toBeLessThanOrEqual(
      200,
    );
    expect(() =>
      sessionConversationPage(session, { work: "yes" as unknown as boolean }),
    ).toThrow("work must be true or false");
  });

  it("keeps each turn's newest work rows under the CLI reply cap and counts the rest", () => {
    const ansi = "\u001b[32m✓\u001b[0m\n".repeat(1000);
    const blocks: Block[] = [];
    for (let t = 1; t <= 3; t += 1) {
      blocks.push({ id: `u${t}`, role: "user", text: "Go" });
      for (let i = 1; i <= 120; i += 1)
        blocks.push({
          id: `t${t}-${i}`,
          role: "tool",
          text: `step ${i} ${ansi}`,
          tool: { kind: "execute", detail: ansi },
        });
    }
    const page = sessionConversationPage(conversation(blocks), {
      work: true,
      maxChars: 6000,
    });
    expect(new TextEncoder().encode(JSON.stringify(page)).length).toBeLessThan(
      4_000_000,
    );
    for (const turn of page.turns) {
      const kept = turn.work ?? [];
      expect(turn.omittedWork).toBeGreaterThan(0);
      expect(kept.length + (turn.omittedWork ?? 0)).toBe(120);
      expect(kept[0]).toMatchObject({
        input: {
          text: expect.stringMatching(`^step ${turn.omittedWork! + 1} `),
        },
      });
      expect(kept.at(-1)).toMatchObject({
        input: { text: expect.stringMatching(/^step 120 /) },
      });
    }
  });

  it("redacts secrets in work rows", () => {
    const token = `ghp_${"A1b2".repeat(9)}`;
    const session = conversation([
      { id: "u", role: "user", text: "Auth?" },
      { id: "r", role: "reasoning", text: `Saw ${token}` },
      {
        id: "t",
        role: "tool",
        text: `gh auth login --with-token ${token}`,
        tool: { kind: "execute", detail: `Token: ${token}` },
      },
    ]);
    const json = JSON.stringify(
      sessionConversationPage(session, { work: true }).turns[0].work,
    );
    expect(json).not.toContain(token);
    expect(json.match(/«redacted/g)).toHaveLength(3);
  });

  it("caps individual messages and lets a caller request a larger excerpt", () => {
    const session = conversation([
      { id: "u", role: "user", text: "U".repeat(7000) },
      { id: "a", role: "assistant", text: "A".repeat(7000) },
    ]);
    expect(sessionConversationPage(session).turns[0].assistant).toEqual({
      text: "A".repeat(1200),
      truncated: true,
    });
    expect(
      sessionConversationPage(session, { maxChars: 6000 }).turns[0].user.text,
    ).toHaveLength(6000);
    expect(() => sessionConversationPage(session, { limit: 4 })).toThrow(
      "limit",
    );
    expect(() =>
      sessionConversationPage(session, { before: "missing" }),
    ).toThrow("before");
  });

  it("omits drafts and shows the latest assistant response in each exchange", () => {
    const session = conversation([
      { id: "u", role: "user", text: "/monocode list notes" },
      { id: "a1", role: "assistant", text: "Working" },
      { id: "a2", role: "assistant", text: "Done" },
      { id: "draft", role: "user", text: "unsent", draft: true },
    ]);
    const page = sessionConversationPage(session);
    expect(page.hasDraft).toBe(true);
    expect(page.turns).toEqual([
      {
        turnId: "u",
        user: { text: "list notes", truncated: false },
        assistant: { text: "Done", truncated: false },
        earlierAssistantMessages: 1,
      },
    ]);
  });
});
