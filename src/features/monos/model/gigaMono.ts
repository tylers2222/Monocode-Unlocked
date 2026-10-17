import type { AgentModelChoice } from "../../agent-app/model/agentModelChoice";
import { monoError } from "../../agent-app/model/appActionError";
import { loadRecents, projectRailItems } from "../../projects/model/recents";
import {
  newSession,
  type HarnessId,
  type Session,
} from "../../sessions/model/session";
import { reservedProjectNameError } from "../../projects/model/reservedProjectName";
import { pathKey, projectName } from "../../../shared/lib/paths";
import {
  createMono,
  findMono,
  listMonos,
  monoLook,
  saveMonoSessionId,
  updateMono,
  type Mono,
} from "./mono";

const PROJECTS_MAX = 50;

/** A Mono as monos.create and monos.update report it. */
export type MonoSettings = {
  monoId: string;
  name: string;
  giga: boolean;
  projects: string[];
  /** The Monos it may message without a shared project (design 6.1). */
  messageAllowlist: string[];
  harness: HarnessId;
  model: string;
};

export type NewMono = {
  name: string;
  projects: readonly string[];
  choice: AgentModelChoice;
};

export type MonoChange = {
  name?: string;
  projects?: readonly string[];
  /** Replaces the whole list; empty clears it. */
  messageAllowlist?: readonly string[];
  /** The new harness and model, checked against its chat's current harness. */
  choice?: (current: HarnessId) => AgentModelChoice;
};

function monoSettings(
  mono: Mono,
  chat: Pick<Session, "harness" | "model">,
): MonoSettings {
  return {
    monoId: mono.id,
    name: monoLook(mono).name,
    giga: mono.giga === true,
    projects: mono.projects,
    messageAllowlist: mono.messageAllowlist ?? [],
    harness: chat.harness,
    model: chat.model,
  };
}

/**
 * A Mono's projects, each named by path or by name, as the projects on the
 * rail: the same ones its settings offer. A folder named MonoCode other than
 * the workspace at `<home>/MonoCode` is refused.
 */
export function resolveMonoProjects(
  value: unknown,
  home: string | undefined,
): string[] {
  if (
    !Array.isArray(value) ||
    value.length > PROJECTS_MAX ||
    value.some(
      (named) =>
        typeof named !== "string" || !named.trim() || named.length > 4096,
    )
  )
    throw new Error(
      `projects must be an array of at most ${PROJECTS_MAX} project paths or names`,
    );
  const rail = projectRailItems(loadRecents(), "").map((item) => item.path);
  return (value as string[]).map((named) => {
    const byPath = rail.find((path) => pathKey(path) === pathKey(named));
    const byName = rail.filter((path) => projectName(path) === named.trim());
    const match = byPath ?? (byName.length === 1 ? byName[0] : undefined);
    const reserved = reservedProjectNameError(match ?? named, home);
    if (reserved) throw new Error(reserved);
    if (!match)
      throw new Error(
        `Not a project in MonoCode: ${named}. Projects: ${
          rail.map((path) => `${projectName(path)} (${path})`).join(", ") ||
          "none yet"
        }`,
      );
    return match;
  });
}

/**
 * The Monos that Mono `monoId` may message, and be messaged by, without a
 * shared project: other Monos on the rail, each named once by its id.
 */
export function resolveMessageAllowlist(
  value: unknown,
  monoId: string,
): string[] {
  if (!Array.isArray(value) || value.some((id) => typeof id !== "string"))
    throw new Error(
      "messageAllowlist must be an array of mono_id values; pass [] to clear it",
    );
  const others = new Set(
    listMonos().flatMap((mono) => (mono.id === monoId ? [] : [mono.id])),
  );
  const unknown = (value as string[]).filter((id) => !others.has(id));
  if (unknown.length)
    throw new Error(
      `Not another Mono's mono_id: ${unknown.join(", ")}. Run monos.list for IDs`,
    );
  return [...new Set(value as string[])];
}

/**
 * Creates a Mono with its chat on exactly the harness and model chosen, never
 * the composer's last model. Like a chat opened from the rail, it starts with
 * Auto permissions in the home folder.
 */
export async function createMonoWithChat(
  settings: NewMono,
  host: { home(): Promise<string>; add(chat: Session): void },
): Promise<MonoSettings> {
  const home = await host.home();
  const mono = createMono(settings.projects, { name: settings.name });
  const chat = newSession(
    settings.choice.harness,
    home,
    settings.choice.model.id,
    "auto",
  );
  saveMonoSessionId(mono.id, chat.id);
  host.add(chat);
  return monoSettings(mono, chat);
}

/**
 * Changes another Mono's name, projects, harness or model; undefined when
 * there is no such Mono. A busy Mono, or one preparing a handoff, is left
 * exactly as it was, and so is one whose new model is refused.
 */
export async function updateMonoSettings(
  monoId: string,
  change: MonoChange,
  host: {
    /** Its chat as the window has it now; opened, or created when it has none. */
    openChat(monoId: string): Promise<Session | undefined>;
    preparingHandoff(chat: Session): boolean;
    switchModel(chat: Session, choice: AgentModelChoice): Session;
    replace(chat: Session): void;
  },
): Promise<MonoSettings | undefined> {
  if (!findMono(monoId)) return undefined;
  const chat = await host.openChat(monoId);
  if (!chat || !findMono(monoId)) return undefined;
  if (chat.busy || host.preparingHandoff(chat)) throw monoError("mono_busy");
  const choice = change.choice?.(chat.harness);
  const next = choice ? host.switchModel(chat, choice) : chat;
  const updated = updateMono(monoId, (current) => ({
    ...current,
    ...(change.name !== undefined ? { name: change.name } : {}),
    ...(change.projects ? { projects: [...change.projects] } : {}),
    ...(change.messageAllowlist
      ? { messageAllowlist: [...change.messageAllowlist] }
      : {}),
  }));
  if (!updated) return undefined;
  if (next !== chat) host.replace(next);
  return monoSettings(updated, next);
}
