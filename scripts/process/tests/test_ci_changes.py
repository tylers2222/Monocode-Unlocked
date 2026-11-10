"""Tests for ci_changes.py with the real ci-map.toml, run on real git diffs in a scratch repo.

    python3 -m unittest discover -s scripts/process/tests
"""
import os
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import tomllib
import unittest
from collections.abc import Mapping, Sequence
from pathlib import Path

HERE = Path(__file__).parent
SCRIPT = HERE.parent / "ci_changes.py"
WORKFLOWS = HERE.parents[2] / ".github/workflows"
sys.path.insert(0, str(HERE.parent))
from ci_changes import MAP, parse  # noqa: E402

REAL_MAP = MAP.read_text()
JOBS = list(parse(REAL_MAP).jobs)
REACH = tomllib.loads(REAL_MAP)["reach"]
UNMATCHED_PATHS = tomllib.loads(REAL_MAP)["unmatched"]
REPO = HERE.parents[2]
REACH_AT = REAL_MAP.index("\n[reach]\n") + 1
# The run block of the `changes` step in ci.yml, e2e.yml and native.yml.
CHANGES_STEP = ("set -euo pipefail\n"
                'python3 scripts/process/ci_changes.py github "$EVENT" "$HEAD" | tee -a "$GITHUB_OUTPUT"\n')
EVERY_JOB = set(JOBS)
# Real pull requests' file lists (gh pr view <n> --json files).
PR_429_SCRIPT = ["scripts/spaces/merge-dry-run.py", "scripts/spaces/tests/test_merge_dry_run.py"]
PR_436_RUST = ["src-tauri/src/harness.rs", "src-tauri/src/lib.rs", "src-tauri/src/session_store.rs",
               "src-tauri/src/spaces/mod.rs", "src-tauri/src/tests/scenario/mod.rs",
               "src-tauri/src/tests/scenario/spaces.rs"]
PR_444_RUST_TS = ["src-tauri/src/control_cli.rs", "src-tauri/src/mono_gate.rs", "src-tauri/src/mono_messages.rs",
                  "src-tauri/src/tests/scenario/cross_space.rs", "src-tauri/src/tests/scenario/mod.rs",
                  "src/features/monos/model/monoMessages.ts"]
PR_417_UI = ["src/app/App.tsx", "src/app/hooks/useMonoHabits.test.ts", "src/app/hooks/useMonoHabits.ts",
             "src/features/monos/model/habitQuestionHost.test.ts", "src/features/monos/model/habitQuestionHost.ts",
             "tests/browser/ade67-question-card.spec.ts", "tests/browser/ade71-habit-question-mono-chat.spec.ts"]


def git(repo: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True).stdout.strip()


