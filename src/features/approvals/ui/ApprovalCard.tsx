import { useSyncExternalStore, type ReactNode } from "react";
import { UserRemove } from "../../../shared/ui/icons";
import {
  findMono,
  listMonos,
  monoLook,
  monosSnapshot,
  subscribeMonos,
  type Mono,
} from "../../monos/model/mono";
import { PixelMascot } from "../../projects/ui/PixelMascot";
import type { ProjectLook } from "../../projects/ui/ProjectList";
import { quoteText } from "../../sessions/ui/composerIntent";
import { GigaText } from "./GigaText";
import { ago, filerRemoved, type Approval } from "../model/approvals";
import { CardBody } from "./CardBody";

export const PILL =
  "inline-flex h-5 items-center gap-1.5 rounded-[5px] px-[7px] text-[11px] font-semibold whitespace-nowrap";

function Urgency({ card }: { card: Approval }) {
  return card.urgency === "blocking" ? (
    <span className={`${PILL} bg-red-400/13 text-red-300`}>
      <i className="size-1.5 rounded-full bg-red-500" />
      Blocking
    </span>
  ) : (
    <span className={`${PILL} bg-content/7 text-content/72`}>
      <i className="size-1.5 rounded-full border-[1.5px] border-content/55" />
      Not blocking
    </span>
  );
}

function Mono({
  mono,
  name,
  removed = false,
}: {
  mono: Mono | undefined;
  name: string;
  removed?: boolean;
}) {
  const look = mono && monoLook(mono);
  return (
    <span className="inline-flex items-center gap-[5px] text-content/85">
      {look ? (
        <PixelMascot
          name={look.mascot}
          color={look.color}
          still
          className="size-3 shrink-0"
        />
      ) : null}
      {removed ? (
        <del className="text-content/60 decoration-content/40">
          <GigaText text={name} />
        </del>
      ) : (
        <GigaText text={name} />
      )}
    </span>
  );
}

export function CardHead({
  card,
  look,
  statusPill,
  time,
}: {
  card: Approval;
  look: ProjectLook;
  statusPill?: ReactNode;
  time: string;
}) {
  useSyncExternalStore(subscribeMonos, monosSnapshot);
  const project = look(card.projectPath, "size-3");
  const removed = filerRemoved(card);
  const origin = card.origin;
  return (
    <>
      <div className="flex h-5 items-center gap-2 text-[11px]">
        {statusPill}
        <Urgency card={card} />
        <span className="font-mono text-content/55">APR-{card.number}</span>
        <span className="ml-auto whitespace-nowrap text-content/60">
          {time}
        </span>
      </div>
      <GigaText
        text={card.heading}
        className={`mt-2 block text-base leading-[22px] font-semibold ${
          card.status.state === "withdrawn" ? "text-content/62" : ""
        }`}
      />
      <div className="mt-1.5 flex flex-wrap items-center gap-x-3.5 gap-y-1 text-xs text-content/60">
        <span className="inline-flex items-center gap-[5px] text-content/85">
          {project.icon}
          <span dir="auto">{project.name}</span>
        </span>
        <span>
          filed by{" "}
          <Mono
            mono={removed ? undefined : findMono(card.requestedBy.id ?? "")}
            name={card.requestedBy.name}
            removed={removed}
          />
          {removed ? (
            <span className="ml-1.5 text-amber-400">removed</span>
          ) : null}
        </span>
        {origin ? (
          <span>
            raised by{" "}
            <Mono
              mono={listMonos().find((mono) => monoLook(mono).name === origin)}
              name={origin}
            />
          </span>
        ) : null}
      </div>
    </>
  );
}

export function OpenCard({
  card,
  look,
  now,
}: {
  card: Approval;
  look: ProjectLook;
  now: number;
}) {
  return (
    <article
      aria-label={quoteText(card.heading)}
      className={`overflow-hidden rounded-[10px] border bg-content/3 ${
        card.urgency === "blocking" ? "border-red-300/30" : "border-content/11"
      }`}
    >
      <div className="px-4 pt-3.5 pb-4">
        <CardHead card={card} look={look} time={ago(card.createdAt, now)} />
        {filerRemoved(card) ? <RemovedNote /> : null}
        <GigaText
          text={card.summary}
          className="mt-3 block text-[13px] leading-5 text-content/82"
        />
        <CardBody card={card} />
      </div>
    </article>
  );
}

function RemovedNote() {
  return (
    <div className="mt-3 flex items-start gap-2.5 rounded-lg border border-amber-400/28 bg-amber-400/8 px-3 py-2.5">
      <UserRemove className="mt-0.5 size-3.5 shrink-0 text-amber-400" />
      <div>
        <div className="text-[13px] leading-[19px] font-semibold">
          The Giga that filed this was removed. Handle this manually.
        </div>
        <div className="mt-0.5 text-xs leading-[18px] text-content/72">
          You can still decide it to close the card. Nothing is sent: there is
          no Giga to receive it.
        </div>
      </div>
    </div>
  );
}
