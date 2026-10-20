"""gate/mutation for changed TypeScript: the job, not the PR, decides the result.

Stryker, its vitest runner and vitest come from the job's own tree, installed from the
committed scripts/process/stryker/package-lock.json; mutation.sh points the PR's
node_modules/vitest at that vitest. So the PR's overrides and lockfile cannot change any
module Stryker core loads. Each changed file gets its own Stryker run with per-test
coverage. The job then reads Stryker's JSON report: a file that no test covers, or any
mutant that is not Killed or Timeout (Survived, NoCoverage, Ignored, an error), fails.
More than MAX_TS_MUTANTS mutants fails before any run: split the PR.

Usage: stryker_scope.py <job node_modules> <specs>
specs: mutation_targets.py's comma-separated path:start-end. Run from the PR checkout.
Prints one ::error:: line per problem and exits 1, or exits 0.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

PINNED = '9.6.1'
STRYKER_PACKAGES = frozenset({'api', 'core', 'instrumenter', 'util', 'vitest-runner'})
# #232 took about 78 s per mutant; the Stryker step has 55 minutes.
MAX_TS_MUTANTS = 35
PASSING = frozenset({'Killed', 'Timeout'})
INSTRUMENTED = re.compile(r'Instrumented \d+ source file\(s\) with (\d+) mutant\(s\)')


def stryker_scope_problems(entries, pinned):
    """entries: (name, version, is_link) for each folder in the job's @stryker-mutator
    scope. Every pinned package must be there, at the pinned version, as a real folder,
    and nothing else."""
    problems = [f'@stryker-mutator/{name} is not a package the job installs' for name, _, _ in entries if name not in STRYKER_PACKAGES]
    problems += [f'@stryker-mutator/{name} is a link' for name, _, link in entries if link]
    problems += [f'@stryker-mutator/{name} is {version}, not {pinned}' for name, version, _ in entries
                 if name in STRYKER_PACKAGES and version != pinned]
    missing = STRYKER_PACKAGES - {name for name, _, _ in entries}
    problems += [f'@stryker-mutator/{name} is missing' for name in sorted(missing)]
    return problems


def by_file(specs):
    """{path: [path:start-end, ...]} in the order the files first appear."""
    files = {}
    for spec in specs.split(','):
        files.setdefault(re.sub(r':\d+-\d+$', '', spec), []).append(spec)
    return files


def count_config(mutate):
    """Instruments only: a dry run of a no-op command, so the count costs no test run."""
    return {'testRunner': 'command', 'commandRunner': {'command': 'node -e 0'}, 'plugins': [], 'mutate': mutate,
            'dryRunOnly': True, 'coverageAnalysis': 'off', 'reporters': []}


def stryker_config(mutate, report):
    """One changed file. The vitest runner runs the tests related to it with per-test
    coverage; one run at a time and a 3x timeout keep a slow run from timing out as a
    kill. The JSON report is the job's input."""
    return {'testRunner': 'vitest', 'plugins': ['@stryker-mutator/vitest-runner'], 'mutate': mutate,
            'coverageAnalysis': 'perTest', 'thresholds': {'high': 100, 'low': 100, 'break': 100},
            'concurrency': 1, 'timeoutFactor': 3, 'reporters': ['clear-text', 'progress', 'json'],
            'jsonReporter': {'fileName': report}}


def mutant_count(log):
    """Mutants Stryker instrumented, from its log; None when the line is missing."""
    match = INSTRUMENTED.search(log)
    return int(match[1]) if match else None


def cap_problem(counts):
    total = sum(counts.values())
    if total > MAX_TS_MUTANTS:
        return (f'{total} TypeScript mutants, more than {MAX_TS_MUTANTS} (about 78 s each in a 55-minute step); '
                f'split the PR so each part mutates fewer')
    return None


def report_problems(report, path):
    """The job's verdict on one file from Stryker's JSON report."""
    entry = next((value for name, value in report.get('files', {}).items() if Path(name).as_posix().endswith(path)), None)
    if entry is None:
        return [f'{path}: not in the Stryker report']
    mutants = entry['mutants']
    if mutants and not any(mutant.get('coveredBy') for mutant in mutants):
        return [f'{path}: no test covers it; add a test that runs the changed lines']
    return [f"{path}:{mutant['location']['start']['line']} {mutant['mutatorName']} mutant is {mutant['status']}"
            for mutant in mutants if mutant['status'] not in PASSING]


def stryker(stryker_js, config, name, capture):
    path = Path(os.environ.get('RUNNER_TEMP', '/tmp')) / f'stryker-{name}.json'
    path.write_text(json.dumps(config))
    return subprocess.run(['node', stryker_js, 'run', str(path)], capture_output=capture, text=True)


def main(argv):
    if len(argv) != 3:
        sys.exit('usage: stryker_scope.py <job node_modules> <specs>')
    tools, files = Path(argv[1]), by_file(argv[2])
    scope = tools / '@stryker-mutator'
    entries = [(d.name, json.loads((d / 'package.json').read_text()).get('version') if (d / 'package.json').exists() else None,
                d.is_symlink()) for d in sorted(scope.iterdir())] if scope.is_dir() else []
    problems = stryker_scope_problems(entries, PINNED)
    stryker_js = str(scope / 'core' / 'bin' / 'stryker.js')
    counts = {}
    for n, (path, mutate) in enumerate(files.items()):
        if problems:
            break
        run = stryker(stryker_js, count_config(mutate), f'count-{n}', capture=True)
        counts[path] = mutant_count(run.stdout + run.stderr)
        if run.returncode or counts[path] is None:
            problems.append(f'{path}: the mutant count failed (exit {run.returncode})')
        print(f'stryker: {path}: {counts[path]} mutant(s)')
    problems += [p for p in [None if problems else cap_problem(counts)] if p]
    blocked = bool(problems)
    for n, (path, mutate) in enumerate(files.items()):
        if blocked or not counts[path]:
            continue
        report = Path(os.environ.get('RUNNER_TEMP', '/tmp')) / f'stryker-report-{n}.json'
        run = stryker(stryker_js, stryker_config(mutate, str(report)), f'run-{n}', capture=False)
        found = report_problems(json.loads(report.read_text()), path) if report.exists() else [f'{path}: Stryker wrote no report (no test runs it, or Stryker failed; see its log)']
        problems += found or ([f'{path}: Stryker exited {run.returncode}'] if run.returncode else [])
    for problem in problems:
        print(f'::error::{problem}')
    print('stryker: ' + (f'{len(problems)} problem(s)' if problems else 'every mutant killed, every file covered'))
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
