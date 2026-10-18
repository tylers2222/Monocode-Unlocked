# Browser tests

`npm run test:browser` runs these specs in Chromium and WebKit against the Vite dev server. No Tauri backend runs. Each page replaces it with `mockIPC` from `@tauri-apps/api/mocks`.

## Component pages

Each `<name>.html` loads a `<name>.tsx` that mounts one component with its own mocks. Use this for behavior that one component owns, such as tab drag or scroll position.

## Whole-app pages

`app.html` boots the real `src/main.tsx` on the fake backend in `app/backend.ts`. To test a page of the app:

1. Import `test` and `expect` from `./app/fixture`.
2. Call `app.mount(seed, options)`. The seed fills the in-memory store (`home`, `projects`, `sessions`, `monos`, `automations`). The store adds the fields Rust always sends, such as a Mono's `name` and `sortOrder` or a session's `pinned`. `home` defaults to `/Users/test`; the other fields default to empty.
3. Drive the UI with Playwright as a user would. `app-shell.spec.ts` is the example.

For a live update, change the store with `page.evaluate(() => window.appBackend!.store...)`, then call `app.emit(event, payload)` to send the Tauri event that the Rust side would send.

`window.appBackend.calls` lists every command the app sent, so a test can check that a click wrote what it should.

### Options

- `platform`: `"macos"` (the default) or `"linux"`. The app picks its window chrome and shortcut labels from `navigator.platform`.
- `handlers`: handlers for this test only, which replace or add to `HANDLERS`. A handler gets `(store, args)` and may be async. Write each as an arrow or function expression; `mount` throws on a method (`name() {…}`) and names the command. It runs in the page, so it can use only its arguments and page globals. Throw a string to reject the way a Rust `Err(String)` does.

### Loud failures

The fixture fails the test when the app sent a command that has no handler, or when a handler threw something other than a string or answered with data that cannot cross IPC. The record lives in the test process, so a reload does not clear it. Add a missing command to `HANDLERS` in `app/backend.ts` with the answer the Rust command gives. A test that expects a report reads and clears the list with `app.takeReports()`.

## Screenshots

Save design-check screenshots with `page.screenshot({ path: test.info().outputPath("<name>.png") })`. They go to `test-results/<test>-<browser>/`. CI uploads `test-results/` as the `browser-test-results` artifact. Because `mount` renders the macOS app by default, CI shots on Linux show the macOS chrome. There are no `toHaveScreenshot` baselines. These shots are for comparing with the doop frames by eye, not a pixel gate, and Playwright keeps a separate baseline per OS, so each OS would need its own set.
