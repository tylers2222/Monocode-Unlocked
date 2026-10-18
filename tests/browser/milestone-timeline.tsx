import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { MilestoneTimeline } from "../../src/features/milestones/ui/MilestoneTimeline";
import type { Milestone } from "../../src/features/milestones/model/milestones";
import "../../src/styles/index.css";

// The rows of approved frame M·4; `?live` adds the agent rows of frame M·9.
const M4: Milestone[] = [
  ["done", "Second instance: MonoCode 2", "Own .omp-2, .claude-2 and .codex-2 folders; both apps run side by side.", "2026-10-15"],
  ["done", "Lead and worker Monos (change E)", "Levels, handles on mono_message, waiting-for-reply count.", "2026-10-17"],
  ["in_progress", "Traces in Mono chats (change G)", "Every Mono turn gets the “Worked for 2m 14s” fold line, closed by default.", "2026-10-24"],
  ["in_progress", "Milestones page (change M)", "This page: per-project goals on a simple timeline. Agents add with milestones.add.", "2026-10-31"],
  ["planned", "Giga Mono harness (change H)", "A Giga has only Delegate, Do a task, Message/Reply, Check, Remember and Ask Tyler. Made with a + button; a tool allowlist on the existing harness, no new auth.", "2026-11-21"],
  ["planned", "Promptfoo evals run in CI", "Same suite style as change E; fail the PR when a tool eval regresses.", null],
  ["idea", "Voice notes into Inbox", "Hold a key, talk, and the note lands in Inbox for a Mono to pick up.", null],
  ["idea", "Share one Mono between two Macs", "", null],
].map(([status, title, note, target], order) => ({
  id: `m${order}`,
  projectPath: "/Users/tyler/MonoCode",
  title: title as string,
  note: note as string,
  status: status as Milestone["status"],
  target,
  order: order + 1,
  createdBy: "user",
  createdAt: 0,
  updatedAt: 0,
}));

const params = new URLSearchParams(location.search);
const live = params.has("live");
const milestones: Milestone[] = live
  ? [
      ...M4.map((row) =>
        row.title === "Voice notes into Inbox"
          ? { ...row, createdBy: "session:s1" }
          : row,
      ),
      {
        ...M4[0],
        id: "new",
        title: "Next milestone in the Mono overview",
        note: "So a Mono sees its project’s next goal when it starts a turn.",
        status: "planned",
        target: "2026-11-07",
        order: 0,
        createdBy: "mono:general",
      },
    ]
  : M4;

document.documentElement.classList.toggle(
  "theme-light",
  params.get("theme") === "light",
);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <div className="w-[940px] bg-background-base pb-4 text-content">
      <MilestoneTimeline
        milestones={milestones}
        highlightIds={new Set(live ? ["new"] : [])}
        lookUpCreator={({ kind }) =>
          kind === "mono"
            ? { name: "General", mascot: { name: "invader", color: "#7FA6F0" } }
            : { name: "Execute plan M tasks" }
        }
        today={new Date(2026, 9, 18)}
      />
    </div>
  </StrictMode>,
);
