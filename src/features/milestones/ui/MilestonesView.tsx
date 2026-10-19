import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { OverlayNav } from "../../../app/shell/TitleBar";
import { WindowControls } from "../../../app/shell/WindowControls";
import { IS_MAC } from "../../../platform/tauri/platform";
import { projectKey, projectName } from "../../../shared/lib/paths";
import {
  AlertCircle,
  Flag,
  LoaderCircle,
  Plus,
  Search,
} from "../../../shared/ui/icons";
import { findMono, monoLook } from "../../monos/model/mono";
import { useTabGroupLogos } from "../../projects/hooks/useTabGroupLogos";
import {
  normalizeProjectPath,
  type RecentProject,
} from "../../projects/model/recents";
import { ProjectLogoIcon } from "../../projects/ui/ProjectLogoIcon";
import { ProjectMascot } from "../../projects/ui/ProjectMascot";
import {
  loadTabGroupColors,
  loadTabGroupCustomColors,
  loadTabGroupLabels,
  loadTabGroupMascots,
  resolveTabGroupColor,
  resolveTabGroupLabel,
  resolveTabGroupLogo,
  resolveTabGroupMascot,
} from "../../workspace/model/tabGroups";
import {
  add,
  list,
  subscribe,
  update,
  type Milestone,
  type ProjectMilestones,
} from "../model/milestones";
import {
  MilestoneEditor,
  openEditor,
  type EditorState,
} from "./MilestoneEditor";
import { MilestoneTimeline, type CreatorLook } from "./MilestoneTimeline";
import {
  STATUS_LABELS,
  TIMELINE_STATUSES,
  agentCreator,
  type AgentCreator,
} from "./timeline";

type Props = {
  besideRail?: boolean;
  compactRail?: boolean;
  /** The project the page opens on: the one with the selected session. */
  cwd?: string;
  recents: readonly RecentProject[];
  /** Names the sessions that added milestones. */
  sessions: readonly { id: string; title: string }[];
  onClose: () => void;
  onToggleSidebar?: () => void;
};

/** How long an agent's new row keeps its blue highlight. */
const HIGHLIGHT_MS = 4000;

/** Rows an agent added between two lists of one project. */
function agentAdded(
  before: readonly Milestone[],
  after: readonly Milestone[],
): string[] {
  const known = new Set(before.map((milestone) => milestone.id));
  return after
    .filter(
      (milestone) =>
        !known.has(milestone.id) && agentCreator(milestone.createdBy),
    )
    .map((milestone) => milestone.id);
}

/** "8 milestones · 2 in progress", or which statuses the settings hide. */
function projectSummary({ settings, milestones }: ProjectMilestones): string {
  if (milestones.length === 0) return "No milestones yet";
  const count = `${milestones.length} ${milestones.length === 1 ? "milestone" : "milestones"}`;
  const hidden = TIMELINE_STATUSES.filter((status) =>
    settings.hiddenStatuses.includes(status),
  );
  if (hidden.length > 0)
    return `${count} · ${hidden.map((status) => STATUS_LABELS[status]).join(", ")} hidden`;
  const inProgress = milestones.filter(
    (milestone) => milestone.status === "in_progress",
  ).length;
  return inProgress > 0 ? `${count} · ${inProgress} in progress` : count;
}

export function MilestonesView({
  besideRail = false,
  compactRail = false,
  cwd,
  recents,
  sessions,
  onClose,
  onToggleSidebar,
}: Props) {
  return (
    <div
      role="region"
      aria-label="Milestones"
      data-app-milestones
      className="flex min-h-0 min-w-0 flex-1 flex-col text-content"
    >
      <div
        className="flex h-10 shrink-0 select-none items-center border-b border-stroke"
        data-tauri-drag-region="deep"
      >
        {IS_MAC && compactRail ? <div className="w-4 shrink-0" /> : null}
        {IS_MAC && !besideRail ? <div className="w-[78px] shrink-0" /> : null}
        {besideRail ? null : (
          <OverlayNav onBack={onClose} onToggleSidebar={onToggleSidebar} />
        )}
        <div className="flex min-w-0 flex-1 items-center gap-2 px-3 text-[13px]">
          <Flag
            className="size-3.5 shrink-0 text-content/45"
            strokeWidth={1.75}
          />
          <span className="min-w-0 truncate text-content">Milestones</span>
        </div>
        {IS_MAC ? null : <WindowControls />}
      </div>
      <MilestonesContent cwd={cwd} recents={recents} sessions={sessions} />
    </div>
  );
}

