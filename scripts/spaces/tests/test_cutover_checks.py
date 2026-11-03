"""cutover_checks.py: the env check, the import count checks and the manifest.

    python3 -I scripts/spaces/tests/test_cutover_checks.py
"""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("cutover_checks", Path(__file__).resolve().parents[1] / "cutover_checks.py")
checks = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checks)

APP = "/Applications/MonoCode 2.app/Contents/MacOS/monocode"


class EnvVerdict(unittest.TestCase):
    def test_value_with_a_space_is_read_whole(self):
        allowed = {"CODEX_HOME": "/Users/u/Library/Application Support/x"}
        env = f"{APP} HOME=/Users/u CODEX_HOME=/Users/u/Library/Application Support/x PATH=/usr/bin"
        self.assertEqual(checks.env_verdict(env, allowed), (["CODEX_HOME=/Users/u/Library/Application Support/x"], []))

    def test_value_at_the_end_of_the_env(self):
        allowed = {"CODEX_HOME": "/Users/u/.codex-2"}
        self.assertEqual(checks.env_verdict(f"{APP} CODEX_HOME=/Users/u/.codex-2", allowed)[1], [])

    def test_var_not_in_ls_environment_is_bad(self):
        env = f"{APP} HOME=/Users/u MONOCODE_APP_TOKEN=dummy"
        self.assertEqual(checks.env_verdict(env, {}), ([], ["MONOCODE_APP_TOKEN"]))

    def test_other_value_than_ls_environment_is_bad(self):
        allowed = {"CODEX_HOME": "/Users/u/.codex-2"}
        self.assertEqual(checks.env_verdict(f"{APP} CODEX_HOME=/Users/u/.codex", allowed)[1], ["CODEX_HOME"])

    def test_allowed_value_as_a_prefix_is_bad(self):
        allowed = {"CODEX_HOME": "/Users/u/.codex"}
        self.assertEqual(checks.env_verdict(f"{APP} CODEX_HOME=/Users/u/.codex-other", allowed)[1], ["CODEX_HOME"])

    def test_non_agent_vars_are_ignored(self):
        self.assertEqual(checks.env_verdict(f"{APP} HOME=/Users/u XCLAUDE=1", {}), ([], []))


TABLE = {"source": 2, "before": 1, "insert": 2, "after": 3}


class CountChecks(unittest.TestCase):
    def test_dry_after_must_be_before_plus_insert(self):
        self.assertEqual(checks.dry_mismatches({"tables": {"monos": TABLE}}), [])
        self.assertEqual(checks.dry_mismatches({"tables": {"monos": {**TABLE, "after": 4}}}), ["monos"])

    def test_dry_insert_cannot_exceed_space2_rows(self):
        self.assertEqual(checks.dry_mismatches({"tables": {"monos": {**TABLE, "insert": 3, "after": 4}}}), ["monos"])

    def test_apply_equal_to_dry_and_live(self):
        run = {"tables": {"monos": TABLE}, "registered": ["a"]}
        self.assertEqual(checks.apply_mismatches(run, run, lambda t: 3), [])

    def test_apply_count_differs_from_dry(self):
        dry = {"tables": {"monos": {**TABLE, "insert": 3}}, "registered": []}
        real = {"tables": {"monos": TABLE}, "registered": []}
        self.assertEqual(checks.apply_mismatches(dry, real, lambda t: 3), ["monos"])

    def test_apply_registered_and_live_differ(self):
        dry = {"tables": {"monos": TABLE}, "registered": ["a"]}
        real = {"tables": {"monos": TABLE}, "registered": ["b"]}
        self.assertEqual(checks.apply_mismatches(dry, real, lambda t: 2), ["monos(live)", "registered"])


class Manifest(unittest.TestCase):
    def test_absent_root(self):
        self.assertEqual(checks.manifest_lines(Path("/nonexistent/made-up")), ["ABSENT"])

    def test_same_tree_same_manifest_and_a_changed_byte_differs(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b = Path(tmp, "a"), Path(tmp, "b")
            for root in (a, b):
                (root / "sub").mkdir(parents=True)
                (root / "sub" / "f.txt").write_text("made-up")
                (root / "link").symlink_to("sub/f.txt")
            self.assertEqual(checks.manifest_lines(a), checks.manifest_lines(b))
            (b / "sub" / "f.txt").write_text("made-uq")
            self.assertNotEqual(checks.manifest_lines(a), checks.manifest_lines(b))


if __name__ == "__main__":
    unittest.main()
