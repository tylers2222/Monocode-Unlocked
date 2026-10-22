import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from pr_body import HINTS, decide, lint_body, tiny_overlaps

# Excerpts of PR bodies recorded on 2026-10-20. Each keeps every line of its PR
# that the rules read, so it gives the same result as the whole body.
PR_142 = """\
Size: +395/-11 in 5 files, all non-test. `gate/integrity`: `ok, 406 non-test lines changed`.
- `scripts/scenarios/app.sh` 166 (new), `scripts/scenarios/puppet/puppet.mjs` 130 (new), `active_clock.rs` +85/-9, `instance.sh` +9/-2, `Cargo.toml` +5.

## Review gate

- **Self-pass.** I ran thermo-nuclear and deslop on the diff. Fixed:
- **Duplication.** `npx jscpd . --baseline-from-ref tyler/tyler/main --fail-on-new-clones` exits 0.
- **Size.** `ok, 406 non-test lines changed`.
- **File caps.** `active_clock.rs` is 403 lines. The new files are 166 and 130 lines.
- **Suppressions.** None added.
"""

PR_155 = """\
Change R v2, issue R5, part 2 of 2. Linear: [ADE-23](https://linear.app/adebuild/issue/ADE-23/r5-ask-and-ack-ledger).
Review rounds: 0

Part 1 (#150) is merged as `ed381066`. This PR is based on it.

Size: +591/-10 in 5 files against `ed381066`. `gate/integrity ed381066 HEAD`: `ok, 212 non-test lines changed`. Non-test: `asks.rs` +147/-7, `ask_gate.rs` +55/-2, `mono_messages.rs` +1. The scenario files are under `tests/` and count as test code.
"""

PR_159 = """\
Review rounds: 0

Size: +436/-143 in 4 files against `033ff007`. `gate/integrity 033ff007 HEAD`: `ok, 62 non-test lines changed` (all in `MilestoneTimeline.tsx`).

## Review gate
- jscpd: `npx jscpd . --baseline-from-ref tyler/tyler/main --fail-on-new-clones` exit 0, no new clones.
- Size: 62 non-test lines (cap about 400).
- Complexity: eslint `complexity` 15 and `max-lines-per-function` 80 pass on all 4 files (`eslint --max-warnings 0`).
- File caps: `MilestoneTimeline.tsx` 451 lines (cap 600); new files 264 and 122.
- Suppressions: 0 new (`eslint-disable`, `ts-ignore`, `ts-expect-error`).
"""

PR_160 = """\
- `eslint` on both files and `tsc --noEmit` pass. `scripts/gate/integrity.sh` prints `integrity: ok, 10 non-test lines changed`. No new duplication, no suppressions, no function grows past the complexity limit.
- Self-pass (thermo-nuclear and deslop). It removed a probe log, and kept the comment to the one fact a reader needs: why a Continue can be asked for twice.

## Size

10 non-test lines (+7 −3) in `App.tsx`. Tests: +189 in one new spec.

## Review

Review rounds: 0
"""

PR_161 = """\
## Size

Non-test: +20 / -171 in 7 files. Test: +0 / -72 in 4 files.

## Review gate (self-check)

I ran thermo-nuclear and deslop on this diff. One finding, fixed: `historyForCwd` became a one-caller helper whose promise parameter existed only for the deleted branch, so I inlined it.

- No new duplicate code. The diff only deletes code, apart from the inline.
- Non-test lines: +20 / -171, under 400.
- No function grew past the complexity limit. `loadBootWorkspace` lost a branch.
- No large file grew. `App.tsx` lost 15 lines and `appLifecycle.ts` lost 31.
"""


def without_line(body: str, prefix: str) -> str:
    lines = body.splitlines()
    (index,) = [i for i, line in enumerate(lines) if line.startswith(prefix)]
    return "\n".join(lines[:index] + lines[index + 1 :])


