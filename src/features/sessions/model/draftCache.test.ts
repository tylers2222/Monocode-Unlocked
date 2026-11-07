import { describe, expect, it } from "vitest";
import {
  clearComposerDraft,
  getComposerDraft,
  getComposerMcpTags,
  getComposerStash,
  setComposerDraft,
  setComposerMcpTags,
  setComposerStash,
  updateComposerStash,
  type ComposerStash,
} from "./draftCache";
import { newMcpTag } from "./mcpPicker";

describe("draftCache", () => {
  it("returns undefined for a session that never had a draft", () => {
    expect(getComposerDraft("never-seen")).toBeUndefined();
  });

  it("gives back the last text set for a session, across separate reads", () => {
    // This is the actual bug: SessionPane used to keep the composer's text
    // in a component-local ref, so closing a session's pane (unmounting
    // SessionPane) and reopening it - a fresh component instance calling
    // getComposerDraft again - lost whatever was typed. A module-level
    // cache has to answer the same text back on a later, independent read.
    setComposerDraft("s1", "half-typed message");

    expect(getComposerDraft("s1")).toBe("half-typed message");
  });

  it("keeps drafts for different sessions apart", () => {
    setComposerDraft("s2", "draft for session two");
    setComposerDraft("s3", "draft for session three");

    expect(getComposerDraft("s2")).toBe("draft for session two");
    expect(getComposerDraft("s3")).toBe("draft for session three");
  });

  it("treats setting an empty string as clearing the draft", () => {
    setComposerDraft("s4", "something");
    setComposerDraft("s4", "");

    expect(getComposerDraft("s4")).toBeUndefined();
  });

  it("clearComposerDraft removes a stored draft", () => {
    setComposerDraft("s5", "will be cleared");
    clearComposerDraft("s5");

    expect(getComposerDraft("s5")).toBeUndefined();
  });

  it("keeps MCP tag metadata with its session draft", () => {
    const tag = newMcpTag(
      {
        provider: "codex",
        name: "docs",
        scope: "user",
        configPath: "/config.toml",
        transport: "stdio",
      },
      [],
    );
    setComposerDraft("mcp-one", `Ask ${tag.token}`);
    setComposerMcpTags("mcp-one", [tag]);
    setComposerDraft("mcp-two", "Another draft");
    expect(getComposerMcpTags("mcp-one")).toEqual([tag]);
    expect(getComposerMcpTags("mcp-two")).toEqual([]);
    setComposerDraft("mcp-one", "");
    expect(getComposerMcpTags("mcp-one")).toEqual([]);
  });

  describe("Composer stash", () => {
    const empty: ComposerStash = {
      attachments: [],
      borrowedAttachmentIds: [],
      plan: false,
      operator: false,
      orchestration: false,
      draft: false,
    };
    const file = {
      id: "f1",
      name: "a.png",
      mimeType: "image/png",
      kind: "image" as const,
      size: 1,
    };

    it.each([
      ["a file", { attachments: [file] }],
      ["Plan", { plan: true }],
      ["Operator", { operator: true }],
      ["Orchestrator", { orchestration: true }],
      ["Save as draft", { draft: true }],
    ])("keeps a stash holding %s", (_name, unsent) => {
      const stash = { ...empty, ...unsent };
      setComposerStash("stash-kept", stash);
      expect(getComposerStash("stash-kept")).toBe(stash);
      clearComposerDraft("stash-kept");
      expect(getComposerStash("stash-kept")).toBeUndefined();
    });

    it("merges each change into the session's stash", () => {
      updateComposerStash("stash-merge", { plan: true });
      updateComposerStash("stash-merge", { attachments: [file] });
      expect(getComposerStash("stash-merge")).toEqual({
        ...empty,
        plan: true,
        attachments: [file],
      });
      updateComposerStash("stash-merge", { plan: false, attachments: [] });
      expect(getComposerStash("stash-merge")).toBeUndefined();
    });

    it("drops a stash with nothing unsent, and on null", () => {
      setComposerStash("stash-drop", { ...empty, plan: true });
      setComposerStash("stash-drop", {
        ...empty,
        borrowedAttachmentIds: ["only-borrowed-id"],
      });
      expect(getComposerStash("stash-drop")).toBeUndefined();
      setComposerStash("stash-drop", { ...empty, draft: true });
      setComposerStash("stash-drop", null);
      expect(getComposerStash("stash-drop")).toBeUndefined();
    });
  });
});
