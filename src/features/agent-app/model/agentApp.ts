import { isHarnessAvailable } from "../../../integrations/harness/core/availability";
import { looksLikeProject } from "../../projects/model/recents";
import {
  mergeModelSettings,
  modelEffortSetting,
  modelsFor,
} from "../../sessions/model/models";
import { requireHarnessModel } from "./agentModelChoice";
import {
  parseGetInput,
  parseReplyInput,
  parseSendInput,
  parseSessionId,
  type GetInput,
  type ReplyInput,
  type SendInput,
} from "../../monos/model/monoMessages";
import {
  AppActionError,
  isRustCommandCode,
  monoError,
} from "./appActionError";
import {
  HARNESSES,
  RUNTIME_MODE_HINT,
  RUNTIME_MODE_LABEL,
  RUNTIME_MODES,
  type HarnessId,
  type Session,
} from "../../sessions/model/session";
import {
  addSessionToFolder,
  createFolderWithSessions,
  loadSessionFolders,
  placeSessionInFolder,
  saveSessionFolders,
  setFolderCustomColor,
} from "../../sessions/model/sessionFolders";
import {
  normalizeNoteTags,
  noteTitle,
  type Note,
  type NoteUpsert,
} from "../../notes";
import {
  ARTIFACT_KINDS,
  type Artifact,
  type ArtifactCard,
  type ArtifactKind,
  type ArtifactUpsert,
} from "../../artifacts/artifacts";
import type { QuickLaunch } from "../../quick-composer/model/quickComposer";
import type { Worktree, Worktrees } from "../../source-control/model/worktrees";
import { knownHomeDir, pathKey, projectName } from "../../../shared/lib/paths";
import type { SplitDir } from "../../workspace/model/layout";
import {
  appAccess,
  consumeOperatorCommand,
} from "../../sessions/model/operatorCommand";
import {
  sessionConversationPage,
  type SessionReadOptions,
} from "./sessionConversation";
import {
  CARD_FIELDS,
  parseCard,
  type MonoCard,
} from "../../monos/model/monoCards";
import {
  HABITS_MAX,
  checkHabitsNow,
  habitRunningSince,
  habitSchedule,
  habitScheduleLabel,
  newHabit,
  nextHabitRunAt,
  type Habit,
} from "../../monos/model/monoHabits";
import {
  memoryWithinBudget,
  MonoFileConflict,
  type AgentFilePath,
  type MonoFiles,
} from "../../monos/model/monoFiles";
import {
  addMemoryEntry,
  archiveMemoryEntries,
  fitMemoryBudget,
  memoryDate,
  memoryEntry,
  removeMemoryEntry,
  searchMemory,
  sinceDate,
  supersedeMemoryEntry,
  topicName,
} from "../../monos/model/monoMemory";
import {
  resolveMessageAllowlist,
  resolveMonoProjects,
  type MonoChange,
  type MonoSettings,
  type NewMono,
} from "../../monos/model/gigaMono";
import {
  customMonoName,
  monoLook,
  MONO_NAME_MAX,
  type Mono,
} from "../../monos/model/mono";
import {
  gigaFolder,
  type WorkspaceStatus,
} from "../../monos/model/gigaWorkspace";
import { isGigaWorkspaceProject } from "../../projects/model/reservedProjectName";
import {
  MILESTONE_STATUSES,
  type Milestone,
  type MilestonePatch,
  type MilestoneStatus,
  type Writer,
} from "../../milestones/model/milestones";
import type * as milestonesModel from "../../milestones/model/milestones";
import * as approvalsModel from "../../approvals/model/approvals";

export type AppSessionListing = {
  id: string;
  title: string;
  harness: HarnessId;
  model: string;
  busy: boolean;
  hasDraft: boolean;
  archived: boolean;
};

export type AppSessionPlacement = {
  direction: SplitDir;
  besideSessionId: string;
};

/** A Mono as app calls from its chat or habit runs see it. */
export type AppMono = {
  id: string;
  name?: string;
  /** It may create and configure other Monos. */
  giga?: boolean;
  /** Its folder is `gigas/<slug>/` in the Giga workspace. */
  gigaSlug?: string;
  projects: readonly string[];
  showStartedSessionsInSidebar?: boolean;
  /** Files the sessions it starts into a folder named after it. */
  folder?: { name: string; color: string };
};

export function appMono(mono: Mono): AppMono {
  const { name, giga, gigaSlug } = monoLook(mono);
  return {
    id: mono.id,
    name,
    ...(giga ? { giga } : {}),
    ...(gigaSlug ? { gigaSlug } : {}),
    projects: mono.projects,
    showStartedSessionsInSidebar: mono.showStartedSessionsInSidebar,
    ...(mono.useSidebarFolders ? { folder: { name, color: mono.color } } : {}),
  };
}

/** What `sessions.send` does with a prompt for a busy session. */
export type WhenBusy = "steer" | "followUp";
export type AppSendResult = {
  alreadySubmitted: boolean;
  delivery: "sent" | "steered" | "queued";
  queuedMessageId?: string;
};

export type AppMonoListing = {
  id: string;
  name: string;
  role: "giga" | "mono";
  projects: string[];
  /** Its chat is running a turn now. */
  busy: boolean;
  /** Messages waiting in its queue. */
  queued: number;
  /** False until the Mono's chat exists. */
  hasConversation: boolean;
  /** Epoch ms of the last change to its chat, if known. */
  updatedAt?: number;
  /** True for the calling Mono. */
  self: boolean;
};

/**
 * monos.get: the listing, what its chat runs on (null until it has one) and
 * its allowlist, which monos.update replaces as a whole.
 */
export type AppMonoDetails = AppMonoListing & {
  harness: HarnessId | null;
  model: string | null;
  messageAllowlist: string[];
};

/** A Mono as the host knows it: its record and, when loaded, its chat. */
export type AppMonoEntry = {
  mono: Mono;
  chat?: Session;
  /** Epoch ms of the last change to its chat, if known. */
  updatedAt?: number;
};