class Repo:
    """A scratch git repo shaped like a pull_request run: `head` adds the given files on a PR
    branch, `base_later` then adds its files to the base branch, and `merge` merges the PR
    into the base. `lower` adds its files to the base before the PR forks, as a stacked PR's
    lower branch does. `moves` maps old paths to new ones, moved by `head` with git mv. `fork`
    is the commit the PR forked from: a stale base.sha."""

    def __init__(self, files: list[str], base_later: Sequence[str] = (), lower: Sequence[str] = (),
                 moves: Mapping[str, str] | None = None):
        self.dir = tempfile.TemporaryDirectory()
        self.path = Path(self.dir.name)
        git(self.path, "init", "-q", "-b", "base")
        git(self.path, "config", "user.email", "t@example.invalid")
        git(self.path, "config", "user.name", "t")
        self.commit(["seed"], "root")
        self.root = git(self.path, "rev-parse", "HEAD")
        self.commit(lower, "lower branch")
        self.fork = git(self.path, "rev-parse", "HEAD")
        git(self.path, "switch", "-qc", "pr")
        for old, new in (moves or {}).items():
            (self.path / new).parent.mkdir(parents=True, exist_ok=True)
            git(self.path, "mv", old, new)
        self.commit(files, "head")
        self.head = git(self.path, "rev-parse", "HEAD")
        git(self.path, "switch", "-q", "base")
        self.commit(base_later, "base moved on")
        git(self.path, "merge", "-q", "--no-ff", "--no-edit", "pr")
        self.merge = git(self.path, "rev-parse", "HEAD")

    def commit(self, files: Sequence[str], message: str) -> None:
        for name in files:
            (self.path / name).parent.mkdir(parents=True, exist_ok=True)
            (self.path / name).write_text(f"{message}\n")
        git(self.path, "add", "-A")
        git(self.path, "commit", "-qm", message, "--allow-empty")

    def select(self, event: str = "pull_request", merge: str | None = None, cwd: Path | None = None) -> tuple[dict[str, str], str]:
        """ci_changes.py's outputs (job -> "true"/"false") and its reason line."""
        run = subprocess.run([sys.executable, str(SCRIPT), "github", event, self.merge if merge is None else merge],
                             cwd=cwd or self.path, check=True, capture_output=True, text=True)
        return dict(line.split("=", 1) for line in run.stdout.splitlines()), run.stderr.strip().splitlines()[-1]

    def clone(self, depth: int) -> Path:
        """A clone holding only the last depth commits, as actions/checkout's fetch-depth does."""
        target = Path(self.dir.name) / f"clone-{depth}"
        subprocess.run(["git", "clone", "-q", f"--depth={depth}", "--branch=base", self.path.as_uri(), str(target)],
                       check=True, capture_output=True)
        return target


def classes(paths: list[str]) -> dict[str, str]:
    run = subprocess.run([sys.executable, str(SCRIPT), "files"], input="\n".join(paths) + "\n",
                         check=True, capture_output=True, text=True)
    return dict(line.split("\t") for line in run.stdout.splitlines())


class SelectorOnRealDiffs(unittest.TestCase):
    def runs(self, files: list[str], event: str = "pull_request", merge: str | None = None) -> tuple[set[str], str]:
        repo = Repo(files)
        self.addCleanup(repo.dir.cleanup)
        outputs, reason = repo.select(event, merge)
        self.assertEqual(list(outputs), JOBS, "one output line per job in the map, in map order")
        return {job for job, on in outputs.items() if on == "true"}, reason

    def test_rust_only_runs_check_packaging_and_e2e(self):
        jobs, _ = self.runs(PR_436_RUST)
        self.assertEqual(jobs, {"check", "rpm", "e2e-linux", "e2e-macos"})

    def test_rust_with_a_src_model_adds_browser_host_and_mobile(self):
        jobs, _ = self.runs(PR_444_RUST_TS)
        self.assertEqual(jobs, {"browser", "host", "check", "rpm", "e2e-linux", "e2e-macos", "mobile"})

    def test_src_only_runs_front_end_check_mobile_and_e2e_without_packaging(self):
        jobs, _ = self.runs(PR_417_UI)
        self.assertEqual(jobs, {"browser", "host", "check", "e2e-linux", "e2e-macos", "mobile"})

    def test_mobile_app_and_native_app_run_only_their_job(self):
        self.assertEqual(self.runs(["mobile/src/app/index.tsx"])[0], {"mobile"})
        self.assertEqual(self.runs(["crates/monocode-gpui/src/main.rs", "scripts/native-release.mjs"])[0], {"native"})

    def test_mobile_link_runs_every_job_that_builds_src_tauri(self):
        jobs, _ = self.runs(["crates/mobile-link/src/lib.rs"])
        self.assertEqual(jobs, {"check", "rpm", "e2e-linux", "e2e-macos", "native"})

    def test_scripts_only_runs_no_gated_job(self):
        jobs, reason = self.runs(PR_429_SCRIPT)
        self.assertEqual(jobs, set())
        self.assertIn("runs: no gated job", reason)

    def test_docs_only_runs_no_gated_job(self):
        jobs, _ = self.runs(["docs/notes.md", "README.md", "LICENSE"])
        self.assertEqual(jobs, set())

    def test_e2e_harness_change_runs_only_its_job(self):
        self.assertEqual(self.runs(["scripts/e2e-macos.sh"])[0], {"e2e-macos"})
        self.assertEqual(self.runs(["evals/run.sh", "docker/e2e.sh"])[0], {"e2e-linux"})

    def test_unmatched_file_runs_every_job(self):
        jobs, reason = self.runs(["docs/notes.md", "newtool/config.yaml"])
        self.assertEqual(jobs, EVERY_JOB)
        self.assertEqual(reason, "ci-changes: newtool/config.yaml matches no glob in the map: every job runs")

    def test_workflow_change_runs_every_job(self):
        jobs, reason = self.runs(["docs/notes.md", ".github/workflows/gate.yml"])
        self.assertEqual(jobs, EVERY_JOB)
        self.assertEqual(reason, "ci-changes: .github/workflows/gate.yml is in the map's all list: every job runs")

    def test_lockfile_toolchain_map_and_selector_run_every_job(self):
        for path in ["package-lock.json", "Cargo.lock", ".nvmrc", ".github/rust-version", "rust-toolchain.toml",
                     "scripts/process/ci-map.toml", "scripts/process/ci_changes.py"]:
            with self.subTest(path=path):
                self.assertEqual(self.runs([path]), (EVERY_JOB, f"ci-changes: {path} is in the map's all list: every job runs"))

    def test_push_and_dispatch_run_every_job_whatever_changed(self):
        for event in ["push", "workflow_dispatch"]:
            with self.subTest(event=event):
                self.assertEqual(self.runs(PR_429_SCRIPT, event=event), (EVERY_JOB, f"ci-changes: {event} event: every job runs"))

    def test_a_diff_that_cannot_be_read_runs_every_job(self):
        no_diff = "ci-changes: no diff could be read (failed or empty): every job runs"
        for merge in ["", "0" * 40, "deadbeef" * 5]:
            with self.subTest(merge=merge):
                self.assertEqual(self.runs(PR_429_SCRIPT, merge=merge), (EVERY_JOB, no_diff))
        self.assertEqual(self.runs([]), (EVERY_JOB, no_diff))


