import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  PLAN_APPROVED,
  freshRequestId,
  holdAct,
  provisionerAct,
  tylerAct,
  type AlarmActHost,
} from "./alarmActs";
import { parseAlarmsSnapshot, type EpisodeWire } from "./alarmsWire";
import type { AlarmActEvent, AlarmAgent } from "./alarmViews";
import { snapshotWire } from "../test/alarmsFixtures";

const episodes = new Map(
  parseAlarmsSnapshot(snapshotWire()).episodes.map((entry) => [
    entry.id,
    entry,
  ]),
);
const episode = (id: string): EpisodeWire => episodes.get(id)!;
const withDialog = (
  id: string,
  dialog: Partial<NonNullable<EpisodeWire["dialog"]>>,
): EpisodeWire => {
  const found = episode(id);
  if (found.cause !== "dialog") throw new Error(`${id} has no dialog`);
  return { ...found, dialog: { ...found.dialog, ...dialog } };
};

const WORKER: AlarmAgent = {
  name: "PR S1 builder",
  actor: "session:9f3c",
  role: "worker",
};
const REVIEWER: AlarmAgent = {
  name: "PR S2 reviewer",
  actor: "session:a71e",
  role: "worker",
};
const TYLER_AGENT: AlarmAgent = {
  name: "U2 builder",
  actor: "session:77aa",
  role: "session",
};

type Calls = { invoke: unknown[][]; order: string[] };
let calls: Calls;
let host: AlarmActHost;
let result: unknown;
let ids = 0;

beforeEach(() => {
  ids = 0;
  calls = { invoke: [], order: [] };
  result = { ok: true, result: {} };
  host = {
    invoke: vi.fn(async (command: string, args: Record<string, unknown>) => {
      calls.invoke.push([command, args]);
      calls.order.push(command);
      return result;
    }),
    openAgent: vi.fn((actor: string) => calls.order.push(`open ${actor}`)),
    answerDialog: vi.fn((sessionId: string, input: Record<string, unknown>) =>
      calls.order.push(`answer ${sessionId} ${JSON.stringify(input)}`),
    ),
    sendAsUser: vi.fn((sessionId: string, text: string) =>
      calls.order.push(`send ${sessionId} ${text}`),
    ),
    refused: vi.fn(),
    requestId: () => `ui-req${++ids}`,
  };
});

const card = (
  alarmId: string,
  target: AlarmAgent,
  body: Record<string, unknown>,
) =>
  ({ kind: "alarm", alarmId, target, ...body }) as AlarmActEvent & {
    kind: "alarm";
  };

const controlAct = (action: string, input: Record<string, unknown>) => [
  "control_alarm_act",
  { sessionId: "51aa", action, input, requestId: "ui-req1" },
];

