import type { ReactNode } from "react";
import { IS_MAC } from "../../platform/tauri/platform";
import type { IconComponent } from "../../shared/ui/icons";
import { OverlayNav } from "./TitleBar";
import { WindowControls } from "./WindowControls";

type PageProps = {
  label: string;
  icon: IconComponent;
  besideRail?: boolean;
  compactRail?: boolean;
  onClose?: () => void;
  onToggleSidebar?: () => void;
  /** Open as a title tab in the inset layout, whose tab names the page. */
  tabbed?: boolean;
  children: ReactNode;
} & { [marker: `data-app-${string}`]: true };

export function PageFrame({
  label,
  icon: Icon,
  besideRail = false,
  compactRail = false,
  onClose,
  onToggleSidebar,
  tabbed = false,
  children,
  ...marker
}: PageProps) {
  return (
    <div
      role="region"
      aria-label={label}
      {...marker}
      className="flex min-h-0 min-w-0 flex-1 flex-col text-content"
    >
      {tabbed ? null : (
        <div
          data-app-view-header
          className="flex h-10 shrink-0 select-none items-center border-b border-stroke"
          data-tauri-drag-region="deep"
        >
          {IS_MAC && compactRail ? <div className="w-4 shrink-0" /> : null}
          {IS_MAC && !besideRail ? <div className="w-[78px] shrink-0" /> : null}
          {besideRail ? null : (
            <OverlayNav onBack={onClose} onToggleSidebar={onToggleSidebar} />
          )}
          <div className="flex min-w-0 flex-1 items-center gap-2 px-3 text-[13px]">
            <Icon
              className="size-3.5 shrink-0 text-content/45"
              strokeWidth={1.75}
            />
            <span className="min-w-0 truncate text-content">{label}</span>
          </div>
          {IS_MAC ? null : <WindowControls />}
        </div>
      )}
      {children}
    </div>
  );
}
