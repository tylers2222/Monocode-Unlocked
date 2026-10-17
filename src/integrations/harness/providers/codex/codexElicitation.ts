import { MONO_MCP_SERVER } from "../../core/monoTools";
import { asRecord, stringField } from "./codexProtocol";

/** Only confirmations can be represented faithfully by the Allow/Deny UI. */
export function codexMcpConfirmation(params: unknown): {
  title: string;
  content: Record<string, boolean>;
} | null {
  const rec = asRecord(params);
  const schema = asRecord(rec?.requestedSchema);
  const properties = asRecord(schema?.properties);
  if (
    !["form", "openai/form", "openaiForm"].includes(String(rec?.mode)) ||
    schema?.type !== "object" ||
    !properties ||
    Object.keys(schema).some(
      (key) =>
        ![
          "type",
          "properties",
          "required",
          "title",
          "description",
          "$schema",
          "additionalProperties",
        ].includes(key),
    )
  )
    return null;

  const entries = Object.entries(properties);
  const required = schema.required ?? [];
  if (
    !Array.isArray(required) ||
    required.some(
      (key) =>
        typeof key !== "string" ||
        !Object.prototype.hasOwnProperty.call(properties, key),
    ) ||
    entries.length > 1
  )
    return null;

  let detail: string | undefined;
  let content: Record<string, boolean> = {};
  if (entries.length === 1) {
    const [key, value] = entries[0];
    const field = asRecord(value);
    if (
      field?.type !== "boolean" ||
      Object.keys(field).some(
        (name) => !["type", "title", "description", "default"].includes(name),
      )
    )
      return null;
    detail = [
      stringField(field, "title") ?? key,
      stringField(field, "description"),
    ]
      .filter(Boolean)
      .join(" — ");
    content = { [key]: true };
  }

  const message = stringField(rec, "message") ?? "Approve request";
  return {
    title: `${stringField(rec, "serverName") ?? "MCP"}: ${message}${detail ? ` — ${detail}` : ""}`,
    content,
  };
}

/**
 * The `_meta` of an accepted MCP consent. Outside full-access, codex asks
 * before each tool call of an MCP server. A consent to MonoCode's own
 * `monocode` server holds for the session, so the reports of a lead or a
 * worker ask once, not once per message; the app's level gate still decides
 * each call (PR 7 review r2 m1). A consent to any other server holds for one
 * call.
 */
export function codexMcpAcceptMeta(
  params: unknown,
): { persist: "session" } | null {
  const rec = asRecord(params);
  return rec?.serverName === MONO_MCP_SERVER &&
    asRecord(rec._meta)?.codex_approval_kind === "mcp_tool_call"
    ? { persist: "session" }
    : null;
}
