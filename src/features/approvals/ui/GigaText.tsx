import { quoteText } from "../../sessions/ui/composerIntent";

export function GigaText({
  text,
  className,
}: {
  text: string;
  className?: string;
}) {
  return (
    <span dir="auto" className={className}>
      {quoteText(text)}
    </span>
  );
}