export type AgentAppHost = {
  start(
    launch: QuickLaunch,
    id: string,
    placement?: AppSessionPlacement,
    notifyMonoId?: string,
  ): Promise<void>;
  sessions(cwd: string): Promise<AppSessionListing[]>;
  session(id: string): Promise<Session | null>;
  readConversation?(
    session: Session,
    options: SessionReadOptions,
  ): Promise<ReturnType<typeof sessionConversationPage>>;
  send(
    id: string,
    prompt: string,
    requestId: string,
    options: { notifyMonoId?: string; whenBusy?: WhenBusy },
  ): Promise<AppSendResult>;
  draft(
    id: string,
    prompt: string,
    requestId: string,
  ): Promise<{ alreadySaved: boolean; draft: boolean }>;
  stop(id: string): Promise<void>;
  /** Drops the session's queued items; answers how many went. */
  clearQueue(id: string): Promise<number>;
  remove(id: string, mode: "archive" | "delete"): Promise<void>;
  worktrees(cwd: string): Promise<Worktrees>;
  createWorktree(
    cwd: string,
    branch: string,
    base: string,
    existing: boolean,
  ): Promise<Worktree>;
  notes(): Promise<Note[]>;
  note(id: string): Promise<Note | null>;
  saveNote(note: NoteUpsert): Promise<Note>;
  artifacts?(): Promise<Artifact[]>;
  artifact?(id: string): Promise<Artifact | null>;
  saveArtifact?(artifact: ArtifactUpsert): Promise<Artifact>;
  postArtifact?(
    sourceSessionId: string,
    card: ArtifactCard,
  ): void | Promise<void>;
  /** Whether the session is a Mono's own conversation, which owns memory. */
  isMono(sessionId: string): boolean;
  /**
   * The Mono a session works for: its own conversation or one of its habit
   * runs. Its projects are the ones it may name with "project".
   */
  monoOf?(sessionId: string): AppMono | undefined;
  /** A run of one of a Mono's habits: it may remember, not schedule. */
  isHabitRun?(sessionId: string): boolean;
  /** Puts a card in the Mono's chat, or holds it for a habit run's report. */
  postCard?(sourceSessionId: string, card: MonoCard): void;
  habits?: {
    load(monoId: string): Promise<Habit[]>;
    update<T>(
      monoId: string,
      change: (habits: Habit[]) => { habits: Habit[]; result: T },
    ): Promise<T>;
  };
  monos?: {
    /** Every Mono in rail order, with its chat when the window has it loaded. */
    list(): Promise<AppMonoEntry[]>;
    /** One Mono with its chat loaded; undefined when there is no such Mono. */
    get(monoId: string): Promise<AppMonoEntry | undefined>;
    /** Throws unknown_mono or no_conversation. */
    read(
      monoId: string,
      options: SessionReadOptions,
    ): Promise<ReturnType<typeof sessionConversationPage>>;
    /** The choice and projects are already checked. */
    create?(settings: NewMono): Promise<MonoSettings>;
    /**
     * Undefined when there is no such Mono. Throws mono_busy without changing
     * it. Writes the roster through `updateMono`.
     */
    update?(
      monoId: string,
      change: MonoChange,
    ): Promise<MonoSettings | undefined>;
  };
  /** messages.*: the calling session's Mono messaging (`monoRoute.ts`). */
  messages?: {
    getMonos(): Promise<unknown>;
    send(requestId: string, input: SendInput): Promise<unknown>;
    ask(requestId: string, input: Record<string, unknown>): Promise<unknown>;
    ack(requestId: string, input: Record<string, unknown>): Promise<unknown>;
    reply(requestId: string, input: ReplyInput): Promise<unknown>;
    get(input: GetInput): Promise<unknown>;
  };
  /**
   * Design 6.6: the level gate of this call's caller, Rust's
   * `mono_app_action_check`. Required, so a host cannot run app actions with
   * no level rule. Throws the refusal or rejection as an `AppActionError`;
   * allowed, it answers the lead id `sessions.start` writes on the new
   * session.
   */
  levels: {
    check(
      requestId: string,
      call: {
        action: string;
        /** The session the action names: the parsed `input.sessionId`. */
        target: string | null;
        /** The id a `sessions.start` launches (`appLaunchId`); null for any other action. */
        startedSessionId: string | null;
        input: Record<string, unknown>;
      },
    ): Promise<{ leadSessionId: string | null }>;
    /**
     * `mono_message_record_start`: the accepted start row of session
     * `sessionId`, which this submitted start launched, as its message id.
     * Throws its failure as an `AppActionError`.
     */
    recordStart(
      requestId: string,
      sessionId: string,
      input: Record<string, unknown>,
    ): Promise<string>;
  };
  /** milestones.*: `milestones.ts`; Rust refuses agent writes while off. */
  milestones?: Pick<
    typeof milestonesModel,
    "list" | "add" | "update" | "remove" | "reorder"
  >;
  /** approvals.*: `approvals.ts`; the Rust gate lets only a Giga through. */
  approvals?: Pick<typeof approvalsModel, "file" | "update" | "withdraw">;
  agentFiles(monoId: string): Promise<MonoFiles>;
  /** The shared Giga workspace, as the last ensure run left it. */
  workspaceStatus?(monoId: string): Promise<WorkspaceStatus>;
  readAgentFile(
    monoId: string,
    path: AgentFilePath,
  ): Promise<{ text: string | null; hash: string }>;
  /** Throws `MonoFileConflict` when the file moved past `hash`. */
  writeAgentFile(
    monoId: string,
    path: AgentFilePath,
    text: string,
    hash: string,
  ): Promise<string>;
  /** The clock entries are dated by; tests pin it. */
  now?(): Date;
};

/** Every app action with the input fields it takes. */
export const FIELDS = new Map<string, readonly string[]>([
  ["models.list", []],
  ["sessions.list", ["project"]],
  ["sessions.read", ["sessionId", "before", "limit", "maxChars", "project"]],
  [
    "sessions.send",
    ["sessionId", "prompt", "project", "notifyOnComplete", "whenBusy"],
  ],
  ["sessions.draft", ["sessionId", "prompt", "project"]],
  ["sessions.stop", ["sessionId", "project"]],
  ["sessions.queue.clear", ["sessionId", "project"]],
  ["sessions.archive", ["sessionId", "project"]],
  ["sessions.delete", ["sessionId", "project"]],
  [
    "sessions.start",
    [
      "prompt",
      "draft",
      "harness",
      "model",
      "modelSettings",
      "effort",
      "runtimeMode",
      "reveal",
      "workspaceMode",
      "worktreeBase",
      "worktreeCwd",
      "placement",
      "besideSessionId",
      "project",
      "notifyOnComplete",
    ],
  ],
  ["worktrees.list", ["project"]],
  ["worktrees.create", ["branch", "base", "existing", "project"]],
  ["folders.list", ["project"]],
  ["folders.move", ["sessionId", "folderId", "newFolderName", "project"]],
  ["notes.list", ["limit", "offset"]],
  ["notes.read", ["id"]],
  ["notes.write", ["id", "title", "body", "tags"]],
  ["artifacts.list", ["kind", "limit", "offset"]],
  ["artifacts.read", ["id"]],
  ["artifacts.write", ["id", "kind", "title", "body", "summary"]],
  ["soul.read", []],
  ["soul.update", ["text", "expectedHash"]],
  ["memory.read", ["topic"]],
  ["memory.search", ["query", "since"]],
  ["memory.add", ["fact", "topic", "until"]],
  ["memory.replace", ["find", "fact", "topic", "until"]],
  ["memory.remove", ["find", "topic"]],
  ["habits.list", []],
  ["habits.add", ["name", "instructions", "schedule"]],
  ["habits.update", ["id", "name", "instructions", "schedule", "enabled"]],
  ["habits.run", ["id"]],
  ["habits.remove", ["id"]],
  ["monos.list", []],
  ["monos.get", ["monoId"]],
  ["monos.read", ["monoId", "before", "limit", "maxChars", "work"]],
  ["monos.create", ["name", "harness", "model", "projects", "soul", "giga"]],
  [
    "monos.update",
    ["monoId", "harness", "model", "name", "projects", "messageAllowlist"],
  ],
  ["messages.get_monos", []],
  [
    "messages.send",
    ["message", "send_to", "wanting_reply", "intent", "reply_to", "scope"],
  ],
  ["messages.ask", ["message", "send_to", "due_minutes"]],
  ["messages.ack", ["ask_id", "message"]],
  ["messages.reply", ["message", "wanting_reply", "to"]],
  ["messages.get", ["handle"]],
  ["milestones.list", ["project"]],
  ["milestones.add", ["project", "title", "note", "status", "target"]],
  [
    "milestones.update",
    ["project", "id", "title", "note", "status", "target"],
  ],
  ["milestones.remove", ["project", "id"]],
  ["milestones.reorder", ["project", "ids"]],
  ["approvals.file", ["project", ...approvalsModel.CARD_FIELDS]],
  ["approvals.update", ["id", ...approvalsModel.CARD_FIELDS]],
  ["approvals.withdraw", ["id"]],
  ["workspace.status", []],
  ["chat.card", [...CARD_FIELDS]],
]);

/** Actions that were renamed, with what to use instead. */
const REMOVED_ACTIONS = new Map([
  ["monos.configure", "Use monos.update."],
  ["monos.send", "Use messages.send (MCP send_message)."],
]);

/** Fields that were removed, with what to use instead. */
const REMOVED_FIELDS: Record<string, string> = {
  action_type:
    'action_type is gone: use intent "new", or "steer" with reply_to.',
};

function fields(action: string, input: Record<string, unknown>) {
  const removed = REMOVED_ACTIONS.get(action);
  if (removed) throw new AppActionError("action_removed", removed);
  const allowed = FIELDS.get(action);
  if (!allowed)
    throw new AppActionError(
      "unknown_action",
      `Unknown app action: ${action}. Run app --help for the actions.`,
    );
  const unknown = Object.keys(input).filter((key) => !allowed.includes(key));
  if (unknown.length)
    throw new AppActionError(
      "invalid_input",
      [
        `Unknown ${action} fields: ${unknown.join(", ")}`,
        ...unknown.flatMap((key) =>
          Object.prototype.hasOwnProperty.call(REMOVED_FIELDS, key)
            ? [REMOVED_FIELDS[key]]
            : [],
        ),
      ].join(". "),
    );
}

function requiredString(value: unknown, name: string, max = 30_000): string {
  if (typeof value !== "string" || !value.trim() || value.length > max)
    throw new Error(
      `${name} must be a non-empty string under ${max} characters`,
    );
  return value.trim();
}

/**
 * An action's input with its `sessionId` parsed once (trimmed, then a
 * session id of at most 256 characters), so the level check and the action
 * read the same id. A prompt with U+0000 is refused here too, before the
 * check: SQLite counts its text only up to that character, so its row could
 * never be written.
 */
