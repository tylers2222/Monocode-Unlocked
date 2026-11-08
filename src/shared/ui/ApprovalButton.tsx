import type { ReactNode } from "react";

/** The transcript's Allow / Deny buttons: the first answer filled, the rest quiet. */
export function ApprovalButton({
  primary,
  disabled,
  children,
  onClick,
}: {
  primary: boolean;
  disabled?: boolean;
  children: ReactNode;
  onClick?: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={`rounded-md px-2.5 py-0.5 text-[11px] disabled:cursor-default disabled:opacity-40 ${
        primary
          ? "bg-content text-background-base hover:bg-content/80"
          : "bg-content/10 text-content/70 hover:bg-content/20"
      }`}
    >
      {children}
    </button>
  );
}
