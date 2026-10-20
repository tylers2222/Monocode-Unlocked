"""gate/mutation: checks the two package trees Stryker runs with.

Stryker and its vitest runner come from the job's own tree, installed from the
committed scripts/process/stryker/package-lock.json, so a PR's package.json
(overrides, extra @stryker-mutator packages) cannot touch them. The one package the
job takes from the PR's tree is vitest, because the runner must load the same vitest
instance the PR's tests import; it must be a published vitest from the npm registry.

Usage: stryker_scope.py <job node_modules> <PR node_modules> <pinned Stryker version>
Prints one ::error:: line per problem and exits 1, or prints "stryker-scope: ok".
"""
import json
import sys
from pathlib import Path

STRYKER_PACKAGES = frozenset({'api', 'core', 'instrumenter', 'util', 'vitest-runner'})
REGISTRY = 'https://registry.npmjs.org/'


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


def vitest_problems(lock_entry, is_link):
    """lock_entry: the PR tree's node_modules/.package-lock.json entry for vitest."""
    if is_link or lock_entry is None or lock_entry.get('link'):
        return ['the PR tree has no installed vitest, or it is a link']
    if not str(lock_entry.get('resolved', '')).startswith(REGISTRY) or not lock_entry.get('integrity'):
        return [f'the PR tree\'s vitest does not come from {REGISTRY} ({lock_entry.get("resolved")})']
    return []


def main(argv):
    if len(argv) != 4:
        sys.exit('usage: stryker_scope.py <job node_modules> <PR node_modules> <pinned version>')
    job, pr, pinned = Path(argv[1]), Path(argv[2]), argv[3]
    scope = job / '@stryker-mutator'
    entries = [(d.name, json.loads((d / 'package.json').read_text()).get('version') if (d / 'package.json').exists() else None,
                d.is_symlink()) for d in sorted(scope.iterdir())] if scope.is_dir() else []
    lock = pr / '.package-lock.json'
    packages = json.loads(lock.read_text()).get('packages', {}) if lock.exists() else {}
    problems = stryker_scope_problems(entries, pinned) + vitest_problems(packages.get('node_modules/vitest'), (pr / 'vitest').is_symlink())
    for problem in problems:
        print(f'::error::{problem}')
    print('stryker-scope: ' + (f'{len(problems)} problem(s)' if problems else 'ok'))
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
