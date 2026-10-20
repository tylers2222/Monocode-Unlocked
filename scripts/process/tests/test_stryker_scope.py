import json
import os
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
REPO_LOCK = Path(__file__).resolve().parents[3] / 'package-lock.json'
PIN = '9.6.1'
CLEAN = [(name, PIN, False) for name in sorted(S.STRYKER_PACKAGES)]


def mutant(status, line=1, covered=('t1',), mutator='ArithmeticOperator'):
    return {'mutatorName': mutator, 'status': status, 'location': {'start': {'line': line}}, 'coveredBy': list(covered)}


def report(files):
    return {'files': {path: {'mutants': mutants} for path, mutants in files.items()}}


class StrykerScopeProblems(unittest.TestCase):
    def test_the_pinned_set_passes(self):
        self.assertEqual(S.stryker_scope_problems(CLEAN, PIN), [])

    def test_an_extra_package_a_link_a_wrong_version_or_a_missing_one_fails(self):
        self.assertEqual(S.stryker_scope_problems(CLEAN + [('forge', PIN, False)], PIN),
                         ['@stryker-mutator/forge is not a package the job installs'])
        self.assertEqual(S.stryker_scope_problems([(n, v, n == 'util') for n, v, _ in CLEAN], PIN), ['@stryker-mutator/util is a link'])
        self.assertEqual(S.stryker_scope_problems([(n, '9.0.0' if n == 'core' else v, l) for n, v, l in CLEAN], PIN),
                         ['@stryker-mutator/core is 9.0.0, not 9.6.1'])
        self.assertEqual(S.stryker_scope_problems([e for e in CLEAN if e[0] != 'vitest-runner'], PIN),
                         ['@stryker-mutator/vitest-runner is missing'])

    def test_the_committed_lockfile_pins_stryker_and_the_repos_vitest(self):
        packages = json.loads(LOCK.read_text())['packages']
        pinned = {path.split('/')[-1]: entry['version'] for path, entry in packages.items()
                  if path.startswith('node_modules/@stryker-mutator/') and path.count('/') == 2}
        self.assertEqual(pinned, {name: PIN for name in S.STRYKER_PACKAGES})
        repo = json.loads(REPO_LOCK.read_text())['packages']
        self.assertEqual(packages['node_modules/vitest']['version'], repo['node_modules/vitest']['version'],
                         'the job runs the vitest the repo locks; the PR cannot pick another')
        self.assertTrue(all(entry['resolved'].startswith('https://registry.npmjs.org/') and entry.get('integrity')
                            for path, entry in packages.items() if path))


class Configs(unittest.TestCase):
    def test_one_run_per_changed_file(self):
        self.assertEqual(S.by_file("src/a.ts:1-3,src/b c.ts:2-2,src/a.ts:9-9"),
                         {'src/a.ts': ['src/a.ts:1-3', 'src/a.ts:9-9'], 'src/b c.ts': ['src/b c.ts:2-2']})

    def test_the_run_uses_per_test_coverage_from_the_job_owned_runner(self):
        config = S.stryker_config(['src/a.ts:1-3'], '/tmp/r.json')
        self.assertEqual((config['testRunner'], config['plugins']), ('vitest', ['@stryker-mutator/vitest-runner']))
        self.assertEqual(config['coverageAnalysis'], 'perTest', 'an untested mutant must be NoCoverage, not killed')
        self.assertEqual((config['concurrency'], config['timeoutFactor']), (1, 3), 'a slow run must not time out as a kill')
        self.assertEqual((config['mutate'], config['jsonReporter'], config['thresholds']['break']),
                         (['src/a.ts:1-3'], {'fileName': '/tmp/r.json'}, 100))
        self.assertIn('json', config['reporters'])

    def test_the_count_instruments_without_a_test_run(self):
        config = S.count_config(['src/a.ts:1-3'])
        self.assertEqual((config['dryRunOnly'], config['testRunner'], config['commandRunner']['command'], config['mutate']),
                         (True, 'command', 'node -e 0', ['src/a.ts:1-3']))

    def test_the_mutant_count_comes_from_strykers_log(self):
        self.assertEqual(S.mutant_count('04:00:01 (9) INFO Instrumenter Instrumented 1 source file(s) with 12 mutant(s)'), 12)
        self.assertIsNone(S.mutant_count('ERROR something else'))

    def test_above_the_cap_fails_and_says_split_the_pr(self):
        self.assertIsNone(S.cap_problem({'a.ts': 20, 'b.ts': 15}))
        self.assertIn('split the PR', S.cap_problem({'a.ts': 20, 'b.ts': 16}))


