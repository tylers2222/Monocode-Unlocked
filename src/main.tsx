import React, { useLayoutEffect } from "react";
import ReactDOM from "react-dom/client";
// TEMPORARY: remove with src/debugMonoScroll.ts.
if (import.meta.env.DEV) void import("./debugMonoScroll");
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  activateWindowAppearance,
  initAppearance,
} from "./features/settings/model/appearance";
import { initSounds } from "./features/settings/model/sounds";
import {
  abortQuit,
  askQuitConfirmation,
  commitQuit,
  loadBootWorkspace,
  reportQuitPoll,
} from "./app/model/appLifecycle";
import { homeDir } from "./platform/tauri/fs";
import { setHomeDir } from "./shared/lib/paths";
import { consumeInstalledUpdate } from "./app/model/updateNotice";
import { initializeProviderBinaryPaths } from "./features/providers/model/providerBinaryPaths";
// Lets file commands reach a connected machine for `remote://` paths.
import "./features/connections/model/remoteCommands";
import "./styles/index.css";

performance.mark("monocode:bootstrap");
// Let local boot IPC overlap loading/evaluating the workspace UI.
const appLoaded = import("./app/App");

initAppearance();
initSounds();
// Prime the real home directory before the first render so every `~/` file
// reference resolves consistently. The IPC call is local and failures remain
// best-effort, falling back to inference from a session's cwd.
const homeDirPrimed = homeDir()
  .then(setHomeDir)
  .catch(() => {});
const providerBinaryPathsPrimed = initializeProviderBinaryPaths().catch(
  () => undefined,
);
// Every reader of the Mono roster is synchronous, so load it before the first
// render. Imported lazily, like App, to keep it off the bootstrap chunk. A
// failed load falls back inside, to the roster WebKit storage held.
const monosPrimed = import("./features/monos/model/mono")
  .then((mono) => mono.loadMonoRoster())
  .catch(() => undefined);

function dismissBootSplash() {
  const splash = document.getElementById("boot-splash");
  if (!splash || splash.dataset.dismissed === "1") return;
  splash.dataset.dismissed = "1";
  const fade = () => {
    activateWindowAppearance();
    splash.classList.add("boot-splash-out");
    window.setTimeout(() => {
      splash.remove();
      performance.mark("monocode:ui-ready");
      performance.measure("monocode:navigation-to-ui", {
        start: 0,
        end: "monocode:ui-ready",
      });
    }, 180);
  };
  // useLayoutEffect runs before paint. Two frames later the app is on
  // screen, so the fade reveals UI instead of the desktop blur.
  requestAnimationFrame(() => {
    requestAnimationFrame(fade);
  });
}

function BootGate({ children }: { children: React.ReactNode }) {
  useLayoutEffect(() => {
    dismissBootSplash();
  }, []);
  return children;
}

void listen<number>("quit_poll", (event) => {
  void reportQuitPoll(event.payload);
});
// Scoped to this window on purpose: a global `listen` is registered as `Any`,
// which Tauri matches for every event regardless of the emitter's target, so
// one dialog would become one per window.
void getCurrentWebviewWindow().listen<{ id: number; inFlight: number }>(
  "quit_confirm",
  (event) => {
    void askQuitConfirmation(event.payload.id, event.payload.inFlight);
  },
);
void listen<number>("quit_commit", (event) => {
  void commitQuit(event.payload);
});
void listen("quit_aborted", () => {
  abortQuit();
});

void Promise.all([
  homeDirPrimed,
  providerBinaryPathsPrimed,
  monosPrimed,
  loadBootWorkspace(),
  appLoaded,
]).then(
  ([
    ,
    ,
    ,
    { resumed, monoCompletions, history, historyCwd },
    { default: App },
  ]) => {
    performance.mark("monocode:workspace-ready");
    const installedUpdate = consumeInstalledUpdate();
    ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
      <React.StrictMode>
        <BootGate>
          <App
            resumed={resumed}
            monoCompletions={monoCompletions}
            installedUpdate={installedUpdate}
            history={history}
            historyCwd={historyCwd}
          />
        </BootGate>
      </React.StrictMode>,
    );
  },
);
