# Browser tests

`npm run test:browser` runs these specs in Chromium and WebKit against the Vite dev server. No Tauri backend runs. Each page replaces it with `mockIPC` from `@tauri-apps/api/mocks`.

## Component pages

Each `<name>.html` loads a `<name>.tsx` that mounts one component with its own mocks. Use this for behavior that one component owns, such as tab drag or scroll position.

## Whole-app pages

`app.html` boots the real `src/main.tsx` on the fake backend in `app/backend.ts`. To test a page of the app:

1. Import `test` and `expect` from `./app/fixture`.
2. Call `app.mount(seed)`. The seed fills the in-memory store (`projects`, `sessions`, `monos`, `automations`). Fields you leave out are empty.
3. Drive the UI with Playwright as a user would. `app-shell.spec.ts` is the example.

For a live update, change the store with `page.evaluate(() => window.appBackend!.store...)`, then call `app.emit(event, payload)` to send the Tauri event that the Rust side would send.

`window.appBackend.calls` lists every command the app sent, so a test can check that a click wrote what it should.

A command that has no handler rejects, and the fixture fails the test with the list of those commands. Add each to `HANDLERS` in `app/backend.ts` with the answer the Rust command gives.

## Screenshots

Save design-check screenshots with `page.screenshot({ path: test.info().outputPath("<name>.png") })`. They go to `test-results/<test>-<browser>/`. CI uploads `test-results/` as the `browser-test-results` artifact. There are no `toHaveScreenshot` baselines, because macOS and Linux render fonts differently and the CI job runs only on Linux.