function boundaryInput(
  action: string,
  input: Record<string, unknown>,
): Record<string, unknown> {
  if (typeof input.prompt === "string" && input.prompt.includes("\u0000"))
    throw new AppActionError(
      "invalid_input",
      "prompt must not contain the character U+0000",
    );
  if (!FIELDS.get(action)?.includes("sessionId")) return input;
  // Only a session card names a session.
  if (action === "chat.card" && input.sessionId === undefined) return input;
  const sessionId =
    typeof input.sessionId === "string" ? input.sessionId.trim() : "";
  if (sessionId.length > 256 || !parseSessionId(sessionId))
    throw new AppActionError(
      "invalid_input",
      "sessionId must be a session id: 1 to 256 letters, digits, underscores or hyphens",
    );
  return { ...input, sessionId };
}

/**
 * The id of the session a `sessions.start` launches, which the level check
 * reads before any launch (A1). Later calls name a session by an id of at
 * most 256 characters, so a longer one is refused before the check.
 */
function appLaunchId(source: Session, requestId: string): string {
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(requestId))
    throw new Error(
      "request ID must use letters, digits, underscores or hyphens",
    );
  const prefix = `app-${source.id}-`;
  if (prefix.length + requestId.length > 256)
    throw new AppActionError(
      "invalid_input",
      `The new session's id, app-<this session's id>-<request id>, would be over 256 characters. Use a shorter request id, of at most ${Math.max(256 - prefix.length, 0)} characters.`,
    );
  return prefix + requestId;
}

/**
 * A start row that failed after `host.start` launched session `id`. The
 * answer says so in the same sentence, since a hint like "use a new request
 * id" alone would start the work a second time.
 */
function startedAnyway(error: unknown, id: string): Error {
  const failure =
    error instanceof AppActionError
      ? error.hint
      : error instanceof Error
        ? error.message
        : String(error);
  const text = `${failure.replace(/[.\s]+$/, "")}; session ${id} was started anyway, so do not start it again; read or steer it by its id.`;
  return error instanceof AppActionError
    ? new AppActionError(error.code, text, error.details)
    : new Error(text);
}

function agentPrompt(value: unknown): string {
  const prompt = requiredString(value, "prompt", 240_000);
  if (consumeOperatorCommand(prompt).matched)
    throw new Error("App calls cannot enable /operator in another session");
  return prompt;
}

function completionRecipient(
  source: Session,
  input: Record<string, unknown>,
  host: AgentAppHost,
  defaultForMono = false,
): string | undefined {
  const requested = input.notifyOnComplete;
  if (requested !== undefined && typeof requested !== "boolean")
    throw new Error("notifyOnComplete must be a boolean");
  const isMono = host.isMono(source.id);
  if (!(requested ?? (defaultForMono && isMono && !input.draft)))
    return undefined;
  if (!isMono)
    throw new Error(
      "Completion notifications are only available in a Mono's chat",
    );
  if (input.draft === true)
    throw new Error("An unsent draft cannot send a completion notification");
  return source.id;
}

function parseWhenBusy(value: unknown): WhenBusy | undefined {
  if (value === undefined || value === "steer" || value === "followUp")
    return value;
  throw new Error('whenBusy must be "steer" or "followUp"');
}

function optionalString(
  value: unknown,
  name: string,
  max = 512,
): string | undefined {
  return value === undefined ? undefined : requiredString(value, name, max);
}

function noteBody(value: unknown): string {
  if (typeof value !== "string" || value.length > 240_000)
    throw new Error("body must be a string under 240000 characters");
  return value.replace(/\r\n?/g, "\n");
}

function noteTags(value: unknown): string[] {
  if (
    !Array.isArray(value) ||
    value.length > 20 ||
    value.some((tag) => typeof tag !== "string" || tag.length > 48)
  )
    throw new Error(
      "tags must be an array of at most 20 strings under 48 characters each",
    );
  return normalizeNoteTags(value as string[]);
}

/**
 * The project an action works in. A session works in its own; a Mono works on
 * several, so it names one with "project" (its path or name), and may leave it
 * out when it has one project or is in one of its own.
 */
function requireProject(
  source: Session,
  input: Record<string, unknown>,
  host: AgentAppHost,
): string {
  const mono = host.monoOf?.(source.id);
  const named = optionalString(input.project, "project", 4096);
  if (!mono) {
    if (named) throw new Error("project is only for a Mono");
    if (!looksLikeProject(source.cwd))
      throw new Error("Choose a project folder in this session first");
    return source.cwd;
  }
  const choices = () =>
    mono.projects.length
      ? mono.projects.map((path) => `${projectName(path)} (${path})`).join(", ")
      : "none yet; the user adds them from your details panel";
  if (named) {
    const byPath = mono.projects.find(
      (path) => pathKey(path) === pathKey(named),
    );
    const byName = mono.projects.filter((path) => projectName(path) === named);
    const match = byPath ?? (byName.length === 1 ? byName[0] : undefined);
    if (!match) throw new Error(`Not one of your projects. Yours: ${choices()}`);
    return match;
  }
  const own = mono.projects.find(
    (path) => pathKey(path) === pathKey(source.cwd),
  );
  if (own) return own;
  if (mono.projects.length === 1) return mono.projects[0];
  throw new Error(`Pass "project" to choose one. Yours: ${choices()}`);
}

/**
 * The session an app CLI call comes from. Inbox Asks and orchestration
 * sessions keep their own tools, and a session this window no longer has
 * cannot act, so each gets `app_cli_unavailable`.
 */
export function appCliSource(
  source: Session | undefined,
  orchestrated: (sessionId: string) => boolean,
): Session {
  if (
    !source ||
    source.inboxAsk ||
    source.orchestrationLeadId ||
    orchestrated(source.id)
  )
    throw monoError("app_cli_unavailable");
  return source;
}

/** A Mono's chat lives outside its projects; project access follows its roster. */
export function canAccessAgentAppProject(
  source: Session,
  cwd: string,
  monoProjects?: readonly string[],
): boolean {
  return (monoProjects ?? [source.cwd]).some(
    (project) => pathKey(project) === pathKey(cwd),
  );
}

