import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

const REPO_ROOT = resolve(__dirname, "../..");
const STACKED_BASE = "ade-263/links-resolver";
const LOOKUP_TAIL = "--json baseRefName --jq .baseRefName";

// Records each call and answers `pr view` with GH_STUB_BASE. An empty base fails, as a 403 does.
const STUB_GH = `#!/bin/sh
printf '%s|%s\\n' "$PWD" "$*" >> "$GH_STUB_LOG"
[ -n "$GH_STUB_BASE" ] || { echo "HTTP 403: API rate limit exceeded" >&2; exit 1; }
printf '%s\\n' "$GH_STUB_BASE"
`;

// The stub is a POSIX shell script, so these do not run on the Windows runner.
describe.skipIf(process.platform === "win32")("gh-merge-guard hook, run as settings.json runs it", () => {
  let sandbox: string;
  let workdir: string;
  let stubDir: string;

  beforeAll(() => {
    sandbox = realpathSync(mkdtempSync(join(tmpdir(), "gh-merge-guard-")));
    workdir = join(sandbox, "work");
    stubDir = join(sandbox, "bin");
    mkdirSync(join(workdir, "sub"), { recursive: true });
    mkdirSync(stubDir);
    writeFileSync(join(stubDir, "gh"), STUB_GH);
    chmodSync(join(stubDir, "gh"), 0o755);
  });

  afterAll(() => rmSync(sandbox, { recursive: true, force: true }));

  function hookEntry() {
    return JSON.parse(readFileSync(join(REPO_ROOT, ".claude/settings.json"), "utf8")).hooks.PreToolUse[0];
  }

  function runHook(command: string, base: string) {
    const entry = hookEntry();
    expect("Bash").toMatch(new RegExp(`^(${entry.matcher})$`));
    expect("Monitor").toMatch(new RegExp(`^(${entry.matcher})$`));
    const log = join(sandbox, `calls-${Math.random().toString(36).slice(2)}.log`);
    writeFileSync(log, "");
    const result = spawnSync("sh", ["-c", entry.hooks[0].command], {
      input: JSON.stringify({ tool_name: "Bash", tool_input: { command }, cwd: workdir }),
      encoding: "utf8",
      env: {
        PATH: [stubDir, dirname(process.execPath), process.env.PATH].join(delimiter),
        CLAUDE_PROJECT_DIR: REPO_ROOT,
        GH_STUB_LOG: log,
        GH_STUB_BASE: base,
      },
    });
    const calls = readFileSync(log, "utf8").split("\n").filter(Boolean);
    return { status: result.status, stderr: result.stderr, calls };
  }

  it("refuses a stacked PR, with or without --auto, and says what to do instead", () => {
    for (const command of ["gh pr merge 558", "gh pr merge 558 --auto --merge"]) {
      const { status, stderr, calls } = runHook(command, STACKED_BASE);
      expect(status).toBe(2);
      expect(stderr).toContain(`has base ${STACKED_BASE}, not tyler/main`);
      expect(stderr).toContain("gh pr edit --base tyler/main");
      expect(calls).toEqual([`${workdir}|pr view 558 ${LOOKUP_TAIL}`]);
    }
  });

  it("passes a tyler/main PR", () => {
    for (const command of ["gh pr merge 559", "gh pr merge 559 --auto --merge"]) {
      const { status, stderr, calls } = runHook(command, "tyler/main");
      expect(status).toBe(0);
      expect(stderr).toBe("");
      expect(calls).toEqual([`${workdir}|pr view 559 ${LOOKUP_TAIL}`]);
    }
  });

  it("passes when the lookup fails, and says so on stderr", () => {
    const { status, stderr, calls } = runHook("gh pr merge 558 --auto", "");
    expect(status).toBe(0);
    expect(stderr).toContain("base lookup failed for PR 558, so this merge passes unchecked");
    expect(stderr).toContain("HTTP 403");
    expect(calls).toHaveLength(1);
  });

  it.each([
    ["a PR number", "gh pr merge 5", "pr view 5"],
    ["a URL", "gh pr merge https://github.com/a/b/pull/5", "pr view https://github.com/a/b/pull/5"],
    ["a branch name", "gh pr merge ade-265/purge-scope -d", "pr view ade-265/purge-scope"],
    ["no argument", "gh pr merge --auto", "pr view"],
    ["-R", "gh pr merge 5 -R a/b", "pr view 5 -R a/b"],
    ["--repo", "gh pr merge --repo a/b 5", "pr view 5 -R a/b"],
    ["--repo=", "gh pr merge --repo=a/b 5", "pr view 5 -R a/b"],
    ["-R before merge", "gh pr -R a/b merge 5", "pr view 5 -R a/b"],
    ["a flag value holding words", 'gh pr merge -t "gh pr view 9" 5', "pr view 5"],
    ["a redirect", "gh pr merge 5 2>&1 | tail -1", "pr view 5"],
    ["&&", "git status && gh pr merge 5", "pr view 5"],
    [";", "git status; gh pr merge 5", "pr view 5"],
    ["a pipe", "yes | gh pr merge 5", "pr view 5"],
    ["||", "false || gh pr merge 5", "pr view 5"],
    ["a new line", "git status\ngh pr merge 5", "pr view 5"],
    ["a subshell", "(gh pr merge 5)", "pr view 5"],
    ["a substitution", 'echo "$(gh pr merge 5)"', "pr view 5"],
    ["backticks", "echo `gh pr merge 5`", "pr view 5"],
    ["bash -c", "bash -lc 'gh pr merge 5'", "pr view 5"],
    ["eval", 'eval "gh pr merge 5"', "pr view 5"],
    ["a wrapper", "FOO=1 timeout 60 gh pr merge 5", "pr view 5"],
    ["a full path to gh", "/opt/homebrew/bin/gh pr merge 5", "pr view 5"],
    ["an if block", "if true; then gh pr merge 5; fi", "pr view 5"],
  ])("sees a merge through %s", (_form, command, lookup) => {
    const { status, calls } = runHook(command, STACKED_BASE);
    expect(status).toBe(2);
    expect(calls).toEqual([`${workdir}|${lookup} ${LOOKUP_TAIL}`]);
  });

  it("looks up from the directory the command changed to", () => {
    const { status, calls } = runHook("cd sub && gh pr merge 5", STACKED_BASE);
    expect(status).toBe(2);
    expect(calls).toEqual([`${join(workdir, "sub")}|pr view 5 ${LOOKUP_TAIL}`]);
  });

  it("checks every merge in a command", () => {
    const { status, calls } = runHook("gh pr merge 5; gh pr merge 6", STACKED_BASE);
    expect(status).toBe(2);
    expect(calls).toHaveLength(2);
  });

  it.each([
    ["a variable", 'gh pr merge "$PR"'],
    ["a loop variable", "for p in 560 561; do gh pr merge $p --auto; done"],
    ["a substitution", "gh pr merge $(cat pr.txt)"],
    ["xargs", "echo 560 | xargs -n1 gh pr merge --auto"],
    ["a variable repository", 'gh pr merge 5 -R "$REPO"'],
    ["a variable directory", 'cd "$DIR" && gh pr merge 5'],
  ])("refuses a PR only known at run time: %s", (_form, command) => {
    const { status, stderr, calls } = runHook(command, "tyler/main");
    expect(status).toBe(2);
    expect(stderr).toContain("literal PR number");
    expect(calls).toEqual([]);
  });

  it.each([
    ["echo", 'echo "gh pr merge 5"'],
    ["grep", "grep -rn 'gh pr merge' scripts"],
    ["a comment", "git status # gh pr merge 5"],
    ["a heredoc", "cat <<'EOF' > note.md\ngh pr merge 5\nEOF"],
    ["a heredoc in a PR body", `gh pr create --body "$(cat <<'EOF'\ngh pr merge 5\nEOF\n)"`],
    ["a commit message", 'git commit -m "gh pr merge 5"'],
    ["another gh command", "gh pr view 5 --json state"],
    ["gh pr merge --help", "gh pr merge --help"],
    ["cancelling auto-merge", "gh pr merge 5 --disable-auto"],
  ])("leaves a command that merges nothing alone: %s", (_form, command) => {
    const { status, stderr, calls } = runHook(command, STACKED_BASE);
    expect(status).toBe(0);
    expect(stderr).toBe("");
    expect(calls).toEqual([]);
  });

  it("passes a payload it cannot read, and says so on stderr", () => {
    const result = spawnSync("sh", ["-c", hookEntry().hooks[0].command], {
      input: "not json",
      encoding: "utf8",
      env: { PATH: [dirname(process.execPath), process.env.PATH].join(delimiter), CLAUDE_PROJECT_DIR: REPO_ROOT },
    });
    expect(result.status).toBe(0);
    expect(result.stderr).toContain("passes unchecked");
  });
});
