import { useMemo, useState } from "react";
import { countBadge } from "../../../shared/lib/numbers";
import {
  AlertCircle,
  CheckCircle,
  LoaderCircle,
} from "../../../shared/ui/icons";
import {
  normalizeProjectPath,
  type RecentProject,
} from "../../projects/model/recents";
import { PageFrame } from "../../../app/shell/PageFrame";
import {
  ProjectList,
  useProjectLook,
  type ProjectLook,
} from "../../projects/ui/ProjectList";
import { useApprovalList } from "../hooks/useApprovalList";
import { useOpenCounts } from "../hooks/useApprovalsBadge";
import { board } from "../model/approvals";
import { OpenCard } from "./ApprovalCard";

type Props = {
  besideRail?: boolean;
  compactRail?: boolean;
  /** The project the page opens on: the one with the selected session. */
  cwd?: string;
  recents: readonly RecentProject[];
  onClose: () => void;
  onToggleSidebar?: () => void;
};

export function ApprovalsView({ cwd, recents, ...page }: Props) {
  return (
    <PageFrame
      label="Approvals"
      icon={CheckCircle}
      data-app-approvals
      {...page}
    >
      <ApprovalsContent cwd={cwd} recents={recents} />
    </PageFrame>
  );
}

function ApprovalsContent({ cwd, recents }: Pick<Props, "cwd" | "recents">) {
  const counts = useOpenCounts();
  const paths = useMemo(
    () => [
      ...new Set([
        ...recents.map((recent) => normalizeProjectPath(recent.path)),
        ...Object.keys(counts.byProject),
      ]),
    ],
    [recents, counts],
  );
  const [selected, setSelected] = useState(() => {
    const current = cwd ? normalizeProjectPath(cwd) : null;
    return current && paths.includes(current) ? current : (paths[0] ?? null);
  });
  const listed =
    selected && !paths.includes(selected) ? [...paths, selected] : paths;
  const look = useProjectLook();
  return (
    <div className="flex min-h-0 min-w-0 flex-1 text-content">
      <ProjectList
        paths={listed}
        selected={selected}
        look={look}
        onSelect={setSelected}
        rowEnd={(path) =>
          counts.byProject[path] ? (
            <span className="ml-auto grid h-4 min-w-4 shrink-0 place-items-center rounded-full bg-accent px-1 text-[10px] leading-none font-medium text-white tabular-nums">
              {countBadge(counts.byProject[path])}
            </span>
          ) : null
        }
      />
      <main className="min-h-0 min-w-0 flex-1 overflow-y-auto">
        {selected ? (
          <Board key={selected} project={selected} look={look} />
        ) : null}
      </main>
    </div>
  );
}

function Group({ title, count }: { title: string; count: number }) {
  return (
    <div className="mt-3 flex h-6 items-center gap-1.5 text-xs font-semibold text-content/80">
      {title} <span className="font-normal text-content/55">{count}</span>
    </div>
  );
}

function Board({ project, look }: { project: string; look: ProjectLook }) {
  const list = useApprovalList(project);
  if (!list)
    return (
      <div className="grid h-full place-items-center text-content/35">
        <LoaderCircle className="size-4 animate-spin" />
      </div>
    );
  if ("error" in list)
    return (
      <div className="m-4 flex items-start gap-2 rounded-lg border border-red-400/20 bg-red-400/8 px-3 py-2 text-[12px] text-red-300">
        <AlertCircle className="mt-0.5 size-3.5 shrink-0" />
        <span>{list.error}</span>
      </div>
    );
  const heading = look(project, "size-[18px]");
  const { open, summary } = board(list.cards);
  const now = Date.now();
  return (
    <div className="mx-auto w-full max-w-[780px] px-8 pt-[30px] pb-7">
      <h2 className="flex items-center gap-2.5 text-xl leading-7 font-semibold">
        {heading.icon}
        <span className="min-w-0 truncate">{heading.name}</span>
      </h2>
      <p className="mt-0.5 text-xs text-content/60">{summary}</p>
      <div className="mt-1.5 flex flex-col gap-2.5">
        {open.length > 0 ? <Group title="Open" count={open.length} /> : null}
        {open.map((card) => (
          <OpenCard key={card.id} card={card} look={look} now={now} />
        ))}
      </div>
    </div>
  );
}
