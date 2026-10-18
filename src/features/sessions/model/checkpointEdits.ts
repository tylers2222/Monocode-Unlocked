import { isEditTool } from "../../../integrations/harness/core/preview";
import { CONTINUE_PROMPT } from "./inFlight";
import { opensTurn, type AgentCwd, type Block, type WorkCwd } from "./session";
import { joinPath, pathKey } from "../../../shared/lib/paths";

/** Successful edits from the latest turn and its interrupted continuations. */
export function sessionEditPaths(blocks: readonly Block[]): string[] {
  let start = 0;
  for (let index = blocks.length - 1; index >= 0; index--) {
    const block = blocks[index];
    if (!opensTurn(block)) continue;
    start = index;
    if (block.text.trim() !== CONTINUE_PROMPT) break;
  }
  const paths = new Set<string>();
  for (let index = start; index < blocks.length; index++) {
    const block = blocks[index];
    const tool = block.role === "tool" ? block.tool : undefined;
    if (
      !tool ||
      !["completed", "success"].includes(tool.status ?? "") ||
      !isEditTool(tool.kind, tool.title, tool.preview)
    )
      continue;
    for (const path of tool.paths ?? []) if (path) paths.add(path);
    if (tool.preview?.path) paths.add(tool.preview.path);
  }
  return [...paths];
}

/**
 * Edit paths for a checkpoint kept at the working copy. An agent reports a
 * relative path against its own folder, which may sit below the copy.
 */
export function checkpointPaths(
  paths: readonly string[],
  agentCwd: AgentCwd,
  workCwd: WorkCwd,
): string[] {
  const unique = [...new Set(paths)];
  if (pathKey(agentCwd) === pathKey(workCwd)) return unique;
  return unique.map((path) =>
    /^(?:[/\\~]|[A-Za-z]:[/\\])/.test(path) ? path : joinPath(agentCwd, path),
  );
}
