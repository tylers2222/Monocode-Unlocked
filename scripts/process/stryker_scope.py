"""gate/mutation: checks the package tree Stryker runs from, and writes its config.

Stryker core comes from the job's own tree, installed from the committed
scripts/process/stryker/package-lock.json, so a PR's package.json (overrides, extra
@stryker-mutator packages) cannot touch it. Core runs the PR's tests only as a child
process (the command runner), so no PR module loads in its process.

Usage: stryker_scope.py check <job node_modules> <pinned Stryker version>
         prints one ::error:: line per problem and exits 1, or prints "stryker-scope: ok"
       stryker_scope.py config <specs>
         prints the Stryker config for mutation_targets.py's comma-separated path:start-end
"""
import json
import re
import shlex
import sys
from pathlib import Path

STRYKER_PACKAGES = frozenset({'api', 'core', 'instrumenter', 'util'})


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


def stryker_config(specs):
    """Mutates only the changed lines. The command runner runs the tests that import a
    changed file (vitest related) as a child process in Stryker's sandbox, so Stryker loads
    no test-runner plugin and no PR module. A surviving mutant fails the run."""
    mutate = specs.split(',')
    files = list(dict.fromkeys(re.sub(r':\d+-\d+$', '', spec) for spec in mutate))
    return {'testRunner': 'command', 'commandRunner': {'command': shlex.join(['node_modules/.bin/vitest', 'related', '--run', *files])},
            'plugins': [], 'mutate': mutate, 'thresholds': {'high': 100, 'low': 100, 'break': 100}, 'concurrency': 2,
            'coverageAnalysis': 'off', 'reporters': ['clear-text', 'progress']}


def main(argv):
    if argv[1:2] == ['config'] and len(argv) == 3:
        print(json.dumps(stryker_config(argv[2])))
        return 0
    if argv[1:2] != ['check'] or len(argv) != 4:
        sys.exit('usage: stryker_scope.py check <job node_modules> <pinned version> | config <specs>')
    scope, pinned = Path(argv[2]) / '@stryker-mutator', argv[3]
    entries = [(d.name, json.loads((d / 'package.json').read_text()).get('version') if (d / 'package.json').exists() else None,
                d.is_symlink()) for d in sorted(scope.iterdir())] if scope.is_dir() else []
    problems = stryker_scope_problems(entries, pinned)
    for problem in problems:
        print(f'::error::{problem}')
    print('stryker-scope: ' + (f'{len(problems)} problem(s)' if problems else 'ok'))
    return 1 if problems else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
