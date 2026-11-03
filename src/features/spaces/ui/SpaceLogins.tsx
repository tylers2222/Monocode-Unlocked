import { useMemo } from "react";
import { HarnessIcon } from "../../sessions/ui/HarnessIcon";
import { useMaskEmails } from "../../settings/model/displayPrefs";
import {
  identityKey,
  useProviderAccountIdentities,
  type ProviderAccountIdentity,
} from "../../providers/model/providerAccountIdentity";
import {
  providerAccountLabel,
  type ProviderAccount,
} from "../../providers/model/providerAccounts";
import { spaceLoginId } from "../model/activeSpace";
import {
  SPACE_LOGIN_PROVIDERS,
  type SpaceLogin,
  type SpaceLoginProvider,
} from "../model/spaces";

const PROVIDER_NAME: Record<SpaceLoginProvider, string> = {
  claude: "Claude",
  codex: "Codex",
};

/**
 * Both logins of Space `spaceId`, one line each, Claude then Codex: the
 * provider icon and the login's email (with `withPlan`, "email · plan"). A
 * provider the Space has no login for reads "No Codex login", muted.
 */
export function SpaceLogins({
  spaceId,
  logins,
  withPlan = false,
}: {
  spaceId: string;
  logins: readonly SpaceLogin[];
  withPlan?: boolean;
}) {
  const accounts = useMemo(
    () =>
      SPACE_LOGIN_PROVIDERS.map((provider): ProviderAccount | undefined => {
        const id = spaceLoginId(logins, spaceId, provider);
        return id ? { id, provider, label: id } : undefined;
      }),
    [logins, spaceId],
  );
  const identities = useProviderAccountIdentities(
    accounts.filter(
      (account): account is ProviderAccount => account !== undefined,
    ),
  );
  return SPACE_LOGIN_PROVIDERS.map((provider, index) => {
    const account = accounts[index];
    return (
      <span
        key={provider}
        data-space-login={provider}
        data-missing={account ? undefined : ""}
        className={`flex min-w-0 items-center gap-[5px] text-[10.5px] leading-[15px] ${
          account ? "text-content/45" : "text-content/28"
        }`}
      >
        <HarnessIcon
          harness={provider}
          className={`size-[11px] shrink-0${account ? "" : " opacity-35 grayscale"}`}
        />
        <span className="min-w-0 truncate">
          {account ? (
            <LoginText
              identity={identities[identityKey(account)]}
              fallback={providerAccountLabel(provider, account.id)}
              withPlan={withPlan}
            />
          ) : (
            `No ${PROVIDER_NAME[provider]} login`
          )}
        </span>
      </span>
    );
  });
}

function LoginText({
  identity,
  fallback,
  withPlan,
}: {
  identity: ProviderAccountIdentity | null | undefined;
  fallback: string;
  withPlan: boolean;
}) {
  const masked = useMaskEmails();
  const email = identity?.email;
  const plan = withPlan ? identity?.plan : undefined;
  return (
    <>
      {email ? (
        <span className={masked ? "select-none blur-[5px]" : undefined}>
          {email}
        </span>
      ) : (
        fallback
      )}
      {plan ? ` · ${plan}` : null}
    </>
  );
}
