// Breaks each G-2 rule in src/ in turn and checks that its test in
// g2-giga-folder.spec.ts fails, then puts the file back.
//
//   node tests/browser/g2-giga-folder.mutate.mjs [mutation-name ...]
//
// Prints each mutation's failure text. Exits 1 if the spec fails on clean
// code or a mutation survives. Runs in Chromium only, one test per mutation,
// so port 1435 must be free. Ctrl-C puts the mutated file back.
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";

const SPEC = "tests/browser/g2-giga-folder.spec.ts";

const TESTS = {
  giga: "a session a Giga starts runs in the Giga's folder and its tab stays in the MonoCode deck",
  mono: "a session a plain Mono starts runs at the project root",
  checkpoint:
    "a Giga session's checkpoint and changed-files card stay at the MonoCode checkout",
  secondOpinion: "a second opinion on a Giga session runs in the Giga's folder",
  handoff: "a handoff from a Giga session runs in the Giga's folder",
  restart:
    "after a restart a Giga session keeps its folder and its tab in the MonoCode deck",
};

const MUTATIONS = [
  {
    name: "giga-start-gets-no-folder",
    rule: "a session a Giga's chat starts gets the Giga's folder",
    test: TESTS.giga,
    edits: [
      {
        file: "src/features/agent-app/model/agentApp.ts",
        find: "const workFolder = listedWorktree ? undefined : ownFolder;",
        replace: "const workFolder = undefined;",
      },
    ],
  },
  {
    name: "folder-as-worktree",
    rule: "the folder is workFolder, not worktreeCwd (G-2 round-1 major)",
    test: TESTS.giga,
    edits: [
      {
        file: "src/features/quick-composer/model/quickWorkspace.ts",
        find: "...(launch.workFolder ? { workFolder: launch.workFolder } : {}),",
        replace:
          "...(launch.workFolder ? { worktreeCwd: launch.workFolder } : {}),",
      },
    ],
  },
  {
    name: "any-mono-with-a-slug",
    rule: "only a Giga's slug names a folder; a former Giga is a plain Mono",
    test: TESTS.mono,
    edits: [
      {
        file: "src/features/monos/model/mono.ts",
        find: "...(mono.giga && mono.gigaSlug ? { gigaSlug: mono.gigaSlug } : {}),",
        replace: "...(mono.gigaSlug ? { gigaSlug: mono.gigaSlug } : {}),",
      },
    ],
  },
  {
    name: "checkpoint-at-agent-folder",
    rule: "turn checkpoints use the working copy, not the agent folder (G-2 round-2 major)",
    test: TESTS.checkpoint,
    edits: [
      {
        file: "src/app/App.tsx",
        find: "trackSessionEdits(sessionId, workCwd, agentCwd, event);",
        replace: "trackSessionEdits(sessionId, agentCwd, agentCwd, event);",
      },
      {
        file: "src/app/App.tsx",
        find: "? await beginSessionTurn(\n                sessionId,\n                workCwd,",
        replace:
          "? await beginSessionTurn(\n                sessionId,\n                agentCwd,",
      },
      {
        file: "src/app/App.tsx",
        find: "await finishSessionTurn(sessionId, workCwd, {",
        replace: "await finishSessionTurn(sessionId, agentCwd, {",
        all: true,
      },
    ],
  },
  {
    name: "edit-paths-not-joined-to-folder",
    rule: "relative edit paths are joined onto the agent folder (checkpointPaths)",
    test: TESTS.checkpoint,
    edits: [
      {
        file: "src/app/App.tsx",
        find: "  const paths = checkpointPaths(\n    [\n      ...(event.paths ?? []),\n      ...(event.preview?.path ? [event.preview.path] : []),\n    ],\n    agentCwd,\n    cwd,\n  );",
        replace:
          "  const paths = [\n    ...(event.paths ?? []),\n    ...(event.preview?.path ? [event.preview.path] : []),\n  ];",
      },
    ],
  },
  {
    name: "second-opinion-drops-folder",
    rule: "a second opinion keeps the source's workFolder (G-2 round-2 minor)",
    test: TESTS.secondOpinion,
    edits: [
      {
        file: "src/app/App.tsx",
        find: "        ...sessionWorkingCopy(source),\n        modelSettings: mergeModelSettings(\n          resolveModel(harness, model),\n          modelSettings,\n        ),\n        title: formatSessionTitle(harness, SECOND_OPINION_TITLE),",
        replace:
          "        worktreeCwd: source.worktreeCwd,\n        branch: source.branch,\n        modelSettings: mergeModelSettings(\n          resolveModel(harness, model),\n          modelSettings,\n        ),\n        title: formatSessionTitle(harness, SECOND_OPINION_TITLE),",
      },
    ],
  },
  {
    name: "handoff-drops-folder",
    rule: "a handoff keeps the source's workFolder (G-2 round-2 minor)",
    test: TESTS.handoff,
    edits: [
      {
        file: "src/app/App.tsx",
        find: '        ...sessionWorkingCopy(source),\n        modelSettings: mergeModelSettings(\n          resolveModel(harness, model),\n          modelSettings,\n        ),\n        title: formatSessionTitle(\n          harness,\n          display === "New session" ? HANDOFF_TITLE : display,',
        replace:
          '        worktreeCwd: source.worktreeCwd,\n        branch: source.branch,\n        modelSettings: mergeModelSettings(\n          resolveModel(harness, model),\n          modelSettings,\n        ),\n        title: formatSessionTitle(\n          harness,\n          display === "New session" ? HANDOFF_TITLE : display,',
      },
    ],
  },
  {
    name: "restore-drops-folder",
    rule: "a restored session keeps its saved workFolder",
    test: TESTS.restart,
    edits: [
      {
        file: "src/features/sessions/data/sessionStore.ts",
        find: "    ...(record.workFolder ? { workFolder: record.workFolder } : {}),\n",
        replace: "",
      },
    ],
  },
  {
    name: "save-drops-folder",
    rule: "a saved session keeps its workFolder",
    test: TESTS.restart,
    edits: [
      {
        file: "src/features/sessions/data/sessionStore.ts",
        find: "    ...(session.workFolder ? { workFolder: session.workFolder } : {}),\n",
        replace: "",
      },
    ],
  },
  {
    name: "folder-as-worktree-after-restart",
    rule: "the folder is not a worktree, so the tab survives a restart",
    test: TESTS.restart,
    edits: [
      {
        file: "src/features/quick-composer/model/quickWorkspace.ts",
        find: "...(launch.workFolder ? { workFolder: launch.workFolder } : {}),",
        replace:
          "...(launch.workFolder ? { worktreeCwd: launch.workFolder } : {}),",
      },
    ],
  },
];