async function projectSession(
  source: Session,
  id: string,
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<Session> {
  const cwd = requireProject(source, input, host);
  if (!(await host.sessions(cwd)).some((session) => session.id === id))
    throw new Error("Session was not found in this project");
  const target = await host.session(id);
  if (!target || pathKey(target.cwd) !== pathKey(cwd))
    throw new Error("Session was not found in this project");
  return target;
}

/** Two short body paragraphs, with a hard cap independent of Markdown length. */
export function notePreview(body: string): string {
  return body
    .trim()
    .split(/\n\s*\n/)
    .map((paragraph) => paragraph.replace(/\s+/g, " ").trim())
    .filter(Boolean)
    .slice(0, 2)
    .join("\n\n")
    .slice(0, 400);
}

/**
 * Puts a session a Mono started into the sidebar folder named after it,
 * making that folder in its color the first time.
 */
function fileInMonoFolder(
  cwd: string,
  sessionId: string,
  folder: { name: string; color: string },
): void {
  const folders = loadSessionFolders(cwd);
  const own = folders.find((entry) => entry.name === folder.name);
  if (own) {
    saveSessionFolders(cwd, addSessionToFolder(folders, own.id, sessionId));
    return;
  }
  const created = createFolderWithSessions(folders, [sessionId], folder.name);
  saveSessionFolders(
    cwd,
    setFolderCustomColor(created.folders, created.id, folder.color),
  );
}

/**
 * Design 6.6: a session that sessions.start made starts nothing more
 * permissive than itself. A Mono and an /operator session choose freely.
 */
function runtimeCeiling(source: Session, runtimeMode: Session["runtimeMode"]) {
  if (
    appAccess(source.blocks) === "started" &&
    RUNTIME_MODES.indexOf(runtimeMode) >
      RUNTIME_MODES.indexOf(source.runtimeMode)
  )
    throw new Error(
      `runtimeMode cannot be more permissive than this session's (${source.runtimeMode})`,
    );
}

/**
 * The launch, and the worktree the call named that must be a listed one. A
 * Giga's session started from its chat into the Giga workspace runs in the
 * Giga's own folder (`workFolder`): the project and working copy stay
 * `~/MonoCode`, the work and its commits stay in `gigas/<slug>/`.
 */
function startLaunch(
  source: Session,
  input: Record<string, unknown>,
  host: AgentAppHost,
  leadSessionId: string | null,
): { launch: QuickLaunch; listedWorktree?: string } {
  const cwd = requireProject(source, input, host);
  const prompt = agentPrompt(input.prompt);
  const draft = input.draft ?? false;
  if (typeof draft !== "boolean") throw new Error("draft must be a boolean");
  const { harness: chosenHarness, model } = requireHarnessModel(input);
  const rawSettings = input.modelSettings;
  if (
    rawSettings !== undefined &&
    (!rawSettings ||
      typeof rawSettings !== "object" ||
      Array.isArray(rawSettings))
  )
    throw new Error(
      "modelSettings must be an object of setting IDs and values",
    );
  const requestedSettings = {
    ...((rawSettings ?? {}) as Record<string, unknown>),
  };
  if (input.effort !== undefined) {
    const effort = modelEffortSetting(model);
    if (!effort)
      throw new Error(`${model.id} does not expose an effort setting`);
    requestedSettings[effort.id] = requiredString(input.effort, "effort", 128);
  }
  for (const [key, value] of Object.entries(requestedSettings)) {
    const setting = model.settings?.find((entry) => entry.id === key);
    if (
      !setting ||
      typeof value !== "string" ||
      !setting.options.some((option) => option.value === value)
    )
      throw new Error(
        `Invalid model setting ${key}; run models.list for allowed values`,
      );
  }
  const runtimeMode = input.runtimeMode ?? source.runtimeMode;
  if (!RUNTIME_MODES.includes(runtimeMode as Session["runtimeMode"]))
    throw new Error(`runtimeMode must be one of: ${RUNTIME_MODES.join(", ")}`);
  runtimeCeiling(source, runtimeMode as Session["runtimeMode"]);
  const reveal = input.reveal ?? false;
  if (typeof reveal !== "boolean") throw new Error("reveal must be a boolean");
  const workspaceMode = input.workspaceMode ?? "current";
  if (workspaceMode !== "current" && workspaceMode !== "worktree")
    throw new Error("workspaceMode must be current or worktree");
  const worktreeBase = optionalString(input.worktreeBase, "worktreeBase");
  if (worktreeBase && workspaceMode !== "worktree")
    throw new Error("worktreeBase requires workspaceMode worktree");
  const worktreeCwd = optionalString(input.worktreeCwd, "worktreeCwd");
  if (worktreeCwd && workspaceMode !== "current")
    throw new Error("worktreeCwd requires workspaceMode current");
  const home = knownHomeDir();
  const mono = host.monoOf?.(source.id);
  const gigaSlug = host.isMono(source.id) ? mono?.gigaSlug : undefined;
  const ownFolder =
    workspaceMode === "current" &&
    home &&
    gigaSlug &&
    isGigaWorkspaceProject(cwd, home)
      ? gigaFolder(home, gigaSlug)
      : undefined;
  const listedWorktree =
    ownFolder && worktreeCwd && pathKey(worktreeCwd) === pathKey(ownFolder)
      ? undefined
      : worktreeCwd;
  const workFolder = listedWorktree ? undefined : ownFolder;
  const currentWorktree =
    listedWorktree ||
    (!workFolder && pathKey(cwd) === pathKey(source.cwd)
      ? source.worktreeCwd
      : undefined);
  const launch: QuickLaunch = {
    cwd,
    prompt,
    ...(mono?.showStartedSessionsInSidebar !== true
      ? { sidebarHidden: true }
      : {}),
    ...(draft ? { draft: true } : {}),
    harness: chosenHarness,
    model: model.id,
    modelSettings: mergeModelSettings(model, {
      ...(chosenHarness === source.harness && model.id === source.model
        ? source.modelSettings
        : {}),
      ...(requestedSettings as Record<string, string>),
    }),
    runtimeMode: runtimeMode as Session["runtimeMode"],
    // A Mono delegates work without navigating the user out of their chat.
    reveal: host.isMono(source.id) ? false : reveal,
    workspaceMode,
    ...(workspaceMode === "current" && currentWorktree
      ? { worktreeCwd: currentWorktree }
      : {}),
    ...(workFolder ? { workFolder } : {}),
    ...(worktreeBase ? { worktreeBase } : {}),
    startedBy: source.id,
    ...(leadSessionId ? { leadSessionId } : {}),
  };
  return { launch, listedWorktree };
}

function memoryPath(topic: unknown): AgentFilePath {
  return topic === undefined
    ? "MEMORY.md"
    : `memory/${topicName(requiredString(topic, "topic", 80))}.md`;
}

async function handleSoul(
  source: Session,
  action: string,
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  const monoId = host.isMono(source.id)
    ? host.monoOf?.(source.id)?.id
    : undefined;
  if (!monoId)
    throw new Error("Only a Mono's own conversation can manage its soul");
  if (action === "soul.read") {
    const files = await host.agentFiles(monoId);
    return { file: "SOUL.md", text: files.soul, hash: files.soulHash };
  }
  if (typeof input.text !== "string" || input.text.length > 240_000)
    throw new Error("text must be a string under 240000 characters");
  const expectedHash = requiredString(input.expectedHash, "expectedHash", 128);
  try {
    const hash = await host.writeAgentFile(
      monoId,
      "SOUL.md",
      input.text,
      expectedHash,
    );
    return { file: "SOUL.md", updated: true, hash };
  } catch (error) {
    if (error instanceof MonoFileConflict)
      throw new Error(
        "SOUL.md changed since you read it. Run soul.read and reapply the user's requested changes to the current text before calling soul.update again.",
      );
    throw error;
  }
}

/**
 * Read, change and write one memory file over the version it read, again
 * from the top when the user saved the same file in between.
 */
async function editAgentFile<T>(
  host: AgentAppHost,
  monoId: string,
  path: AgentFilePath,
  edit: (text: string) => { text: string; result: T },
): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    const current = await host.readAgentFile(monoId, path);
    const fallback =
      current.text == null && path !== "MEMORY.md"
        ? `# ${path.slice("memory/".length, -".md".length)}\n\n`
        : "";
    const next = edit(current.text ?? fallback);
    try {
      if (next.text !== (current.text ?? ""))
        await host.writeAgentFile(monoId, path, next.text, current.hash);
      return next.result;
    } catch (error) {
      if (!(error instanceof MonoFileConflict) || attempt >= 2)
        throw error;
    }
  }
}

/**
 * Keeps MEMORY.md within what loads. The archive is written first, so a line
 * is never out of one file without already being in the other. If the user
 * saved MEMORY.md in between, the trim waits for the next write.
 */
async function keepMemoryInBudget(
  host: AgentAppHost,
  monoId: string,
  keep: string,
  date: string,
): Promise<string[]> {
  const memory = await host.readAgentFile(monoId, "MEMORY.md");
  const fitted = fitMemoryBudget(memory.text ?? "", keep, date);
  if (!fitted.moved.length) return [];
  await editAgentFile(host, monoId, "memory/archive.md", (archive) => ({
    text: archiveMemoryEntries(archive, fitted.moved, date),
    result: undefined,
  }));
  try {
    await host.writeAgentFile(monoId, "MEMORY.md", fitted.text, memory.hash);
  } catch (error) {
    if (!(error instanceof MonoFileConflict)) throw error;
    return [];
  }
  return fitted.moved;
}

