import { installBackend } from "./backend";

// The real boot path, after the backend is in place: app modules may call IPC
// at import, so each loads only after installBackend.
installBackend(window.appBackendSeed, window.appBackendHandlers);
const cannotSteer = window.appBackendCannotSteer ?? [];
if (cannotSteer.length) {
  const { registerBuiltinHarnesses } =
    await import("../../../src/integrations/harness/core/register");
  const { getHarness, registerHarness } =
    await import("../../../src/integrations/harness/core/registry");
  // App.tsx registers the built-in adapters again at import. Each one except
  // codex skips a second registration, so these copies are the ones the app
  // uses; `SteerlessHarness` leaves codex out.
  registerBuiltinHarnesses();
  for (const id of cannotSteer)
    registerHarness({ ...getHarness(id)!, canSteer: false });
}
void import("../../../src/main");
