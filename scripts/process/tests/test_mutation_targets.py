import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from mutation_targets import targets


def file_diff(path, *hunks):
    head = f'diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n'
    return head + ''.join(f'{h}\n+x\n' for h in hunks)


class Targets(unittest.TestCase):
    def test_changed_ts_lines_become_stryker_ranges(self):
        diff = file_diff('src/app/a.ts', '@@ -3,0 +4,2 @@', '@@ -10 +12 @@')
        self.assertEqual(targets(diff), (False, ['src/app/a.ts:4-5', 'src/app/a.ts:12-12']))

    def test_pure_deletion_has_no_range(self):
        self.assertEqual(targets(file_diff('src/app/a.ts', '@@ -3,2 +2,0 @@')), (False, []))

    def test_test_files_and_declarations_are_not_mutated(self):
        diff = file_diff('src/app/a.test.ts', '@@ -1 +1 @@') + file_diff('src/types.d.ts', '@@ -1 +1 @@')
        self.assertEqual(targets(diff), (False, []))

    def test_rust_counts_only_outside_tests(self):
        self.assertEqual(targets(file_diff('src-tauri/src/foo/tests.rs', '@@ -1 +1 @@')), (False, []))
        self.assertEqual(targets(file_diff('src-tauri/src/foo.rs', '@@ -1 +1 @@')), (True, []))

    def test_deleted_file_is_skipped(self):
        diff = 'diff --git a/src/a.ts b/src/a.ts\n--- a/src/a.ts\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-x\n-y\n'
        self.assertEqual(targets(diff), (False, []))


if __name__ == '__main__':
    unittest.main()
