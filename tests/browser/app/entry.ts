import { installBackend } from "./backend";

// The real boot path, after the backend is in place: main.tsx calls IPC at import.
installBackend(window.appBackendSeed, window.appBackendHandlers);
void import("../../../src/main");