function MilestonesContent({
  cwd,
  recents,
  sessions,
}: Pick<Props, "cwd" | "recents" | "sessions">) {
  const paths = useMemo(
    () => [
      ...new Set(recents.map((recent) => normalizeProjectPath(recent.path))),
    ],
    [recents],
  );
  const [selected, setSelected] = useState(() => {
    const current = cwd ? normalizeProjectPath(cwd) : null;
    return current && paths.includes(current) ? current : (paths[0] ?? null);
  });
  const [lists, setLists] = useState<Record<string, ProjectMilestones>>({});
  const [highlightIds, setHighlightIds] = useState<ReadonlySet<string>>(
    new Set(),
  );
  const [query, setQuery] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  /** The open editor: which row, and what the user typed so far. */
  const [editing, setEditing] = useState<EditorState | null>(null);
  const listsRef = useRef(lists);
  const requests = useRef(new Map<string, number>());
  const timers = useRef(new Set<number>());
  const look = useProjectLook();

  useEffect(() => {
    const pending = timers.current;
    return () => pending.forEach((timer) => window.clearTimeout(timer));
  }, []);

  const load = useCallback(async (path: string) => {
    const request = (requests.current.get(path) ?? 0) + 1;
    requests.current.set(path, request);
    try {
      const next = await list(path);
      if (requests.current.get(path) !== request) return;
      const before = listsRef.current[path];
      listsRef.current = { ...listsRef.current, [path]: next };
      setLists(listsRef.current);
      setErrors(({ [path]: _, ...others }) => others);
      const added = before
        ? agentAdded(before.milestones, next.milestones)
        : [];
      if (added.length === 0) return;
      setHighlightIds((current) => new Set([...current, ...added]));
      const timer = window.setTimeout(() => {
        timers.current.delete(timer);
        setHighlightIds(
          (current) =>
            new Set([...current].filter((id) => !added.includes(id))),
        );
      }, HIGHLIGHT_MS);
      timers.current.add(timer);
    } catch (reason: unknown) {
      if (requests.current.get(path) !== request) return;
      const message = reason instanceof Error ? reason.message : String(reason);
      setErrors((current) => ({ ...current, [path]: message }));
    }
  }, []);

  useEffect(() => {
    for (const path of paths) void load(path);
    return subscribe((path) => {
      if (paths.includes(path)) void load(path);
    });
  }, [paths, load]);

  const lookUpCreator = useCallback(
    (creator: AgentCreator): CreatorLook | undefined => {
      if (creator.kind === "session") {
        const title = sessions.find(
          (session) => session.id === creator.id,
        )?.title;
        return title ? { name: title } : undefined;
      }
      const mono = findMono(creator.id);
      if (!mono) return undefined;
      const { name, mascot, color } = monoLook(mono);
      return { name, mascot: { name: mascot, color } };
    },
    [sessions],
  );

  const needle = query.trim().toLocaleLowerCase();
  const shown = paths.filter((path) =>
    look(path, "size-3.5").name.toLocaleLowerCase().includes(needle),
  );
  const data = selected ? lists[selected] : undefined;
  const heading = selected ? look(selected, "size-[18px]") : null;
  const error = selected ? errors[selected] : undefined;
  const addButton = (
    <button
      type="button"
      onClick={() =>
        setEditing((current) =>
          current?.opened === null ? current : openEditor(null),
        )
      }
      className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md border border-content/15 px-3 text-[12px] text-content/80 hover:border-content/30 hover:bg-content/10 hover:text-content"
    >
      <Plus className="size-3.5" />
      Add milestone
    </button>
  );
  // The editor's callbacks act only on the opening they came from, so a save
  // that settles after Escape cannot close or fail the editor open now.
  const editor =
    editing && selected ? (
      <MilestoneEditor
        state={editing}
        setState={(change) =>
          setEditing((current) =>
            current?.session === editing.session ? change(current) : current,
          )
        }
        onAdd={async (milestone) => {
          await add(selected, milestone, "user");
          await load(selected);
        }}
        onUpdate={async (id, patch) => {
          await update(selected, id, patch, "user");
          await load(selected);
        }}
        onClose={() =>
          setEditing((current) =>
            current?.session === editing.session ? null : current,
          )
        }
      />
    ) : null;

  return (
    <div className="flex min-h-0 min-w-0 flex-1 text-content">
      <aside className="flex w-[240px] shrink-0 flex-col border-r border-stroke">
        <label className="relative flex h-9 shrink-0 items-center border-b border-stroke px-2">
          <Search className="pointer-events-none absolute left-3 size-3 text-content/40" />
          <span className="sr-only">Filter projects</span>
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Filter projects"
            spellCheck={false}
            autoComplete="off"
            className="h-7 w-full bg-transparent pl-5 text-[12px] outline-none placeholder:text-content/40"
          />
        </label>
        <ul className="min-h-0 flex-1 space-y-0.5 overflow-y-auto p-1.5">
          {shown.map((path) => {
            const project = lists[path];
            const { name, icon } = look(path, "size-3.5");
            return (
              <li key={path}>
                <button
                  type="button"
                  aria-current={path === selected ? "true" : undefined}
                  onClick={() => {
                    setSelected(path);
                    setEditing(null);
                  }}
                  className={`flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[13px] font-medium ${
                    path === selected
                      ? "bg-selection text-content"
                      : "text-content/70 hover:bg-content/5"
                  }`}
                >
                  {icon}
                  <span className="min-w-0 truncate">{name}</span>
                  {project && !project.settings.enabled ? (
                    <span className="ml-auto rounded border border-content/12 px-[5px] text-[10px] leading-[15px] text-content/40">
                      Off
                    </span>
                  ) : project?.milestones.length ? (
                    <span className="ml-auto text-[11px] font-normal text-content/40 tabular-nums">
                      {project.milestones.length}
                    </span>
                  ) : null}
                </button>
              </li>
            );
          })}
        </ul>
      </aside>
      <main className="min-h-0 min-w-0 flex-1 overflow-y-auto">
        {error ? (
          <div className="m-4 flex items-start gap-2 rounded-lg border border-red-400/20 bg-red-400/8 px-3 py-2 text-[12px] text-red-300">
            <AlertCircle className="mt-0.5 size-3.5 shrink-0" />
            <span>{error}</span>
          </div>
        ) : null}
        {heading && data ? (
          <div className="flex min-h-full flex-col">
            <header className="mx-auto flex w-full max-w-[780px] items-start gap-3 px-8 pt-[30px] pb-1.5">
              <div className="min-w-0 flex-1">
                <h2 className="flex items-center gap-2.5 text-xl leading-7 font-semibold">
                  {heading.icon}
                  <span className="min-w-0 truncate">{heading.name}</span>
                </h2>
                <p className="mt-0.5 text-xs text-content/45">
                  {projectSummary(data)}
                </p>
              </div>
              {data.milestones.length > 0 ? addButton : null}
            </header>
            {data.milestones.length > 0 || editing?.opened === null ? (
              <MilestoneTimeline
                milestones={data.milestones}
                hiddenStatuses={data.settings.hiddenStatuses}
                highlightIds={highlightIds}
                lookUpCreator={lookUpCreator}
                open={
                  editor && editing
                    ? { id: editing.opened?.id ?? null, editor }
                    : undefined
                }
                onOpen={(id) => {
                  const row = data.milestones.find((m) => m.id === id);
                  if (row) setEditing(openEditor(row));
                }}
              />
            ) : (
              <div className="grid flex-1 place-items-center px-8 pb-24">
                <div className="flex max-w-[380px] flex-col items-center text-center">
                  <span className="grid size-10 place-items-center rounded-[10px] bg-content/6 text-content/60">
                    <Flag className="size-4" strokeWidth={1.75} />
                  </span>
                  <h3 className="mt-3.5 text-sm font-semibold">
                    No milestones for {heading.name} yet
                  </h3>
                  <p className="mt-1.5 text-xs leading-[18px] text-content/45">
                    Write down where this project is going, so you don’t have to
                    remember it. A Mono can add one too: “remember that we want
                    X later”.
                  </p>
                  <div className="mt-4">{addButton}</div>
                </div>
              </div>
            )}
          </div>
        ) : selected && !error ? (
          <div className="grid h-full place-items-center text-content/35">
            <LoaderCircle className="size-4 animate-spin" />
          </div>
        ) : null}
      </main>
    </div>
  );
}

/** The rail's name and mascot (or logo) for a project. */
function useProjectLook() {
  const logos = useTabGroupLogos();
  const [groups] = useState(() => ({
    labels: loadTabGroupLabels(),
    mascots: loadTabGroupMascots(),
    colors: loadTabGroupColors(),
    customColors: loadTabGroupCustomColors(),
  }));
  return useCallback(
    (path: string, size: string) => {
      const key = projectKey(path);
      const seed = projectName(path);
      const logo = resolveTabGroupLogo(key, logos);
      return {
        name: resolveTabGroupLabel(key, groups.labels, seed),
        icon: logo ? (
          <ProjectLogoIcon
            path={logo}
            className={`${size} shrink-0 rounded-sm`}
            imageClassName={size}
          />
        ) : (
          <ProjectMascot
            project={seed}
            color={resolveTabGroupColor(
              key,
              groups.colors,
              groups.customColors,
              seed,
            )}
            name={resolveTabGroupMascot(key, groups.mascots)}
            className={`${size} shrink-0`}
          />
        ),
      };
    },
    [logos, groups],
  );
}
