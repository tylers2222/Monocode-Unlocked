import { openUrl } from "@tauri-apps/plugin-opener";
import type { ReactNode } from "react";
import { GitPullRequest, Minus, Palette, Plus } from "../../../shared/ui/icons";
import type { Approval } from "../model/approvals";
import { GigaText } from "./GigaText";

const LINK_ICON = { pr: GitPullRequest, doop: Palette };

function Section({
  title,
  children,
}: {
  title: ReactNode;
  children: ReactNode;
}) {
  return (
    <>
      <div className="mt-3.5 text-xs font-semibold text-content/72">
        {title}
      </div>
      <div className="mt-1.5">{children}</div>
    </>
  );
}

function Tradeoffs({ card }: { card: Approval }) {
  const column = (title: string, items: string[], plus: boolean) => (
    <div
      role="group"
      aria-label={title}
      className="rounded-lg bg-content/4 px-3 pt-[9px] pb-2.5"
    >
      <div
        className={`mb-1 flex items-center gap-1.5 text-[11px] font-semibold ${
          plus ? "text-diff-add-fg" : "text-diff-del-fg"
        }`}
      >
        {plus ? (
          <Plus className="size-[11px]" />
        ) : (
          <Minus className="size-[11px]" />
        )}
        {title}
      </div>
      {items.length === 0 ? (
        <p className="text-[12.5px] leading-[19px] text-content/60">
          None given
        </p>
      ) : (
        <ul className="space-y-[3px]">
          {items.map((item, index) => (
            <li
              key={index}
              className="relative pl-3 text-[12.5px] leading-[19px] text-content/80 before:absolute before:top-2 before:left-px before:size-1 before:rounded-full before:bg-content/45"
            >
              <GigaText text={item} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
  const { pros, cons } = card.tradeoffs;
  if (pros.length === 0 && cons.length === 0) return null;
  return (
    <Section title="Tradeoffs">
      <div className="grid grid-cols-2 gap-2">
        {column("Pros", pros, true)}
        {column("Cons", cons, false)}
      </div>
    </Section>
  );
}

function Options({ card }: { card: Approval }) {
  return (
    <Section
      title={
        <>
          Options{" "}
          <span className="font-normal text-content/60">
            · tick one or more
          </span>
        </>
      }
    >
      <div className="divide-y divide-content/7 overflow-hidden rounded-lg border border-content/10">
        {card.options.map((option, index) => (
          <div
            key={option.label}
            className="grid grid-cols-[14px_16px_1fr] items-start gap-2.5 px-3 py-[9px]"
          >
            <span className="text-right font-mono text-[11px] leading-5 text-content/55">
              {index + 1}
            </span>
            <span className="mt-0.5 size-4 rounded-[4px] border-[1.5px] border-content/50" />
            <div>
              <div className="flex items-center gap-2 text-[13px] leading-5 font-medium">
                <GigaText text={option.label} />
                {option.recommended ? (
                  <span className="rounded bg-accent/12 px-1.5 text-[10px] leading-4 font-semibold whitespace-nowrap text-(--link-color) dark:bg-accent/16 dark:text-accent">
                    Recommended
                  </span>
                ) : null}
              </div>
              {option.description ? (
                <GigaText
                  text={option.description}
                  className="mt-px block text-xs leading-[18px] text-content/62"
                />
              ) : null}
            </div>
          </div>
        ))}
      </div>
    </Section>
  );
}

function LinearMark() {
  return (
    <svg aria-hidden viewBox="0 0 24 24" className="size-3" fill="currentColor">
      <path d="M2.886 4.18A11.98 11.98 0 0 1 11.99 0C18.624 0 24 5.376 24 12.009c0 3.64-1.62 6.903-4.18 9.105L2.887 4.18ZM1.817 5.626l16.556 16.556q-.787.496-1.65.866L.951 7.277q.371-.863.866-1.65ZM.322 9.163l14.515 14.515q-1.066.26-2.195.322L0 11.358a12 12 0 0 1 .322-2.195m-.17 4.862l9.823 9.824a12.02 12.02 0 0 1-9.824-9.824Z" />
    </svg>
  );
}

function Links({ card }: { card: Approval }) {
  return (
    <Section title="Links">
      <div className="flex flex-wrap gap-1.5">
        {card.links.map((link, index) => {
          const Icon =
            link.kind === "linear" ? LinearMark : LINK_ICON[link.kind];
          return (
            <button
              type="button"
              key={`${index}:${link.url}`}
              onClick={() => void openUrl(link.url)}
              className="inline-flex h-6 items-center gap-1.5 rounded-md bg-content/6 px-2 text-xs whitespace-nowrap text-content/85 hover:bg-content/10"
            >
              <span className="text-content/60">
                <Icon className="size-3" />
              </span>
              <GigaText text={link.title ?? link.url} />
            </button>
          );
        })}
      </div>
    </Section>
  );
}

export function CardBody({ card }: { card: Approval }) {
  return (
    <>
      <Tradeoffs card={card} />
      {card.options.length > 0 ? <Options card={card} /> : null}
      {card.links.length > 0 ? <Links card={card} /> : null}
    </>
  );
}
