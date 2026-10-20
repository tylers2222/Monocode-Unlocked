import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import workflow_events as W

SCRIPT = Path(__file__).resolve().parents[1] / 'workflow_events.py'
WORKFLOWS = Path(__file__).resolve().parents[3] / '.github' / 'workflows'


def failing(text, path='.github/workflows/x.yml'):
    return [p for p, _ in W.violations({path: text})]


class Violations(unittest.TestCase):
    def test_only_gate_integrity_may_use_the_event(self):
        on = 'on:\n  pull_request_target:\n    branches: [tyler/main]\n'
        self.assertEqual(failing(on, '.github/workflows/gate-integrity.yml'), [])
        self.assertEqual(failing(on, '.github/workflows/ci.yml'), ['.github/workflows/ci.yml'])

    def test_every_way_to_name_the_event(self):
        for text in ('on: pull_request_target\n', 'on: [push, pull_request_target]\n', 'on:\n  pull_request_target:\n',
                     'on: >\n  pull_request_target\n'):
            with self.subTest(text=text):
                self.assertEqual(failing(text, '.github/workflows/x.yaml'), ['.github/workflows/x.yaml'])

    def test_yaml_spellings_that_hide_the_event_fail(self):
        for text in ('on:\n  "pull_request\\u005ftarget":\n    branches: [tyler/main]\n',
                     'on: ["pull_request_\\\n  target"]\n',
                     'on: "pull_request\\x5ftarget"\n',
                     'on: ["pull_request\\U0000005ftarget"]\n',
                     "on:\n  'pull_request':\n    branches: [main]\n",
                     'x: &e "pull_request\\u005ftarget"\non: *e\n',
                     'x: &e "pull_request\\u005ftarget"\non: [push, *e]\n',
                     'on: !!str pull_request\n',
                     'jobs: {}\n'):
            with self.subTest(text=text):
                self.assertEqual(failing(text), ['.github/workflows/x.yml'])

    def test_globs_comments_and_other_files_do_not_count(self):
        self.assertEqual(failing("on:\n  push:\n    branches: ['**']\n    paths: ['src/**', \"*.md\"]\n  pull_request:\n"), [])
        self.assertEqual(failing('on: push  # not pull_request_target\n# pull_request_target\n'), [])
        self.assertEqual(failing('pull_request_target', 'docs/actions.md'), [])
        self.assertEqual(failing('on: pull_request_target\n', '.github/workflows/sub/x.yml'), [], 'GitHub runs only top-level workflow files')

    def test_the_repos_own_workflows_pass(self):
        files = {f'.github/workflows/{p.name}': p.read_text() for p in WORKFLOWS.glob('*.y*ml')}
        self.assertGreater(len(files), 3)
        self.assertEqual(W.violations(files), [])


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
