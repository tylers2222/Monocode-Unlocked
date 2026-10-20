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
VITEST = {'version': '3.2.4', 'resolved': 'https://registry.npmjs.org/vitest/-/vitest-3.2.4.tgz', 'integrity': 'sha512-x'}


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
        entries = [e for e in CLEAN if e[0] != 'vitest-runner']
        self.assertEqual(S.stryker_scope_problems(entries, PIN), ['@stryker-mutator/vitest-runner is missing'])

    def test_the_committed_lockfile_pins_the_same_set(self):
        packages = json.loads(LOCK.read_text())['packages']
        pinned = {path.split('/')[-1]: entry['version'] for path, entry in packages.items()
                  if path.startswith('node_modules/@stryker-mutator/') and path.count('/') == 2}
        self.assertEqual(pinned, {name: PIN for name in S.STRYKER_PACKAGES})
        self.assertTrue(all(entry['resolved'].startswith(S.REGISTRY) and entry.get('integrity')
                            for path, entry in packages.items() if path))
        self.assertNotIn('node_modules/vitest', packages, 'vitest comes from the PR tree, linked in by mutation.sh')


class VitestProblems(unittest.TestCase):
    def test_a_registry_vitest_passes(self):
        self.assertEqual(S.vitest_problems(VITEST, False), [])

    def test_a_missing_linked_or_non_registry_vitest_fails(self):
        for entry, link in ((None, False), (VITEST, True), ({**VITEST, 'link': True}, False),
                            ({**VITEST, 'resolved': 'file:evil'}, False), ({**VITEST, 'integrity': ''}, False)):
            with self.subTest(entry=entry, link=link):
                self.assertEqual(len(S.vitest_problems(entry, link)), 1)


class Cli(unittest.TestCase):
    """main() on real folders: the job tree's @stryker-mutator scope and the PR tree's lock."""

    def tree(self, extra=None):
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root)
        job, pr = root / 'job', root / 'pr'
        for name in sorted(S.STRYKER_PACKAGES | ({extra} if extra else set())):
            (job / '@stryker-mutator' / name).mkdir(parents=True)
            (job / '@stryker-mutator' / name / 'package.json').write_text(json.dumps({'version': PIN}))
        (pr / 'vitest').mkdir(parents=True)
        (pr / '.package-lock.json').write_text(json.dumps({'packages': {'node_modules/vitest': VITEST}}))
        return job, pr

    def run_cli(self, job, pr):
        return subprocess.run([sys.executable, str(SCRIPT), str(job), str(pr), PIN], capture_output=True, text=True)

    def test_clean_trees_pass(self):
        run = self.run_cli(*self.tree())
        self.assertEqual((run.returncode, run.stdout.strip()), (0, 'stryker-scope: ok'))

    def test_an_extra_package_in_the_job_tree_fails(self):
        run = self.run_cli(*self.tree(extra='forge'))
        self.assertEqual(run.returncode, 1)
        self.assertIn('::error::@stryker-mutator/forge is not a package the job installs', run.stdout)


if __name__ == '__main__':
    unittest.main()