async function handleMemory(
  source: Session,
  action: string,
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  const monoId = host.monoOf?.(source.id)?.id;
  if (!monoId) throw new Error("Memory belongs to the Mono's conversation");
  const path = memoryPath(input.topic);
  const date = memoryDate(host.now?.() ?? new Date());
  const until = optionalString(input.until, "until", 10);
  const report = async (result: Record<string, unknown>, keep?: string) => {
    const moved =
      path === "MEMORY.md" && keep
        ? await keepMemoryInBudget(host, monoId, keep, date)
        : [];
    return {
      file: path,
      ...result,
      ...(moved.length
        ? { movedToArchive: moved.length, moved: moved.slice(0, 5) }
        : {}),
    };
  };
  switch (action) {
    case "memory.search": {
      const query =
        input.query === undefined
          ? ""
          : requiredString(input.query, "query", 500);
      const since =
        input.since === undefined
          ? undefined
          : sinceDate(
              requiredString(input.since, "since", 20),
              host.now?.() ?? new Date(),
            );
      const files = await host.agentFiles(monoId);
      const paths: AgentFilePath[] = [
        "MEMORY.md",
        ...files.topics.map((topic) => `memory/${topic}.md` as const),
        "memory/archive.md",
      ];
      const texts = await Promise.all(
        paths.map(async (file) => ({
          file,
          text:
            file === "MEMORY.md"
              ? files.memory
              : ((await host.readAgentFile(monoId, file)).text ?? ""),
        })),
      );
      const hits = searchMemory(texts, query, { since });
      return hits.length
        ? { hits }
        : {
            hits,
            note: "Nothing in memory matches. It may never have been saved.",
          };
    }
    case "memory.read": {
      if (path !== "MEMORY.md") {
        const topic = await host.readAgentFile(monoId, path);
        if (topic.text == null) throw new Error("No such memory topic");
        return { file: path, text: topic.text };
      }
      const files = await host.agentFiles(monoId);
      const budget = memoryWithinBudget(files.memory);
      return {
        file: path,
        text: files.memory,
        lines: budget.lines,
        notLoaded: budget.droppedLines,
        topics: files.topics,
      };
    }
    case "memory.add": {
      const entry = memoryEntry(
        requiredString(input.fact, "fact"),
        date,
        until,
      );
      const added = await editAgentFile(host, monoId, path, (text) => {
        const next = addMemoryEntry(text, entry);
        return { text: next.text, result: next.added };
      });
      return added
        ? report({ added: entry }, entry)
        : { file: path, alreadyRemembered: true };
    }
    case "memory.replace": {
      const find = requiredString(input.find, "find", 2000);
      const entry = memoryEntry(
        requiredString(input.fact, "fact"),
        date,
        until,
      );
      await editAgentFile(host, monoId, path, (text) => ({
        text: supersedeMemoryEntry(text, find, entry, date),
        result: undefined,
      }));
      return report({ superseded: find, added: entry }, entry);
    }
    case "memory.remove": {
      const find = requiredString(input.find, "find", 2000);
      const removed = await editAgentFile(host, monoId, path, (text) => {
        const next = removeMemoryEntry(text, find);
        return { text: next.text, result: next.removed };
      });
      return { file: path, removed };
    }
  }
  throw new Error(`Unknown app action: ${action}`);
}

function habitView(habit: Habit) {
  return {
    id: habit.id,
    name: habit.name,
    instructions: habit.instructions,
    schedule: habitScheduleLabel(habit.schedule),
    enabled: habit.enabled,
    nextRun: habit.enabled ? new Date(habit.nextRunAt).toLocaleString() : null,
    ...(habitRunningSince(habit.id) != null ? { running: true } : {}),
    ...(habit.lastRunAt
      ? {
          lastRun: new Date(habit.lastRunAt).toLocaleString(),
          lastOutcome: habit.lastOutcome,
          ...(habit.lastError ? { lastError: habit.lastError } : {}),
        }
      : {}),
  };
}

async function handleHabits(
  source: Session,
  action: string,
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  const habits = host.habits;
  // A habit's own run cannot schedule more runs.
  const monoId = host.isMono(source.id)
    ? host.monoOf?.(source.id)?.id
    : undefined;
  if (!habits || !monoId)
    throw new Error("Only a Mono's own conversation can manage its habits");
  const now = host.now?.() ?? new Date();
  const find = (list: Habit[]) => {
    const id = requiredString(input.id, "id", 128);
    const habit = list.find((entry) => entry.id === id);
    if (!habit) throw new Error("No habit with that id; run habits.list");
    return habit;
  };
  switch (action) {
    case "habits.list":
      return { habits: (await habits.load(monoId)).map(habitView) };
    case "habits.add": {
      const name = requiredString(input.name, "name", 80);
      const instructions = requiredString(
        input.instructions,
        "instructions",
        4_000,
      );
      const schedule = habitSchedule(input.schedule);
      const habit = await habits.update(monoId, (list) => {
        if (list.length >= HABITS_MAX)
          throw new Error(`A Mono can have at most ${HABITS_MAX} habits`);
        const created = newHabit(
          { name, instructions, schedule },
          now.getTime(),
        );
        return { habits: [...list, created], result: created };
      });
      return { habit: habitView(habit) };
    }
    case "habits.update": {
      const patch = {
        ...(input.name === undefined
          ? {}
          : { name: requiredString(input.name, "name", 80) }),
        ...(input.instructions === undefined
          ? {}
          : {
              instructions: requiredString(
                input.instructions,
                "instructions",
                4_000,
              ),
            }),
        ...(input.schedule === undefined
          ? {}
          : { schedule: habitSchedule(input.schedule) }),
      };
      if (input.enabled !== undefined && typeof input.enabled !== "boolean")
        throw new Error("enabled must be true or false");
      const habit = await habits.update(monoId, (list) => {
        const current = find(list);
        const next: Habit = {
          ...current,
          ...patch,
          ...(input.enabled === undefined
            ? {}
            : { enabled: input.enabled as boolean }),
        };
        // A new schedule, or turning it back on, counts from now.
        if (patch.schedule || (next.enabled && !current.enabled))
          next.nextRunAt = nextHabitRunAt(next.schedule, now.getTime());
        return {
          habits: list.map((entry) => (entry.id === next.id ? next : entry)),
          result: next,
        };
      });
      return { habit: habitView(habit) };
    }
    case "habits.run": {
      const running = habitRunningSince(requiredString(input.id, "id", 128));
      if (running != null)
        throw new Error(
          `That habit is already running (started ${new Date(running).toLocaleTimeString()}); its result will be posted here when it ends`,
        );
      const habit = await habits.update(monoId, (list) => {
        const next = { ...find(list), runRequested: true };
        return {
          habits: list.map((entry) => (entry.id === next.id ? next : entry)),
          result: next,
        };
      });
      checkHabitsNow();
      return {
        habit: habitView(habit),
        note: "It starts now, on its own. Whatever it finds is posted to this chat, or nothing if there is nothing to say.",
      };
    }
    case "habits.remove": {
      const removed = await habits.update(monoId, (list) => {
        const habit = find(list);
        return {
          habits: list.filter((entry) => entry.id !== habit.id),
          result: habit,
        };
      });
      return { removed: removed.name };
    }
  }
  throw new Error(`Unknown app action: ${action}`);
}

/**
 * A call already made with this request ID: the same result again, or
 * request_id_reused when the input differs.
 */
export function repeatedAppCall<T>(
  previous: { signature: string; promise: Promise<T> } | undefined,
  signature: string,
): Promise<T> | undefined {
  if (!previous) return undefined;
  if (previous.signature !== signature)
    throw new AppActionError(
      "request_id_reused",
      "This request id was already used with different input. Use a new request id.",
    );
  return previous.promise;
}

/** Runs an input check, reporting what it refuses as invalid_input. */
function checked<T>(parse: () => T): T {
  try {
    return parse();
  } catch (error) {
    if (error instanceof AppActionError) throw error;
    throw new AppActionError(
      "invalid_input",
      error instanceof Error ? error.message : String(error),
    );
  }
}

function monoListing(
  { mono, chat, updatedAt }: AppMonoEntry,
  self: string,
): AppMonoListing {
  return {
    id: mono.id,
    name: monoLook(mono).name,
    role: mono.giga ? "giga" : "mono",
    projects: mono.projects,
    busy: !!chat?.busy,
    queued: chat?.queuedMessages?.length ?? 0,
    hasConversation: !!mono.sessionId,
    ...(updatedAt ? { updatedAt } : {}),
    self: mono.id === self,
  };
}

async function handleMonos(
  source: Session,
  action: string,
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  // A Mono's chat or one of its habit runs; neither needs a project.
  const mono = host.monoOf?.(source.id);
  const monos = host.monos;
  if (!mono || !monos) throw monoError("not_a_mono");
  switch (action) {
    case "monos.list":
      return {
        monos: (await monos.list()).map((entry) => monoListing(entry, mono.id)),
      };
    case "monos.get": {
      const monoId = checked(() => requiredString(input.monoId, "monoId", 256));
      const entry = await monos.get(monoId);
      if (!entry) throw monoError("unknown_mono");
      return {
        ...monoListing(entry, mono.id),
        harness: entry.chat?.harness ?? null,
        model: entry.chat?.model ?? null,
        messageAllowlist: entry.mono.messageAllowlist ?? [],
      } satisfies AppMonoDetails;
    }
    case "monos.read": {
      const [monoId, options] = checked(() => {
        const options = {
          before: optionalString(input.before, "before", 256),
          limit: input.limit as number | undefined,
          maxChars: input.maxChars as number | undefined,
          work: input.work as boolean | undefined,
        };
        // Checks limit, maxChars and work before the host loads anything.
        sessionConversationPage(
          { ...source, blocks: [] },
          { ...options, before: undefined },
        );
        return [requiredString(input.monoId, "monoId", 256), options] as const;
      });
      return monos.read(monoId, options);
    }
  }
  throw new Error(`Unknown app action: ${action}`);
}

