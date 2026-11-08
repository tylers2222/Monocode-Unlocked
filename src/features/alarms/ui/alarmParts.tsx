import type { ReactNode } from "react";

/** Design U7 chips: amber = something it was owed did not happen, red = escalated or refused. */
const CHIP_TONE = {
  amber: "bg-amber-400/12 text-amber-400",
  red: "bg-red-400/12 text-red-300",
  gray: "bg-content/7 text-content/60",
} as const;

export function AlarmChip({
  tone,
  children,
}: {
  tone: keyof typeof CHIP_TONE;
  children: ReactNode;
}) {
  return (
    <span
      className={`inline-flex h-[18px] shrink-0 items-center gap-1 whitespace-nowrap rounded-[5px] px-1.5 font-mono text-[11px] ${CHIP_TONE[tone]}`}
    >
      {children}
    </span>
  );
}

/** The card shell an alarm or a sleep hold sits in (U10 notice, carded). */
export function AlarmCardFrame({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <section
      aria-label={label}
      className="flex flex-col gap-1.5 rounded-[10px] border border-content/10 bg-content/3 px-3 pt-2 pb-2.5 font-sans"
    >
      {children}
    </section>
  );
}

export function AlarmFacts({
  labelWidth,
  rows,
}: {
  labelWidth: "w-[92px]" | "w-[80px]";
  rows: { label: string; value: ReactNode }[];
}) {
  return (
    <dl className="flex flex-col gap-0.5 text-[12px] leading-[18px] text-content/70">
      {rows.map((row) => (
        <div key={row.label} className="flex min-w-0 gap-2">
          <dt className={`${labelWidth} shrink-0 text-content/40`}>
            {row.label}
          </dt>
          <dd className="min-w-0 flex-1 break-words">{row.value}</dd>
        </div>
      ))}
    </dl>
  );
}