describe("provisioner card acts (control_alarm_act as the chat's owner)", () => {
  it("Read logs the read, then opens the stalled session", async () => {
    // Defect: Read acts for the wrong chat, or leaves the stalled agent unopened.
    await provisionerAct(
      host,
      "51aa",
      episode("al_7Q2"),
      card("al_7Q2", WORKER, { act: "read" }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.read", { alarm: "al_7Q2" }),
    ]);
    expect(calls.order).toEqual(["control_alarm_act", "open session:9f3c"]);
  });

  it("Answer Allow on a permission sends allow", async () => {
    // Defect: Allow reaches Rust as a choice, which a permission refuses.
    await provisionerAct(
      host,
      "51aa",
      episode("al_9KD"),
      card("al_9KD", REVIEWER, {
        act: "answer",
        answer: { kind: "option", option: "Allow" },
      }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.answer", { alarm: "al_9KD", allow: true }),
    ]);
  });

  it("Answer Deny on a permission sends deny", async () => {
    // Defect: Deny is sent as allow:false, which Rust counts as no answer.
    await provisionerAct(
      host,
      "51aa",
      episode("al_9KD"),
      card("al_9KD", REVIEWER, {
        act: "answer",
        answer: { kind: "option", option: "Deny" },
      }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.answer", { alarm: "al_9KD", deny: true }),
    ]);
  });

  it("Answer on a question sends the picked choice", async () => {
    // Defect: a question's choice is sent as allow.
    await provisionerAct(
      host,
      "51aa",
      episode("al_TyL"),
      card("al_TyL", TYLER_AGENT, {
        act: "answer",
        answer: { kind: "option", option: "main" },
      }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.answer", { alarm: "al_TyL", choice: "main" }),
    ]);
  });

  it("Answer typed into the text box sends the text", async () => {
    // Defect: a typed answer is dropped for a question with no choices.
    await provisionerAct(
      host,
      "51aa",
      episode("al_TyL"),
      card("al_TyL", TYLER_AGENT, {
        act: "answer",
        answer: { kind: "text", text: "base on alarms/u2-ui-wiring" },
      }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.answer", {
        alarm: "al_TyL",
        text: "base on alarms/u2-ui-wiring",
      }),
    ]);
  });

  it("Answer Approve on a plan sends allow", async () => {
    // Defect: Approve maps to no field, so a plan cannot be approved from the card.
    await provisionerAct(
      host,
      "51aa",
      withDialog("al_9KD", { kind: "plan", requestId: null }),
      card("al_9KD", REVIEWER, {
        act: "answer",
        answer: { kind: "option", option: "Approve" },
      }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.answer", { alarm: "al_9KD", allow: true }),
    ]);
  });

  it("Steer sends its one line", async () => {
    // Defect: the steer text is lost or sent under another field.
    await provisionerAct(
      host,
      "51aa",
      episode("al_7Q2"),
      card("al_7Q2", WORKER, { act: "steer", text: "Rebase and re-run" }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.steer", {
        alarm: "al_7Q2",
        text: "Rebase and re-run",
      }),
    ]);
  });

  it("Nudge sends only the alarm", async () => {
    // Defect: nudge carries fields Rust refuses.
    await provisionerAct(
      host,
      "51aa",
      episode("al_7Q2"),
      card("al_7Q2", WORKER, { act: "nudge" }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.nudge", { alarm: "al_7Q2" }),
    ]);
  });

  it("Dismiss sends its reason as reason", async () => {
    // Defect: the dismiss reason goes as text, which alarms.dismiss refuses.
    await provisionerAct(
      host,
      "51aa",
      episode("al_7Q2"),
      card("al_7Q2", WORKER, { act: "dismiss", text: "waiting on review" }),
    );
    expect(calls.invoke).toEqual([
      controlAct("alarms.dismiss", {
        alarm: "al_7Q2",
        reason: "waiting on review",
      }),
    ]);
  });

  it("says when Rust refuses, and opens nothing for a refused act", async () => {
    // Defect: a refusal reads as done, with nothing said.
    result = { ok: false, error: { code: "not_your_alarm" } };
    await provisionerAct(
      host,
      "51aa",
      episode("al_7Q2"),
      card("al_7Q2", WORKER, { act: "nudge" }),
    );
    expect(host.refused).toHaveBeenCalledWith("control_alarm_act", result);
  });
});

const tylerLog = (
  how: string,
  detail: string | null,
  requestId = "ui-req1",
) => ["alarms_tyler_act", { alarm: "al_TyL", how, detail, requestId }];

