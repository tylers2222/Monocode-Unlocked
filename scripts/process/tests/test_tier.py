import gzip
import json
import sys
import unittest
from unittest import mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import tier as T

RECORDED = Path(__file__).parent / 'fixtures' / 'tier_recorded.json.gz'
PLAN = 'src/features/orchestration/model/orchestrationPlan.ts'
PLAN_TEST = 'src/features/orchestration/model/orchestrationPlan.test.ts'


def recorded():
    with gzip.open(RECORDED, 'rt') as f:
        return json.load(f)


def run(rec):
    return T.tier(rec['files'], rec['diff'], set(rec['string_files']))


def one_file_diff(path, start, lines):
    body = '\n'.join(lines)
    return f'diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -{start},1 +{start},1 @@ fn x\n{body}\n'


def with_test(path):
    return [{'path': path, 'additions': 1}, {'path': PLAN_TEST, 'additions': 1}]


class RecordedPrs(unittest.TestCase):
    """The 62 PRs the 2026-10-20 review tiered, with their real diffs and files."""

    # Only raised, never lowered: #116 (rules text, set to T1 by hand) and #49, #111 (pure
    # model files that can hold a template literal, so any edit there may be prompt text).
    RAISED = {116: 'T3', 49: 'T2', 111: 'T2'}

    def test_every_recorded_pr_keeps_its_tier(self):
        changed = {r['n']: run(r)[0] for r in recorded() if run(r)[0] != r['tier']}
        self.assertEqual(changed, self.RAISED)

    def test_116_rules_text_is_not_t1(self):
        rec = next(r for r in recorded() if r['n'] == 116)
        self.assertEqual(rec['tier'], 'T1', 'the review set #116 to T1 by hand')
        self.assertEqual(run(rec), ('T3', ['path:src-tauri/src/control_cli.rs', 'path:src-tauri/src/mono_messages.rs']))


class StringText(unittest.TestCase):
    """Agent rules, prompts and help text in a pure folder are T2, never T1."""

    def test_ts_prompt_line_in_a_model_file_is_t2(self):
        diff = one_file_diff(PLAN, 251, ['-    "You are the orchestrator: decide the breakdown.",',
                                         '+    "You are the orchestrator: decide the task breakdown.",'])
        self.assertEqual(T.tier(with_test(PLAN), diff, set()),
                         ('T2', ['string text (agent rules, prompts, help) in: ' + PLAN]))

    def test_any_edit_in_a_file_with_a_template_is_t2(self):
        path = 'src/features/monos/model/monoHabits.ts'
        diff = one_file_diff(path, 5, ['-  return n + 1;', '+  return n + 2;'])
        self.assertEqual(T.tier(with_test(path), diff, {path})[0], 'T2')
        self.assertEqual(T.tier(with_test(path), diff, set())[0], 'T1', 'the same edit in a file with no template is code')

    def test_one_line_rust_raw_string_is_t2(self):
        path = 'src-tauri/src/agent/model/rules.rs'
        diff = one_file_diff(path, 2, ['-const R: &str = "x";', '+const R: &str = r#"Never merge without a review."#;'])
        self.assertEqual(T.tier(with_test(path), diff, set())[0], 'T2')

    def test_mid_line_literal_of_40_characters_is_t2(self):
        # handoff.ts:430 shape: the template starts mid-line, so only the 40+ branch sees it.
        path = 'src/features/sessions/model/handoff.ts'
        diff = one_file_diff(path, 430, ['-  const lead = `You are continuing a conversation from ${a}.`;',
                                         '+  const lead = `You are continuing an existing conversation from ${a}.`;'])
        self.assertEqual(T.tier(with_test(path), diff, set())[0], 'T2')

    def test_quote_first_short_literal_is_t2(self):
        diff = one_file_diff(PLAN, 252, ['-    "Never merge.",', '+    "Never merge alone.",'])
        self.assertEqual(T.tier(with_test(PLAN), diff, set())[0], 'T2')

    def test_short_literal_in_code_stays_t1(self):
        diff = one_file_diff(PLAN, 9, ['-  return kind === "a";', '+  return kind === "b";'])
        self.assertEqual(T.tier(with_test(PLAN), diff, set())[0], 'T1')

    def test_two_fence_checks_around_a_prompt_are_t2(self):
        # The r3 probe: two stray triple-backtick checks around a prompt template.
        path = 'src/features/sessions/model/plan.ts'
        src = ('if (line.startsWith("```")) inFence = true;\nconst prompt = `You are the reviewer.\n'
               'Never merge without review.\n`;\nif (/^```/.test(line)) inFence = false;\n')
        self.assertTrue(T.can_hold_multi_line_string(src, path))
        diff = one_file_diff(path, 3, ['-Never merge without review.', '+Never merge without a review.'])
        self.assertEqual(T.tier(with_test(path), diff, {path})[0], 'T2')

    def test_which_files_can_hold_a_multi_line_string(self):
        self.assertTrue(T.can_hold_multi_line_string('const a = `x`;', 'a.ts'))
        self.assertFalse(T.can_hold_multi_line_string('const a = "x";', 'a.ts'))
        self.assertTrue(T.can_hold_multi_line_string('const A: &str = r#"x"#;', 'a.rs'))
        self.assertTrue(T.can_hold_multi_line_string('let a = r"x";', 'a.rs'))
        self.assertFalse(T.can_hold_multi_line_string('let a = "x"; let for_ = 1;', 'a.rs'))