class FirstParentDiff(unittest.TestCase):
    """The diff runs from the merge's first parent: the base's later changes are not the PR's."""
    NO_DIFF = "ci-changes: no diff could be read (failed or empty): every job runs"
    WORKFLOW = ".github/workflows/ci.yml"

    def repo(self, *args: list[str], **kwargs: Sequence[str] | Mapping[str, str]) -> Repo:
        repo = Repo(*args, **kwargs)
        self.addCleanup(repo.dir.cleanup)
        return repo

    def jobs(self, repo: Repo, **kwargs: str | Path) -> tuple[set[str], str]:
        outputs, reason = repo.select(**kwargs)
        return {job for job, on in outputs.items() if on == "true"}, reason

    def test_a_pr_behind_a_base_that_changed_github_skips_what_it_should(self):
        repo = self.repo(PR_429_SCRIPT, base_later=[self.WORKFLOW])
        # The old two-dot diff from a stale base.sha sees the workflow, which ran every job.
        self.assertIn(self.WORKFLOW, git(repo.path, "diff", "--name-only", repo.fork, repo.merge))
        self.assertEqual(self.jobs(repo), (set(), "ci-changes: 2 changed files; runs: no gated job"))

    def test_a_pr_with_its_own_src_change_still_runs_its_jobs_when_behind(self):
        src = ["src/features/monos/model/monoMessages.ts"]
        behind, _ = self.jobs(self.repo(src, base_later=[self.WORKFLOW]))
        self.assertEqual(behind, self.jobs(self.repo(src))[0])
        self.assertIn("check", behind)
        self.assertNotEqual(behind, EVERY_JOB)

    def test_a_prs_own_file_in_the_all_list_runs_every_job(self):
        repo = self.repo(["docs/notes.md", self.WORKFLOW], base_later=["docs/other.md"])
        self.assertEqual(self.jobs(repo), (EVERY_JOB, f"ci-changes: {self.WORKFLOW} is in the map's all list: every job runs"))

    def test_a_stacked_pr_counts_only_its_own_files(self):
        repo = self.repo(["docs/notes.md"], base_later=[self.WORKFLOW], lower=["src-tauri/src/lib.rs"])
        self.assertIn("src-tauri/src/lib.rs", git(repo.path, "diff", "--name-only", repo.root, repo.merge))
        self.assertEqual(self.jobs(repo), (set(), "ci-changes: 1 changed files; runs: no gated job"))

    def test_a_missing_first_parent_runs_every_job(self):
        repo = self.repo(PR_429_SCRIPT)
        self.assertEqual(self.jobs(repo, cwd=repo.clone(depth=1)), (EVERY_JOB, self.NO_DIFF))

    def test_a_shallow_clone_that_holds_the_first_parent_reads_the_diff(self):
        repo = self.repo(PR_429_SCRIPT, base_later=[self.WORKFLOW])
        self.assertEqual(self.jobs(repo, cwd=repo.clone(depth=2))[0], set())

    def test_a_commit_that_is_no_merge_runs_every_job(self):
        repo = self.repo(["docs/notes.md"])
        octopus = git(repo.path, "commit-tree", f"{repo.merge}^{{tree}}", "-m", "three parents",
                      "-p", f"{repo.merge}^1", "-p", repo.head, "-p", repo.root)
        for merge in [repo.head, repo.root, octopus]:
            with self.subTest(merge=merge):
                self.assertEqual(self.jobs(repo, merge=merge), (EVERY_JOB, self.NO_DIFF))

    def test_a_merge_with_an_empty_diff_runs_every_job(self):
        self.assertEqual(self.jobs(self.repo([], base_later=[self.WORKFLOW])), (EVERY_JOB, self.NO_DIFF))

    def test_a_file_moved_out_of_a_gated_folder_still_runs_its_jobs(self):
        repo = self.repo([], lower=["src/a.ts"], moves={"src/a.ts": "docs/a.ts"})
        self.assertEqual(git(repo.path, "diff", "--name-only", "-M", f"{repo.merge}^1", repo.merge), "docs/a.ts",
                         "git sees a rename, so only the old path can run the jobs")
        self.assertEqual(self.jobs(repo), ({"browser", "host", "check", "e2e-linux", "e2e-macos", "mobile"},
                                           "ci-changes: 2 changed files; runs: browser host check e2e-linux e2e-macos mobile"))

    def test_the_old_form_with_a_base_argument_is_a_usage_error(self):
        run = subprocess.run([sys.executable, str(SCRIPT), "github", "pull_request", "aaa", "bbb"], capture_output=True, text=True)
        self.assertEqual((run.returncode, run.stdout), (2, ""), "exit 2 turns the changes job red; no job line reaches GITHUB_OUTPUT")