/**
 * A name the database keeps: the same trim and the same limit as
 * `mono_roster.rs`, so a rename never reports success and then disappears.
 */
function monoName(value: unknown): string {
  const name = typeof value === "string" ? customMonoName(value) : undefined;
  if (!name || [...name].length > MONO_NAME_MAX)
    throw new AppActionError(
      "invalid_input",
      `name must have 1 to ${MONO_NAME_MAX} characters besides outer spaces`,
    );
  return name;
}

/**
 * monos.create and monos.update: only a Giga Mono's chat or one of its habit
 * runs may make Monos or change another Mono.
 */
async function handleGigaMonos(
  source: Session,
  action: "monos.create" | "monos.update",
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  const mono = host.monoOf?.(source.id);
  const monos = host.monos;
  if (!mono?.giga || !monos?.create || !monos.update)
    throw monoError("giga_only");
  if (action === "monos.create") {
    const settings = checked((): NewMono => {
      const name = monoName(input.name);
      const choice = requireHarnessModel(input);
      if (input.projects === undefined)
        throw new Error("projects is required; pass [] for none");
      const projects = resolveMonoProjects(input.projects, knownHomeDir());
      if (input.giga !== undefined && input.giga !== false)
        throw new Error(
          "giga can only be false; only the user makes a Giga Mono",
        );
      if (
        input.soul !== undefined &&
        (typeof input.soul !== "string" ||
          !input.soul.trim() ||
          input.soul.length > 240_000)
      )
        throw new Error(
          "soul must be a non-empty string under 240000 characters",
        );
      return { name, projects, choice };
    });
    const created = await monos.create(settings);
    if (typeof input.soul === "string") {
      const files = await host.agentFiles(created.monoId);
      await host.writeAgentFile(
        created.monoId,
        "SOUL.md",
        input.soul,
        files.soulHash,
      );
    }
    return { ...created, created: true };
  }
  const monoId = checked(() => requiredString(input.monoId, "monoId", 256));
  if (monoId === mono.id) throw monoError("self_update");
  const change = checked((): MonoChange => {
    const name = input.name === undefined ? undefined : monoName(input.name);
    const projects =
      input.projects === undefined
        ? undefined
        : resolveMonoProjects(input.projects, knownHomeDir());
    const messageAllowlist =
      input.messageAllowlist === undefined
        ? undefined
        : resolveMessageAllowlist(input.messageAllowlist, monoId);
    const changesModel =
      input.harness !== undefined || input.model !== undefined;
    if (
      !changesModel &&
      name === undefined &&
      projects === undefined &&
      messageAllowlist === undefined
    )
      throw new Error(
        "Pass harness and model, model, name, projects or messageAllowlist to change",
      );
    return {
      ...(name !== undefined ? { name } : {}),
      ...(projects ? { projects } : {}),
      ...(messageAllowlist ? { messageAllowlist } : {}),
      // A model alone has to suit the harness the Mono's chat is on now.
      ...(changesModel
        ? {
            choice: (current: HarnessId) =>
              checked(() =>
                requireHarnessModel({
                  harness: input.harness ?? current,
                  model: input.model,
                }),
              ),
          }
        : {}),
    };
  });
  const updated = await monos.update(monoId, change);
  if (!updated) throw monoError("unknown_mono");
  return updated;
}

function artifactKind(value: unknown): ArtifactKind {
  if (value === undefined) return "document";
  if (ARTIFACT_KINDS.includes(value as ArtifactKind))
    return value as ArtifactKind;
  throw new Error('Unsupported artifact kind; use "document" or "html"');
}

async function handleArtifacts(
  source: Session,
  requestId: string,
  action: string,
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  if (!(host.isMono(source.id) || host.isHabitRun?.(source.id)))
    throw new Error("Only a Mono can create or read artifacts");
  if (!host.artifact) throw new Error("Artifacts are unavailable");
  if (action === "artifacts.list") {
    if (!host.artifacts) throw new Error("Artifacts are unavailable");
    const kind =
      input.kind === undefined ? undefined : artifactKind(input.kind);
    const limit = input.limit ?? 30;
    const offset = input.offset ?? 0;
    if (
      !Number.isInteger(limit) ||
      (limit as number) < 1 ||
      (limit as number) > 100
    )
      throw new Error("limit must be an integer from 1 to 100");
    if (!Number.isInteger(offset) || (offset as number) < 0)
      throw new Error("offset must be a non-negative integer");
    const artifacts = (await host.artifacts()).filter(
      (artifact) => kind === undefined || artifact.kind === kind,
    );
    return {
      total: artifacts.length,
      offset,
      artifacts: artifacts
        .slice(offset as number, (offset as number) + (limit as number))
        .map(({ id, kind, title, updatedAt }) => ({
          id,
          kind,
          title,
          updatedAt,
        })),
    };
  }
  if (action === "artifacts.read") {
    const artifact = await host.artifact(requiredString(input.id, "id", 256));
    if (!artifact) throw new Error("Artifact was not found");
    return artifact;
  }
  if (!host.saveArtifact || !host.postArtifact)
    throw new Error("Artifacts are unavailable");
  const kind = artifactKind(input.kind);
  const id = optionalString(input.id, "id", 256);
  if (id && !/^[A-Za-z0-9_-]+$/.test(id))
    throw new Error("Invalid artifact ID");
  const title =
    input.title === undefined
      ? undefined
      : requiredString(input.title, "title", 200);
  const body = input.body === undefined ? undefined : noteBody(input.body);
  const summary =
    input.summary === undefined
      ? undefined
      : requiredString(input.summary, "summary", 280);
  let artifact: Artifact;
  if (id) {
    if (title === undefined && body === undefined)
      throw new Error("Supply title or body to update an artifact");
    const current = await host.artifact(id);
    if (!current) throw new Error("Artifact was not found");
    if (current.kind !== kind)
      throw new Error("An artifact's kind cannot be changed");
    artifact = await host.saveArtifact({
      id,
      kind,
      title: title ?? current.title,
      body: body ?? current.body,
    });
  } else {
    if (body === undefined)
      throw new Error("body is required to create an artifact");
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(requestId))
      throw new Error("Invalid request ID");
    const createdId = `artifact-${source.id}-${requestId}`;
    const existing = await host.artifact(createdId);
    if (
      existing &&
      (existing.kind !== kind ||
        existing.title !== (title ?? noteTitle(body)) ||
        existing.body !== body)
    )
      throw new Error("Request ID was already used for another artifact");
    artifact =
      existing ??
      (await host.saveArtifact({
        id: createdId,
        kind,
        title: title ?? noteTitle(body),
        body,
        sourceSessionId: source.id,
        ...(looksLikeProject(source.cwd) ? { sourceCwd: source.cwd } : {}),
      }));
  }
  const card: ArtifactCard = {
    id: artifact.id,
    kind: artifact.kind,
    title: artifact.title,
    ...(summary ? { summary } : {}),
  };
  await host.postArtifact(source.id, card);
  return { ...card, saved: true, attached: true };
}

function milestoneStatus(value: unknown): MilestoneStatus {
  if (!MILESTONE_STATUSES.includes(value as MilestoneStatus))
    throw new Error(`status must be one of ${MILESTONE_STATUSES.join(", ")}`);
  return value as MilestoneStatus;
}

/** "" and null both mean Later; the reply and the row hold null. */
function milestoneTarget(value: unknown): string | null {
  if (value === null || value === "") return null;
  const day =
    typeof value === "string" && /^\d{4}-\d{2}-\d{2}$/.test(value)
      ? new Date(`${value}T00:00:00Z`)
      : undefined;
  if (!day || Number.isNaN(day.getTime()) || day.toISOString().slice(0, 10) !== value)
    throw new Error('target must be a real YYYY-MM-DD date, or "" or null for later');
  return value;
}

/** Types only; Rust checks the lengths, so there is one limit. */
function milestonePatch(input: Record<string, unknown>): MilestonePatch {
  for (const name of ["title", "note"] as const)
    if (input[name] !== undefined && typeof input[name] !== "string")
      throw new Error(`${name} must be a string`);
  return {
    ...(input.title === undefined ? {} : { title: input.title as string }),
    ...(input.note === undefined ? {} : { note: input.note as string }),
    ...(input.status === undefined
      ? {}
      : { status: milestoneStatus(input.status) }),
    ...(input.target === undefined
      ? {}
      : { target: milestoneTarget(input.target) }),
  };
}

