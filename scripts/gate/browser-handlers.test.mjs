import assert from "node:assert/strict";
import { test } from "node:test";
import { newUnhandled, unhandledCommands } from "./browser-handlers.mjs";

const backend = ["tests/browser/app/backend.ts", "const HANDLERS = { known_command: none };"];
const known = ["src/a.ts", 'await invoke("known_command");'];

test("job 125406442573: a new invoke with no handler is a problem, and the message names the fix", () => {
  const after = [backend, ["src/a.ts", 'await invoke("known_command");\nawait invoke<number>("spaces_settings_get");']];
  const problems = newUnhandled([backend, known], after);
  assert.equal(problems.length, 1);
  assert.match(problems[0], /src\/a\.ts: invokes spaces_settings_get/);
  assert.match(problems[0], /HANDLERS in tests\/browser\/app\/backend\.ts/);
});

test("a handler a rebase dropped is a problem", () => {
  const dropped = ["tests/browser/app/backend.ts", "const HANDLERS = {};"];
  assert.match(newUnhandled([backend, known], [dropped, known])[0], /invokes known_command/);
});

test("a gap the base already has is allowed, and fixing it is allowed", () => {
  const gap = [backend, ["src/a.ts", 'invoke("old_gap")']];
  assert.deepEqual(newUnhandled(gap, gap), []);
  assert.deepEqual(newUnhandled(gap, [backend, known]), []);
});

test("a spec's own override counts as a handler", () => {
  const after = [backend, ["src/a.ts", 'invoke("spec_only")'], ["tests/browser/x.spec.ts", "installBackend({}, { spec_only: none })"]];
  assert.deepEqual(newUnhandled([backend], after), []);
});

test("a longer word holding the command name is not a handler", () => {
  const files = [["tests/browser/x.ts", "git_checkout_all"], ["src/a.ts", 'invoke("git_checkout")']];
  assert.deepEqual([...unhandledCommands(files).keys()], ["git_checkout"]);
});

test("invoke finds a generic type, a line break and single quotes, not a test file", () => {
  const files = [
    ["src/a.ts", "invoke<Result<(string)[]>>(\n  'one')"],
    ["src/a.test.ts", 'invoke("two")'],
    ["src/b.realdb.ts", 'invoke("three")'],
    ["src/d.ts", 'revoke("five"); invoke(name)'],
  ];
  assert.deepEqual([...unhandledCommands(files).keys()], ["one"]);
});