class MapStep(unittest.TestCase):
    """Runs the workflows' `changes` step with a given map."""
    SPEC = ["tests/browser/a.spec.ts"]
    MAP_ONLY = ["scripts/process/ci-map.toml"]

    def step(self, map_text: str, files: list[str]) -> tuple[int, str]:
        """The step's exit code and what it appended to GITHUB_OUTPUT, with map_text as the
        map beside ci_changes.py in the checkout."""
        code, output, _ = self.run_step(map_text, files)
        return code, output

    def run_step(self, map_text: str, files: list[str]) -> tuple[int, str, str]:
        """step() and the last line the step wrote to stderr."""
        repo = Repo(files)
        self.addCleanup(repo.dir.cleanup)
        process = repo.path / "scripts/process"
        process.mkdir(parents=True, exist_ok=True)
        shutil.copy(SCRIPT, process / SCRIPT.name)
        (process / MAP.name).write_text(map_text)
        output = Path(self.enterContext(tempfile.TemporaryDirectory())) / "github_output"
        output.touch()
        env = {**os.environ, "EVENT": "pull_request", "HEAD": repo.merge, "GITHUB_OUTPUT": str(output)}
        run = subprocess.run(["bash", "-c", CHANGES_STEP], cwd=repo.path, env=env, capture_output=True, text=True)
        return run.returncode, output.read_text(), (run.stderr.strip().splitlines() or [""])[-1]

    def faulty(self, old: str, new: str) -> str:
        self.assertEqual(REAL_MAP.count(old), 1, old)
        return REAL_MAP.replace(old, new)

    def emptied(self, key: str) -> str:
        text, n = re.subn(rf"(?ms)^{re.escape(key)} = \[.*?^\]", f"{key} = []", REAL_MAP)
        self.assertEqual(n, 1, key)
        return text


