import { ArrowUpRight } from "../../../shared/ui/icons";
import type { Approval } from "../model/approvals";
import { GigaText } from "./GigaText";

export function OpenGiga({
  card,
  onOpenMono,
  className,
}: {
  card: Approval;
  onOpenMono: (monoId: string) => void;
  className: string;
}) {
  const { id, name } = card.requestedBy;
  if (!id) return null;
  return (
    <button
      type="button"
      onClick={() => onOpenMono(id)}
      className={`inline-flex shrink-0 items-center gap-1.5 rounded-md border border-content/16 font-medium whitespace-nowrap text-content/88 hover:bg-content/6 ${className}`}
    >
      <ArrowUpRight className="size-3" />
      <span>
        Open <GigaText text={name} />
      </span>
    </button>
  );
}