class LintBodyTest(unittest.TestCase):
    def test_pr_142_passes(self):
        self.assertEqual(lint_body(PR_142), [])

    def test_pr_155_misses_self_pass_and_file_caps(self):
        self.assertEqual(lint_body(PR_155), ["self-pass", "file-caps"])

    def test_pr_159_misses_self_pass(self):
        self.assertEqual(lint_body(PR_159), ["self-pass"])

    def test_pr_160_size_with_unicode_minus_counts(self):
        self.assertEqual(lint_body(PR_160), ["file-caps"])

    def test_pr_161_size_with_spaced_counts(self):
        self.assertEqual(lint_body(PR_161), ["file-caps"])

    def test_size_without_counts_is_missing(self):
        self.assertEqual(lint_body(re.sub(r"\+\d+ / -\d+", "some", PR_161)), ["size", "file-caps"])

    def test_dropping_the_size_line_misses_size(self):
        self.assertEqual(lint_body(without_line(PR_142, "Size:")), ["size"])

    def test_dropping_the_self_pass_line_misses_self_pass(self):
        self.assertEqual(lint_body(without_line(PR_142, "- **Self-pass.**")), ["self-pass"])

    def test_dropping_the_file_caps_line_misses_file_caps(self):
        self.assertEqual(lint_body(without_line(PR_142, "- **File caps.**")), ["file-caps"])

    def test_size_line_may_be_a_heading_or_a_bold_bullet(self):
        for body in (
            "## Size\n\nNon-test: +5/-2 in 1 file (`src/shared/ui/Popover.tsx`). Test: +37.",
            "- **Size:** non-test +141/-28, under 400.",
        ):
            with self.subTest(body=body):
                self.assertNotIn("size", lint_body(body))

    def test_size_must_start_its_line(self):
        body = (
            "The round-1 review found 0 blockers, 0 majors, 4 minors and 3 nits. Size: `gate/integrity` "
            "`ok, 401 non-test lines changed` against `f808e4af`. The fix commit is +203/-98 in 3 files."
        )
        self.assertIn("size", lint_body(body))

    def test_size_line_reads_itself_and_the_next_three_non_empty_lines(self):
        counts = "+20/-171 in 7 files, 191 non-test lines"
        self.assertNotIn("size", lint_body(f"Size:\n\nOne.\n\nTwo.\n\n{counts}\n"))
        self.assertIn("size", lint_body(f"Size:\nOne.\nTwo.\nThree.\n{counts}\n"))

    def test_size_heading_reads_its_whole_section(self):
        # PR 150: the counts sit under "## Size and split", after a table.
        body = ("## Size and split\n\nR5 as one PR was 576 non-test lines, over the 500 cap.\n\n| a | b |\n|---|---|\n"
                "| 1/2 | x |\n| 2/2 | y |\n\nPart 1: +829/-48 in 8 files against `a363d977`. Non-test: `asks.rs` 255.\n")
        self.assertNotIn("size", lint_body(body))
        self.assertIn("size", lint_body("## Size\n\nNon-test: `asks.rs` 255.\n\n## Next\n\nPart 1: +829/-48 in 8 files\n"))

    def test_size_needs_both_counts_and_non_test(self):
        for body in ("Size: +31/-20 in 3 files.", "- Size: 62 non-test lines (cap about 400)."):
            with self.subTest(body=body):
                self.assertIn("size", lint_body(body))

    def test_self_pass_needs_both_passes_on_one_line(self):
        body = "- **Thermo-nuclear:** no finding.\n- **Deslop:** no finding."
        self.assertIn("self-pass", lint_body(body))

    def test_self_pass_accepts_thermonuclear_in_any_case(self):
        self.assertNotIn("self-pass", lint_body("- **Self-pass.** I ran Thermonuclear and Deslop on the diff."))


def open_pr(number: int, *paths: str, base: str = "tyler/main") -> dict:
    return {"number": number, "files": [{"path": path} for path in paths], "baseRefName": base, "headRefName": f"b{number}"}


# PR 160 (10 non-test lines) and PR 161 both changed src/app/App.tsx.
PR_160_FILES = {"src/app/App.tsx", "tests/browser/ade60-resume-once.spec.ts"}
OPEN_159 = open_pr(159, "src/features/milestones/ui/MilestoneTimeline.tsx", "tests/browser/milestones-page.spec.ts")
OPEN_161 = open_pr(161, "src-tauri/src/lib.rs", "src/app/App.tsx", "src/main.tsx")


class TinyOverlapsTest(unittest.TestCase):
    def test_99_non_test_lines_with_a_shared_file_warn(self):
        self.assertEqual(tiny_overlaps(99, PR_160_FILES, [OPEN_159, OPEN_161], 160), [(161, ["src/app/App.tsx"])])

    def test_100_non_test_lines_never_warn(self):
        self.assertEqual(tiny_overlaps(100, PR_160_FILES, [OPEN_159, OPEN_161], 160), [])

    def test_the_pr_itself_is_not_an_overlap(self):
        self.assertEqual(tiny_overlaps(10, PR_160_FILES, [open_pr(160, *PR_160_FILES), OPEN_159], 160), [])

    def test_no_shared_file_no_warning(self):
        self.assertEqual(tiny_overlaps(10, PR_160_FILES, [OPEN_159], 160), [])

    def test_overlaps_sort_by_pr_number_with_sorted_paths(self):
        pr_155 = {
            "src-tauri/src/ask_gate.rs",
            "src-tauri/src/asks.rs",
            "src-tauri/src/mono_messages.rs",
            "src-tauri/src/tests/scenario/asks.rs",
            "src-tauri/src/tests/scenario/world.rs",
        }
        open_prs = [
            open_pr(
                150,
                "src-tauri/src/tests/scenario/asks.rs",
                "src-tauri/src/mono_messages.rs",
                "src-tauri/src/lib.rs",
                "src-tauri/src/asks.rs",
                "src-tauri/src/ask_gate.rs",
            ),
            open_pr(142, "src-tauri/src/tests/scenario/world.rs", "src-tauri/src/active_clock.rs"),
        ]
        self.assertEqual(
            tiny_overlaps(40, pr_155, open_prs, 155),
            [
                (142, ["src-tauri/src/tests/scenario/world.rs"]),
                (
                    150,
                    [
                        "src-tauri/src/ask_gate.rs",
                        "src-tauri/src/asks.rs",
                        "src-tauri/src/mono_messages.rs",
                        "src-tauri/src/tests/scenario/asks.rs",
                    ],
                ),
            ],
        )


