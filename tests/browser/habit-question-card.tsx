import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { HabitQuestionCard } from "../../src/features/sessions/ui/HabitQuestionCard";
import "../../src/styles/index.css";

// The real card, open, in a chat that can answer and in one that cannot
// (frame A4b: a Mono chat whose worktree was removed).
const asked = {
  runId: "run-1",
  runAt: 0,
  prompt: {
    requestId: 1,
    questions: [
      {
        id: "reset",
        prompt: "Spend a saved Claude rate-limit reset?",
        multiSelect: false,
        allowCustom: false,
        options: [
          { id: "yes", label: "Yes", description: "Use one saved reset now." },
          {
            id: "no",
            label: "No",
            description: "Wait for the limit to reset.",
          },
        ],
      },
    ],
  },
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <div className="text-content">
      <div data-chat="answerable">
        <HabitQuestionCard
          asked={asked}
          habit="Feature check-in"
          runFrom="8:15 pm"
          onReply={() => {}}
          onOpenRun={() => {}}
        />
      </div>
      <div data-chat="read-only">
        <HabitQuestionCard
          asked={asked}
          habit="Feature check-in"
          runFrom="8:15 pm"
          onOpenRun={() => {}}
        />
      </div>
    </div>
  </StrictMode>,
);