class Rules(unittest.TestCase):
    def test_gate_logic_in_scripts_process_is_t3(self):
        path = 'scripts/process/tier.py'
        files = [{'path': path, 'additions': 1}, {'path': 'scripts/process/tests/test_tier.py', 'additions': 1}]
        diff = one_file_diff(path, 25, ['-X = 1', '+X = 2'])
        self.assertEqual(T.tier(files, diff, set()), ('T3', ['gate:' + path]))

    def test_no_diff_is_t3(self):
        for diff in (None, ''):
            self.assertEqual(T.tier(with_test(PLAN), diff, set()), ('T3', ['no diff from GitHub']))

    def test_pure_code_without_a_test_is_t2(self):
        diff = one_file_diff(PLAN, 9, ['-  return 1;', '+  return 2;'])
        self.assertEqual(T.tier([{'path': PLAN, 'additions': 1}], diff, set())[0], 'T2')
        self.assertEqual(T.tier(with_test(PLAN), diff, set())[0], 'T1')


class Main(unittest.TestCase):
    def run_main(self, diff, string_files):
        path = 'src/features/monos/model/monoHabits.ts'
        files = '\n'.join(json.dumps(f) for f in with_test(path))
        answers = lambda *args, check=True: files if args[0] == 'api' else diff
        with mock.patch.object(T, 'gh', answers), \
                mock.patch.object(T, 'string_files', lambda repo, pr, f: string_files), \
                mock.patch('builtins.print') as out:
            T.main(['tier.py', '26'])
        return [c.args[0] for c in out.call_args_list]

    def test_main_reads_the_string_files(self):
        path = 'src/features/monos/model/monoHabits.ts'
        diff = one_file_diff(path, 3, ['-  return 1;', '+  return 2;'])
        self.assertTrue(self.run_main(diff, {path})[0].startswith('tier: T2'))

    def test_main_without_a_diff_is_t3(self):
        self.assertEqual(self.run_main('', set()), ['tier: T3, 1 non-test lines added', 'tier:   no diff from GitHub'])


class Labels(unittest.TestCase):
    def test_old_tier_label_is_replaced(self):
        self.assertEqual(T.label_changes(['tier:T1', 'instance:2'], 'T3'), (['tier:T1'], ['tier:T3']))

    def test_current_label_needs_no_call(self):
        self.assertEqual(T.label_changes(['tier:T3', 'process-fix'], 'T3'), ([], []))

    def test_extra_tier_label_is_removed(self):
        self.assertEqual(T.label_changes(['tier:T2', 'tier:T3'], 'T3'), (['tier:T2'], []))

    def test_t3_label_is_a_tier_label(self):
        self.assertEqual(T.label_changes(['tier:T3'], 'T1'), (['tier:T3'], ['tier:T1']))


if __name__ == '__main__':
    unittest.main()