class TicketPartTest(unittest.TestCase):
    ADE = "ADE-53: approvals board"

    def test_good_line_on_an_ade_pr(self):
        self.assertNotIn("ticket-part", lint_body(PR_142 + "\nTicket-part: 2/3\n", self.ADE))

    def test_missing_line_on_an_ade_pr(self):
        self.assertIn("ticket-part", lint_body(PR_142, self.ADE))

    def test_only_the_exact_line_with_1_to_n_passes(self):
        for line in ("Ticket-part: 4/3", "Ticket-part: 0/3", "Ticket-part: 1/3 (draft)", "ticket-part: 1/3", "Ticket-part: 1 / 2"):
            with self.subTest(line=line):
                self.assertIn("ticket-part", lint_body(PR_142 + f"\n{line}\n", self.ADE))

    def test_non_ade_pr_needs_no_line(self):
        self.assertEqual(lint_body(PR_142, "Process fixes D2: body lint"), [])
        self.assertEqual(lint_body(PR_142, "Fix ADE-53 follow-up"), [], "only a title that starts with ADE-<digits>")
        self.assertEqual(lint_body(PR_142, "ADE-5x: odd"), [], "ADE-5x is not a ticket id")


class StackTest(unittest.TestCase):
    def test_own_stack_is_not_an_overlap(self):
        parent = open_pr(171, "a.yml")
        me = open_pr(174, "a.yml", base="b171")
        child = open_pr(195, "a.yml", base="b174")
        other = open_pr(200, "a.yml")
        self.assertEqual(tiny_overlaps(10, {"a.yml"}, [parent, me, child, other], 174), [(200, ["a.yml"])])


class DecideTest(unittest.TestCase):
    COMPLETE = PR_142

    def test_overlap_warns_and_still_passes(self):
        lines, code = decide(self.COMPLETE, "", {"src/app/App.tsx"}, [open_pr(161, "src/app/App.tsx")], 10, 160)
        self.assertEqual(code, 0)
        self.assertTrue(lines[-1].startswith("::warning::tiny PR (10 non-test lines)"))

    def test_missing_item_fails(self):
        self.assertEqual(decide("No body items.", "", set(), [], 10, 160)[1], 1)

    def test_without_nt_the_body_is_still_checked(self):
        self.assertEqual(decide("No body items.", "", set(), None, None, 160),
                         ([f"pr-body: missing {i}: {HINTS[i]}" for i in ("size", "self-pass", "file-caps")], 1))

    def test_paths_cannot_end_the_warning_line(self):
        lines, _ = decide(self.COMPLETE, "", {"a\nb%"}, [open_pr(161, "a\nb%")], 10, 160)
        self.assertNotIn("\n", lines[-1])
        self.assertIn("a%0Ab%25", lines[-1])


class GhBoundaryTest(unittest.TestCase):
    def run_with_fake_gh(self, gh_script, *args):
        with tempfile.TemporaryDirectory() as bin_dir:
            fake = Path(bin_dir) / "gh"
            fake.write_text(gh_script)
            fake.chmod(0o755)
            script = Path(__file__).resolve().parents[1] / "pr_body.py"
            env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}
            return subprocess.run([sys.executable, str(script), *args], env=env, capture_output=True, text=True)

    def test_gh_failure_exits_2(self):
        self.assertEqual(self.run_with_fake_gh("#!/bin/sh\nexit 1\n", "160", "--nt", "10").returncode, 2)

    def test_the_title_reaches_the_lint_through_main(self):
        # The fake gh answers only the fields it is asked for, as the real one does.
        answers = {}
        for name, fields in (("with", {"title": "ADE-53: x"}), ("without", {})):
            with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as answer:
                json.dump({"body": PR_142, "files": [], **fields}, answer)
            self.addCleanup(os.unlink, answer.name)
            answers[name] = answer.name
        gh = f'#!/bin/sh\ncase "$*" in *title*) cat {answers["with"]} ;; *) cat {answers["without"]} ;; esac\n'
        run = self.run_with_fake_gh(gh, "160")
        self.assertEqual((run.returncode, run.stdout.splitlines()), (1, [f"pr-body: missing ticket-part: {HINTS['ticket-part']}"]))

    def test_without_nt_the_cli_still_lints(self):
        # gate/tier failed, so the step passes no --nt; the body must still be checked.
        run = self.run_with_fake_gh("#!/bin/sh\necho '{\"body\": \"\", \"files\": [], \"title\": \"x\"}'\n", "160")
        self.assertEqual(run.returncode, 1)
        self.assertEqual(run.stdout.splitlines(), [f"pr-body: missing {i}: {HINTS[i]}" for i in ("size", "self-pass", "file-caps")])


if __name__ == "__main__":
    unittest.main()