class MapErrors(MapStep):
    """A map that parse() refuses ends the `changes` step red with no output line. An empty
    output is not 'false', so every gated job runs, and ci_verdict.py fails the red `changes`
    job. Each map is the real map with one fault, run through the workflows' own step on a PR
    for which the faulty map, if it were read, would skip a job that the real map runs."""

    def test_the_real_map_runs_the_jobs_these_prs_need(self):
        """Control: each PR below runs a job under the real map that a faulty map would skip."""
        code, output = self.step(REAL_MAP, self.SPEC)
        self.assertEqual(code, 0)
        self.assertIn("browser=true\n", output)
        code, output = self.step(REAL_MAP, self.MAP_ONLY)
        self.assertEqual((code, output), (0, "".join(f"{job}=true\n" for job in JOBS)))
        code, output = self.step(REAL_MAP, PR_429_SCRIPT)
        self.assertEqual((code, output), (0, "".join(f"{job}=false\n" for job in JOBS)))

    def test_a_faulty_map_fails_the_step_with_no_output(self):
        jobs_at, reach = REAL_MAP.index("\n[jobs]\n") + 1, REAL_MAP[REACH_AT:]
        cases = {
            "an extra top-level key": ("stray = [\"src/**\"]\n" + REAL_MAP, self.SPEC),
            "no none key": (self.faulty("\nnone = [", "\nnone_ = ["), self.SPEC),
            "an empty all list": (self.emptied("all"), self.MAP_ONLY),
            "an empty none list": (self.emptied("none"), self.SPEC),
            "an empty job glob list": (self.emptied("browser"), self.SPEC),
            "an empty glob": (self.faulty("\nbrowser = [\n", "\nbrowser = [\n  \"\",\n"), self.SPEC),
            "a glob that is no string": (self.faulty("\nbrowser = [\n", "\nbrowser = [\n  1,\n"), self.SPEC),
            "a glob list that is no list": (self.faulty("\nbrowser = [\n", "\nbrowser = \"src/**\"\nbrowser_ = [\n"), self.SPEC),
            "an empty [jobs] table": (REAL_MAP[:jobs_at] + "[jobs]\n" + reach, PR_429_SCRIPT),
            "a jobs key that is no table": (REAL_MAP[:jobs_at] + "jobs = [\"browser\"]\n" + reach, PR_429_SCRIPT),
            "a TOML syntax error": (REAL_MAP + "[jobs\n", PR_429_SCRIPT),
        }
        for fault, (map_text, files) in cases.items():
            with self.subTest(fault):
                self.assertEqual(self.step(map_text, files), (1, ""))