class ReportProblems(unittest.TestCase):
    def test_every_mutant_killed_or_timed_out_passes(self):
        self.assertEqual(S.report_problems(report({'src/add.ts': [mutant('Killed'), mutant('Timeout', 2)]}), 'src/add.ts'), [])

    def test_any_other_status_fails(self):
        for status in ('Survived', 'NoCoverage', 'Ignored', 'RuntimeError', 'CompileError', 'Pending'):
            with self.subTest(status=status):
                self.assertEqual(S.report_problems(report({'src/add.ts': [mutant('Killed'), mutant(status, 7)]}), 'src/add.ts'),
                                 [f'src/add.ts:7 ArithmeticOperator mutant is {status}'])

    def test_r5_probe_an_override_that_fakes_every_kill_still_fails_on_the_untested_file(self):
        """Round 5's probe: PR code exits 1 whenever a mutant is active, so every mutant it
        reaches reads as Killed, and src/lonely.ts has no test. The job fails it."""
        faked = report({'src/add.ts': [mutant('Killed'), mutant('Killed', 2)],
                        'src/lonely.ts': [mutant('Killed', covered=()), mutant('Killed', 2, covered=())]})
        self.assertEqual(S.report_problems(faked, 'src/add.ts'), [])
        self.assertEqual(S.report_problems(faked, 'src/lonely.ts'), ['src/lonely.ts: no test covers it; add a test that runs the changed lines'])

    def test_a_file_missing_from_the_report_fails(self):
        self.assertEqual(S.report_problems(report({'src/add.ts': [mutant('Killed')]}), 'src/b.ts'), ['src/b.ts: not in the Stryker report'])


# Stands in for stryker.js: a count config prints FAKE_COUNT mutants; a run config logs the
# file it mutates and writes FAKE_REPORT as the JSON report.
FAKE_STRYKER = """
const fs = require('fs');
const config = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
if (config.dryRunOnly) {
  console.log(`Instrumented 1 source file(s) with ${process.env.FAKE_COUNT} mutant(s)`);
} else {
  fs.appendFileSync(process.env.FAKE_RUNS, config.mutate.join(',') + '\\n');
  fs.writeFileSync(config.jsonReporter.fileName, process.env.FAKE_REPORT);
}
"""


class Cli(unittest.TestCase):
    """main() on a real job tree with a fake stryker.js."""

    def run_cli(self, specs, count=2, files=None, extra=()):
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root)
        for name in sorted(S.STRYKER_PACKAGES | set(extra)):
            (root / '@stryker-mutator' / name).mkdir(parents=True)
            (root / '@stryker-mutator' / name / 'package.json').write_text(json.dumps({'version': PIN}))
        (root / '@stryker-mutator' / 'core' / 'bin').mkdir()
        (root / '@stryker-mutator' / 'core' / 'bin' / 'stryker.js').write_text(FAKE_STRYKER)
        runs = root / 'runs.log'
        runs.touch()
        env = {**os.environ, 'RUNNER_TEMP': str(root), 'FAKE_COUNT': str(count), 'FAKE_RUNS': str(runs),
               'FAKE_REPORT': json.dumps(report(files or {}))}
        run = subprocess.run([sys.executable, str(SCRIPT), str(root), specs], capture_output=True, text=True, env=env)
        return run.returncode, run.stdout, runs.read_text().split()

    def test_each_changed_file_gets_its_own_run_and_a_clean_report_passes(self):
        files = {'src/a.ts': [mutant('Killed')], 'src/b.ts': [mutant('Timeout')]}
        code, out, runs = self.run_cli('src/a.ts:1-1,src/b.ts:2-2,src/a.ts:5-5', files=files)
        self.assertEqual((code, runs), (0, ['src/a.ts:1-1,src/a.ts:5-5', 'src/b.ts:2-2']), out)

    def test_r5_probe_faked_kills_with_an_untested_file_fail(self):
        files = {'src/add.ts': [mutant('Killed')], 'src/lonely.ts': [mutant('Killed', covered=())]}
        code, out, _ = self.run_cli('src/add.ts:1-1,src/lonely.ts:1-1', files=files)
        self.assertEqual(code, 1)
        self.assertIn('::error::src/lonely.ts: no test covers it', out)

    def test_above_the_cap_fails_before_any_run(self):
        code, out, runs = self.run_cli('src/a.ts:1-1,src/b.ts:1-1', count=18)
        self.assertEqual((code, runs), (1, []))
        self.assertIn('36 TypeScript mutants, more than 35', out)
        self.assertIn('split the PR', out)

    def test_a_bad_job_tree_fails_before_any_run(self):
        code, out, runs = self.run_cli('src/a.ts:1-1', extra=['forge'])
        self.assertEqual((code, runs), (1, []))
        self.assertIn('::error::@stryker-mutator/forge is not a package the job installs', out)
        self.assertNotIn('mutant(s)', out)


if __name__ == '__main__':
    unittest.main()
