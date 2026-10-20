import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import stryker_scope as S

SCRIPT = Path(__file__).resolve().parents[1] / 'stryker_scope.py'
LOCK = Path(__file__).resolve().parents[1] / 'stryker' / 'package-lock.json'
PIN = '9.6.1'
CLEAN = [(name, PIN, False) for name in sorted(S.STRYKER_PACKAGES)]


class StrykerScopeProblems(unittest.TestCase):
    def test_the_pinned_set_passes(self):
        self.assertEqual(S.stryker_scope_problems(CLEAN, PIN), [])

    def test_an_extra_package_fails(self):
        self.assertEqual(S.stryker_scope_problems(CLEAN + [('forge', PIN, False)], PIN),
                         ['@stryker-mutator/forge is not a package the job installs'])

    def test_a_link_fails(self):
        entries = [(n, v, n == 'util') for n, v, _ in CLEAN]
        self.assertEqual(S.stryker_scope_problems(entries, PIN), ['@stryker-mutator/util is a link'])

    def test_a_wrong_version_fails(self):
        entries = [(n, '9.0.0' if n == 'core' else v, link) for n, v, link in CLEAN]
        self.assertEqual(S.stryker_scope_problems(entries, PIN), ['@stryker-mutator/core is 9.0.0, not 9.6.1'])

    def test_a_missing_package_fails(self):
        entries = [e for e in CLEAN if e[0] != 'instrumenter']
        self.assertEqual(S.stryker_scope_problems(entries, PIN), ['@stryker-mutator/instrumenter is missing'])

    def test_the_committed_lockfile_pins_the_same_set(self):
        packages = json.loads(LOCK.read_text())['packages']
        pinned = {path.split('/')[-1]: entry['version'] for path, entry in packages.items()
                  if path.startswith('node_modules/@stryker-mutator/') and path.count('/') == 2}
        self.assertEqual(pinned, {name: PIN for name in S.STRYKER_PACKAGES})
        self.assertTrue(all(entry['resolved'].startswith('https://registry.npmjs.org/') and entry.get('integrity')
                            for path, entry in packages.items() if path))
        self.assertFalse([path for path in packages if 'vitest' in path], 'the PR\'s vitest runs only in a child process')


class StrykerConfig(unittest.TestCase):
    def test_the_command_runner_runs_the_related_tests_in_a_child_process(self):
        config = S.stryker_config("src/a.ts:1-3,src/a.ts:9-9,src/b c'.ts:2-2")
        self.assertEqual((config['testRunner'], config['plugins']), ('command', []), 'no plugin and no PR module in Stryker core')
        self.assertEqual(config['commandRunner']['command'], "node_modules/.bin/vitest related --run src/a.ts 'src/b c'\"'\"'.ts'")
        self.assertEqual(config['mutate'], ['src/a.ts:1-3', 'src/a.ts:9-9', "src/b c'.ts:2-2"])
        self.assertEqual(config['thresholds']['break'], 100, 'one surviving mutant fails the run')

    def test_cli_prints_the_config(self):
        run = subprocess.run([sys.executable, str(SCRIPT), 'config', 'src/a.ts:1-1'], capture_output=True, text=True)
        self.assertEqual(json.loads(run.stdout), S.stryker_config('src/a.ts:1-1'))


class Cli(unittest.TestCase):
    """main() on a real job tree's @stryker-mutator scope."""

    def tree(self, extra=None):
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root)
        job = root / 'job'
        for name in sorted(S.STRYKER_PACKAGES | ({extra} if extra else set())):
            (job / '@stryker-mutator' / name).mkdir(parents=True)
            (job / '@stryker-mutator' / name / 'package.json').write_text(json.dumps({'version': PIN}))
        return job

    def run_cli(self, job):
        return subprocess.run([sys.executable, str(SCRIPT), 'check', str(job), PIN], capture_output=True, text=True)

    def test_clean_trees_pass(self):
        run = self.run_cli(self.tree())
        self.assertEqual((run.returncode, run.stdout.strip()), (0, 'stryker-scope: ok'))

    def test_an_extra_package_in_the_job_tree_fails(self):
        run = self.run_cli(self.tree(extra='forge'))
        self.assertEqual(run.returncode, 1)
        self.assertIn('::error::@stryker-mutator/forge is not a package the job installs', run.stdout)


if __name__ == '__main__':
    unittest.main()