describe("Tyler landing acts (effect here, logged with alarms_tyler_act)", () => {
  it("Answer answers the dialog Rust named, then logs it", async () => {
    // Defect: the log goes first, or the answer is not bound to the alarm's dialog.
    await tylerAct(
      host,
      episode("al_TyL"),
      card("al_TyL", TYLER_AGENT, {
        act: "answer",
        answer: { kind: "option", option: "main" },
      }),
    );
    expect(calls.order).toEqual([
      'answer 77aa {"kind":"question","dialogRequestId":"q-77aa-4","choice":"main"}',
      "alarms_tyler_act",
    ]);
    expect(calls.invoke).toEqual([tylerLog("answer", "main")]);
  });

  it("Answer on a permission answers allow, and logs nothing when the dialog is gone", async () => {
    // Defect: a gone dialog is still logged as answered.
    const permission = { ...episode("al_9KD"), id: "al_TyL" };
    await tylerAct(
      host,
      permission,
      card("al_TyL", REVIEWER, {
        act: "answer",
        answer: { kind: "option", option: "Allow" },
      }),
    );
    expect(calls.order[0]).toBe(
      'answer a71e {"kind":"permission","dialogRequestId":"toolu_01RmRf","decision":"allow"}',
    );
    expect(calls.invoke).toEqual([tylerLog("answer", "allow")]);

    calls.invoke = [];
    vi.mocked(host.answerDialog).mockImplementation(() => {
      throw new Error("dialog_gone");
    });
    await tylerAct(
      host,
      permission,
      card("al_TyL", REVIEWER, {
        act: "answer",
        answer: { kind: "option", option: "Deny" },
      }),
    );
    expect(calls.invoke).toEqual([]);
    expect(host.refused).toHaveBeenCalledWith(
      "alarms.answer",
      expect.any(Error),
    );
  });

  it("Approve on a plan logs allow, then sends Rust's approval line as the user", async () => {
    // Defect: a plan, which has no harness dialog, is answered through one, or the approval reaches the agent for an alarm Rust refused.
    const plan = {
      ...withDialog("al_9KD", { kind: "plan", requestId: null }),
      id: "al_TyL",
    };
    await tylerAct(
      host,
      plan,
      card("al_TyL", REVIEWER, {
        act: "answer",
        answer: { kind: "option", option: "Approve" },
      }),
    );
    expect(calls.order).toEqual([
      "alarms_tyler_act",
      `send a71e ${PLAN_APPROVED}`,
    ]);
    expect(calls.invoke).toEqual([tylerLog("answer", "allow")]);

    calls.order = [];
    result = { ok: false, error: { code: "alarm_closed" } };
    await tylerAct(
      host,
      plan,
      card("al_TyL", REVIEWER, {
        act: "answer",
        answer: { kind: "option", option: "Approve" },
      }),
    );
    expect(calls.order).toEqual(["alarms_tyler_act"]);
    expect(host.refused).toHaveBeenCalledWith("alarms_tyler_act", result);
  });

  it("Steer logs, then sends the text into the stalled session", async () => {
    // Defect: the steer is sent before Rust accepts it, or to another session.
    await tylerAct(
      host,
      episode("al_TyL"),
      card("al_TyL", TYLER_AGENT, { act: "steer", text: "Use main" }),
    );
    expect(calls.invoke).toEqual([tylerLog("steer", "Use main")]);
    expect(calls.order).toEqual(["alarms_tyler_act", "send 77aa Use main"]);
  });

  it("Nudge logs, then sends the prompt Rust wrote", async () => {
    // Defect: the nudge sends nothing, or a prompt of its own instead of Rust's.
    result = {
      ok: true,
      result: { alarm: "al_TyL", prompt: "You have open work: answer it." },
    };
    await tylerAct(
      host,
      episode("al_TyL"),
      card("al_TyL", TYLER_AGENT, { act: "nudge" }),
    );
    expect(calls.invoke).toEqual([tylerLog("nudge", null)]);
    expect(calls.order).toEqual([
      "alarms_tyler_act",
      "send 77aa You have open work: answer it.",
    ]);
  });

  it("Dismiss logs its reason and sends nothing", async () => {
    // Defect: dismiss sends a message into the session.
    await tylerAct(
      host,
      episode("al_TyL"),
      card("al_TyL", TYLER_AGENT, { act: "dismiss", text: "I'm on it" }),
    );
    expect(calls.invoke).toEqual([tylerLog("dismiss", "I'm on it")]);
    expect(host.sendAsUser).not.toHaveBeenCalled();
  });

  it("sends nothing when Rust refuses the log", async () => {
    // Defect: a refused steer (not Tyler's alarm) still reaches the agent.
    result = { ok: false, error: { code: "not_your_alarm" } };
    await tylerAct(
      host,
      episode("al_TyL"),
      card("al_TyL", TYLER_AGENT, { act: "steer", text: "Use main" }),
    );
    expect(host.sendAsUser).not.toHaveBeenCalled();
    expect(host.refused).toHaveBeenCalledWith("alarms_tyler_act", result);
  });
});

describe("hold card acts (holds_tyler_decide)", () => {
  const hold = (act: "allow" | "deny") =>
    ({
      kind: "hold",
      holdId: "sh_K1",
      toolUseId: "toolu_sleep1",
      act,
      target: WORKER,
    }) as const;

  it("Allow decides the hold by its id", async () => {
    // Defect: the hold is answered by its tool call id, which Rust does not know.
    await holdAct(host, hold("allow"));
    expect(calls.invoke).toEqual([
      [
        "holds_tyler_decide",
        { hold: "sh_K1", allow: true, reason: null, requestId: "ui-req1" },
      ],
    ]);
  });

  it("Deny decides it with allow false", async () => {
    // Defect: Deny is sent as Allow.
    await holdAct(host, hold("deny"));
    expect(calls.invoke).toEqual([
      [
        "holds_tyler_decide",
        { hold: "sh_K1", allow: false, reason: null, requestId: "ui-req1" },
      ],
    ]);
  });
});

describe("request ids", () => {
  it("are fresh per click and keep to the persisted id characters", () => {
    // Defect: two clicks share an id (Rust drops the second as a repeat), or an id fails sessionStore's rule.
    const first = freshRequestId();
    expect(first).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(freshRequestId()).not.toBe(first);
  });
});
