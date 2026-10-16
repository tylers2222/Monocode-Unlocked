/**
 * Mono messaging tools for one session (design 4). Rust owns the schemas
 * (`app_tool_schemas`); MCP harnesses start the app executable as
 * `app-mcp`, which reads the session's token from the environment the
 * harness inherited, so no registration ever carries the token.
 */
export type MonoTool = {
  name: string;
  /** The app action the tool calls, e.g. `messages.send`. */
  action: string;
  description: string;
  inputSchema: Record<string, unknown>;
};

export type MonoTools = {
  /** The MonoCode executable; `app-mcp` is its MCP server mode. */
  exe: string;
  tools: readonly MonoTool[];
  /** Runs one host tool call (omp) for this session and returns the app's answer. */
  call: (action: string, input: unknown, requestId: string) => Promise<unknown>;
};

export const MONO_MCP_SERVER = "monocode";
const MCP_ARGS = ["app-mcp"];

/** `--mcp-config` for an interactive Claude Code session. */
export function claudeMcpConfig(tools: MonoTools): string {
  return JSON.stringify({
    mcpServers: {
      [MONO_MCP_SERVER]: { type: "stdio", command: tools.exe, args: MCP_ARGS },
    },
  });
}

/** ACP `mcpServers` for `session/new`, `session/load` and `session/resume`. */
export function acpMcpServers(tools: MonoTools | undefined): unknown[] {
  if (!tools) return [];
  return [{ name: MONO_MCP_SERVER, command: tools.exe, args: MCP_ARGS, env: [] }];
}

/**
 * codex clears an MCP child's environment and copies back only `env_vars`,
 * so the server names the two variables it needs, never their values.
 */
export function codexMcpServer(tools: MonoTools): Record<string, unknown> {
  return {
    command: tools.exe,
    args: MCP_ARGS,
    env_vars: ["MONOCODE_APP_ENDPOINT", "MONOCODE_APP_TOKEN"],
  };
}

/**
 * omp `set_host_tools` definitions. `essential` keeps them in the model's
 * tool list; omp hides a discoverable tool, and plan mode's `--tools` list
 * leaves essential host tools in place.
 */
export function ompHostTools(tools: MonoTools): Record<string, unknown>[] {
  return tools.tools.map((tool) => ({
    name: tool.name,
    label: tool.name,
    description: tool.description,
    parameters: tool.inputSchema,
    loadMode: "essential",
  }));
}

/** One omp `host_tool_call` frame. */
export type HostToolCall = { id: string; toolName: string; input: unknown };

export function parseHostToolCall(
  rec: Record<string, unknown>,
): HostToolCall | undefined {
  if (rec.type !== "host_tool_call") return undefined;
  if (typeof rec.id !== "string" || typeof rec.toolName !== "string")
    return undefined;
  return { id: rec.id, toolName: rec.toolName, input: rec.arguments ?? {} };
}

/**
 * The `host_tool_result` frame for one call. Each call gets a fresh request
 * id: omp's frame ids restart with every process, and a reused id would
 * return an earlier call's stored outcome (design 3.4, 3.5).
 */
export async function answerHostToolCall(
  tools: MonoTools | undefined,
  call: HostToolCall,
): Promise<Record<string, unknown>> {
  const names = tools?.tools.map((tool) => tool.name) ?? [];
  const tool = tools?.tools.find((candidate) => candidate.name === call.toolName);
  const value =
    tools && tool
      ? await tools
          .call(tool.action, call.input, crypto.randomUUID())
          .catch((error: unknown) => ({
            ok: false,
            error: error instanceof Error ? error.message : String(error),
          }))
      : {
          ok: false,
          code: "invalid_input",
          hint: `Unknown tool ${call.toolName}. This session has: ${names.join(", ") || "none"}.`,
        };
  const ok =
    value !== null &&
    typeof value === "object" &&
    "ok" in value &&
    value.ok === true;
  return {
    type: "host_tool_result",
    id: call.id,
    result: { content: [{ type: "text", text: JSON.stringify(value) }] },
    ...(ok ? {} : { isError: true }),
  };
}
