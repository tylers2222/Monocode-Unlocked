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
  // App.tsx registers the built-in adapters at import, each only once, so the
  // copies registered here are the ones the app uses.
  registerBuiltinHarnesses();
  for (const id of cannotSteer)
    registerHarness({ ...getHarness(id)!, canSteer: false });
}
void import("../../../src/main");
