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
 * Changes another Mono's name, projects, harness or model. A busy Mono is left
 * exactly as it was, and so is one whose new model is refused.
 */
export function updateMonoSettings(
  mono: Mono,
  chat: Session,
  change: MonoChange,
  host: {
    switchModel(chat: Session, choice: AgentModelChoice): Session;
    replace(chat: Session): void;
  },
): MonoSettings {
  if (chat.busy) throw monoError("mono_busy");
  const choice = change.choice?.(chat.harness);
  const next = choice ? host.switchModel(chat, choice) : chat;
  const updated = updateMono(mono.id, (current) => ({
    ...current,
    ...(change.name !== undefined ? { name: change.name } : {}),
    ...(change.projects ? { projects: [...change.projects] } : {}),
  }));
  if (!updated) throw monoError("unknown_mono");
  if (next !== chat) host.replace(next);
  return monoSettings(updated, next);
}
