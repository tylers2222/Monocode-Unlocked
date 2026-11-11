import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  applyAccentColor,
  activateWindowAppearance,
  applyBodyGlass,
  applySidebarBlur,
  applySidebarOpacity,
  applyThemeDarkLightness,
  applyThemePreference,
  applyThemeTint,
  loadAccentColor,
  loadBodyGlass,
  loadSidebarBlur,
  loadSidebarOpacity,
  loadThemeDarkLightness,
  loadThemeHue,
  loadThemePreference,
  loadThemeSaturation,
  watchSystemColorScheme,
} from "../settings/model/appearance";
import { FloatingMonoChat } from "./ui/FloatingMonoChat";
import {
  AlarmsProvider,
  useAlarmsFeed,
  type AlarmsWindowHost,
} from "../alarms/ui/AlarmsProvider";
import { homeDir } from "../../platform/tauri/fs";
import { setHomeDir } from "../../shared/lib/paths";
import { installNativeContextMenuGuard } from "../../shared/lib/nativeContextMenu";
import "../../styles/index.css";

let appearanceKey = "";
function applyFloatingAppearance() {
  const nextKey = JSON.stringify([
    loadAccentColor(),
    loadThemeHue(),
    loadThemeSaturation(),
    loadThemeDarkLightness(),
    loadThemePreference(),
    loadSidebarOpacity(),
    loadSidebarBlur(),
    loadBodyGlass(),
  ]);
  if (nextKey === appearanceKey) return;
  appearanceKey = nextKey;
  document.documentElement.classList.add("is-mac");
  applyAccentColor(loadAccentColor());
  applyThemeTint(loadThemeHue(), loadThemeSaturation());
  applyThemeDarkLightness(loadThemeDarkLightness());
  applySidebarOpacity(loadSidebarOpacity());
  applySidebarBlur(loadSidebarBlur());
  applyBodyGlass(loadBodyGlass());
  const scheme = applyThemePreference(loadThemePreference());
  activateWindowAppearance();
  void getCurrentWindow()
    .setTheme(scheme)
    .catch(() => undefined);
}

applyFloatingAppearance();
watchSystemColorScheme();
installNativeContextMenuGuard();
window.addEventListener("storage", applyFloatingAppearance);
void homeDir()
  .then(setHomeDir)
  .catch(() => undefined);
/**
 * This window shows one Mono's chat: its alarm cards act through Rust as the
 * chat's owner. It holds no other session, so it opens, answers and sends
 * nothing itself (no Tyler landing docks here: its composer is MonoComposer).
 */
const FLOATING_ALARMS: AlarmsWindowHost = {
  openAgent: () => {},
  answerDialog: () => {
    throw new Error("The floating chat holds no other session's dialog");
  },
  sendAsUser: (sessionId) =>
    console.error(`The floating chat sends nothing into ${sessionId}`),
  refused: (what, detail) => console.error(`${what} refused`, detail),
};

function FloatingRoot() {
  const alarms = useAlarmsFeed(FLOATING_ALARMS);
  return (
    <AlarmsProvider feed={alarms}>
      <FloatingMonoChat onShown={applyFloatingAppearance} />
    </AlarmsProvider>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <FloatingRoot />
  </React.StrictMode>,
);
