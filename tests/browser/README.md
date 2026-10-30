# Browser tests

`npm run test:browser` runs these specs in Chromium and WebKit against the Vite dev server. No Tauri backend runs. Each page replaces it with `mockIPC` from `@tauri-apps/api/mocks`.

## Component pages

Each `<name>.html` loads a `<name>.tsx` that mounts one component with its own mocks. Use this for behavior that one component owns, such as tab drag or scroll position.

## Whole-app pages

`app.html` boots the real `src/main.tsx` on the fake backend in `app/backend.ts`. To test a page of the app:

1. Import `test` and `expect` from `./app/fixture`.
2. Call `app.mount(seed, options)`. The seed fills the in-memory store (`home`, `projects`, `sessions`, `monos`, `automations`, `milestones`, `milestoneSettings`). The store adds the fields Rust always sends, such as a Mono's `name` and `sortOrder` or a session's `pinned`. `home` defaults to `/Users/test`; the other fields default to empty.
3. Drive the UI with Playwright as a user would. `app-shell.spec.ts` is the example.

Before the first test, `global-setup.ts` loads `app.html` once in Chromium. The first load makes Vite pre-bundle the deps and transform the whole app, so this keeps that cost out of each CI shard's first test.

For a live update, change the store with `page.evaluate(() => window.appBackend!.store...)`, then call `app.emit(event, payload)` to send the Tauri event that the Rust side would send.

For a second window, open another page in the same context and call the exported `mount(page, seed)` on it, after `page.exposeFunction("appBackendReport", …)`. Each page keeps its own store, so write the change into each page's store and emit the event in each. `milestones-page.spec.ts` lane 6 does this.

`window.appBackend.calls` lists every command the app sent, so a test can check that a click wrote what it should. `window.appBackend.store.blocks` holds each session's saved blocks, as `session_upsert` and `mono_session_upsert` left them.

To run a turn, fake the harness binary in `handlers`. Answer its `harness_resolve_<id>` and `harness_spawn`, and answer each line the app sends with `harness_write` by emitting `harness-stdout` lines. Install `page.clock` before `mount` to control turn timers. `g3-turn-guard.spec.ts` fakes Pi this way.

To check what survives a restart, call `app.restart()`. It sends the `quit_commit` event that Rust's quit coordinator sends, waits for the app's `quit_ready`, and opens the app again on the same store, with the sessions, blocks and workspace snapshot that the quit saved. `g2-giga-folder.spec.ts` uses it.

### Options

- `platform`: `"macos"` (the default) or `"linux"`. The app picks its window chrome and shortcut labels from `navigator.platform`.
- `handlers`: handlers for this test only, which replace or add to `HANDLERS`. A handler gets `(store, args)` and may be async. Write each as an arrow or function expression; `mount` throws on a method (`name() {…}`) and names the command. It runs in the page, so it can use only its arguments and page globals. Throw a string to reject the way a Rust `Err(String)` does.
- `cannotSteer`: harnesses whose adapter the app sees with `canSteer: false`. Codex is not allowed: the app registers its own Codex adapter again over the copy. No shipped adapter reports a ready turn and also refuses steers, so this is the way to reach the app's own can-steer checks on a running turn.

### Loud failures

The fixture fails the test when the app sent a command that has no handler, or when a handler threw something other than a string or answered with data that cannot cross IPC. The record lives in the test process, so a reload does not clear it. Add a missing command to `HANDLERS` in `app/backend.ts` with the answer the Rust command gives. A test that expects a report reads and clears the list with `app.takeReports()`.

## Screenshots

Save design-check screenshots with `page.screenshot({ path: test.info().outputPath("<name>.png") })`. They go to `test-results/<test>-<browser>/`. CI uploads each shard's `test-results/` as the `browser-test-results-<shard>` artifact. Because `mount` renders the macOS app by default, CI shots on Linux show the macOS chrome. There are no `toHaveScreenshot` baselines. These shots are for comparing with the doop frames by eye, not a pixel gate, and Playwright keeps a separate baseline per OS, so each OS would need its own set.
