import {
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { ChevronDown, Clock, Gauge, Undo2 } from "../../../shared/ui/icons";
import { Popover } from "../../../shared/ui/Popover";
import {
  HARNESS_TITLE,
  type HarnessId,
  type Session,
  type UsageLimit,
} from "../../sessions/model/session";
import { UsageLimitNotice } from "../../sessions/ui/UsageLimitNotice";
import { ModelPicker } from "../../sessions/ui/ModelPicker";
import { HarnessIcon } from "../../sessions/ui/HarnessIcon";
import {
  providerAccounts,
  sameProviderAccountId,
  subscribeProviderAccounts,
  supportsProviderAccounts,
  type ProviderAccount,
} from "../../providers/model/providerAccounts";
import { useProviderAccountUsage } from "../../providers/model/accountUsage";
import {
  identityKey,
  useProviderAccountIdentities,
} from "../../providers/model/providerAccountIdentity";
import { ProviderAccountSubtitle } from "../../providers/ui/ProviderAccountSubtitle";
import {
  meterWindows,
  UsageMeter,
} from "../../providers/ui/ProviderAccountUsage";
import {
  loadRouteGrantDefault,
  ROUTE_GRANT_DEFAULT,
  ROUTE_GRANT_DURATIONS,
  type RouteGrantDuration,
} from "../../settings/model/settings";
import { RouteGrantExpired } from "../../spaces/ui/RouteGrantExpired";
import {
  loadSpaceLogins,
  routeGrantAgainCard,
  routeGrantCard,
  routeGrantExpiry,
  routeGrantOutUntil,
  routeGrantRequest,
  routeGrantYes,
  saveRouteGrant,
  type RouteGrantCard,
  type RouteGrantRequest,
  type SpaceLogins,
} from "../model/monoUsageLimit";

type Props = {
  session: Session;
  onModelChange: (harness: HarnessId, model: string) => void;
  onAccountChange?: (accountId: string) => void;
  onResume: () => void;
  onResumeAtReset: (enabled: boolean) => void;
};

/**
 * The Mono view keeps one pane while the shown Mono changes, so everything a
 * user picks on the card belongs to one session: the card remounts per
 * session. The Spaces read is shared and reloads when the session changes.
 */
export function MonoUsageLimitNotice(props: Props) {
  const [spaces, setSpaces] = useState<SpaceLogins>();
  useEffect(() => {
    loadSpaceLogins().then(setSpaces, () => {});
  }, [props.session.id]);
  return <Notice key={props.session.id} {...props} spaces={spaces} />;
}

function Notice(props: Props & { spaces: SpaceLogins | undefined }) {
  const { session, spaces, onAccountChange, onResume, onResumeAtReset } =
    props;
  const snapshot = useSyncExternalStore(
    subscribeProviderAccounts,
    () =>
      supportsProviderAccounts(session.harness)
        ? JSON.stringify(providerAccounts(session.harness))
        : "[]",
    () => "[]",
  );
  const provider = supportsProviderAccounts(session.harness)
    ? session.harness
    : undefined;
  const usage = useProviderAccountUsage(snapshot, {
    provider,
    enabled: provider != null,
  });
  const [askAgain, setAskAgain] = useState(false);
  if (!session.usageLimit) return null;
  const expired = askAgain
    ? undefined
    : routeGrantExpiry(session, usage.usage, usage.now);
  if (expired)
    return (
      <RouteGrantExpired
        {...expired}
        onGoBack={onResume}
        onAskAgain={() => setAskAgain(true)}
      />
    );
  const card =
    spaces &&
    ((askAgain &&
      routeGrantAgainCard(session, spaces, usage.usage, usage.now)) ||
      routeGrantCard(session, spaces, usage.usage, usage.now));
  if (card && onAccountChange)
    return (
      <RouteGrantPrompt
        session={session}
        limit={session.usageLimit}
        card={card}
        now={usage.now}
        modelPicker={<ChooseModel {...props} />}
        onAccountChange={onAccountChange}
        onResumeAtReset={onResumeAtReset}
        onGoBack={onResume}
      />
    );
  const accounts = (JSON.parse(snapshot) as ProviderAccount[]).filter(
    (account) => !sameProviderAccountId(account.id, session.providerAccountId),
  );
  return (
    <UsageLimitNotice
      variant="mono"
      limit={session.usageLimit}
      providerName={HARNESS_TITLE[session.harness]}
      onResume={onResume}
      onResumeAtReset={onResumeAtReset}
      modelPicker={
        <>
          <ChooseModel {...props} />
          {onAccountChange && accounts.length ? (
            <AccountPicker
              harness={session.harness}
              accounts={accounts}
              onSelect={onAccountChange}
            />
          ) : null}
        </>
      }
    />
  );
}

function ChooseModel({ session, onModelChange }: Props) {
  return (
    <ModelPicker
      harness={session.harness}
      model={session.model}
      values={session.modelSettings}
      project={session.cwd}
      hideSettings
      triggerLabel="Choose another model"
      onChange={onModelChange}
      onSettingsChange={() => {}}
    />
  );
}

/**
 * The usage-limit card with another Space's login to move to, and for how
 * long (approved frame 4, ADE-170). Yes stores the grant first, then moves
 * the session; a refused grant moves nothing and its reason replaces the hint.
 * After Ask again it is frame 15's card: what ended, the same login, no
 * "until reset", and one click back.
 */
function RouteGrantPrompt({
  session,
  limit,
  card,
  now,
  modelPicker,
  onAccountChange,
  onResumeAtReset,
  onGoBack,
}: {
  session: Session;
  limit: UsageLimit;
  card: RouteGrantCard;
  now: number;
  modelPicker: ReactNode;
  onAccountChange: (accountId: string) => void;
  onResumeAtReset: (enabled: boolean) => void;
  onGoBack: () => void;
}) {
  const [defaultDuration] = useState(loadRouteGrantDefault);
  const [duration, setDuration] = useState(startsOn(card, defaultDuration));
  const [picked, setPicked] = useState<string>();
  const to =
    card.offers.find((offer) => offer.account.id === picked)?.account ??
    card.offers[0].account;
  const grant = useGrantSave(onAccountChange);
  return (
    <div
      className="mx-1.5 mb-2 rounded-lg border border-amber-400/20 bg-amber-400/6 px-3 py-2.5 text-[12px] text-content/60"
      data-usage-limit
      role="status"
    >
      <PromptHead card={card} now={now} />
      <div className="pl-5.5">
        <p className="mt-1 leading-relaxed">
          {card.ended?.line ??
            "Use another login for this session? Your conversation is saved and moves with it."}
        </p>
        <LoginChoices card={card} to={to} now={now} onPick={setPicked} />
        <DurationPicker
          fromLabel={card.fromLabel}
          withReset={!card.ended}
          duration={duration}
          defaultDuration={defaultDuration}
          onPick={setDuration}
        />
        {grant.error || !card.ended ? (
          <p className="mt-1 text-[11px] text-content/45">
            {grant.error ??
              "Only logins that still have usage are listed. Default from Settings › Spaces."}
          </p>
        ) : null}
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1.5">
          <button
            type="button"
            disabled={grant.saving}
            onClick={() =>
              grant.save(
                routeGrantRequest(session, to.id, duration, card.resetsAt)!,
              )
            }
            className="flex h-6.5 shrink-0 items-center rounded-md bg-white px-2.5 text-[12px] font-medium text-black hover:bg-white/90 disabled:opacity-50"
          >
            {routeGrantYes(to.label, card.fromLabel, duration)}
          </button>
          {card.ended ? (
            <GoBack fromLabel={card.fromLabel} onGoBack={onGoBack} />
          ) : (
            <>
              <WaitForReset limit={limit} onResumeAtReset={onResumeAtReset} />
              {modelPicker}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

/** Frame 15 has no "until reset": a "reset" default starts on six hours. */
function startsOn(
  card: RouteGrantCard,
  duration: RouteGrantDuration,
): RouteGrantDuration {
  return card.ended && duration === "reset" ? ROUTE_GRANT_DEFAULT : duration;
}

/** Frame 4: when the first login is back; frame 15: what ended. */
function PromptHead({ card, now }: { card: RouteGrantCard; now: number }) {
  return (
    <div className="flex items-center gap-2">
      <Gauge className="size-3.5 shrink-0 text-amber-400" />
      <span className="text-content/85">
        {card.ended?.title ??
          `${card.fromLabel} is out until ${routeGrantOutUntil(card.resetsAt, now)}`}
      </span>
    </div>
  );
}

/** Frame 15: back to the first login in one click. */
function GoBack({
  fromLabel,
  onGoBack,
}: {
  fromLabel: string;
  onGoBack: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onGoBack}
      className="flex h-6 shrink-0 items-center gap-1.5 rounded-md px-1.5 hover:bg-content/10 hover:text-content"
    >
      <Undo2 className="size-3.5" />
      Go back to {fromLabel}
    </button>
  );
}

/** Stores the grant, then moves the session to its login; a refusal stays. */
function useGrantSave(onAccountChange: (accountId: string) => void) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const save = (request: RouteGrantRequest) => {
    setSaving(true);
    setError(undefined);
    saveRouteGrant(request).then(
      () => onAccountChange(request.toAccountId),
      (reason: unknown) => {
        setSaving(false);
        setError(String(reason));
      },
    );
  };
  return { saving, error, save };
}

function LoginChoices({
  card,
  to,
  now,
  onPick,
}: {
  card: RouteGrantCard;
  to: ProviderAccount;
  now: number;
  onPick: (accountId: string) => void;
}) {
  const identities = useProviderAccountIdentities(
    card.offers.map((offer) => offer.account),
  );
  return (
    <div
      role="radiogroup"
      aria-label="Use another login"
      className="mt-2 flex max-w-[520px] flex-col gap-0.5"
    >
      {card.offers.map(({ account, limits }) => (
        <div
          key={account.id}
          className="group pointer-events-none relative flex h-10 items-center gap-2.5 rounded-lg border border-transparent px-2 text-left hover:bg-content/5 has-[[aria-checked=true]]:border-content/10 has-[[aria-checked=true]]:bg-content/5"
        >
          {/* A sibling target keeps the email button out of the radio. */}
          <button
            type="button"
            role="radio"
            aria-checked={account.id === to.id}
            aria-label={account.label}
            onClick={() => onPick(account.id)}
            className="pointer-events-auto absolute inset-0 rounded-lg"
          />
          <span
            aria-hidden
            className="size-3.5 shrink-0 rounded-full border-[1.5px] border-content/30 group-has-[[aria-checked=true]]:border-4 group-has-[[aria-checked=true]]:border-accent"
          />
          <HarnessIcon harness={card.provider} className="size-3.5 shrink-0" />
          <span className="flex min-w-0 flex-1 flex-col">
            <span className="truncate text-content/90">{account.label}</span>
            <ProviderAccountSubtitle
              identity={identities[identityKey(account)]}
              className="truncate text-[10.5px] text-content/40"
            />
          </span>
          <span className="flex shrink-0 gap-3">
            {meterWindows(limits).map((entry) => (
              <UsageMeter
                key={entry.title}
                title={entry.title}
                window={entry.window}
                now={now}
                className="w-28"
              />
            ))}
          </span>
        </div>
      ))}
    </div>
  );
}

function DurationPicker({
  fromLabel,
  withReset,
  duration,
  defaultDuration,
  onPick,
}: {
  fromLabel: string;
  withReset: boolean;
  duration: RouteGrantDuration;
  defaultDuration: RouteGrantDuration;
  onPick: (duration: RouteGrantDuration) => void;
}) {
  return (
    <div className="mt-2 flex items-center gap-2.5">
      <span>For</span>
      <span
        role="radiogroup"
        aria-label="For"
        className="inline-flex gap-0.5 rounded-md border border-content/10 p-0.5"
      >
        {ROUTE_GRANT_DURATIONS.filter(
          (option) => withReset || option !== "reset",
        ).map((option) => (
          <button
            key={option}
            type="button"
            role="radio"
            aria-checked={option === duration}
            onClick={() => onPick(option)}
            className="whitespace-nowrap rounded-[5px] px-2.5 py-1 text-content/50 hover:text-content aria-checked:bg-selection aria-checked:text-content"
          >
            {option === "reset" ? `Until ${fromLabel} resets` : `${option} h`}
            {option === defaultDuration ? (
              <span className="ml-1 text-[10px] text-content/35">default</span>
            ) : null}
          </button>
        ))}
      </span>
    </div>
  );
}

/** "Wait for the reset" arms the resume the stop's own reset time drives. */
function WaitForReset({
  limit,
  onResumeAtReset,
}: {
  limit: UsageLimit;
  onResumeAtReset: (enabled: boolean) => void;
}) {
  if (limit.resetsAt == null) return null;
  const armed = limit.resumeAtReset === true;
  return (
    <button
      type="button"
      aria-pressed={armed}
      onClick={() => onResumeAtReset(!armed)}
      className="flex h-6 shrink-0 items-center gap-1.5 rounded-md px-1.5 hover:bg-content/10 hover:text-content aria-pressed:text-amber-400"
    >
      <Clock className="size-3.5" />
      {armed ? "Resuming at reset" : "Wait for the reset"}
    </button>
  );
}

function AccountPicker({
  harness,
  accounts,
  onSelect,
}: {
  harness: HarnessId;
  accounts: ProviderAccount[];
  onSelect: (accountId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const button = useRef<HTMLButtonElement>(null);
  const menuId = useId();
  const dismiss = (restoreFocus = false) => {
    setOpen(false);
    if (restoreFocus) button.current?.focus();
  };
  const pick = (account: ProviderAccount) => {
    dismiss(true);
    onSelect(account.id);
  };
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const direction = event.key === "ArrowDown" ? 1 : -1;
      setActive(
        (index) => (index + direction + accounts.length) % accounts.length,
      );
    } else if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      setActive(event.key === "Home" ? 0 : accounts.length - 1);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const account = accounts[active];
      if (account) pick(account);
    } else if (event.key === "Escape") {
      event.preventDefault();
      dismiss(true);
    } else if (event.key === "Tab") {
      dismiss();
    }
  };

  return (
    <>
      <button
        ref={button}
        type="button"
        aria-label="Choose another account"
        aria-haspopup="menu"
        aria-expanded={open}
        data-mono-account-picker
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => {
          if (open) dismiss(true);
          else {
            setActive(0);
            setOpen(true);
          }
        }}
        className="flex h-6.5 shrink-0 items-center gap-1 rounded-md bg-selection px-1.5 text-[11px] text-content hover:bg-selection-hover"
      >
        <span>Choose another account</span>
        <ChevronDown
          className={`size-3 shrink-0 text-content/50 ${open ? "rotate-180" : ""}`}
          strokeWidth={1.75}
        />
      </button>
      {open ? (
        <Popover
          anchor={button}
          side="top"
          width={250}
          maxHeight={260}
          autoFocus
          onDismiss={(reason) => dismiss(reason === "escape")}
          ignore="[data-mono-account-picker]"
          role="menu"
          aria-label="Choose another account"
          aria-activedescendant={`${menuId}-${active}`}
          tabIndex={-1}
          onKeyDown={onKeyDown}
          className="overflow-y-auto p-1 font-sans text-content"
        >
          {accounts.map((account, index) => (
            <button
              key={account.id}
              id={`${menuId}-${index}`}
              type="button"
              role="menuitem"
              tabIndex={-1}
              aria-label={account.label}
              onMouseDown={(event) => event.preventDefault()}
              onMouseEnter={() => setActive(index)}
              onClick={() => pick(account)}
              className={`flex h-9 w-full items-center gap-2 rounded-lg px-2 text-left text-[13px] ${index === active ? "bg-selection" : "hover:bg-content/5"}`}
            >
              <HarnessIcon
                harness={harness}
                className="size-3.5 shrink-0 text-content/55"
              />
              <span className="min-w-0 flex-1 truncate">{account.label}</span>
            </button>
          ))}
        </Popover>
      ) : null}
    </>
  );
}
