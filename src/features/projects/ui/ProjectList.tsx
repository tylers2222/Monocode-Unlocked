import { useCallback, useState, type ReactNode } from "react";
import { projectKey, projectName } from "../../../shared/lib/paths";
import { Search } from "../../../shared/ui/icons";
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
import { useTabGroupLogos } from "../hooks/useTabGroupLogos";
import { ProjectLogoIcon } from "./ProjectLogoIcon";
import { ProjectMascot } from "./ProjectMascot";

export type ProjectLook = (
  path: string,
  size: string,
) => { name: string; icon: ReactNode };

export function ProjectList({
  paths,
  selected,
  look,
  onSelect,
  rowEnd,
}: {
  paths: readonly string[];
  selected: string | null;
  look: ProjectLook;
  onSelect: (path: string) => void;
  rowEnd: (path: string) => ReactNode;
}) {
  const [query, setQuery] = useState("");
  const needle = query.trim().toLocaleLowerCase();
  return (
    <aside className="flex w-[240px] shrink-0 flex-col border-r border-stroke">
      <label className="relative flex h-9 shrink-0 items-center border-b border-stroke px-2">
        <Search className="pointer-events-none absolute left-3 size-3 text-content/40" />
        <span className="sr-only">Filter projects</span>
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Filter projects"
          dir="auto"
          spellCheck={false}
          autoComplete="off"
          className="h-7 w-full bg-transparent pl-5 text-[12px] outline-none placeholder:text-content/40"
        />
      </label>
      <ul className="min-h-0 flex-1 space-y-0.5 overflow-y-auto p-1.5">
        {paths.map((path) => {
          const { name, icon } = look(path, "size-3.5");
          if (!name.toLocaleLowerCase().includes(needle)) return null;
          return (
            <li key={path}>
              <button
                type="button"
                aria-current={path === selected ? "true" : undefined}
                onClick={() => onSelect(path)}
                className={`flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[13px] font-medium ${
                  path === selected
                    ? "bg-selection text-content"
                    : "text-content/70 hover:bg-content/5"
                }`}
              >
                {icon}
                <span dir="auto" className="min-w-0 truncate">
                  {name}
                </span>
                {rowEnd(path)}
              </button>
            </li>
          );
        })}
      </ul>
    </aside>
  );
}

/** The rail's name and mascot (or logo) for a project. */
export function useProjectLook(): ProjectLook {
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