function milestoneIds(value: unknown): string[] {
  if (
    !Array.isArray(value) ||
    value.some((id) => typeof id !== "string" || id.length > 128)
  )
    throw new Error("ids must be an array of milestone ids");
  return value as string[];
}

/** The reply drops `projectPath`: every row is in the one project named. */
function milestoneView({ projectPath: _, ...view }: Milestone) {
  return view;
}

/** Rust refuses with a bare code, or `invalid_input: <reason>`. */
function rust<T>(call: Promise<T>): Promise<T> {
  return call.catch((error: unknown) => {
    if (isRustCommandCode(error)) throw monoError(error);
    const reason =
      typeof error === "string" ? /^invalid_input: (.*)$/s.exec(error)?.[1] : undefined;
    throw reason === undefined ? error : new AppActionError("invalid_input", reason);
  });
}

async function handleMilestones(
  source: Session,
  action: string,
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  const milestones = host.milestones;
  if (!milestones) throw new Error("Milestones are unavailable here");
  const project = checked(() => requireProject(source, input, host));
  // The writer is the caller Rust's grant names, never input: Rust lets
  // "user" write while milestones are off and refuses every agent.
  const monoId = host.monoOf?.(source.id)?.id;
  const by: Writer = monoId ? `mono:${monoId}` : `session:${source.id}`;
  switch (action) {
    case "milestones.list": {
      const listed = await milestones.list(project);
      return {
        project,
        settings: listed.settings,
        milestones: listed.milestones.map(milestoneView),
      };
    }
    case "milestones.add": {
      const milestone = checked(() => {
        if (input.title === undefined) throw new Error("title is required");
        return { ...milestonePatch(input), title: input.title as string };
      });
      return milestoneView(await milestones.add(project, milestone, by));
    }
    case "milestones.update": {
      const [id, patch] = checked(
        () => [requiredString(input.id, "id", 128), milestonePatch(input)] as const,
      );
      return milestoneView(await milestones.update(project, id, patch, by));
    }
    case "milestones.remove": {
      const id = checked(() => requiredString(input.id, "id", 128));
      await milestones.remove(project, id, by);
      return { removed: id };
    }
    case "milestones.reorder": {
      const ids = checked(() => milestoneIds(input.ids));
      const listed = await milestones.reorder(project, ids, by);
      return { project, milestones: listed.map(milestoneView) };
    }
  }
  throw new Error(`Unknown app action: ${action}`);
}

async function handleApprovals(
  source: Session,
  action: string,
  input: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  const giga = host.monoOf?.(source.id);
  const approvals = host.approvals;
  if (!giga || !approvals)
    throw new Error("Approval cards are unavailable here");
  switch (action) {
    case "approvals.file": {
      const card = checked(() => approvalsModel.parseApprovalCard(input));
      // Not requireProject: a Giga files for any project in MonoCode, not
      // only its own.
      const project = checked(
        () =>
          resolveMonoProjects(
            [requiredString(input.project, "project", 4096)],
            knownHomeDir(),
          )[0],
      );
      return approvals.file(project, card, giga.id);
    }
    case "approvals.update": {
      const id = checked(() => requiredString(input.id, "id", 128));
      const card = checked(() => approvalsModel.parseApprovalCard(input));
      return approvals.update(id, card, giga.id);
    }
    case "approvals.withdraw": {
      const id = checked(() => requiredString(input.id, "id", 128));
      return approvals.withdraw(id, giga.id);
    }
  }
  throw new Error(`Unknown app action: ${action}`);
}

