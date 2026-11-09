"""Tests for ci_level.py, and for how the workflows use its output.

    python3 -m unittest discover -s scripts/process/tests
"""
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
WORKFLOWS = HERE.parents[2] / ".github/workflows"
SCRIPT = HERE.parent / "ci_level.py"
sys.path.insert(0, str(HERE.parent))
from ci_level import FULL_LABEL, LivePr, decide, parse_live, stale_reason  # noqa: E402

DRAFT = LivePr(True, ())
READY = LivePr(False, ())
HEAD = "a" * 40


def must_not_read():
    raise AssertionError("the PR must not be read")


class Decide(unittest.TestCase):
    def full(self, event, payload, live=must_not_read):
        return decide(event, payload, live if live is must_not_read else lambda: live)[0]

    def test_only_a_draft_in_the_event_and_now_with_no_label_is_fast(self):
        self.assertIs(self.full("pull_request", "true", DRAFT), False)
        self.assertIs(self.full("pull_request", "true", LivePr(True, ("process-fix", "instance:2"))), False)

    def test_a_ready_event_is_full_and_reads_nothing(self):
        for payload in ("false", ""):
            self.assertIs(self.full("pull_request", payload), True)

    def test_events_that_are_not_pull_requests_are_full(self):
        for event in ("push", "workflow_dispatch", ""):
            self.assertIs(self.full(event, "true"), True)

    def test_a_re_run_of_a_draft_run_on_a_ready_pr_is_full(self):
        self.assertIs(self.full("pull_request", "true", READY), True)

    def test_a_pr_that_cannot_be_read_is_full(self):
        self.assertIs(self.full("pull_request", "true", None), True)

    def test_the_label_adds_the_full_set_on_a_draft(self):
        self.assertIs(self.full("pull_request", "true", LivePr(True, (FULL_LABEL,))), True)

    def test_a_near_label_adds_nothing(self):
        self.assertIs(self.full("pull_request", "true", LivePr(True, ("ci:full ", "CI:FULL", "ci"))), False)

    def test_removing_the_label_cannot_make_a_ready_pr_fast(self):
        self.assertIs(self.full("pull_request", "false"), True)
        self.assertIs(self.full("pull_request", "true", LivePr(False, ())), True)


class ParseLive(unittest.TestCase):
    def test_reads_draft_and_labels(self):
        text = json.dumps({"draft": True, "labels": [{"name": "ci:full"}, {"name": "x"}]})
        self.assertEqual(parse_live(text), LivePr(True, ("ci:full", "x")))

    def test_anything_else_is_unreadable(self):
        for text in ("", "not json", "[]", "{}", '{"draft": true}', '{"draft": "true", "labels": []}',
                     '{"draft": null, "labels": []}', '{"draft": true, "labels": [{}]}'):
            self.assertIsNone(parse_live(text), text)


class StaleReason(unittest.TestCase):
    def test_a_merge_commit_holding_the_head_is_fine(self):
        self.assertIsNone(stale_reason(HEAD, HEAD))

    def test_another_head_no_parent_or_no_head_is_stale(self):
        for head, held in ((HEAD, "b" * 40), (HEAD, None), ("", ""), ("", None)):
            self.assertIn("merge commit", stale_reason(head, held))


class Main(unittest.TestCase):
    """The script itself, on a real merge commit. A push event reads no PR and no git."""

    def git(self, repo, *args):
        return subprocess.run(["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@example.com", *args],
                              capture_output=True, text=True, check=True).stdout.strip()

    def run_script(self, cwd, *args):
        return subprocess.run([sys.executable, str(SCRIPT), *args], cwd=cwd, capture_output=True, text=True)

    def test_prints_the_level_then_fails_on_a_stale_merge_ref(self):
        with tempfile.TemporaryDirectory() as repo:
            self.git(repo, "init", "-q", "-b", "base")
            self.git(repo, "commit", "-q", "--allow-empty", "-m", "base")
            self.git(repo, "checkout", "-q", "-b", "pr-branch")
            self.git(repo, "commit", "-q", "--allow-empty", "-m", "head")
            head = self.git(repo, "rev-parse", "HEAD")
            self.git(repo, "checkout", "-q", "base")
            self.git(repo, "merge", "-q", "--no-ff", "-m", "merge", "pr-branch")

            good = self.run_script(repo, "pull_request", "false", "1", "o/r", head)
            self.assertEqual((good.returncode, good.stdout), (0, "level=full\n"))

            stale = self.run_script(repo, "pull_request", "false", "1", "o/r", "b" * 40)
            self.assertEqual(stale.returncode, 1)
            # The output comes first, so the full set still runs and `ci` reports the red.
            # Exactly that line: anything else on stdout voids GITHUB_OUTPUT (PR #592 run 42585298831).
            self.assertEqual(stale.stdout, "level=full\n")
            self.assertIn(head, stale.stderr)

    def test_a_push_needs_no_merge_ref(self):
        with tempfile.TemporaryDirectory() as repo:
            self.git(repo, "init", "-q")
            result = self.run_script(repo, "push", "", "", "o/r", "")
            self.assertEqual((result.returncode, result.stdout), (0, "level=full\n"))

    def test_wrong_arguments_exit_2(self):
        self.assertEqual(self.run_script(HERE, "push").returncode, 2)


