import { describe, expect, it } from "vitest";
import {
  appAccess,
  consumeOperatorCommand,
  isOperatorUserTurn,
  operatorEnabledInThread,
  operatorUserPrompt,
  OPERATOR_COMMAND,
  submitAccess,
  turnAccess,
} from "./operatorCommand";
import { runtimeModeToCodexConfig } from "../../../integrations/harness/providers/codex/codexProtocol";

describe("Operator composer command", () => {
  it("exposes a local slash command", () => {
    expect(OPERATOR_COMMAND.invocation).toBe("operator");
    expect(OPERATOR_COMMAND.kind).toBe("builtin");
  });

  it("consumes only a leading standalone command", () => {
    expect(consumeOperatorCommand("/operator list my notes")).toEqual({
      text: "list my notes",
      matched: true,
    });
    expect(consumeOperatorCommand("  /OPERATOR\nstart a session")).toEqual({
      text: "start a session",
      matched: true,
    });
    expect(consumeOperatorCommand("/operator-extra list notes")).toEqual({
      text: "/operator-extra list notes",
      matched: false,
    });
    expect(consumeOperatorCommand("Explain /operator")).toEqual({
      text: "Explain /operator",
      matched: false,
    });
    for (const alias of ["/mono", "/monocode"]) {
      expect(consumeOperatorCommand(`${alias} list notes`)).toEqual({
        text: "list notes",
        matched: true,
      });
    }
  });

  it("keeps access for later turns when a submitted user turn enabled it", () => {
    expect(
      operatorEnabledInThread([
        { id: "first", role: "user", text: "list notes", monocode: true },
        { id: "reply", role: "assistant", text: "Here are your notes." },
        { id: "followup", role: "user", text: "Start two sessions" },
      ]),
    ).toBe(true);
    expect(
      operatorEnabledInThread([
        {
          id: "draft",
          role: "user",
          text: "list notes",
          draft: true,
          monocode: true,
        },
        { id: "other", role: "user", text: "Explain /operator" },
      ]),
    ).toBe(false);
    for (const alias of ["/mono", "/monocode"]) {
      const legacy = {
        id: "legacy",
        role: "user" as const,
        text: `${alias} list notes`,
      };
      expect(isOperatorUserTurn(legacy)).toBe(true);
      expect(operatorUserPrompt(legacy)).toBe("list notes");
      expect(operatorEnabledInThread([legacy])).toBe(true);
    }
  });
});

// Design 13.4. Defect: a session that sessions.start made gets no app
// access, a user's /operator in it stays "started" and keeps the narrow
// scope, a plain user session gets access, or a draft stamp counts.
it("gives started access to a thread whose first sent user block has startedBy, and operator access once /operator is sent", () => {
  const started = [
    { id: "start", role: "user" as const, text: "Fix CI", startedBy: "lead" },
    { id: "reply", role: "assistant" as const, text: "Fixed." },
  ];
  expect(appAccess(started)).toBe("started");
  expect(
    appAccess([
      ...started,
      { id: "op", role: "user", text: "list notes", monocode: true },
    ]),
  ).toBe("operator");
  expect(
    appAccess([
      { id: "user", role: "user", text: "Fix CI" },
      { id: "reply", role: "assistant", text: "Fixed." },
    ]),
  ).toBeNull();
  expect(
    appAccess([
      { id: "d", role: "user", text: "Fix CI", draft: true, startedBy: "lead" },
    ]),
  ).toBeNull();
});

// Design 13.4 and 15. Defect: app access from a sessions.start stamp also
// turns on codex sandbox network, so every Mono-started lead and worker
// reaches the network in auto-accept-edits and auto with no approval; or a
// Mono chat, a typed /operator or an active orchestration run loses it.
describe("turnAccess", () => {
  it("gives a started session app access and its runtime mode's network", () => {
    const access = turnAccess("started", false, false);
    expect(access).toEqual({ appAccess: true, controlsAgents: false });
    for (const mode of ["supervised", "auto-accept-edits", "auto"] as const)
      expect(
        runtimeModeToCodexConfig(mode, access.controlsAgents).sandboxPolicy,
        mode,
      ).not.toHaveProperty("networkAccess", true);
  });

  it("opens the network only for /operator, a Mono's chat or an orchestration run", () => {
    expect(turnAccess("operator", false, false)).toEqual({
      appAccess: true,
      controlsAgents: true,
    });
    expect(turnAccess(null, true, false)).toEqual({
      appAccess: true,
      controlsAgents: true,
    });
    expect(turnAccess(null, false, true)).toEqual({
      appAccess: false,
      controlsAgents: true,
    });
    expect(turnAccess(null, false, false)).toEqual({
      appAccess: false,
      controlsAgents: false,
    });
  });
});

// Design 13.4. Defect: the first turn of a session that sessions.start made
// gets no app access (its block is not stamped yet), or gets /operator
// access, so its codex sandbox opens the network; or a typed /operator in
// it stays started.
it("gives a started session's first submit started access, and a typed /operator operator", () => {
  expect(submitAccess([], false, "lead")).toBe("started");
  expect(submitAccess([], true, "lead")).toBe("operator");
  expect(submitAccess([], false, undefined)).toBeNull();
  expect(
    submitAccess(
      [{ id: "op", role: "user", text: "x", monocode: true }],
      false,
      undefined,
    ),
  ).toBe("operator");
});
