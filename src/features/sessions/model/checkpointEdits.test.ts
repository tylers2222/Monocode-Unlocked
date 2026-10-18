import { expect, it } from "vitest";
import { applyHarnessEvent } from "../../../integrations/harness/core/apply";
import {
  newSession,
  sessionAgentCwd,
  sessionWorkCwd,
  type Block,
} from "./session";
import { checkpointPaths, sessionEditPaths } from "./checkpointEdits";

it("recovers successful edits from before a continuation, excluding reads and failed edits", () => {
  const blocks: Block[] = [
    { id: "older-user", role: "user", text: "Edit another file" },
    {
      id: "older-edit",
      role: "tool",
      text: "Edit",
      tool: {
        kind: "edit",
        status: "completed",
        preview: { kind: "write", path: "older.ts" },
      },
    },
    { id: "user", role: "user", text: "Edit README" },
    {
      id: "read",
      role: "tool",
      text: "Read",
      tool: {
        kind: "read",
        status: "completed",
        preview: { kind: "read", path: "foreign.ts" },
      },
    },
    {
      id: "failed",
      role: "tool",
      text: "Edit",
      tool: {
        kind: "edit",
        status: "failed",
        preview: { kind: "write", path: "failed.ts" },
      },
    },
    {
      id: "running",
      role: "tool",
      text: "Edit",
      tool: {
        kind: "edit",
        status: "in_progress",
        preview: { kind: "write", path: "running.ts" },
      },
    },
    {
      id: "edit",
      role: "tool",
      text: "Edit",
      tool: {
        kind: "edit",
        status: "completed",
        preview: { kind: "write", path: "README.md" },
      },
    },
    { id: "continue", role: "user", text: "Continue from where you left off." },
    { id: "done", role: "assistant", text: "The edit is complete." },
  ];
  expect(sessionEditPaths(blocks)).toEqual(["README.md"]);
  expect(
    sessionEditPaths([
      ...blocks,
      { id: "next", role: "user", text: "Investigate without editing" },
    ]),
  ).toEqual([]);
});

it("retains every path in a completed multi-file edit after serializing the transcript", () => {
  let session = applyHarnessEvent(newSession("codex", "/repo"), {
    type: "tool.started",
    callId: "edit",
    kind: "edit",
    title: "Edit README.md",
    paths: ["README.md", "docs/setup.md"],
    preview: { kind: "write", path: "README.md" },
  });
  session = applyHarnessEvent(session, {
    type: "tool.updated",
    callId: "edit",
    status: "completed",
  });
  expect(sessionEditPaths(JSON.parse(JSON.stringify(session.blocks)))).toEqual([
    "README.md",
    "docs/setup.md",
  ]);
});

it("keeps a Giga session's relative edits in its folder for the checkpoint at the working copy", () => {
  const session = { ...newSession("codex", "/repo"), workFolder: "/repo/gigas/alpha" };
  expect(
    checkpointPaths(
      ["plan.md", "/repo/gigas/alpha/notes.md", "plan.md"],
      sessionAgentCwd(session),
      sessionWorkCwd(session),
    ),
  ).toEqual(["/repo/gigas/alpha/plan.md", "/repo/gigas/alpha/notes.md"]);
  const plain = newSession("codex", "/repo");
  expect(
    checkpointPaths(["src/a.ts"], sessionAgentCwd(plain), sessionWorkCwd(plain)),
  ).toEqual(["src/a.ts"]);
});