class LiveRead(unittest.TestCase):
    """main with a stub gh on PATH, as a re-run of a draft run reads the PR."""

    def run_with_gh(self, gh_body):
        with tempfile.TemporaryDirectory() as tmp:
            stub = Path(tmp) / "gh"
            calls = Path(tmp) / "calls"
            stub.write_text(f'#!/bin/sh\necho "$@" >> "{calls}"\n{gh_body}\n')
            stub.chmod(0o755)
            env = {**os.environ, "PATH": f"{tmp}{os.pathsep}{os.environ['PATH']}"}
            result = subprocess.run([sys.executable, str(SCRIPT), "pull_request", "true", "7", "o/r", "unused"],
                                    capture_output=True, text=True, env=env, cwd=tmp)
            return result.stdout.splitlines()[0], calls.read_text()

    @staticmethod
    def pr(draft, *labels):
        body = json.dumps({"draft": draft, "labels": [{"name": name} for name in labels]})
        return f"cat <<'EOF'\n{body}\nEOF"

    def test_reads_this_pull_request(self):
        self.assertEqual(self.run_with_gh(self.pr(True))[1], "api repos/o/r/pulls/7\n")

    def test_a_draft_is_fast(self):
        self.assertEqual(self.run_with_gh(self.pr(True))[0], "level=fast")

    def test_a_pr_ready_now_is_full(self):
        self.assertEqual(self.run_with_gh(self.pr(False))[0], "level=full")

    def test_a_draft_with_the_label_is_full(self):
        self.assertEqual(self.run_with_gh(self.pr(True, "x", FULL_LABEL))[0], "level=full")

    def test_a_failed_read_is_full(self):
        self.assertEqual(self.run_with_gh(self.pr(True) + "\nexit 1")[0], "level=full")

    def test_bad_json_is_full(self):
        self.assertEqual(self.run_with_gh("echo not-json")[0], "level=full")


# One block: the `changes` job's path step has its own EVENT and set -e lines.
LEVEL_STEP = """\
        id: level
        env:
          GH_TOKEN: ${{ github.token }}
          EVENT: ${{ github.event_name }}
          DRAFT: ${{ github.event.pull_request.draft }}
          PR: ${{ github.event.pull_request.number }}
          REPO: ${{ github.repository }}
          HEAD_SHA: ${{ github.event.pull_request.head.sha }}
        run: |
          set -euo pipefail
          python3 scripts/process/ci_level.py "$EVENT" "$DRAFT" "$PR" "$REPO" "$HEAD_SHA" | tee -a "$GITHUB_OUTPUT"
"""
FULL = "needs.changes.outputs.level != 'fast'"