let current;

const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

async function run(title) {
  const args = ["playwright", "test", SPEC, "--project", "chromium"];
  args.push("--reporter", "line", "--retries", "0");
  if (title) args.push("-g", ` ${escape(title)}$`);
  const child = spawn("npx", args, {
    env: { ...process.env, CI: "", FORCE_COLOR: "0" },
  });
  current = child;
  let output = "";
  child.stdout.on("data", (chunk) => (output += chunk));
  child.stderr.on("data", (chunk) => (output += chunk));
  const status = await new Promise((resolve) => child.on("close", resolve));
  const ran = title ? /\b1 (passed|failed)\b/.test(output) : status === 0;
  if (!ran) {
    console.log(output);
    throw new Error(`the run did not finish as one test: ${title ?? SPEC}`);
  }
  return { passed: status === 0, output };
}

/** The assertion's report: from the first Error line to its call log or code frame. */
function failure(output) {
  const lines = output.split("\n").map((line) => line.trimEnd());
  const start = lines.findIndex((line) => line.trim().startsWith("Error:"));
  if (start < 0) return "(no Error line)";
  const end = lines.findIndex(
    (line, index) =>
      index > start && /^\s*(Call [Ll]og|>?\s*\d+ \|)/.test(line),
  );
  return lines
    .slice(start, end < 0 ? start + 12 : end)
    .filter((line) => line.trim())
    .map((line) => line.trim())
    .join("\n    ");
}

function apply(edits) {
  const originals = new Map();
  for (const { file, find, replace, all } of edits) {
    if (!originals.has(file)) originals.set(file, readFileSync(file, "utf8"));
    const text = readFileSync(file, "utf8");
    const count = text.split(find).length - 1;
    if (count === 0 || (count > 1 && !all))
      throw new Error(
        `${file}: expected the mutated text once, found ${count}`,
      );
    writeFileSync(file, text.split(find).join(replace));
  }
  return () => {
    for (const [file, text] of originals) writeFileSync(file, text);
  };
}

const chosen = process.argv.slice(2);
const mutations = chosen.length
  ? MUTATIONS.filter((mutation) => chosen.includes(mutation.name))
  : MUTATIONS;

const clean = await run();
if (!clean.passed) {
  console.log(clean.output);
  console.error("The spec fails on clean code; no mutation was run.");
  process.exit(1);
}
console.log("clean: every test passes\n");

// An interrupted run must not leave src/ mutated.
let restore = () => {};
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"])
  process.on(signal, () => {
    current?.kill("SIGINT");
    restore();
    process.exit(130);
  });

let survived = 0;
for (const mutation of mutations) {
  restore = apply(mutation.edits);
  let result;
  try {
    result = await run(mutation.test);
  } finally {
    restore();
    restore = () => {};
  }
  if (result.passed) survived += 1;
  console.log(`${result.passed ? "SURVIVED" : "KILLED"}  ${mutation.name}`);
  console.log(`  rule: ${mutation.rule}`);
  console.log(`  test: ${mutation.test}`);
  if (!result.passed) console.log(`    ${failure(result.output)}`);
  console.log();
}
process.exit(survived ? 1 : 0);
