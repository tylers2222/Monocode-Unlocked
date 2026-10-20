import gzip
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import tier as T

RECORDED = Path(__file__).parent / 'fixtures' / 'tier_recorded.json.gz'


def recorded():
    with gzip.open(RECORDED, 'rt') as f:
        return json.load(f)


def run(rec):
    raw = {p: (set(old), set(new)) for p, (old, new) in rec['raw'].items()}
    return T.tier(rec['files'], rec['diff'], raw)


def one_file_diff(path, start, lines):
    body = '\n'.join(lines)
    return f'diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -{start},1 +{start},1 @@ fn x\n{body}\n'


class RecordedPrs(unittest.TestCase):
    """The 62 PRs the 2026-10-20 review tiered by hand-checked script, with their real diffs."""

    def test_every_recorded_pr_keeps_its_tier(self):
        wrong = [(r['n'], r['tier'], run(r)) for r in recorded() if r['n'] != 116 and run(r)[0] != r['tier']]
        self.assertEqual(wrong, [])

    def test_116_rules_text_is_not_t1(self):
        rec = next(r for r in recorded() if r['n'] == 116)
        self.assertEqual(rec['tier'], 'T1', 'the review set #116 to T1 by hand')
        self.assertEqual(run(rec)[0], 'T3')


class RawStrings(unittest.TestCase):
    def test_lines_of_a_multi_line_raw_string(self):
        src = 'const A: &str = r##"first\nsecond "quoted"\n"##;\nlet b = r#"one line"#;\nfn c() {}\n'
        self.assertEqual(T.raw_string_lines(src), {1, 2, 3})

    def test_plain_strings_and_one_line_raw_strings_are_code(self):
        self.assertEqual(T.raw_string_lines('let a = "r";\nlet b = r"x";\nlet c = br#"y"#;\n'), set())

    def test_rules_text_in_a_pure_folder_is_t2_not_t1(self):
        path = 'src/features/x/model/rules.rs'
        files = [{'path': path, 'additions': 1}, {'path': 'src/features/x/model/rules.test.ts', 'additions': 1}]
        diff = one_file_diff(path, 2, ['-Old rule.', '+New rule.'])
        self.assertEqual(T.tier(files, diff, {path: ({2}, {2})})[0], 'T2')
        self.assertEqual(T.tier(files, diff, {})[0], 'T1', 'the same lines as code stay T1')

    def test_rules_text_does_not_lower_a_state_path(self):
        path = 'src-tauri/src/control_cli.rs'
        files = [{'path': path, 'additions': 1}]
        diff = one_file_diff(path, 5, ['-Old help.', '+New help.'])
        self.assertEqual(T.tier(files, diff, {path: ({5}, {5})})[0], 'T3')


class Interrogate(unittest.TestCase):
    def test_review_outliers_need_interrogate(self):
        for rec in recorded():
            if rec['n'] in (18, 103):
                tier, why = run(rec)
                self.assertTrue(T.needs_interrogate(tier, T.non_test_lines(rec['files']), why), rec['n'])

    def test_t3_from_300_non_test_lines(self):
        why = ['path:src/features/sessions/data/sessionStore.ts']
        self.assertFalse(T.needs_interrogate('T3', 299, why))
        self.assertTrue(T.needs_interrogate('T3', 300, why))

    def test_t3_gate_or_concurrency_file_at_any_size(self):
        self.assertTrue(T.needs_interrogate('T3', 5, ['gate:scripts/gate/integrity.sh']))
        self.assertTrue(T.needs_interrogate('T3', 5, ['code:src-tauri/src/x.rs:Mutex']))
        self.assertTrue(T.needs_interrogate('T3', 5, ['path:src-tauri/src/heavy_lock.rs']))
        self.assertFalse(T.needs_interrogate('T3', 5, ['code:src/app/App.tsx:useState']))

    def test_only_t3(self):
        self.assertFalse(T.needs_interrogate('T2', 900, ['app logic: src/a.ts']))