class MapReach(MapStep):
    """A map that parses but whose globs would skip a job on a later PR is refused too
    (ADE-301). Each fault is one plausible edit to the real map. The PR is one the edit
    would mis-route if the map were read: the map's own PR runs every job, so the harm lands
    on the PRs after it."""

    def test_a_map_whose_globs_miss_a_needed_path_fails_the_step_with_no_output(self):
        none_scripts = self.faulty('  "scripts/spaces/**",\n  "scripts/bump-version.mjs",\n  "scripts/test-remote-ssh.py",\n',
                                   '  "scripts/**",\n')
        cases = {
            # The new file would be none and skip every job; the real map runs every job for it.
            "a none glob of every file": (self.faulty('\nnone = [\n', '\nnone = [\n  "**",\n'), ["newtool/config.yaml"],
                                          "ci-map.toml: unmatched path newtool/config.yaml is none, so a new file there would not run every job"),
            "a none glob of every top-level file": (self.faulty('\nnone = [\n', '\nnone = [\n  "*",\n'), ["new-config.ts"],
                                                    "ci-map.toml: unmatched path new-config.ts is none, so a new file there would not run every job"),
            "none tidied to scripts/**": (none_scripts, ["scripts/new-tool.sh"],
                                          "ci-map.toml: unmatched path scripts/new-tool.sh is none, so a new file there would not run every job"),
            # The spec would run no browser job; the real map runs browser for it.
            "a job glob with a typo": (self.faulty('  "tests/browser/**",\n', '  "test/browser/**",\n'), self.SPEC,
                                       "ci-map.toml: reach.browser path tests/browser/ade179-space-switcher.spec.ts runs check, not browser"),
            "a job glob dropped": (self.faulty('  "scripts/release-channel*",\n', ''), ["scripts/release-channel.test.cjs"],
                                   "ci-map.toml: reach.release-channel path scripts/release-channel.test.cjs runs unmatched, not release-channel"),
            "a reach path moved to all": (self.faulty('  ".npmrc",\n]', '  ".npmrc",\n  "scripts/repack-appimage.sh",\n]'), self.SPEC,
                                          "ci-map.toml: reach.appimage path scripts/repack-appimage.sh runs all, not appimage"),
            "a new job with no reach": (self.faulty('\n[reach]\n', '\nlint = ["src/**"]\n\n[reach]\n'), self.SPEC,
                                        "ci-map.toml: [reach] must list exactly the jobs in [jobs]; it has " + str(sorted(JOBS))),
            "a reach entry for no job": (REAL_MAP + 'lint = ["src/app/App.tsx"]\n', self.SPEC,
                                         "ci-map.toml: [reach] must list exactly the jobs in [jobs]; it has " + str(sorted([*JOBS, "lint"]))),
            "no [reach] table": (REAL_MAP[:REACH_AT], self.SPEC,
                                             "ci-map.toml needs exactly the keys all, none, unmatched, [jobs] and [reach]; it has ['all', 'jobs', 'none', 'unmatched']"),
            "an empty reach list": (self.faulty('\nappimage = ["scripts/repack-appimage.sh"]\n', '\nappimage = []\n'), self.SPEC,
                                    "ci-map.toml: reach.appimage must be a non-empty list of globs or paths"),
            "an empty unmatched list": (self.emptied("unmatched"), ["newtool/config.yaml"],
                                        "ci-map.toml: unmatched must be a non-empty list of globs or paths"),
        }
        for fault, (map_text, files, error) in cases.items():
            with self.subTest(fault):
                code, output, stderr = self.run_step(map_text, files)
                self.assertEqual((code, output), (1, ""))
                self.assertEqual(stderr, f"ValueError: {error}")

    def test_the_real_map_reaches_what_these_prs_need(self):
        """Control for the cases above: the real map runs these PRs' jobs."""
        for files, want in [(["newtool/config.yaml"], set(JOBS)), (["new-config.ts"], set(JOBS)),
                            (["scripts/new-tool.sh"], set(JOBS)), (self.SPEC, {"browser", "check"}),
                            (["scripts/release-channel.test.cjs"], {"release-channel"})]:
            with self.subTest(files=files):
                code, output = self.step(REAL_MAP, files)
                self.assertEqual(code, 0)
                self.assertEqual({line.split("=")[0] for line in output.splitlines() if line.endswith("=true")}, want)