class Workflows(unittest.TestCase):
    """The lines that decide pass, fail or skip. Whole lines, so `|| true` or a changed value
    fails. A real run proves the behaviour; these stop a later edit. PyYAML is not here."""

    def text(self, name):
        return (WORKFLOWS / name).read_text()

    def job(self, name, job_id):
        match = re.search(rf"^  {re.escape(job_id)}:\n(.*?)(?=^  \S|\Z)", self.text(name), re.S | re.M)
        self.assertIsNotNone(match, job_id)
        return match.group(1)

    def assert_lines(self, name, job_id, *lines):
        have = [line.strip() for line in self.job(name, job_id).splitlines()]
        for line in lines:
            self.assertIn(line, have, f"{name} {job_id}")

    def test_each_two_level_workflow_starts_on_ready_for_review(self):
        for name in ("ci.yml", "e2e.yml", "native.yml"):
            self.assertIn("    types: [opened, synchronize, reopened, ready_for_review]\n", self.text(name), name)

    def test_each_changes_job_decides_the_level(self):
        for name in ("ci.yml", "e2e.yml", "native.yml"):
            self.assertIn(LEVEL_STEP, self.job(name, "changes"), name)
            self.assert_lines(name, "changes", "level: ${{ steps.level.outputs.level }}", "pull-requests: read")

    def test_the_required_names(self):
        self.assert_lines("ci.yml", "ci", "name: ${{ needs.changes.outputs.level == 'fast' && 'ci (fast)' || 'ci' }}")
        self.assert_lines("e2e.yml", "e2e-linux", "name: ${{ github.event_name == 'workflow_dispatch' && 'e2e-linux (dispatch)' "
                          "|| needs.changes.outputs.level == 'fast' && 'e2e-linux (not run on a draft)' || 'e2e-linux' }}")

    def test_a_required_job_is_never_skipped_on_a_pull_request(self):
        self.assert_lines("ci.yml", "ci", "if: always()")
        self.assert_lines("e2e.yml", "e2e-linux", "if: ${{ always() && github.event_name != 'schedule' }}")

    def test_no_other_job_uses_a_required_name(self):
        for name in ("ci.yml", "e2e.yml"):
            for required in ("ci", "e2e-linux"):
                self.assertNotRegex(self.text(name), rf"(?m)^    name: {required}$", name)

    def test_the_required_checks_wait_for_every_job_of_the_full_set(self):
        self.assert_lines("ci.yml", "ci", "needs: [changes, mobile, browser, host, check, appimage, rpm, rpm-clean-install]")
        self.assert_lines("e2e.yml", "e2e-linux", "needs: [changes, e2e-linux-run, e2e-macos-1, e2e-macos-2]")

    def test_the_required_checks_take_their_result_from_ci_verdict(self):
        self.assert_lines("ci.yml", "ci", "set -euo pipefail", "EVENT: ${{ github.event_name }}", "NEEDS: ${{ toJSON(needs) }}",
                          'python3 scripts/process/ci_verdict.py ci | tee -a "$GITHUB_OUTPUT"')
        self.assert_lines("e2e.yml", "e2e-linux", "EVENT: ${{ github.event_name }}", "NEEDS: ${{ toJSON(needs) }}",
                          "run: python3 scripts/process/ci_verdict.py e2e")
        self.assertIn("DISPATCH_SKIPS_MACOS: ${{ github.event_name == 'workflow_dispatch' && (inputs.macos_build == 'release' "
                      "|| !contains(", self.job("e2e.yml", "e2e-linux"))

    def test_the_full_set_jobs_run_only_on_the_full_level(self):
        self.assert_lines("ci.yml", "browser", f"if: ${{{{ !cancelled() && {FULL} && needs.changes.outputs.browser != 'false' }}}}")
        self.assert_lines("e2e.yml", "e2e-linux-run", "if: ${{ !cancelled() && github.event_name != 'schedule' && "
                          f"{FULL} && needs.changes.outputs.e2e-linux != 'false' }}}}")
        self.assertIn(f"&& {FULL} && needs.changes.outputs.e2e-macos != 'false' &&", self.job("e2e.yml", "e2e-macos-1"))
        self.assert_lines("e2e.yml", "e2e-macos-2", "if: *e2e-macos-if")
        self.assert_lines("native.yml", "desktop", f"if: ${{{{ !cancelled() && {FULL} && needs.changes.outputs.native != 'false' }}}}")
        self.assert_lines("native.yml", "core", "if: ${{ !cancelled() && needs.changes.outputs.native != 'false' }}")

    def test_the_fast_level_keeps_only_ubuntu_legs(self):
        self.assertIn("needs.changes.outputs.level == 'fast' && fromJSON('[\"ubuntu-latest\"]') || "
                      "fromJSON('[\"macos-latest\", \"ubuntu-latest\"]')", self.job("ci.yml", "check"))
        self.assert_lines("native.yml", "core", "os: ${{ needs.changes.outputs.level == 'fast' && fromJSON('[\"ubuntu-latest\"]') || "
                          "fromJSON('[\"ubuntu-latest\", \"macos-latest\", \"windows-latest\"]') }}")

    def test_the_level_is_not_a_path_decision(self):
        # merge-when-green.sh accepts a skipped e2e job on the `<job>=false` log line of
        # `Changed paths (e2e)`, so the draft level must never print one.
        source = SCRIPT.read_text()
        self.assertEqual(re.findall(r'print\(f"(\w[\w-]*)=', source), ["level"])


if __name__ == "__main__":
    unittest.main()
