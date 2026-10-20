import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import workflow_events as W

SCRIPT = Path(__file__).resolve().parents[1] / 'workflow_events.py'


class Violations(unittest.TestCase):
    def test_only_gate_integrity_may_use_the_event(self):
        on = 'on:\n  pull_request_target:\n    branches: [tyler/main]\n'
        self.assertEqual(W.violations({'.github/workflows/gate-integrity.yml': on}), [])
        self.assertEqual(W.violations({'.github/workflows/ci.yml': on}), ['.github/workflows/ci.yml'])

    def test_every_way_to_name_the_event(self):
        for text in ('on: pull_request_target\n', 'on: [push, pull_request_target]\n', 'on:\n  pull_request_target:\n'):
            with self.subTest(text=text):
                self.assertEqual(W.violations({'.github/workflows/x.yaml': text}), ['.github/workflows/x.yaml'])

    def test_comments_and_other_files_do_not_count(self):
        self.assertEqual(W.violations({'.github/workflows/x.yml': 'on: push  # not pull_request_target\n# pull_request_target\n'}), [])
        self.assertEqual(W.violations({'docs/actions.md': 'pull_request_target'}), [])
        self.assertEqual(W.violations({'.github/workflows/sub/x.yml': 'on: pull_request_target\n'}), [], 'GitHub runs only top-level workflow files')


class Cli(unittest.TestCase):
    """main() on a real git repo: only the files the PR changes, at the PR head."""

    def git(self, *args):
        return subprocess.run(['git', '-C', self.repo, *args], check=True, capture_output=True, text=True).stdout.strip()

    def setUp(self):
        self.repo = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.repo)
        self.git('init', '-q', '-b', 'main')
        self.git('config', 'user.email', 't@example.com')
        self.git('config', 'user.name', 't')
        workflows = Path(self.repo) / '.github' / 'workflows'
        workflows.mkdir(parents=True)
        (workflows / 'gate-integrity.yml').write_text('on:\n  pull_request_target:\n')
        (workflows / 'ci.yml').write_text('on:\n  pull_request:\n')
        self.git('add', '-A')
        self.git('commit', '-qm', 'base')
        self.base = self.git('rev-parse', 'HEAD')
        self.workflows = workflows

    def run_cli(self):
        self.git('add', '-A')
        self.git('commit', '-qm', 'pr')
        return subprocess.run([sys.executable, str(SCRIPT), self.base, 'HEAD'], cwd=self.repo, capture_output=True, text=True)

    def test_a_new_workflow_with_the_event_fails(self):
        (self.workflows / 'deploy.yml').write_text('on:\n  pull_request_target:\njobs: {}\n')
        run = self.run_cli()
        self.assertEqual(run.returncode, 1)
        self.assertIn('::error file=.github/workflows/deploy.yml::', run.stdout)

    def test_editing_gate_integrity_or_another_trigger_passes(self):
        (self.workflows / 'gate-integrity.yml').write_text('on:\n  pull_request_target:\n    types: [opened]\n')
        (self.workflows / 'ci.yml').write_text('on:\n  pull_request:\n  push:\n')
        run = self.run_cli()
        self.assertEqual((run.returncode, run.stdout.strip()), (0, 'workflow-events: ok'))


if __name__ == '__main__':
    unittest.main()
