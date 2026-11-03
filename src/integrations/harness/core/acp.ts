import type { HarnessEvent } from "./types";
import type { TextPromptInput } from "./registry";
import { abortTextPromptRace } from "./abortTextPrompt";
import {
  JsonRpcClient,
  type JsonRpcHandlers,
  type JsonRpcId,
  type JsonRpcMessage,
} from "./jsonRpc";

export type { JsonRpcMessage };

/**
 * A warm ACP text child (cursor, grok): its client, the folder it was asked
 * for and the one Rust placed it in, the one it tells its agent (ADE-189),
 * and the answer it is collecting.
 */
export type AcpTextLive = {
  acp: AcpClient;
  cwd: string;
  placed: string;
  model: string;
  settingsKey: string;
  acpSessionId: string;
  collecting: boolean;
  output: string;
  closed: boolean;
  onEvent?: (event: HarnessEvent) => void;
};

/**
 * ADE-189: a warm text child serves a prompt only when it was placed for the
 * same folder and the same session, so a prompt never runs in another Space.
 */
export function placedFor(
  live: { cwd: string; owner?: string },
  cwd: string,
  owner?: string,
): boolean {
  return live.cwd === cwd && live.owner === owner;
}

/**
 * ADE-192: the turn queue every text adapter shares. Its one child serves one
 * call at a time, in call order. A warm-up starts the child before the first
 * prompt; a warm-up of no folder does nothing, and a warm-up never rejects.
 */
export function textTurns(
  warm: (cwd: string) => Promise<unknown>,
  prompt: (input: TextPromptInput) => Promise<string>,
): {
  warmup: (cwd: string) => Promise<void>;
  run: (input: TextPromptInput) => Promise<string>;
} {
  let turns: Promise<void> = Promise.resolve();
  function queue<T>(work: () => Promise<T>): Promise<T> {
    const run = turns.then(work);
    turns = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }
  return {
    warmup(cwd) {
      if (!cwd || cwd === "~") return Promise.resolve();
      return queue(async () => {
        await warm(cwd);
      }).catch(() => undefined);
    },
    run: (input) => queue(() => prompt(input)),
  };
}

/**
 * ADE-192: one prompt on a live ACP text child (Cursor and Grok text). It
 * collects the agent's message text until `session/prompt` answers, cancels
 * the session on an error or an abort, and then drops the child, once, in
 * `finally`, whether the child closed or not.
 */
export async function promptAcpText(
  session: AcpTextLive,
  input: TextPromptInput,
  timeoutMs: number,
  dropLive: () => Promise<void>,
): Promise<string> {
  input.signal?.throwIfAborted();
  session.output = "";
  session.collecting = true;
  session.onEvent = input.onEvent;
  const cancel = () =>
    session.acp.notify("session/cancel", { sessionId: session.acpSessionId });
  const abort = abortTextPromptRace(input.signal, cancel);
  try {
    const answer = session.acp.request(
      "session/prompt",
      {
        sessionId: session.acpSessionId,
        prompt: [{ type: "text", text: input.prompt }],
      },
      input.timeoutMs ?? timeoutMs,
    );
    // With no signal there is no abort to race: the answer races itself.
    await Promise.race([answer, abort.promise ?? answer]);
    return session.output;
  } catch (error) {
    await cancel().catch(() => undefined);
    throw error;
  } finally {
    abort.detach();
    session.onEvent = undefined;
    await dropLive();
  }
}

export type AcpHandlers = {
  onNotification?: (method: string, params: unknown) => void;
  onRequest?: (
    id: number,
    method: string,
    params: unknown,
  ) => void | Promise<void>;
};

/** ACP JSON-RPC client — thin wrapper preserving the Cursor numeric-id API. */
export class AcpClient {
  private readonly rpc: JsonRpcClient;

  constructor(
    sessionId: string,
    private readonly handlers: AcpHandlers,
  ) {
    const rpcHandlers: JsonRpcHandlers = {
      onNotification: (method, params) =>
        this.handlers.onNotification?.(method, params),
      onRequest: (id, method, params) => {
        const numeric =
          typeof id === "number" ? id : Number(id);
        void this.handlers.onRequest?.(numeric, method, params);
      },
    };
    this.rpc = new JsonRpcClient(sessionId, rpcHandlers, {
      includeJsonrpc: true,
      label: "acp",
    });
  }

  pushLine(line: string) {
    this.rpc.pushLine(line);
  }

  close(error?: Error) {
    this.rpc.close(error);
  }

  rejectPending(error?: Error) {
    this.rpc.rejectPending(error);
  }

  request<T>(method: string, params?: unknown, timeoutMs = 0): Promise<T> {
    return this.rpc.request<T>(method, params, timeoutMs);
  }

  notify(method: string, params?: unknown): Promise<void> {
    return this.rpc.notify(method, params);
  }

  respond(id: number, result: unknown): Promise<void> {
    return this.rpc.respond(id as JsonRpcId, result);
  }

  respondError(
    id: number,
    error: { code: number; message: string; data?: unknown },
  ): Promise<void> {
    return this.rpc.respondError(id as JsonRpcId, error);
  }
}
