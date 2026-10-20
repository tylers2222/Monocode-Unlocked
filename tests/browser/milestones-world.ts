import type {
  Milestone,
  MilestoneStatus,
} from "../../src/features/milestones/model/milestones";
import type { FakeSeed } from "./app/fixture";

/**
 * The Milestones world of approved frames M·4 and M·9 (doop canvas
 * oRuRCkta7A, page "Change M: Milestones"), shared by the page spec and the
 * M-11 frame screenshots.
 */
export const ORBIT = "/Users/test/orbit";
export const SITE = "/Users/test/orbit-site";
export const REVIEW = "/Users/test/orbit-review";
export const CHANGED = "monocode:milestones-changed";

export function milestone(
  projectPath: string,
  order: number,
  status: MilestoneStatus,
  title: string,
  fields: Partial<Milestone> = {},
): Milestone {
  return {
    id: `${projectPath.split("/").pop()}-${order}`.padEnd(36, "0"),
    projectPath,
    title,
    note: "",
    status,
    target: null,
    order,
    createdBy: "user",
    createdAt: order,
    updatedAt: order,
    ...fields,
  };
}

/** The rows of approved frame M·4. */
export const EVERY_STATUS = [
  milestone(ORBIT, 0, "done", "Second instance: MonoCode 2", {
    note: "Own .omp-2, .claude-2 and .codex-2 folders; both apps run side by side.",
    target: "2026-10-15",
  }),
  milestone(ORBIT, 1, "done", "Lead and worker Monos (change E)", {
    note: "Levels, handles on mono_message, waiting-for-reply count.",
    target: "2026-10-17",
  }),
  milestone(ORBIT, 2, "in_progress", "Traces in Mono chats (change G)", {
    note: "Every Mono turn gets the “Worked for 2m 14s” fold line, closed by default.",
    target: "2026-10-24",
  }),
  milestone(ORBIT, 3, "in_progress", "Milestones page (change M)", {
    note: "This page: per-project goals on a simple timeline. Agents add with milestones.add.",
    target: "2026-10-31",
  }),
  milestone(ORBIT, 4, "planned", "Giga Mono harness (change H)", {
    note: "A Giga has only Delegate, Do a task, Message/Reply, Check, Remember and Ask Tyler. Made with a + button; a tool allowlist on the existing harness, no new auth.",
    target: "2026-11-21",
  }),
  milestone(ORBIT, 5, "planned", "Promptfoo evals run in CI", {
    note: "Same suite style as change E; fail the PR when a tool eval regresses.",
  }),
  milestone(ORBIT, 6, "idea", "Voice notes into Inbox", {
    note: "Hold a key, talk, and the note lands in Inbox for a Mono to pick up.",
    createdBy: "session:s-plan",
  }),
  milestone(ORBIT, 7, "idea", "Share one Mono between two Macs"),
  milestone(SITE, 0, "planned", "Signed Windows build"),
  milestone(SITE, 1, "idea", "Linux tray icon"),
  milestone(SITE, 2, "idea", "Portable settings"),
];

/** What `cli.sh Alpha milestones.add` writes: a Mono's row in Planned. */
export const AGENT_ROW = milestone(
  ORBIT,
  8,
  "planned",
  "Next milestone in the Mono overview",
  {
    note: "So a Mono sees its project’s next goal when it starts a turn.",
    target: "2026-11-07",
    createdBy: "mono:general",
  },
);

export function seed(milestones: Milestone[], extra: FakeSeed = {}): FakeSeed {
  return {
    projects: [ORBIT, SITE, REVIEW],
    sessions: [
      {
        id: "s-plan",
        cwd: ORBIT,
        harness: "claude",
        model: "sonnet",
        runtimeMode: "supervised",
        title: "Execute plan M tasks",
        createdAt: 1,
        updatedAt: 2,
      },
      {
        id: "s-site",
        cwd: SITE,
        harness: "claude",
        model: "sonnet",
        runtimeMode: "supervised",
        title: "Fix the release script",
        createdAt: 1,
        updatedAt: 1,
      },
    ],
    monos: [
      {
        id: "general",
        role: "mono",
        data: { name: "General", mascot: "invader", projects: [ORBIT] },
      },
    ],
    milestones,
    ...extra,
  };
}