class FileClasses(unittest.TestCase):
    def test_each_reach_path_is_a_real_file_that_runs_its_job_in_box_ci(self):
        paths = sorted({path for listed in REACH.values() for path in listed})
        self.assertEqual([path for path in paths if not (REPO / path).is_file()], [], "reach paths must be real files")
        got = classes(paths)
        for job, listed in REACH.items():
            for path in listed:
                with self.subTest(job=job, path=path):
                    self.assertIn(job, got[path].split(","))

    def test_unmatched_paths_are_new_files_that_run_every_job_in_box_ci(self):
        self.assertEqual([path for path in UNMATCHED_PATHS if (REPO / path).exists()], [], "unmatched paths must not exist yet")
        self.assertEqual(classes(UNMATCHED_PATHS), {path: "unmatched" for path in UNMATCHED_PATHS})

    def test_box_ci_classes(self):
        self.assertEqual(classes([".github/workflows/ci.yml", "scripts/process/ci-map.toml", "newtool/x",
                                  "scripts/process/tier.py", "scripts/process/heavy-lock.sh",
                                  "scripts/install-linux-deps-fedora.sh", "src-tauri/tauri.linux.conf.json"]), {
            ".github/workflows/ci.yml": "all",
            "scripts/process/ci-map.toml": "all",
            "newtool/x": "unmatched",
            "scripts/process/tier.py": "none",
            "scripts/process/heavy-lock.sh": "check",
            "scripts/install-linux-deps-fedora.sh": "rpm",
            "src-tauri/tauri.linux.conf.json": "appimage,check,e2e-linux,e2e-macos,release-channel,rpm",
        })

    def test_globs_keep_single_star_inside_one_folder(self):
        self.assertEqual(classes(["notes.md", "src/notes.md", "src-tauri/src/remote_bootstrap.sh"]), {
            "notes.md": "none",
            "src/notes.md": "browser,check,e2e-linux,e2e-macos,host,mobile",
            "src-tauri/src/remote_bootstrap.sh": "check,e2e-linux,e2e-macos,host,rpm",
        })


class WorkflowWiring(unittest.TestCase):
    """Each map job is one job-level `if:` on its `changes` output, and nothing else gates by path."""

    def test_every_map_job_is_gated_on_its_own_output_and_nothing_else_is(self):
        text = {path.name: path.read_text() for path in WORKFLOWS.glob("*.yml")}
        gated = {job for body in text.values() for job in re.findall(r"needs\.changes\.outputs\.([\w-]+) != 'false'", body)}
        declared = {job for body in text.values() for job in re.findall(r"^      ([\w-]+): \$\{\{ steps\.paths\.outputs\.\1 \}\}$", body, re.M)}
        self.assertEqual(gated, EVERY_JOB)
        self.assertEqual(declared, EVERY_JOB)
        for name, body in text.items():
            with self.subTest(workflow=name):
                self.assertNotRegex(body, r"(?m)^\s+paths(-ignore)?:", "workflow-level paths filters leave required checks pending")

    def test_every_changes_job_passes_the_merge_and_no_base_sha(self):
        callers = {path.name: path.read_text() for path in WORKFLOWS.glob("*.yml") if "ci_changes.py github" in path.read_text()}
        self.assertEqual(set(callers), {"ci.yml", "e2e.yml", "native.yml"})
        for name, body in callers.items():
            with self.subTest(workflow=name):
                self.assertIn("          EVENT: ${{ github.event_name }}\n"
                              "          HEAD: ${{ github.sha }}\n"
                              "        run: |\n" + textwrap.indent(CHANGES_STEP, " " * 10), body)
                self.assertNotIn("pull_request.base.sha", body)

    def test_every_changes_job_fetches_the_merge_and_its_parents_and_only_the_map(self):
        """Depth 1 hides the merge's parents, so every PR would run every job; depth 2 is
        the smallest that reads the diff (test_a_shallow_clone_that_holds_the_first_parent...)."""
        callers = {path.name: path.read_text() for path in WORKFLOWS.glob("*.yml") if "ci_changes.py github" in path.read_text()}
        for name, body in callers.items():
            with self.subTest(workflow=name):
                checkout = re.search(r"(?ms)^  changes:\n.*?- uses: actions/checkout@[^\n]*\n        with:\n((?:          [^\n]*\n)+)", body)
                self.assertIsNotNone(checkout, "the changes job checks out the repo")
                settings = re.findall(r"(?m)^          ([\w-]+): (\S+)$", checkout.group(1))
                self.assertEqual(settings, [("fetch-depth", "2"), ("sparse-checkout", "scripts/process"),
                                            ("persist-credentials", "false")])


if __name__ == "__main__":
    unittest.main()
