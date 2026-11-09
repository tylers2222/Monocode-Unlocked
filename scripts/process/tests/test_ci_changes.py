"""Tests for ci_changes.py with the real ci-map.toml, run on real git diffs in a scratch repo.

    python3 -m unittest discover -s scripts/process/tests
"""
import re
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Mapping, Sequence
from pathlib import Path

HERE = Path(__file__).parent
SCRIPT = HERE.parent / "ci_changes.py"
WORKFLOWS = HERE.parents[2] / ".github/workflows"
sys.path.insert(0, str(HERE.parent))
from ci_changes import MAP, parse  # noqa: E402

JOBS = list(parse(MAP.read_text()).jobs)
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


class FileClasses(unittest.TestCase):
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
                self.assertIn('ci_changes.py github "$EVENT" "$HEAD"', body)
                self.assertIn("HEAD: ${{ github.sha }}", body)
                self.assertNotIn("pull_request.base.sha", body)


if __name__ == "__main__":
    unittest.main()
