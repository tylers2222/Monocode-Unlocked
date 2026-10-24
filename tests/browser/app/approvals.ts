import type {
  Approval,
  ApprovalStatus,
} from "../../../src/features/approvals/model/approvals";
import type { Verdict } from "../../../src/features/approvals/model/boardDecision";

export type CardRow = {
  number: number;
  projectPath: string;
  status: "open" | "withdrawn" | Verdict;
};

function statusOf(status: CardRow["status"], at: number): ApprovalStatus {
  if (status === "open") return { state: "open" };
  if (status === "withdrawn") return { state: "withdrawn", withdrawnAt: at };
  return {
    state: "decided",
    verdict: status,
    decidedAt: at,
    note: null,
    chosen: [],
    undelivered: null,
  };
}

export function cardOf(
  { number, projectPath, status }: CardRow,
  filer: Approval["requestedBy"],
): Approval {
  const createdAt = 1_760_000_000_000 + number;
  return {
    id: `00000000-0000-4000-8000-${String(number).padStart(12, "0")}`,
    number,
    projectPath,
    heading: `Approval ${number}`,
    summary: `Card ${number} filed by ${filer.name}.`,
    tradeoffs: { pros: ["Unblocks the Mono"], cons: ["Needs Tyler"] },
    options: [],
    urgency: number % 3 === 0 ? "blocking" : "not_blocking",
    links: [],
    origin: null,
    requestedBy: filer,
    createdAt,
    status: statusOf(status, createdAt + 100_000_000),
  };
}