export async function handleAgentApp(
  source: Session,
  requestId: string,
  action: string,
  rawInput: Record<string, unknown>,
  host: AgentAppHost,
): Promise<unknown> {
  fields(action, rawInput);
  const input = boundaryInput(action, rawInput);
  const launchId =
    action === "sessions.start" ? appLaunchId(source, requestId) : null;
  // Design 6.6: Rust decides every app action once, before it runs.
  // messages.* get their own gate in mono_message_submit.
  const { leadSessionId } = action.startsWith("messages.")
    ? { leadSessionId: null }
    : await host.levels.check(requestId, {
        action,
        target: typeof input.sessionId === "string" ? input.sessionId : null,
        startedSessionId: launchId,
        input,
      });
  if (action.startsWith("artifacts."))
    return handleArtifacts(source, requestId, action, input, host);
  if (action.startsWith("soul."))
    return handleSoul(source, action, input, host);
  if (action.startsWith("memory."))
    return handleMemory(source, action, input, host);
  if (action.startsWith("habits."))
    return handleHabits(source, action, input, host);
  if (action.startsWith("milestones."))
    return rust(handleMilestones(source, action, input, host));
  if (action.startsWith("approvals."))
    return rust(handleApprovals(source, action, input, host));
  if (action === "monos.create" || action === "monos.update")
    return handleGigaMonos(source, action, input, host);
  if (action.startsWith("monos."))
    return handleMonos(source, action, input, host);
  if (action.startsWith("messages.")) {
    const messages = host.messages;
    if (!messages) throw new Error("Mono messaging is unavailable here");
    if (action === "messages.send")
      return messages.send(
        requestId,
        checked(() => parseSendInput(input)),
      );
    // Rust parses an ask or ack and names the field in its invalid_input hint.
    if (action === "messages.ask") return messages.ask(requestId, input);
    if (action === "messages.ack") return messages.ack(requestId, input);
    if (action === "messages.reply")
      return messages.reply(
        requestId,
        checked(() => parseReplyInput(input)),
      );
    if (action === "messages.get")
      return messages.get(checked(() => parseGetInput(input)));
    return messages.getMonos();
  }
  if (action === "workspace.status") {
    const mono = host.monoOf?.(source.id);
    if (!mono || !host.workspaceStatus)
      throw new Error("Only a Mono or its habit runs can read the workspace");
    return host.workspaceStatus(mono.id);
  }
  if (action === "chat.card") {
    if (
      !host.postCard ||
      !(host.isMono(source.id) || host.isHabitRun?.(source.id))
    )
      throw new Error("Only a Mono can put cards in its chat");
    const card = parseCard(input);
    host.postCard(source.id, card);
    return {
      posted: card.type,
      note: host.isMono(source.id)
        ? "It shows in the chat where you are in your reply."
        : "It goes out with your report, after its text; if you stay quiet, it is dropped.",
    };
  }
  switch (action) {
    case "models.list":
      return {
        runtimeModes: RUNTIME_MODES.map((id) => ({
          id,
          label: RUNTIME_MODE_LABEL[id],
          description: RUNTIME_MODE_HINT[id],
        })),
        harnesses: HARNESSES.map((harness) => ({
          id: harness,
          available: isHarnessAvailable(harness),
          models: modelsFor(harness).map((model) => ({
            id: model.id,
            name: model.name,
            settings: model.settings ?? [],
          })),
        })),
      };
    case "sessions.list": {
      const cwd = requireProject(source, input, host);
      return { cwd, sessions: await host.sessions(cwd) };
    }
    case "sessions.read": {
      const id = requiredString(input.sessionId, "sessionId", 256);
      const target = id === source.id && host.isMono?.(id)
        ? source : await projectSession(source, id, input, host);
      const options = {
        before: optionalString(input.before, "before", 256),
        limit: input.limit as number | undefined,
        maxChars: input.maxChars as number | undefined,
      };
      return host.readConversation ? host.readConversation(target, options) : sessionConversationPage(target, options);
    }
    case "sessions.send": {
      const id = requiredString(input.sessionId, "sessionId", 256);
      const prompt = agentPrompt(input.prompt);
      const whenBusy = parseWhenBusy(input.whenBusy);
      const notifyMonoId = completionRecipient(source, input, host);
      if (id === source.id)
        throw new Error(
          "Use the current conversation to continue this session",
        );
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(requestId))
        throw new Error("Invalid request ID");
      await projectSession(source, id, input, host);
      const { alreadySubmitted, delivery, queuedMessageId } = await host.send(
        id,
        prompt,
        `app-${source.id}-${requestId}`,
        {
          ...(notifyMonoId ? { notifyMonoId } : {}),
          ...(whenBusy ? { whenBusy } : {}),
        },
      );
      return {
        sessionId: id,
        submitted: true,
        alreadySubmitted,
        delivery,
        ...(queuedMessageId ? { queuedMessageId } : {}),
        ...(notifyMonoId ? { notifyOnComplete: true } : {}),
      };
    }
    case "sessions.draft": {
      const id = requiredString(input.sessionId, "sessionId", 256);
      const prompt = agentPrompt(input.prompt);
      if (id === source.id)
        throw new Error("Use the composer to save a draft in this session");
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(requestId))
        throw new Error("Invalid request ID");
      await projectSession(source, id, input, host);
      const result = await host.draft(
        id,
        prompt,
        `app-${source.id}-${requestId}`,
      );
      return { sessionId: id, saved: true, ...result };
    }
    case "sessions.queue.clear": {
      const id = requiredString(input.sessionId, "sessionId", 256);
      const target = await projectSession(source, id, input, host);
      const owner = target.blocks.find((block) => block.role === "user")
        ?.startedBy;
      if (target.leadSessionId !== source.id && owner !== source.id)
        throw monoError("not_session_owner");
      return { sessionId: id, cleared: await host.clearQueue(id) };
    }
    case "sessions.stop":
    case "sessions.archive":
    case "sessions.delete": {
      const id = requiredString(input.sessionId, "sessionId", 256);
      if (id === source.id)
        throw new Error("Cannot stop, archive or delete the calling session");
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(requestId))
        throw new Error("Invalid request ID");
      const target = await projectSession(source, id, input, host);
      if (
        target.orchestrationLeadId ||
        host.isMono(id) ||
        host.isHabitRun?.(id)
      )
        throw new Error("Only regular project sessions can be managed here");
      if (action === "sessions.stop") {
        await host.stop(id);
        return { sessionId: id, stopped: true };
      }
      const mode = action === "sessions.archive" ? "archive" : "delete";
      await host.remove(id, mode);
      return {
        sessionId: id,
        [mode === "archive" ? "archived" : "deleted"]: true,
      };
    }
    case "sessions.start": {
      const { launch, listedWorktree } = startLaunch(
        source,
        input,
        host,
        leadSessionId,
      );
      const notifyMonoId = completionRecipient(source, input, host, true);
      if (listedWorktree) {
        const chosen = (await host.worktrees(launch.cwd)).worktrees.find(
          (tree) =>
            !tree.missing && pathKey(tree.path) === pathKey(listedWorktree),
        );
        if (!chosen)
          throw new Error(
            "Worktree is unavailable in this project; run worktrees.list",
          );
        launch.worktreeCwd =
          pathKey(chosen.path) === pathKey(launch.cwd)
            ? undefined
            : chosen.path;
      }
      const placement = input.placement ?? "tab";
      if (placement !== "tab" && placement !== "right" && placement !== "down")
        throw new Error("placement must be tab, right or down");
      if (input.besideSessionId !== undefined && placement === "tab")
        throw new Error("besideSessionId requires placement right or down");
      const besideSessionId =
        placement === "tab"
          ? undefined
          : (optionalString(input.besideSessionId, "besideSessionId", 256) ??
            source.id);
      // The id the level check was sent (appLaunchId is pure).
      const id = appLaunchId(source, requestId);
      if (besideSessionId)
        await host.start(
          launch,
          id,
          { direction: placement as SplitDir, besideSessionId },
          ...(notifyMonoId ? [notifyMonoId] : []),
        );
      else if (notifyMonoId) await host.start(launch, id, undefined, notifyMonoId);
      else await host.start(launch, id);
      const folder = host.monoOf?.(source.id)?.folder;
      if (folder && !launch.sidebarHidden)
        fileInMonoFolder(launch.cwd, id, folder);
      // The start row names this work for steers (design 16.2). It takes
      // the input as the caller gave it, the input the level check hashed.
      // A draft runs no work, so it has none.
      const messageId = launch.draft
        ? undefined
        : await host.levels
            .recordStart(requestId, id, input)
            .catch((error: unknown) => {
              throw startedAnyway(error, id);
            });
      return {
        id,
        cwd: launch.cwd,
        harness: launch.harness,
        model: launch.model,
        submitted: !launch.draft,
        draft: !!launch.draft,
        ...(notifyMonoId ? { notifyOnComplete: true } : {}),
        ...(messageId ? { message_id: messageId } : {}),
      };
    }
    case "worktrees.list":
      return host.worktrees(requireProject(source, input, host));
    case "worktrees.create": {
      const cwd = requireProject(source, input, host);
      const branch = requiredString(input.branch, "branch", 400);
      const existing = input.existing ?? false;
      if (typeof existing !== "boolean")
        throw new Error("existing must be a boolean");
      const base = optionalString(input.base, "base", 400);
      if (existing && base)
        throw new Error("base cannot be set for an existing branch");
      return host.createWorktree(cwd, branch, base ?? "HEAD", existing);
    }
    case "folders.list": {
      const cwd = requireProject(source, input, host);
      return {
        cwd,
        folders: loadSessionFolders(cwd).map(({ id, name, sessionIds }) => ({
          id,
          name,
          sessionIds,
        })),
      };
    }
    case "folders.move": {
      const cwd = requireProject(source, input, host);
      const sessionId = requiredString(input.sessionId, "sessionId", 256);
      const folderId = optionalString(input.folderId, "folderId", 256);
      const newFolderName = optionalString(
        input.newFolderName,
        "newFolderName",
        100,
      );
      if (!!folderId === !!newFolderName)
        throw new Error("Supply exactly one of folderId or newFolderName");
      if (
        !(await host.sessions(cwd)).some((session) => session.id === sessionId)
      )
        throw new Error("Session was not found in this project");
      const folders = loadSessionFolders(cwd);
      if (folderId && !folders.some((folder) => folder.id === folderId))
        throw new Error("Folder was not found in this project");
      const next = placeSessionInFolder(
        folders,
        sessionId,
        folderId
          ? { kind: "existing", folderId }
          : { kind: "new", name: newFolderName! },
      );
      saveSessionFolders(cwd, next);
      const folder = next.find((entry) => entry.sessionIds.includes(sessionId));
      return { sessionId, folderId: folder?.id, folderName: folder?.name };
    }
    case "notes.list": {
      const limit = input.limit ?? 30;
      const offset = input.offset ?? 0;
      if (
        !Number.isInteger(limit) ||
        (limit as number) < 1 ||
        (limit as number) > 100
      )
        throw new Error("limit must be an integer from 1 to 100");
      if (!Number.isInteger(offset) || (offset as number) < 0)
        throw new Error("offset must be a non-negative integer");
      const notes = await host.notes();
      return {
        total: notes.length,
        offset,
        notes: notes
          .slice(offset as number, (offset as number) + (limit as number))
          .map((note) => ({
            id: note.id,
            title: note.title,
            preview: notePreview(note.body),
            tags: note.tags,
            sourceCwd: note.sourceCwd,
          })),
      };
    }
    case "notes.read": {
      const id = requiredString(input.id, "id", 256);
      const note = await host.note(id);
      if (!note) throw new Error("Note was not found");
      return note;
    }
    case "notes.write": {
      const id = optionalString(input.id, "id", 256);
      if (id && !/^[A-Za-z0-9_-]+$/.test(id))
        throw new Error("Invalid note ID");
      const title =
        input.title === undefined
          ? undefined
          : requiredString(input.title, "title", 200);
      const body = input.body === undefined ? undefined : noteBody(input.body);
      const tags = input.tags === undefined ? undefined : noteTags(input.tags);
      if (id) {
        if (title === undefined && body === undefined && tags === undefined)
          throw new Error("Supply title, body or tags to update a note");
        const current = await host.note(id);
        if (!current) throw new Error("Note was not found");
        return host.saveNote({
          id,
          title: title ?? current.title,
          body: body ?? current.body,
          tags: tags ?? current.tags,
        });
      }
      if (body === undefined)
        throw new Error("body is required to create a note");
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(requestId))
        throw new Error("Invalid request ID");
      const createdId = `app-${source.id}-${requestId}`;
      const existing = await host.note(createdId);
      if (existing) {
        if (
          existing.title !== (title ?? noteTitle(body)) ||
          existing.body !== body ||
          JSON.stringify(existing.tags) !== JSON.stringify(tags ?? [])
        )
          throw new Error("Request ID was already used for another note");
        return existing;
      }
      return host.saveNote({
        id: createdId,
        title: title ?? noteTitle(body),
        body,
        tags: tags ?? [],
        sourceSessionId: source.id,
        ...(looksLikeProject(source.cwd) ? { sourceCwd: source.cwd } : {}),
      });
    }
  }
}
