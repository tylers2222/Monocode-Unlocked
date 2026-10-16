import { invoke, isTauri } from "@tauri-apps/api/core";
import type { HarnessId } from "../../sessions/model/session";
import type {
  MonoTool,
  MonoTools,
} from "../../../integrations/harness/core/monoTools";
import { callerMono } from "./monoCaller";

/**
 * How each harness gets the Mono messaging tools (design 4, v1 column).
 * `mcp`: MonoCode registers `app-mcp`. `host`: omp RPC host tools. `cli`:
 * only the app CLI, because the harness has no per-session path, or its
 * smoke has not run (not installed here), or it scrubs the token (hermes).
 */
export const MONO_TOOL_TRANSPORT: Record<HarnessId, "mcp" | "host" | "cli"> = {
  claude: "mcp",
  codex: "mcp",
  omp: "host",
  cursor: "cli",
  devin: "cli",
  grok: "cli",
  fx: "cli",
  antigravity: "cli",
  hermes: "cli",
  pi: "cli",
  opencode: "cli",
};

let schemas: Promise<{ exe: string; tools: MonoTool[] }> | undefined;

/**
 * The tools a session's harness registers at spawn: only for a Mono chat or
 * habit run (the same rule `resolveCaller` applies to each call), only on a
 * harness that carries them, and only in the desktop app.
 */
export async function monoToolsFor(
  harness: HarnessId,
  sessionId: string,
): Promise<MonoTools | undefined> {
  if (MONO_TOOL_TRANSPORT[harness] === "cli") return undefined;
  if (!isTauri() || !callerMono(sessionId)) return undefined;
  schemas ??= Promise.all([
    invoke<string>("app_cli_path"),
    invoke<MonoTool[]>("app_tool_schemas"),
  ]).then(([exe, tools]) => ({ exe, tools }));
  const { exe, tools } = await schemas.catch((error: unknown) => {
    schemas = undefined;
    throw error;
  });
  return {
    exe,
    tools,
    call: (action, input, requestId) =>
      invoke("control_host_call", { sessionId, action, input, requestId }),
  };
}
